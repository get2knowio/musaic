//! The production sources a host (musaic-api's routes, or any other face
//! over these services) hands to the services: the network transport, the
//! fingerprinter, the resolver's owned parts ([`ResolveParts`]), the
//! checks' resolution source, the cover downloader and the check-fix
//! [`FixSources`]. musaic-api re-exports them from its `sources` module.
//!
//! Everything that borrows a `rusqlite::Connection` is built inside the
//! blocking task from owned, `Send + Sync` parts (see
//! [`crate::sources`]).
//!
//! Decisions taken here:
//!
//! * The replay transport is wrapped in [`FollowRedirects`], so a replayed
//!   resolve sees what the live transport (which follows redirects itself)
//!   would return.
//! * Fingerprinting is local and always available unless
//!   the host configures [`Fingerprinting::Unavailable`]; the AcoustID key is
//!   needed only for lookups (Tier 0c reports unavailable without one).
//! * The fix paths' MusicBrainz name search
//!   (`find_cover`, the uncached half of `find_tracklist`) saves a confident
//!   hit as the album's anchor only after the track-count /
//!   duration check ([`crate::sources::find_release_for_fix`]); the Python
//!   app's `find_release` saved every confident hit.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use crate::apply::CoverSource;
use crate::check_fixes::{
    Anchor, CoverMatch, FingerprintFailure, Fingerprinted, FixSources, TracklistMatch,
};
use crate::fingerprint::{
    AcoustIdLookup, Chromaprint, FingerprintEnv, Fingerprinter, fingerprint_album,
};
use crate::sources::{DbAlbums, NetMusicBrainz, ResolveParts};
use musaic_checks::checks::track_gaps::MbTrack as GapTrack;
use musaic_events::Event;
use musaic_net::{
    AcoustIdClient, CoverArtClient, FollowRedirects, ReplayTransport, Request, Response, Transport,
    TransportError,
};
use musaic_resolve::find_release::{self, FindOptions, MbMatch};
use musaic_resolve::source::AlbumSource;
use rusqlite::Connection;

/// The transport every outbound request goes through, shared by the
/// handlers' blocking tasks and the fingerprint pool thread.
pub type SharedTransport = Arc<dyn Transport + Send + Sync>;

/// The resolver's parts over the app's transport.
pub type Parts = ResolveParts<SharedTransport>;

/// Where outbound HTTP goes.
#[derive(Clone)]
pub enum Network {
    /// Every request fails as if the host did not resolve (offline mode). The default for tests.
    Offline,
    /// Answers from recorded traffic (the recorded replay set (private test data)), redirects
    /// followed hop by hop as the live transport would; a miss fails as
    /// unobserved.
    Replay(PathBuf),
    /// Real HTTPS through musaic-net's paced live transport.
    Live(SharedTransport),
}

impl Network {
    /// The paced live transport the binary uses.
    pub fn live() -> Self {
        Network::Live(Arc::new(musaic_net::live::live_paced(
            musaic_net::LiveTransport::new(),
        )))
    }

    /// One shared transport for the app's lifetime.
    pub fn shared(&self) -> SharedTransport {
        match self {
            Network::Offline => Arc::new(OfflineTransport),
            Network::Replay(dir) => Arc::new(FollowRedirects(ReplayTransport::new(dir))),
            Network::Live(t) => Arc::clone(t),
        }
    }
}

struct OfflineTransport;

impl Transport for OfflineTransport {
    fn send(&self, request: &Request) -> Result<Response, TransportError> {
        Err(TransportError::Dns {
            host: request.host().to_string(),
        })
    }
}

/// How tracks are fingerprinted (the pool, Tier 0c, the fix paths).
#[derive(Clone)]
pub enum Fingerprinting {
    /// In-process fingerprinting with this implementation (the app:
    /// [`Fingerprinting::chromaprint`]).
    Local(Arc<dyn Fingerprinter>),
    /// The fingerprinter cannot run: the pool reports `unavailable` with
    /// this reason, Tier 0c and the fix paths get nothing.
    Unavailable(String),
}

impl Fingerprinting {
    /// The in-process Chromaprint port.
    pub fn chromaprint() -> Self {
        Fingerprinting::Local(Arc::new(Chromaprint))
    }

    /// `Err(reason)` when the fingerprinter cannot run.
    pub fn availability(&self) -> Result<(), String> {
        match self {
            Fingerprinting::Local(_) => Ok(()),
            Fingerprinting::Unavailable(r) => Err(r.clone()),
        }
    }

    /// The fingerprinter; for `Unavailable` one that refuses every file.
    pub fn fingerprinter(&self) -> Arc<dyn Fingerprinter> {
        match self {
            Fingerprinting::Local(f) => Arc::clone(f),
            Fingerprinting::Unavailable(r) => Arc::new(Refuse(r.clone())),
        }
    }
}

struct Refuse(String);

impl Fingerprinter for Refuse {
    fn fingerprint(&self, _path: &std::path::Path) -> Result<(String, u64), String> {
        Err(self.0.clone())
    }
    fn implementation(&self) -> &str {
        musaic_db::FINGERPRINT_IMPL_RUST
    }
}

/// Builds the resolver's parts with the AcoustID key as it is now (the
/// Python app read the setting on every call).
pub type PartsFactory = Arc<dyn Fn() -> Parts + Send + Sync>;

/// The checks' view of the resolver (tags-out-of-sync, unidentified-album).
pub struct ChecksResolution {
    pub parts: PartsFactory,
    pub library_dir: Arc<dyn Fn() -> Option<String> + Send + Sync>,
}

impl musaic_checks::resolution::ResolutionSource for ChecksResolution {
    fn resolve(
        &self,
        conn: &Connection,
        album_id: &str,
    ) -> Result<musaic_checks::Resolution, String> {
        let lib = (self.library_dir)();
        let r = (self.parts)().resolve(conn, lib.as_deref(), album_id, "", false)?;
        musaic_checks::Resolution::from_python_json(&r.to_dict())
            .ok_or_else(|| "resolution is not an object".to_string())
    }
}

/// `tags.download_cover` over musaic-net's CAA client (its public-HTTPS and
/// image checks).
pub struct NetCovers {
    pub transport: SharedTransport,
}

impl CoverSource for NetCovers {
    fn download(&self, url: &str) -> Option<(Vec<u8>, String)> {
        CoverArtClient::new(Arc::clone(&self.transport))
            .download(url)
            .ok()
            .map(|c| (c.bytes, c.mime))
    }
}

/// The check fixes' outside world (`fingerprint`, `resolver`,
/// `mb_lookup.find_release` / `find_release_cached`), built per request.
pub struct AppFixSources<'a> {
    pub parts: &'a Parts,
    pub library_dir: Option<&'a str>,
    pub fingerprinting: &'a Fingerprinting,
}

impl AppFixSources<'_> {
    fn mb(&self) -> NetMusicBrainz<SharedTransport> {
        NetMusicBrainz::new(Arc::clone(&self.parts.transport))
    }

    fn record(
        &self,
        conn: &Connection,
        album_id: &str,
    ) -> Option<musaic_resolve::source::AlbumRecord> {
        DbAlbums {
            conn,
            library_dir: self.library_dir,
        }
        .album(album_id)
    }

    fn find(&self, conn: &Connection, album_id: &str, cover: bool, tracklist: bool) -> MbMatch {
        let Some(rec) = self.record(conn, album_id) else {
            return MbMatch::default();
        };
        let blocked = BTreeSet::new();
        // A confident hit is saved as the anchor (with
        // `AlbumMbAnchorSet("musicbrainz")`) only after it passes the
        // track-count and duration check
        // (musaic-services' `find_release_for_fix`).
        crate::sources::find_release_for_fix(
            conn,
            &self.parts.bus,
            &rec,
            FindOptions {
                hint: "",
                blocked: &blocked,
                fetch_cover: cover,
                fetch_tracklist: tracklist,
            },
            &self.mb(),
            &(self.parts.now_iso)(),
        )
    }
}

fn gap_tracks(m: &MbMatch) -> Vec<GapTrack> {
    m.tracklist
        .iter()
        .map(|t| GapTrack {
            title: t.title.clone(),
            position: t.position,
            recording_mbid: t.recording_mbid.clone(),
        })
        .collect()
}

impl FixSources for AppFixSources<'_> {
    fn fingerprint_available(&self) -> bool {
        self.fingerprinting.availability().is_ok()
    }

    fn fingerprint_album(
        &self,
        conn: &mut Connection,
        album_id: &str,
        album_path: &str,
    ) -> Result<Vec<Fingerprinted>, FingerprintFailure> {
        if let Err(reason) = self.fingerprinting.availability() {
            return Err(FingerprintFailure {
                code: "unavailable".into(),
                detail: reason,
            });
        }
        let fingerprinter = self.fingerprinting.fingerprinter();
        let client = self
            .parts
            .acoustid_key
            .as_deref()
            .filter(|k| !k.is_empty())
            .map(|k| AcoustIdClient::new(Arc::clone(&self.parts.transport), k));
        let now = (self.parts.now_iso)();
        let env = FingerprintEnv {
            conn,
            fingerprinter: fingerprinter.as_ref(),
            acoustid: client.as_ref().map(|c| c as &dyn AcoustIdLookup),
            bus: &self.parts.bus,
            now_iso: &now,
            cancel: None,
        };
        match fingerprint_album(&env, album_id, album_path, false) {
            Ok(done) => Ok(done
                .tracks
                .into_iter()
                .map(|t| Fingerprinted {
                    filename: t.filename,
                    acoustid_recording_mbid: t.acoustid_recording_mbid,
                })
                .collect()),
            Err(e) => Err(FingerprintFailure {
                code: e.code().into(),
                detail: e.to_string(),
            }),
        }
    }

    fn resolve_anchor(
        &self,
        conn: &mut Connection,
        album_id: &str,
    ) -> Result<Option<Anchor>, String> {
        let r = self
            .parts
            .resolve(conn, self.library_dir, album_id, "", false)?;
        Ok(r.anchor.as_ref().map(|f| Anchor {
            value: f.value.clone(),
            strategy: r.anchor_strategy.clone(),
            confidence: Some(f.confidence),
        }))
    }

    /// `mb_lookup.find_release(album, fetch_cover=True, fetch_tracklist=False)`.
    fn find_cover(&self, conn: &mut Connection, album_id: &str) -> CoverMatch {
        let m = self.find(conn, album_id, true, false);
        CoverMatch {
            confident: m.confident,
            cover_url: m.cover_url,
            cover_url_verified: m.cover_url_verified,
        }
    }

    /// `mb_lookup.find_release_cached(album, fetch_cover=False,
    /// fetch_tracklist=True)`: a fresh, unblocked anchor skips the search
    /// and fetches the tracklist by MBID (writing back a newly learned
    /// release id, with `AlbumMbAnchorSet("musicbrainz")`, as the Python app
    /// did); otherwise a name search.
    fn find_tracklist(&self, conn: &mut Connection, album_id: &str) -> TracklistMatch {
        let anchor = musaic_db::mb_anchor::get_album_mb_anchor(conn, album_id)
            .ok()
            .flatten()
            .filter(|a| {
                !musaic_resolve::resolver::is_stale(
                    a.last_checked_at.as_deref(),
                    (self.parts.now_epoch)(),
                )
            });
        let Some(a) = anchor else {
            let m = self.find(conn, album_id, false, true);
            return TracklistMatch {
                confident: m.confident,
                tracklist: gap_tracks(&m),
            };
        };
        let score = a.score.unwrap_or(0);
        let mut m = MbMatch {
            confident: score >= find_release::HIGH_CONFIDENCE_SCORE,
            score,
            mbid: Some(a.release_group_mbid.clone()),
            release_mbid: a.release_mbid.clone(),
            ..MbMatch::default()
        };
        let browse = musaic_resolve::source::MusicBrainzSource::browse_releases(
            &self.mb(),
            &a.release_group_mbid,
        );
        if let Ok((rid, tracks)) = browse.and_then(|b| find_release::parse_tracklist(&b)) {
            let previous = m.release_mbid.clone();
            m.release_mbid = Some(rid.clone());
            m.tracklist = tracks;
            if previous.as_deref() != Some(rid.as_str()) {
                let written = musaic_db::mb_anchor::set_album_mb_anchor(
                    conn,
                    album_id,
                    &a.release_group_mbid,
                    Some(&rid),
                    score,
                    &(self.parts.now_iso)(),
                );
                if written.is_ok() {
                    self.parts.bus.publish(&Event::AlbumMbAnchorSet {
                        album_id: album_id.to_string(),
                        source: "musicbrainz".into(),
                    });
                }
            }
        }
        TracklistMatch {
            confident: m.confident,
            tracklist: gap_tracks(&m),
        }
    }
}
