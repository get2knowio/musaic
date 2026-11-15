//! The service functions behind the album / track tag routes and the two
//! `/api/tags` routes of `musaic/api/tags.py` (plus the
//! `services/albums.py` helpers they use: `resolve_album_dict`,
//! `list_album_filenames`, `track_count_for`, `persist_track_tags_sidecar`).
//!
//! Each function returns the route's response body or the [`HttpError`] the
//! route raises, with the Python app's status and detail text, in its
//! check order (the album is resolved before the tag is validated, so an
//! unknown album with a blank tag is a 404). Request-body validation (422)
//! belongs to the HTTP layer. No bus event is published by any of these, as
//! in the Python app. After every mutation the album's track-tag sidecar is
//! rewritten from the database (an empty set deletes it); a failed write is
//! ignored, as the Python app logs and carries on.
//!
//! Deliberate changes from the Python app:
//!
//! - A mutation of a Missing album's tags is refused with 409 and the same
//!   text apply uses ([`crate::apply::missing_album_detail`]), right after
//!   the album lookup and before anything else is checked; reads
//!   (`GET .../tags`) still answer. The Python app wrote the rows and its
//!   sidecar write failed silently.
//! - Every filename a mutation names must be a FLAC of the album
//!   (its current listing, as `list_album_filenames` gives it); anything else
//!   is refused with 400 ([`bad_filename_detail`]), the status of the
//!   Python app's nearest refusal of a filename (delete-duplicate's "Filename
//!   ... is not one of this finding's duplicates"), checked after the tag.
//!   A name with a path separator is never a listed FLAC. One exception
//!   keeps legacy data removable: `DELETE .../tracks/{filename}/tags/{tag}`
//!   also accepts a filename the album already has rows for (rows an older
//!   build stored for any name). The Python app stored any name, `../x`
//!   included, in the database and the sidecar. In `PUT .../tags` the empty
//!   filename stays skipped, as in the Python app.
//!
//! Golden: testdata/golden/services/tags_vocab.json (recorded from the
//! Python app); test: tests/golden_tags_vocab.rs.

use std::collections::BTreeMap;
use std::path::Path;

use musaic_core::pycompat::strip;
use musaic_db::track_tags as db;
use musaic_scan::{TrackOrder, py_resolve};
use rusqlite::Connection;

use crate::albums::{self, album_to_json};
use crate::apply::{HttpError, ResolvedAlbum, refuse_missing};
use crate::json::Json;

/// What the tag routes need.
pub struct TagEnv<'a> {
    pub conn: &'a mut Connection,
    /// The configured library (`config.library_dir`), as stored.
    pub library_dir: Option<&'a str>,
    pub order: TrackOrder,
    /// `cache._now_iso()` for new rows.
    pub now: &'a str,
}

fn err(status: u16, detail: impl Into<String>) -> HttpError {
    HttpError {
        status,
        detail: detail.into(),
    }
}

pub(crate) fn db_err(e: impl std::fmt::Display) -> HttpError {
    err(500, e.to_string())
}

/// `services.albums.resolve_album_dict`: 409 without a library; the cached
/// row (Missing rows included); else a live walk; else 404.
pub fn resolve_album_dict(
    conn: &Connection,
    library_dir: Option<&str>,
    album_id: &str,
    order: TrackOrder,
) -> Result<ResolvedAlbum, HttpError> {
    let Some(lib) = library_dir.filter(|l| !l.is_empty()) else {
        return Err(err(409, "No library directory configured"));
    };
    let resolved = py_resolve(Path::new(lib)).to_string_lossy().into_owned();
    let cached = albums::load_snapshot(conn, &resolved).map_err(db_err)?;
    if let Some(row) = cached.into_iter().find(|r| r.id() == album_id) {
        return Ok(ResolvedAlbum {
            id: row.album.id.clone(),
            path: row.album.path.clone(),
            json: row.json,
        });
    }
    match albums::find_album_by_id(Path::new(lib), album_id, order) {
        Some(a) => Ok(ResolvedAlbum {
            id: a.id.clone(),
            path: a.path.clone(),
            json: album_to_json(&a),
        }),
        None => Err(err(404, "Album not found (try rescanning the library)")),
    }
}

/// `list_album_filenames`: the album's FLAC basenames (musaic-scan listing).
pub fn list_album_filenames(album_dir: &str) -> Vec<String> {
    musaic_scan::list_flacs(Path::new(album_dir))
        .into_iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect()
}

/// `track_count_for`: the row's `track_count` when > 0, else a listing.
pub fn track_count_for(album: &ResolvedAlbum) -> i64 {
    let n = match album.json.get("track_count") {
        Some(Json::Int(n)) => *n,
        _ => 0,
    };
    if n > 0 {
        n
    } else {
        list_album_filenames(&album.path).len() as i64
    }
}

/// `persist_track_tags_sidecar`: rewrite (or delete) the sidecar from the
/// database. Best effort; returns whether it succeeded.
pub fn persist_track_tags_sidecar(conn: &Connection, album_id: &str, album_path: &str) -> bool {
    match db::album_track_tags(conn, album_id) {
        Ok(map) => musaic_fsops::sidecar::write_sidecar(Path::new(album_path), &map),
        Err(_) => false,
    }
}

fn tag_map_json(map: &BTreeMap<String, Vec<String>>) -> Json {
    Json::Obj(
        map.iter()
            .map(|(f, tags)| {
                (
                    f.clone(),
                    Json::Arr(tags.iter().map(|t| Json::from(t.as_str())).collect()),
                )
            })
            .collect(),
    )
}

/// The body of `GET /api/albums/{id}/tags` for an already resolved album.
fn album_tags_body(
    conn: &Connection,
    album_id: &str,
    album: &ResolvedAlbum,
) -> Result<Json, HttpError> {
    let track_tags = db::album_track_tags(conn, album_id).map_err(db_err)?;
    let album_tags =
        db::album_tag_intersection(conn, album_id, track_count_for(album)).map_err(db_err)?;
    Ok(Json::obj([
        ("album_id", Json::from(album_id)),
        ("track_tags", tag_map_json(&track_tags)),
        (
            "album_tags",
            Json::Arr(album_tags.into_iter().map(Json::Str).collect()),
        ),
    ]))
}

/// The refusal text for a filename that is not a FLAC of the album.
pub fn bad_filename_detail(filename: &str) -> String {
    format!(
        "Filename {} is not a FLAC file of this album",
        musaic_fsops::py_repr(filename)
    )
}

/// Refuse (400) the first of `names` that is not one of the
/// album's FLACs, unless `keep` accepts it.
fn check_filenames<'n>(
    album: &ResolvedAlbum,
    names: impl IntoIterator<Item = &'n str>,
    keep: impl Fn(&str) -> bool,
) -> Result<(), HttpError> {
    let listed = list_album_filenames(&album.path);
    for n in names {
        let plain = !n.contains('/') && !n.contains(std::path::MAIN_SEPARATOR);
        if !(plain && listed.iter().any(|l| l == n)) && !keep(n) {
            return Err(err(400, bad_filename_detail(n)));
        }
    }
    Ok(())
}

/// The lookup every mutation starts with: the album (404 / 409 as
/// `resolve_album_dict`), then the refusal of a Missing album.
fn resolve_for_mutation(env: &TagEnv<'_>, album_id: &str) -> Result<ResolvedAlbum, HttpError> {
    let album = resolve_album_dict(env.conn, env.library_dir, album_id, env.order)?;
    refuse_missing(env.conn, album_id)?;
    Ok(album)
}

fn require_tag(tag: &str) -> Result<&str, HttpError> {
    let t = strip(tag);
    if t.is_empty() {
        Err(err(400, "tag is required"))
    } else {
        Ok(t)
    }
}

/// `GET /api/albums/{album_id}/tags`.
pub fn get_album_tags(env: &mut TagEnv<'_>, album_id: &str) -> Result<Json, HttpError> {
    let album = resolve_album_dict(env.conn, env.library_dir, album_id, env.order)?;
    album_tags_body(env.conn, album_id, &album)
}

fn finish(env: &mut TagEnv<'_>, album_id: &str, album: &ResolvedAlbum) -> Result<Json, HttpError> {
    persist_track_tags_sidecar(env.conn, album_id, &album.path);
    album_tags_body(env.conn, album_id, album)
}

/// `PUT /api/albums/{album_id}/tags`: replace wholesale. `track_tags` is the
/// request's map in its key order.
pub fn replace_album_tags(
    env: &mut TagEnv<'_>,
    album_id: &str,
    track_tags: &[(String, Vec<String>)],
) -> Result<Json, HttpError> {
    let album = resolve_for_mutation(env, album_id)?;
    check_filenames(
        &album,
        track_tags
            .iter()
            .map(|(f, _)| f.as_str())
            .filter(|f| !f.is_empty()),
        |_| false,
    )?;
    db::replace_album_track_tags(env.conn, album_id, track_tags, env.now).map_err(db_err)?;
    finish(env, album_id, &album)
}

/// `POST /api/albums/{album_id}/tags`: `filenames` `None` or empty means
/// every FLAC in the album directory (409 when there is none). Given names
/// must be FLACs of the album.
pub fn add_album_tag(
    env: &mut TagEnv<'_>,
    album_id: &str,
    tag: &str,
    filenames: Option<&[String]>,
) -> Result<Json, HttpError> {
    let album = resolve_for_mutation(env, album_id)?;
    let tag = require_tag(tag)?;
    let names: Vec<String> = match filenames {
        Some(f) if !f.is_empty() => f.to_vec(),
        _ => list_album_filenames(&album.path),
    };
    if names.is_empty() {
        return Err(err(409, "album has no tracks"));
    }
    check_filenames(&album, names.iter().map(String::as_str), |_| false)?;
    db::add_album_tag(env.conn, album_id, tag, &names, env.now).map_err(db_err)?;
    finish(env, album_id, &album)
}

/// `DELETE /api/albums/{album_id}/tags/{tag}`.
pub fn remove_album_tag(
    env: &mut TagEnv<'_>,
    album_id: &str,
    tag: &str,
) -> Result<Json, HttpError> {
    let album = resolve_for_mutation(env, album_id)?;
    let tag = require_tag(tag)?;
    db::remove_album_tag(env.conn, album_id, tag).map_err(db_err)?;
    finish(env, album_id, &album)
}

/// `POST /api/albums/{album_id}/tracks/{filename}/tags`.
pub fn add_track_tag(
    env: &mut TagEnv<'_>,
    album_id: &str,
    filename: &str,
    tag: &str,
) -> Result<Json, HttpError> {
    let album = resolve_for_mutation(env, album_id)?;
    let tag = require_tag(tag)?;
    check_filenames(&album, [filename], |_| false)?;
    db::add_track_tag(env.conn, album_id, filename, tag, env.now).map_err(db_err)?;
    finish(env, album_id, &album)
}

/// `DELETE /api/albums/{album_id}/tracks/{filename}/tags/{tag}`: the
/// filename is a FLAC of the album, or one the album has rows for.
pub fn remove_track_tag(
    env: &mut TagEnv<'_>,
    album_id: &str,
    filename: &str,
    tag: &str,
) -> Result<Json, HttpError> {
    let album = resolve_for_mutation(env, album_id)?;
    let tag = require_tag(tag)?;
    let conn: &Connection = env.conn;
    check_filenames(&album, [filename], |n| {
        db::has_rows_for_file(conn, album_id, n).unwrap_or(false)
    })?;
    db::remove_track_tag(env.conn, album_id, filename, tag).map_err(db_err)?;
    finish(env, album_id, &album)
}

/// `GET /api/tags`.
pub fn list_tags(conn: &Connection) -> Result<Json, HttpError> {
    let tags = db::list_all_tags(conn).map_err(db_err)?;
    Ok(Json::obj([(
        "tags",
        Json::Arr(
            tags.into_iter()
                .map(|(t, n)| Json::obj([("tag", Json::Str(t)), ("count", Json::Int(n))]))
                .collect(),
        ),
    )]))
}

/// `GET /api/tags/{tag}/tracks`.
pub fn tracks_for_tag(conn: &Connection, tag: &str) -> Result<Json, HttpError> {
    let tag = require_tag(tag)?;
    let tracks = db::find_tracks_with_tag(conn, tag).map_err(db_err)?;
    Ok(Json::obj([
        ("tag", Json::from(tag)),
        (
            "tracks",
            Json::Arr(
                tracks
                    .into_iter()
                    .map(|(a, f)| {
                        Json::obj([("album_id", Json::Str(a)), ("filename", Json::Str(f))])
                    })
                    .collect(),
            ),
        ),
    ]))
}
