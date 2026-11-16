//! Production implementations of `musaic-resolve`'s source traits.
//!
//! * [`NetMusicBrainz`]: [`MusicBrainzSource`] over `musaic-net`'s
//!   MusicBrainz and Cover Art Archive clients, generic over the transport
//!   (the paced live one from `LiveClients`, or the replay one wrapped in
//!   `FollowRedirects`).
//! * [`DbAlbums`]: [`AlbumSource`] over the library database and the files
//!   on disk (`services.albums.cached_album_dict`, `list_album_filenames`,
//!   `read_tracks`).
//! * [`AcoustIdFingerprints`]: [`FingerprintSource`] for Tier 0c. As in the
//!   Python app's `_try_fingerprint_vote`, it first runs
//!   [`fingerprint_album`](crate::fingerprint::fingerprint_album) (cached
//!   rows when complete, with any lookup that is due made from the stored
//!   fingerprint; otherwise computed, looked up, stored and
//!   announced), then hands the resolver every AcoustID result of every
//!   track: the answers that call just received, and a fresh
//!   lookup of the stored fingerprint for the others (as the Python app's vote
//!   does; that answer is not stored).
//!
//! Threading: everything here borrows a `rusqlite::Connection`, which is
//! not `Sync`, so a host builds these inside the blocking task
//! (`spawn_blocking`) from owned, `Send + Sync + 'static` parts: a
//! transport (`LivePaced`, or `Arc<FollowRedirects<ReplayTransport>>`), an
//! AcoustID key `String`, an `Arc<dyn Fingerprinter>`, a `Bus` (clone) and
//! a clock. [`ResolveParts`] bundles them and is checked to be all three.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use musaic_events::{Bus, Event};
use musaic_net::{AcoustIdClient, CoverArtClient, FrontCover, MusicBrainzClient, Transport};
use musaic_resolve::source::{
    AlbumRecord, AlbumSource, FingerprintLookups, FingerprintSource, FlacTrack, MusicBrainzSource,
    NetError, TrackLookup, TrackNumber,
};
use musaic_resolve::{AlbumResolution, Env, Resolver, db_store::DbStore};
use musaic_scan::{flacmeta, list_flacs, py_resolve};
use rusqlite::Connection;

use crate::clock::Clock;
use crate::fingerprint::{
    AcoustIdLookup, FingerprintEnv, Fingerprinter, NO_KEY_REASON, fingerprint_album, net_error,
};

/// [`MusicBrainzSource`] over the `musaic-net` clients.
#[derive(Debug, Clone)]
pub struct NetMusicBrainz<T> {
    pub musicbrainz: MusicBrainzClient<T>,
    pub cover_art: CoverArtClient<T>,
    /// Set when a MusicBrainz request failed for a reason other than an
    /// answer (network, server error, rate limit, unreadable body): what
    /// was resolved through this source may be incomplete.
    pub failed: Arc<AtomicBool>,
}

impl<T: Transport + Clone> NetMusicBrainz<T> {
    /// Both clients on one transport, at the real origins.
    pub fn new(transport: T) -> Self {
        Self {
            musicbrainz: MusicBrainzClient::new(transport.clone()),
            cover_art: CoverArtClient::new(transport),
            failed: Arc::default(),
        }
    }
}

/// A failure that says nothing about MusicBrainz's data: anything but an
/// answer of "not found" or another client error.
fn is_outage(e: &NetError) -> bool {
    match e {
        NetError::Http(s) => *s == 429 || *s >= 500,
        NetError::Network(_) | NetError::Parse(_) => true,
    }
}

impl<T: Transport> NetMusicBrainz<T> {
    fn body(&self, req: musaic_net::Request) -> Result<Vec<u8>, NetError> {
        let r = self.musicbrainz.body(req).map_err(net_error);
        if let Err(e) = &r
            && is_outage(e)
        {
            self.failed.store(true, Ordering::SeqCst);
        }
        r
    }
}

type Mbc = MusicBrainzClient<musaic_net::ReplayTransport>;

impl<T: Transport> MusicBrainzSource for NetMusicBrainz<T> {
    fn search_release_groups(&self, artist: &str, title: &str) -> Result<Vec<u8>, NetError> {
        self.body(Mbc::search_request(artist, title))
    }

    fn browse_releases(&self, release_group: &str) -> Result<Vec<u8>, NetError> {
        self.body(Mbc::browse_request(release_group))
    }

    fn browse_release_dates(&self, release_group: &str) -> Result<Vec<u8>, NetError> {
        self.body(Mbc::release_dates_request(release_group))
    }

    fn lookup_release_group(&self, release_group: &str) -> Result<Vec<u8>, NetError> {
        self.body(Mbc::release_group_request(release_group))
    }

    fn cover_front_status(&self, release_group: &str) -> Result<u16, NetError> {
        match self.cover_art.check_front(release_group) {
            FrontCover::Verified { .. } => Ok(200),
            FrontCover::Missing => Ok(404),
            FrontCover::Status(s) => Ok(s),
            FrontCover::Failed(e) => Err(NetError::Network(e.to_string())),
        }
    }
}

/// The check fixes' MusicBrainz name search
/// (`mb_lookup.find_release(album, fetch_cover=..., fetch_tracklist=...)`).
/// Runs [`musaic_resolve::find_release::find_release_for_fix`]; when that
/// says so (a confident hit that passed the track-count and duration
/// check) the anchor is saved with `mb_last_checked_at = now_iso` and
/// `AlbumMbAnchorSet(album_id, "musicbrainz")` is published, as the Python
/// app did for every confident hit. A failed write is not raised and
/// publishes nothing (the Python app logged it). Returns the match, which the fix path
/// uses exactly as before.
pub fn find_release_for_fix(
    conn: &Connection,
    bus: &Bus,
    album: &AlbumRecord,
    opts: musaic_resolve::find_release::FindOptions<'_>,
    mb: &dyn MusicBrainzSource,
    now_iso: &str,
) -> musaic_resolve::find_release::MbMatch {
    use musaic_resolve::find_release::FixAnchor;
    let found = musaic_resolve::find_release::find_release_for_fix(album, opts, mb);
    if let FixAnchor::Save {
        album_id,
        release_group,
        release,
        score,
    } = &found.anchor
    {
        let written = musaic_db::mb_anchor::set_album_mb_anchor(
            conn,
            album_id,
            release_group,
            release.as_deref(),
            *score,
            now_iso,
        );
        if written.is_ok() {
            bus.publish(&Event::AlbumMbAnchorSet {
                album_id: album_id.clone(),
                source: "musicbrainz".into(),
            });
        }
    }
    found.matched
}

/// Album rows of the configured library plus the files on disk.
pub struct DbAlbums<'a> {
    pub conn: &'a Connection,
    /// The configured library directory; an album of another library (or
    /// none configured) is "not in the cache".
    pub library_dir: Option<&'a str>,
}

/// `services.albums.cached_album_dict(album_id)`: the row, when it belongs
/// to the configured library (Missing or not). Never walks the disk.
pub fn cached_album(
    conn: &Connection,
    library_dir: Option<&str>,
    album_id: &str,
) -> Option<crate::albums::AlbumRow> {
    let lib = library_dir.filter(|l| !l.is_empty())?;
    let lib = py_resolve(Path::new(lib)).to_string_lossy().into_owned();
    let owner: String = conn
        .query_row(
            "SELECT library_path FROM albums WHERE id = ?1",
            [album_id],
            |r| r.get(0),
        )
        .ok()?;
    if owner != lib {
        return None;
    }
    crate::albums::get_album_by_id(conn, album_id).ok()?
}

fn album_dir(conn: &Connection, album_id: &str) -> Option<String> {
    musaic_db::renames::album_path(conn, album_id)
        .ok()
        .flatten()
}

/// `Path.name` of a listed file.
pub fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

impl AlbumSource for DbAlbums<'_> {
    fn album(&self, album_id: &str) -> Option<AlbumRecord> {
        let row = cached_album(self.conn, self.library_dir, album_id)?;
        let mbid = row
            .json
            .get("mb_release_group_mbid")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        Some(AlbumRecord::from_album(&row.album, mbid))
    }

    fn filenames(&self, album: &AlbumRecord) -> Vec<String> {
        album_dir(self.conn, &album.id)
            .map(|d| {
                list_flacs(Path::new(&d))
                    .iter()
                    .map(|p| file_name(p))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn tracks(&self, album: &AlbumRecord) -> Vec<FlacTrack> {
        let Some(dir) = album_dir(self.conn, &album.id) else {
            return Vec::new();
        };
        list_flacs(Path::new(&dir))
            .into_iter()
            .map(|p| match flacmeta::read(&p) {
                Ok(m) => FlacTrack {
                    filename: file_name(&p),
                    tracknumber: m
                        .values("tracknumber")
                        .next()
                        .map_or(TrackNumber::Missing, |t| TrackNumber::Text(t.to_string())),
                    duration: Some(musaic_core::pycompat::round1(m.info.length())),
                },
                Err(_) => FlacTrack {
                    filename: file_name(&p),
                    tracknumber: TrackNumber::Missing,
                    duration: None,
                },
            })
            .collect()
    }
}

/// Tier 0c's fingerprint lookups (see the module documentation).
pub struct AcoustIdFingerprints<'a> {
    pub conn: &'a Connection,
    pub fingerprinter: &'a dyn Fingerprinter,
    /// `None` without an AcoustID key: Tier 0c is unavailable.
    pub acoustid: Option<&'a dyn AcoustIdLookup>,
    pub bus: &'a Bus,
    pub now_iso: &'a str,
}

impl FingerprintSource for AcoustIdFingerprints<'_> {
    fn lookups(&self, album: &AlbumRecord) -> FingerprintLookups {
        let Some(acoustid) = self.acoustid else {
            return FingerprintLookups::Unavailable(NO_KEY_REASON.to_owned());
        };
        let Some(dir) = album_dir(self.conn, &album.id) else {
            return FingerprintLookups::Unavailable(format!("no album row for {}", album.id));
        };
        let env = FingerprintEnv {
            conn: self.conn,
            fingerprinter: self.fingerprinter,
            acoustid: Some(acoustid),
            bus: self.bus,
            now_iso: self.now_iso,
            cancel: None,
        };
        // The Python app logs a FingerprintError here and skips the vote.
        let mut done = match fingerprint_album(&env, &album.id, &dir, false) {
            Ok(d) => d,
            Err(e) => return FingerprintLookups::Unavailable(e.to_string()),
        };
        FingerprintLookups::Tracks(
            done.tracks
                .iter()
                .map(|t| TrackLookup {
                    filename: t.filename.clone(),
                    results: done
                        .lookups
                        .remove(&t.filename)
                        .unwrap_or_else(|| acoustid.lookup(&t.fingerprint, t.duration_s as u64)),
                })
                .collect(),
        )
    }
}

/// The owned parts of one resolve, all `Send + Sync + 'static`, so a host
/// can move (or share) them into `spawn_blocking` and build the borrowing
/// sources there.
#[derive(Clone)]
pub struct ResolveParts<T> {
    pub resolver: Arc<Resolver>,
    /// MusicBrainz, CAA and AcoustID requests.
    pub transport: T,
    /// The AcoustID key, when set (read from the secrets store by the host).
    pub acoustid_key: Option<String>,
    pub fingerprinter: Arc<dyn Fingerprinter>,
    pub bus: Bus,
    /// `_now_iso()` for anchor and fingerprint rows.
    pub now_iso: Clock,
    /// Seconds since the Unix epoch (anchor staleness).
    pub now_epoch: Arc<dyn Fn() -> i64 + Send + Sync>,
}

impl<T: Transport + Clone> ResolveParts<T> {
    /// `resolver_service.resolve_album(album_id, user_hint=hint,
    /// force=force)` against `conn`; anchors written on the way are
    /// published as `AlbumMbAnchorSet`, fingerprints as `TrackFingerprinted`.
    pub fn resolve(
        &self,
        conn: &Connection,
        library_dir: Option<&str>,
        album_id: &str,
        hint: &str,
        force: bool,
    ) -> Result<Arc<AlbumResolution>, String> {
        self.resolve_checked(conn, library_dir, album_id, hint, force)
            .map(|(r, _)| r)
    }

    /// [`Self::resolve`], also saying whether a MusicBrainz request failed
    /// on the way (the resolution may then be incomplete). Such a
    /// resolution is not kept in the resolver's memo, so the next call asks
    /// again rather than serving it as an answer.
    ///
    /// An `AlbumMbAnchorSet` is published only when the album's anchor
    /// changed (another release group or release), not for a refresh that
    /// only re-stamps the time and score of the same match: nothing a
    /// subscriber caches depends on those, and a background sweep
    /// refreshing many anchors would otherwise make the UI reload the
    /// album list over and over.
    pub fn resolve_checked(
        &self,
        conn: &Connection,
        library_dir: Option<&str>,
        album_id: &str,
        hint: &str,
        force: bool,
    ) -> Result<(Arc<AlbumResolution>, bool), String> {
        let identity = |id: &str| {
            musaic_db::mb_anchor::get_album_mb_anchor(conn, id)
                .ok()
                .flatten()
                .map(|a| (a.release_group_mbid, a.release_mbid))
        };
        let before = identity(album_id);
        let now_iso = (self.now_iso)();
        let mb = NetMusicBrainz::new(self.transport.clone());
        let albums = DbAlbums { conn, library_dir };
        let store = DbStore {
            conn,
            now_iso: now_iso.clone(),
            read: Default::default(),
        };
        let client = self
            .acoustid_key
            .as_deref()
            .filter(|k| !k.is_empty())
            .map(|k| AcoustIdClient::new(self.transport.clone(), k));
        let fingerprints = AcoustIdFingerprints {
            conn,
            fingerprinter: self.fingerprinter.as_ref(),
            acoustid: client.as_ref().map(|c| c as &dyn AcoustIdLookup),
            bus: &self.bus,
            now_iso: &now_iso,
        };
        let env = Env {
            albums: &albums,
            store: &store,
            mb: &mb,
            fingerprints: &fingerprints,
            now_epoch_s: (self.now_epoch)(),
        };
        let resolved = self
            .resolver
            .resolve(&env, album_id, hint, force)
            .map_err(|e| e.to_string())?;
        let failed = mb.failed.load(Ordering::SeqCst);
        if failed {
            self.resolver.invalidate(Some(album_id));
        }
        for ev in &resolved.events {
            if ev.album_id == album_id && identity(album_id) == before {
                continue;
            }
            self.bus.publish(&Event::AlbumMbAnchorSet {
                album_id: ev.album_id.clone(),
                source: ev.source.clone(),
            });
        }
        Ok((resolved.resolution, failed))
    }
}
