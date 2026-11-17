//! Library and album operations behind `musaic/api/library.py` and
//! `services/albums.py`: dropping a library's snapshot,
//! the cached album list with its scan time and the Python app's sort, the
//! album lookup with the Python app's semantics (cached row, Missing or not;
//! else a live walk), a track listing read from the files, removing an
//! album from the library, clearing an album's MusicBrainz anchor, setting
//! the library directory, the album list route and an album's resolution.
//! This lives here rather than in musaic-api's `routes/library.rs` so a
//! non-HTTP face reads and changes the library the same way. The host takes
//! the restore gate's work guard and renders the answers; each error
//! converts into the host's error type where it happens (a refusal is an
//! [`HttpError`]).

use std::path::Path;

use musaic_core::pycompat::round1;
use musaic_events::{Bus, Event};
use musaic_resolve::Resolver;
use musaic_scan::{TrackOrder, py_resolve};
use rusqlite::{Connection, OptionalExtension};

use crate::albums::{AlbumRow, album_to_json, find_album_by_id, load_snapshot};
use crate::apply::HttpError;
use crate::json::Json;
use crate::sources::{cached_album, file_name};

/// 409 detail without a configured library.
pub const NO_LIBRARY: &str = "No library directory configured";

fn resolved(lib: &str) -> String {
    py_resolve(Path::new(lib)).to_string_lossy().into_owned()
}

fn refuse(status: u16, detail: impl Into<String>) -> HttpError {
    HttpError {
        status,
        detail: detail.into(),
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
) -> Result<(String, String, Json), HttpError> {
    let Some(lib) = library_dir.filter(|l| !l.is_empty()) else {
        return Err(refuse(409, NO_LIBRARY));
    };
    if let Some(row) =
        cached_album(conn, Some(lib), album_id).map_err(|e| refuse(500, e.to_string()))?
    {
        return Ok((row.album.id.clone(), row.album.path.clone(), row.json));
    }
    match find_album_by_id(Path::new(lib), album_id, order) {
        Some(a) => Ok((a.id.clone(), a.path.clone(), album_to_json(&a))),
        None => Err(refuse(404, "Album not found (try rescanning the library)")),
    }
}

/// `GET /api/albums/{id}`: the album in the library's snapshot, else a live
/// walk of `library_dir`, else 404.
pub fn get_album<E: From<rusqlite::Error> + From<HttpError>>(
    conn: &Connection,
    library_dir: &str,
    album_id: &str,
    order: TrackOrder,
) -> Result<Json, E> {
    let (_, rows) = snapshot(conn, library_dir)?;
    if let Some(r) = rows.into_iter().find(|r| r.id() == album_id) {
        return Ok(r.json);
    }
    match find_album_by_id(Path::new(library_dir), album_id, order) {
        Some(a) => Ok(album_to_json(&a)),
        None => Err(E::from(refuse(404, "Album not found"))),
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
    let dir = Path::new(album_dir);
    if !dir.is_dir() {
        return Json::Arr(Vec::new());
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
    Json::Arr(out)
}

/// `DELETE /api/albums/{id}` (the only path that drops an album row): 404
/// unless the album is cached for `library_dir`; the connection is closed
/// before `AlbumDeleted` is published. The caller holds a work guard.
pub fn delete_album<E: From<musaic_db::Error> + From<rusqlite::Error> + From<HttpError>>(
    conn: Connection,
    bus: &Bus,
    library_dir: Option<&str>,
    album_id: &str,
) -> Result<(), E> {
    if cached_album(&conn, library_dir, album_id)?.is_none() {
        return Err(E::from(refuse(
            404,
            format!("Album {} not in cache", musaic_fsops::py_repr(album_id)),
        )));
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
pub fn clear_mb_anchor<E: From<musaic_db::Error> + From<HttpError>>(
    conn: Connection,
    library_dir: impl FnOnce() -> Option<String>,
    order: TrackOrder,
    resolver: &Resolver,
    bus: &Bus,
    now_iso: impl FnOnce() -> String,
    album_id: &str,
) -> Result<Option<String>, E> {
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

/// `POST /api/config`: expand `~`, refuse a path that is not a directory
/// (400), store the library directory, then drop its snapshot (the next
/// scan re-reads it). Returns the stored `library_dir`. `open` opens a
/// database connection (only when a library is stored); `save_failed`
/// maps a failure to write the config.
pub fn set_library_dir<E: From<HttpError> + From<musaic_db::Error>>(
    config: &musaic_core::config::ConfigDir,
    home: Option<&Path>,
    dir: &str,
    open: impl FnOnce() -> Result<Connection, E>,
    save_failed: impl FnOnce(std::io::Error) -> E,
) -> Result<Option<String>, E> {
    let target = musaic_core::config::expand_user(dir, home);
    if !target.is_dir() {
        return Err(refuse(400, format!("Not a directory: {}", py_path_str(&target))).into());
    }
    let cfg = config
        .set_library_dir(&target.to_string_lossy(), home)
        .map_err(save_failed)?;
    if let Some(lib) = cfg.library_dir.as_deref() {
        let mut conn = open()?;
        drop_snapshot(&mut conn, lib)?;
    }
    Ok(cfg.library_dir)
}

/// The configured library directory, which must exist: 409 without one
/// ([`NO_LIBRARY`]) or when it is not a directory.
pub fn configured_library(library_dir: Option<String>) -> Result<String, HttpError> {
    let lib = library_dir
        .filter(|l| !l.is_empty())
        .ok_or_else(|| refuse(409, NO_LIBRARY))?;
    if !Path::new(&lib).is_dir() {
        return Err(refuse(
            409,
            format!("Configured library directory does not exist: {lib}"),
        ));
    }
    Ok(lib)
}

/// `GET /api/albums`: the library's cached albums in the Python app's sort,
/// after dropping the snapshot first when `refresh`.
pub fn list_albums<E: From<musaic_db::Error> + From<rusqlite::Error>>(
    conn: &mut Connection,
    lib: &str,
    refresh: bool,
) -> Result<Json, E> {
    if refresh {
        drop_snapshot(conn, lib)?;
    }
    let (scanned_at, rows) = snapshot(conn, lib)?;
    let mut albums: Vec<Json> = rows.into_iter().map(|r| r.json).collect();
    sort_albums(&mut albums);
    Ok(Json::obj([
        ("library_dir", Json::from(lib)),
        ("count", Json::from(albums.len())),
        ("scanned_at", Json::from(scanned_at)),
        ("albums", Json::Arr(albums)),
    ]))
}

/// `GET /api/albums/{id}/resolution`: the album must resolve (409 / 404,
/// [`resolve_album_dict`]); then the resolver's answer as the Python app's
/// `to_dict()`. `library_dir` is read once, for the lookup and the
/// resolve alike. Tier 0c is reachable here: with an
/// AcoustID key the parts fingerprint the album (stored,
/// `TrackFingerprinted`) and vote. `parts` builds the resolver's parts
/// (reading the AcoustID key) only once the album has resolved; `failed`
/// maps a resolver failure.
#[allow(clippy::too_many_arguments)]
pub fn album_resolution<E: From<HttpError>>(
    conn: &Connection,
    library_dir: &dyn Fn() -> Option<String>,
    order: TrackOrder,
    parts: &dyn Fn() -> crate::production::Parts,
    album_id: &str,
    hint: &str,
    force: bool,
    failed: impl FnOnce(String) -> E,
) -> Result<serde_json::Value, E> {
    let lib = library_dir();
    resolve_album_dict(conn, lib.as_deref(), album_id, order)?;
    let r = parts()
        .resolve(conn, lib.as_deref(), album_id, hint, force)
        .map_err(failed)?;
    Ok(r.to_dict())
}
