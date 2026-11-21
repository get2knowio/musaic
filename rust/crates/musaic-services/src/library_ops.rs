//! Library and album operations behind `musaic/api/library.py` and
//! `services/albums.py`: dropping a library's snapshot,
//! the cached album list with its scan time and the Python app's sort, the
//! album lookup with the Python app's semantics (cached row, Missing or not;
//! else a live walk), a track listing read from the files, removing an
//! album from the library, clearing an album's MusicBrainz anchor, setting
//! the library directory, the album list route and an album's resolution.
//! This lives here rather than in musaic-api's `routes/library.rs` so a
//! non-HTTP face reads and changes the library the same way. The operations
//! (`musaic_app::ops::library`) take the restore gate's work guard; a face
//! renders the typed answers. Every failure is an [`OpError`].

use std::path::Path;

use musaic_core::pycompat::round1;
use musaic_events::{Bus, Event};
use musaic_resolve::Resolver;
use musaic_scan::{TrackOrder, py_resolve};
use rusqlite::{Connection, OptionalExtension};

use crate::albums::{AlbumRow, album_to_json, find_album_by_id, load_snapshot};
use crate::error::{NoLibraryKind, OpError, Subject};
use crate::json::Json;
use crate::sources::{cached_album, file_name};

/// 409 detail without a configured library.
pub const NO_LIBRARY: &str = "No library directory configured";

fn resolved(lib: &str) -> String {
    py_resolve(Path::new(lib)).to_string_lossy().into_owned()
}

/// No library folder is configured.
pub fn no_library() -> OpError {
    OpError::NoLibrary {
        kind: NoLibraryKind::NotConfigured,
        message: NO_LIBRARY.to_string(),
    }
}

fn album_not_found(album_id: &str, message: String) -> OpError {
    OpError::NotFound {
        details: None,
        what: Subject::Album,
        id: album_id.to_string(),
        message,
    }
}

/// `cache.drop_snapshot(library_dir)`: the library's albums are
/// re-read on the next scan; nothing else is touched.
pub fn drop_snapshot(conn: &mut Connection, library_dir: &str) -> musaic_db::Result<()> {
    musaic_db::albums::drop_snapshot(conn, &resolved(library_dir))
}

/// `services.albums.sort_albums`.
///
/// Not [`musaic_core::album_fields::display_artist`]: the Python app's sort
/// key is `(a.get("album_artist") or a.get("artist") or "").lower()`, with
/// no `strip()` (an album artist of only spaces wins over the artist), and
/// a non-string value counts as empty here.
pub fn sort_albums(rows: &mut [Json]) {
    fn s(j: &Json, k: &str) -> String {
        match j.get(k) {
            Some(Json::Str(v)) => v.clone(),
            _ => String::new(),
        }
    }
    rows.sort_by_cached_key(|a| {
        let artist = {
            let aa = s(a, "album_artist");
            if aa.is_empty() { s(a, "artist") } else { aa }
        };
        (
            artist.to_lowercase(),
            s(a, "year"),
            s(a, "title").to_lowercase(),
        )
    });
}

/// The library's cached albums and its `scanned_at` (`""` when never
/// scanned; a database error is returned).
pub fn snapshot(conn: &Connection, library_dir: &str) -> rusqlite::Result<(String, Vec<AlbumRow>)> {
    let lib = resolved(library_dir);
    let scanned_at: Option<String> = conn
        .query_row(
            "SELECT scanned_at FROM libraries WHERE path = ?1",
            [&lib],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    let rows = load_snapshot(conn, &lib)?;
    Ok((scanned_at.unwrap_or_default(), rows))
}

/// `services.albums.resolve_album_dict` with the Python app's semantics (the
/// cached row, Missing or not; else a live walk; else 404): `(id, path,
/// album dict)`.
///
/// Not [`crate::apply::resolve_album`], which the apply paths use: that one
/// refuses a Missing album (409), finds the cached row by loading
/// the library's whole snapshot (a database error is a 500), while this
/// one looks the id up directly ([`cached_album`]: the row must belong to
/// the configured library); a database error is a 500, as there.
pub fn resolve_album_dict(
    conn: &Connection,
    library_dir: Option<&str>,
    album_id: &str,
    order: TrackOrder,
) -> Result<(String, String, Json), OpError> {
    let Some(lib) = library_dir.filter(|l| !l.is_empty()) else {
        return Err(no_library());
    };
    if let Some(row) = cached_album(conn, Some(lib), album_id).map_err(OpError::failed)? {
        return Ok((row.album.id.clone(), row.album.path.clone(), row.json));
    }
    match find_album_by_id(Path::new(lib), album_id, order) {
        Some(a) => Ok((a.id.clone(), a.path.clone(), album_to_json(&a))),
        None => Err(album_not_found(
            album_id,
            "Album not found (try rescanning the library)".to_string(),
        )),
    }
}

/// `GET /api/albums/{id}`: the album in the library's snapshot, else a live
/// walk of `library_dir`, else 404.
pub fn get_album(
    conn: &Connection,
    library_dir: &str,
    album_id: &str,
    order: TrackOrder,
) -> Result<Json, OpError> {
    let (_, rows) = snapshot(conn, library_dir)?;
    if let Some(r) = rows.into_iter().find(|r| r.id() == album_id) {
        return Ok(r.json);
    }
    match find_album_by_id(Path::new(library_dir), album_id, order) {
        Some(a) => Ok(album_to_json(&a)),
        None => Err(album_not_found(album_id, "Album not found".to_string())),
    }
}

/// `services.albums.read_tracks(album_dir)`.
pub fn read_tracks(album_dir: &str) -> Json {
    tracks_json(album_dir, false)
}

/// The track list `GET /api/albums/{id}/tracks` returns: [`read_tracks`]
/// plus each file's ALBUM tag (`album`, `""` when absent). The album's
/// stored title prefers the folder name, so the album panel compares a
/// proposed title with these tags, the values an apply would change: a
/// case-only correction of the tag shows, and stops showing once applied.
pub fn read_tracks_for_panel(album_dir: &str) -> Json {
    tracks_json(album_dir, true)
}

fn tracks_json(album_dir: &str, with_album: bool) -> Json {
    Json::Arr(track_rows(album_dir, with_album))
}

/// The track list [`read_tracks_for_panel`] returns, one dict per file.
pub fn panel_track_rows(album_dir: &str) -> Vec<Json> {
    track_rows(album_dir, true)
}

fn track_rows(album_dir: &str, with_album: bool) -> Vec<Json> {
    let dir = Path::new(album_dir);
    if !dir.is_dir() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for p in musaic_scan::list_flacs(dir) {
        let filename = file_name(&p);
        match musaic_scan::flacmeta::read(&p) {
            Ok(m) => {
                let first = |k: &str| m.values(k).next().unwrap_or("").to_string();
                let opt = |k: &str| {
                    let v = first(k);
                    if v.is_empty() {
                        Json::Null
                    } else {
                        Json::Str(v)
                    }
                };
                let bpm = first("bpm");
                let bpm = if bpm.is_empty() {
                    Json::Null
                } else {
                    bpm.trim().parse::<f64>().map_or(Json::Null, Json::Float)
                };
                let mut track = vec![
                    ("filename", Json::from(filename)),
                    ("tracknumber", Json::from(first("tracknumber"))),
                    ("title", Json::from(first("title"))),
                    ("artist", Json::from(first("artist"))),
                ];
                if with_album {
                    track.push(("album", Json::from(first("album"))));
                }
                track.extend([
                    ("duration", Json::Float(round1(m.info.length()))),
                    ("bpm", bpm),
                    ("replaygain_track_gain", opt("replaygain_track_gain")),
                    ("replaygain_track_peak", opt("replaygain_track_peak")),
                    ("replaygain_album_gain", opt("replaygain_album_gain")),
                    ("replaygain_album_peak", opt("replaygain_album_peak")),
                ]);
                out.push(Json::obj(track));
            }
            Err(e) => out.push(Json::obj([
                ("filename", Json::from(filename)),
                ("error", Json::from(e.to_string())),
            ])),
        }
    }
    out
}

/// `DELETE /api/albums/{id}` (one of the two paths that drop an album row;
/// the other is a user-confirmed merge, `musaic_db::merge::merge_album_rows`):
/// 404 unless the album is cached for `library_dir`; the connection is
/// closed before `AlbumDeleted` is published. The caller holds a work guard.
pub fn delete_album(
    conn: Connection,
    bus: &Bus,
    library_dir: Option<&str>,
    album_id: &str,
) -> Result<(), OpError> {
    if cached_album(&conn, library_dir, album_id)?.is_none() {
        return Err(album_not_found(
            album_id,
            format!("Album {} not in cache", musaic_fsops::py_repr(album_id)),
        ));
    }
    musaic_db::albums::delete_album(&conn, album_id)?;
    drop(conn);
    bus.publish(&Event::AlbumDeleted {
        album_id: album_id.to_string(),
    });
    Ok(())
}

/// `DELETE /api/albums/{id}/mb-anchor`: block the current release group
/// (when there is one) so the resolver does not pick it again, clear the
/// anchor, then (connection closed) drop the resolver's memo and publish
/// `AlbumTagsChanged(mb_release_group_mbid)`. The album must resolve
/// ([`resolve_album_dict`], `library_dir` read after the connection was
/// opened). `now_iso` is asked only when a release group is blocked.
/// Returns the blocked release group. The caller holds a work guard.
#[allow(clippy::too_many_arguments)]
pub fn clear_mb_anchor(
    conn: Connection,
    library_dir: impl FnOnce() -> Option<String>,
    order: TrackOrder,
    resolver: &Resolver,
    bus: &Bus,
    now_iso: impl FnOnce() -> String,
    album_id: &str,
) -> Result<Option<String>, OpError> {
    resolve_album_dict(&conn, library_dir().as_deref(), album_id, order)?;
    let rejected = musaic_db::mb_anchor::get_album_mb_anchor(&conn, album_id)?
        .map(|a| a.release_group_mbid)
        .filter(|r| !r.is_empty());
    if let Some(rg) = &rejected {
        musaic_db::mb_anchor::add_blocked_mb_release_group(&conn, album_id, rg, &now_iso())?;
    }
    musaic_db::mb_anchor::clear_album_mb_anchor(&conn, album_id)?;
    drop(conn);
    resolver.invalidate(Some(album_id));
    bus.publish(&Event::AlbumTagsChanged {
        album_id: album_id.to_string(),
        fields: vec!["mb_release_group_mbid".into()],
    });
    Ok(rejected)
}

/// `str(Path(s))`: duplicate slashes and `.` components dropped, trailing
/// slash removed (`..` kept).
pub fn py_path_str(p: &Path) -> String {
    let mut out = std::path::PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    let s = out.to_string_lossy().into_owned();
    if s.is_empty() { ".".into() } else { s }
}

/// What [`set_library_dir`] stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryChoice {
    /// The stored `library_dir`.
    pub library_dir: Option<String>,
    /// The chosen folder is the one already known under another spelling
    /// (another case or Unicode normalisation on a volume that treats them
    /// as equal), so the known string was kept.
    pub kept_stored_spelling: bool,
}

/// A library string already in use that names the same folder as `target`
/// under a different spelling. Identity is the filesystem's (same device and
/// inode), never a string comparison: case-insensitive and normalising
/// volumes make one folder look like two. The configured folder wins over
/// older `libraries` rows; a row whose path no longer opens cannot be the
/// same file and is skipped.
fn known_spelling(
    current: Option<&str>,
    conn: &Connection,
    resolved: &str,
    target: &Path,
) -> Result<Option<String>, musaic_db::Error> {
    let mut candidates: Vec<String> = current.map(str::to_owned).into_iter().collect();
    candidates.extend(musaic_db::albums::library_paths(conn)?);
    Ok(candidates.into_iter().find(|c| {
        !c.is_empty()
            && c != resolved
            && same_file::is_same_file(Path::new(c), target).unwrap_or(false)
    }))
}

/// `POST /api/config`'s folder checks: expand `~`, refuse a path that is not
/// a directory or cannot be listed (nothing is stored: the previous folder
/// stays in effect). Asked after the work guard, as the route always did.
pub fn check_library_target(dir: &str, home: Option<&Path>) -> Result<std::path::PathBuf, OpError> {
    let target = musaic_core::config::expand_user(dir, home);
    if !target.is_dir() {
        return Err(OpError::Rejected {
            message: format!("Not a directory: {}", py_path_str(&target)),
            details: None,
        });
    }
    if let Err(e) = std::fs::read_dir(&target) {
        return Err(OpError::Rejected {
            message: format!("Cannot read folder: {} ({e})", py_path_str(&target)),
            details: None,
        });
    }
    Ok(target)
}

/// `POST /api/config`: [`check_library_target`], store the library
/// directory, then drop its snapshot (the next scan re-reads it). Only a
/// change is checked: a folder that is already configured and has since gone
/// away (an unmounted drive) is never touched here. A folder that is the
/// same file as one already known, spelled differently, keeps the known
/// string: every album id and `library_path` derives from it, so a new
/// spelling would orphan the rows. Returns the stored `library_dir`. `open`
/// opens a database connection; a failure to write the config is
/// `Internal`.
pub fn set_library_dir(
    config: &musaic_core::config::ConfigDir,
    home: Option<&Path>,
    dir: &str,
    open: impl FnOnce() -> Result<Connection, OpError>,
) -> Result<LibraryChoice, OpError> {
    let target = check_library_target(dir, home)?;
    let mut conn = open()?;
    let current = config.load().library_dir;
    let resolved = musaic_core::config::resolve_path(&target).map_err(OpError::internal)?;
    let kept = match resolved.to_str() {
        Some(r) => known_spelling(current.as_deref(), &conn, r, &target)?,
        None => None,
    };
    let (library_dir, kept_stored_spelling) = match kept {
        Some(known) => {
            if current.as_deref() != Some(known.as_str()) {
                config
                    .set_library_dir(&known, home)
                    .map_err(OpError::internal)?;
            }
            (Some(known), true)
        }
        None => (
            config
                .set_library_dir(&target.to_string_lossy(), home)
                .map_err(OpError::internal)?
                .library_dir,
            false,
        ),
    };
    if let Some(lib) = library_dir.as_deref() {
        drop_snapshot(&mut conn, lib)?;
    }
    Ok(LibraryChoice {
        library_dir,
        kept_stored_spelling,
    })
}

/// The configured library directory, which must exist: [`OpError::NoLibrary`]
/// without one ([`NO_LIBRARY`]) or when it is not a directory.
pub fn configured_library(library_dir: Option<String>) -> Result<String, OpError> {
    let lib = library_dir
        .filter(|l| !l.is_empty())
        .ok_or_else(no_library)?;
    if !Path::new(&lib).is_dir() {
        return Err(OpError::NoLibrary {
            kind: NoLibraryKind::Missing,
            message: format!("Configured library directory does not exist: {lib}"),
        });
    }
    Ok(lib)
}

/// The library's cached albums in the Python app's order, as
/// [`list_albums`] returns them.
#[derive(Debug, Clone, PartialEq)]
pub struct AlbumListing {
    pub library_dir: String,
    /// `""` when never scanned.
    pub scanned_at: String,
    /// Each album as its dict, in the Python app's sort.
    pub albums: Vec<Json>,
}

/// `GET /api/albums`: the library's cached albums in the Python app's sort,
/// after dropping the snapshot first when `refresh`.
pub fn list_albums(
    conn: &mut Connection,
    lib: &str,
    refresh: bool,
) -> Result<AlbumListing, OpError> {
    if refresh {
        drop_snapshot(conn, lib)?;
    }
    let (scanned_at, rows) = snapshot(conn, lib)?;
    let mut albums: Vec<Json> = rows.into_iter().map(|r| r.json).collect();
    sort_albums(&mut albums);
    Ok(AlbumListing {
        library_dir: lib.to_string(),
        scanned_at,
        albums,
    })
}

/// The resolution the resolver already holds for the album, without
/// resolving: no fingerprinting, no network, no work guard and no database
/// write (the agent read tools' accessor, FR-053). The album must be a row of
/// the configured library (`NotFound`); `None` when nothing is memoized for
/// the album's current content ([`musaic_resolve::Resolver::memoized`]: its
/// row and file names as a resolve would read them, so an entry memoized
/// before the album changed is never reported).
pub fn stored_album_resolution(
    conn: &Connection,
    library_dir: Option<&str>,
    resolver: &musaic_resolve::Resolver,
    album_id: &str,
) -> Result<Option<std::sync::Arc<musaic_resolve::AlbumResolution>>, OpError> {
    if library_dir.is_none_or(str::is_empty) {
        return Err(no_library());
    }
    match crate::sources::cached_album(conn, library_dir, album_id).map_err(OpError::failed)? {
        Some(_) => Ok(resolver.memoized(&crate::sources::DbAlbums { conn, library_dir }, album_id)),
        None => Err(album_not_found(album_id, "Album not found".to_string())),
    }
}

/// `GET /api/albums/{id}/resolution`: the album must resolve (409 / 404,
/// [`resolve_album_dict`]); then the resolver's answer. `library_dir` is
/// read once, for the lookup and the resolve alike. Tier 0c is reachable
/// here: with an AcoustID key the parts fingerprint the album (stored,
/// `TrackFingerprinted`) and vote. `parts` builds the resolver's parts
/// (reading the AcoustID key) only once the album has resolved. A resolver
/// failure is `Internal`.
pub fn album_resolution(
    conn: &Connection,
    library_dir: &dyn Fn() -> Option<String>,
    order: TrackOrder,
    parts: &dyn Fn() -> crate::production::Parts,
    album_id: &str,
    hint: &str,
    force: bool,
) -> Result<musaic_resolve::AlbumResolution, OpError> {
    let lib = library_dir();
    resolve_album_dict(conn, lib.as_deref(), album_id, order)?;
    let r = parts()
        .resolve(conn, lib.as_deref(), album_id, hint, force)
        .map_err(OpError::internal)?;
    Ok((*r).clone())
}

// ---------------------------------------------------------------- renames

/// What the rename of an album needs besides its id.
pub struct RenameParts<'a> {
    pub conn: &'a mut Connection,
    pub bus: &'a Bus,
    /// `config.library_dir` as stored.
    pub library_dir: Option<&'a str>,
    pub now: &'a str,
    pub track_order: TrackOrder,
}

/// Carry out a rename plan on an album and publish the `AlbumRenamed` it
/// produced: the Missing album is refused (409) before anything is touched,
/// and a call that renamed nothing is a `Conflict` carrying the rename
/// code's own reason. The filename-mismatch fix and the agent bridge share
/// this, so both publish the same events.
pub fn rename_album_with_plan(
    parts: RenameParts<'_>,
    album_id: &str,
    plan: &[musaic_fsops::rename::PlanEntry],
    dir_rename: Option<&musaic_fsops::rename::PlanEntry>,
) -> Result<musaic_fsops::rename::RenameOutcome, OpError> {
    crate::track_tags::refuse_missing(parts.conn, album_id)?;
    let renv = musaic_fsops::rename::RenameEnv {
        library_dir: parts.library_dir,
        now: parts.now,
        track_order: parts.track_order,
    };
    let out = musaic_fsops::rename::execute_renames(parts.conn, &renv, album_id, plan, dir_rename);
    if let Some(ev) = &out.album_renamed {
        parts.bus.publish(&Event::AlbumRenamed {
            old_album_id: ev.old_album_id.clone(),
            new_album_id: ev.new_album_id.clone(),
        });
    }
    let dir_renamed = out.detail.as_ref().is_some_and(|d| d.dir_renamed);
    if !out.ok && out.renamed == 0 && !dir_renamed {
        let msg = if out.message.is_empty() {
            "Rename failed".to_string()
        } else {
            out.message.clone()
        };
        // The rename code's own code and sentence ride beside `message`.
        let details = match (out.reason, &out.user_message) {
            (Some(r), Some(u)) => Some(crate::error::Details::Refusal(Box::new(
                crate::error::Refusal {
                    reason_code: r.code().to_string(),
                    user_message: u.clone(),
                    blockers: Vec::new(),
                    digest: None,
                },
            ))),
            _ => None,
        };
        return Err(OpError::Conflict {
            message: msg,
            details,
        });
    }
    Ok(out)
}

/// Rename an album's files and folder to the user's templates: the plan is
/// derived now, from the files and the database
/// ([`musaic_fsops::plan::derive_plan`]), and carried out by
/// [`rename_album_with_plan`]. A Missing album is refused (`Conflict`); an
/// album that already matches its templates is `Ok` with nothing renamed.
pub fn rename_album_to_templates(
    parts: RenameParts<'_>,
    album_id: &str,
) -> Result<musaic_fsops::rename::RenameOutcome, OpError> {
    use musaic_fsops::rename::PlanEntry;
    crate::track_tags::refuse_missing(parts.conn, album_id)?;
    let plan = musaic_fsops::plan::derive_plan(parts.conn, album_id)
        .map_err(OpError::failed)?
        .ok_or_else(|| album_not_found(album_id, "Album not found".to_string()))?;
    if let Some(e) = &plan.error {
        return Err(OpError::Conflict {
            message: e.clone(),
            details: None,
        });
    }
    if plan.album_missing {
        return Err(OpError::Conflict {
            message: "The album folder is not on disk".into(),
            details: None,
        });
    }
    if plan.is_empty() {
        return Ok(musaic_fsops::rename::RenameOutcome::nothing_to_do());
    }
    let files: Vec<PlanEntry> = plan
        .renames
        .iter()
        .map(|r| PlanEntry {
            old: r.filename.clone(),
            new: r.expected.clone(),
        })
        .collect();
    let dir = plan.dir_rename.as_ref().map(|d| PlanEntry {
        old: d.old.clone(),
        new: d.new.clone(),
    });
    rename_album_with_plan(parts, album_id, &files, dir.as_ref())
}
