//! The service functions behind the seven `/api/vocabulary` routes of
//! `musaic/api/tags.py`: list, add, patch, rename, merge, delete and
//! promote-orphan, with the routes' 400 / 409 refusals and the sidecar
//! resync (`services.albums.resync_sidecars_for_albums`) after any mutation
//! that touched `track_tags` rows. No bus event, as in the Python app.
//!
//! Deliberate changes from the Python app:
//!
//! - A case-only rename respells the rows in place, an identical
//!   rename is a no-op (`rename_result {vocab: false, tag_rows: 0, albums:
//!   []}`); see [`musaic_db::track_tags::rename_vocabulary_entry`].
//! - One Unicode-aware case fold for every vocabulary question
//!   ([`musaic_db::track_tags::fold`]). A database written under the old
//!   ASCII-only rules may hold entries that are now one tag ("Été" and
//!   "été"); they are never merged silently: `GET /api/vocabulary` (and every
//!   body built on it) then carries `case_conflicts: [[spelling, ...], ...]`
//!   (absent when there is none), names that address several of them are
//!   refused, and a merge of one twin into the other resolves them.
//! - Rows of a Missing album are renamed / merged / stripped with
//!   everyone else's (the database stays consistent), but its sidecar is not
//!   written while it is Missing, and a sidecar write that fails (directory
//!   gone, permissions) is no longer silent: the result object
//!   (`rename_result` / `merge_result` / `delete_result`) then carries
//!   `sidecars_not_written: [album id, ...]` (absent when every write
//!   succeeded). A Missing album keeps its stale sidecar until it returns
//!   (see [`resync_sidecars_for_albums`]).
//!
//! Golden: testdata/golden/services/tags_vocab.json; test:
//! tests/golden_tags_vocab.rs.

use std::path::Path;

use musaic_core::pycompat::strip;
use musaic_db::track_tags::{self as db, RenameResult, VocabEntry};
use rusqlite::Connection;

use crate::apply::HttpError;
use crate::json::Json;
use crate::track_tags::db_err;

fn err(status: u16, detail: impl Into<String>) -> HttpError {
    HttpError {
        status,
        detail: detail.into(),
    }
}

fn entry_json(e: VocabEntry) -> Json {
    Json::obj([
        ("tag", Json::Str(e.tag)),
        ("description", Json::Str(e.description)),
        ("color", e.color.map_or(Json::Null, Json::Str)),
        ("sort_order", Json::Int(e.sort_order)),
        ("created_at", Json::Str(e.created_at)),
        ("count", Json::Int(e.count)),
    ])
}

/// `GET /api/vocabulary`: `{vocabulary, orphans}`, plus `case_conflicts`
/// when entries written under the old rules fold alike.
pub fn list_vocabulary(conn: &Connection) -> Result<Json, HttpError> {
    let vocab = db::list_vocabulary(conn).map_err(db_err)?;
    let orphans = db::find_orphan_tags(conn).map_err(db_err)?;
    let conflicts = db::case_conflicts(conn).map_err(db_err)?;
    let mut body = Json::obj([
        (
            "vocabulary",
            Json::Arr(vocab.into_iter().map(entry_json).collect()),
        ),
        (
            "orphans",
            Json::Arr(
                orphans
                    .into_iter()
                    .map(|(t, n)| Json::obj([("tag", Json::Str(t)), ("count", Json::Int(n))]))
                    .collect(),
            ),
        ),
    ]);
    if !conflicts.is_empty() {
        body.set(
            "case_conflicts",
            Json::Arr(
                conflicts
                    .into_iter()
                    .map(|g| Json::Arr(g.into_iter().map(Json::Str).collect()))
                    .collect(),
            ),
        );
    }
    Ok(body)
}

fn with(mut body: Json, key: &str, value: Json) -> Json {
    body.set(key, value);
    body
}

fn albums_json(ids: &[String]) -> Json {
    Json::Arr(ids.iter().map(|s| Json::from(s.as_str())).collect())
}

fn rename_json(r: &RenameResult, not_written: &[String]) -> Json {
    with_not_written(
        Json::obj([
            ("vocab", Json::Bool(r.vocab)),
            ("tag_rows", Json::from(r.tag_rows)),
            ("albums", albums_json(&r.albums)),
        ]),
        not_written,
    )
}

/// Name the albums whose sidecar was not written (a Missing album's folder
/// is not touched).
fn with_not_written(mut result: Json, not_written: &[String]) -> Json {
    if !not_written.is_empty() {
        result.set("sidecars_not_written", albums_json(not_written));
    }
    result
}

/// `resync_sidecars_for_albums`: rewrite each listed album's sidecar from
/// the database (deleting it when the album has no tags left). Albums whose
/// row is gone are skipped. Returns the albums whose sidecar was NOT
/// written, sorted: a Missing album (its folder is not touched while it is
/// Missing) and any album whose write failed. The Python app
/// logged a failure and carried on, and wrote into a Missing album's
/// folder when it happened to be there.
///
/// Candidate unknown (reported): the sidecar of a Missing album keeps the
/// old tags. If its rows were all stripped meanwhile, the scan that finds it
/// again imports that stale sidecar (the import runs for albums with no
/// rows), bringing the stripped tags back.
pub fn resync_sidecars_for_albums(conn: &Connection, album_ids: &[String]) -> Vec<String> {
    let Ok(rows) = db::albums_for_resync(conn, album_ids) else {
        let mut all = album_ids.to_vec();
        all.sort();
        return all;
    };
    let mut failed: Vec<String> = rows
        .into_iter()
        .filter(|(id, path, missing_since)| {
            let missing = missing_since.as_deref().is_some_and(|m| !m.is_empty());
            missing
                || !match db::album_track_tags(conn, id) {
                    Ok(map) => musaic_fsops::sidecar::write_sidecar(Path::new(path), &map),
                    Err(_) => false,
                }
        })
        .map(|(id, _, _)| id)
        .collect();
    failed.sort();
    failed
}

/// `POST /api/vocabulary`. `description` `None` stands for the payload
/// default `""`. A case-insensitive duplicate is not an error.
pub fn add_vocabulary_entry(
    conn: &mut Connection,
    now: &str,
    tag: &str,
    description: Option<&str>,
    color: Option<&str>,
    sort_order: Option<i64>,
) -> Result<Json, HttpError> {
    let tag = strip(tag);
    if tag.is_empty() {
        return Err(err(400, "tag is required"));
    }
    db::add_vocabulary_entry(conn, tag, description.unwrap_or(""), color, sort_order, now)
        .map_err(db_err)?;
    list_vocabulary(conn)
}

/// `PATCH /api/vocabulary/{tag}`: an unknown tag changes nothing (200); a
/// tag several entries fold to is refused with 409.
pub fn patch_vocabulary_entry(
    conn: &mut Connection,
    tag: &str,
    description: Option<&str>,
    color: Option<&str>,
    sort_order: Option<i64>,
) -> Result<Json, HttpError> {
    let tag = strip(tag);
    if tag.is_empty() {
        return Err(err(400, "tag is required"));
    }
    db::update_vocabulary_entry(conn, tag, description, color, sort_order)
        .map_err(db_err)?
        .map_err(|e| err(409, e.0))?;
    list_vocabulary(conn)
}

/// `POST /api/vocabulary/{tag}/rename`: 400 when either is blank, 409 when
/// another entry already has the new name.
pub fn rename_vocabulary_entry(
    conn: &mut Connection,
    tag: &str,
    new_tag: &str,
) -> Result<Json, HttpError> {
    let (tag, new_tag) = (strip(tag), strip(new_tag));
    if tag.is_empty() || new_tag.is_empty() {
        return Err(err(400, "tag and new_tag are required"));
    }
    let r = db::rename_vocabulary_entry(conn, tag, new_tag)
        .map_err(db_err)?
        .map_err(|e| err(409, e.0))?;
    let not_written = resync_sidecars_for_albums(conn, &r.albums);
    Ok(with(
        list_vocabulary(conn)?,
        "rename_result",
        rename_json(&r, &not_written),
    ))
}

/// `POST /api/vocabulary/{tag}/merge`: every refusal is a 400.
pub fn merge_vocabulary_entry(
    conn: &mut Connection,
    tag: &str,
    into_tag: &str,
) -> Result<Json, HttpError> {
    let (tag, into_tag) = (strip(tag), strip(into_tag));
    if tag.is_empty() || into_tag.is_empty() {
        return Err(err(400, "tag and into_tag are required"));
    }
    let r = db::merge_vocabulary_entries(conn, tag, into_tag)
        .map_err(db_err)?
        .map_err(|e| err(400, e.0))?;
    let not_written = resync_sidecars_for_albums(conn, &r.albums);
    let result = with_not_written(
        Json::obj([
            ("moved", Json::from(r.moved)),
            ("albums", albums_json(&r.albums)),
        ]),
        &not_written,
    );
    Ok(with(list_vocabulary(conn)?, "merge_result", result))
}

/// `DELETE /api/vocabulary/{tag}?strip_from_tracks=`.
pub fn delete_vocabulary_entry(
    conn: &mut Connection,
    tag: &str,
    strip_from_tracks: bool,
) -> Result<Json, HttpError> {
    let tag = strip(tag);
    if tag.is_empty() {
        return Err(err(400, "tag is required"));
    }
    let r = db::delete_vocabulary_entry(conn, tag, strip_from_tracks)
        .map_err(db_err)?
        .map_err(|e| err(400, e.0))?;
    let not_written = if strip_from_tracks {
        resync_sidecars_for_albums(conn, &r.albums)
    } else {
        Vec::new()
    };
    Ok(with(
        list_vocabulary(conn)?,
        "delete_result",
        rename_json(&r, &not_written),
    ))
}

/// `POST /api/vocabulary/orphans/{tag}/promote`: add with defaults.
pub fn promote_orphan_tag(conn: &mut Connection, now: &str, tag: &str) -> Result<Json, HttpError> {
    let tag = strip(tag);
    if tag.is_empty() {
        return Err(err(400, "tag is required"));
    }
    db::add_vocabulary_entry(conn, tag, "", None, None, now).map_err(db_err)?;
    list_vocabulary(conn)
}
