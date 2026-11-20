//! The service functions behind the seven `/api/vocabulary` routes of
//! `musaic/api/tags.py`: list, add, patch, rename, merge, delete and
//! promote-orphan (typed results, `OpError`s; the HTTP face builds the
//! bodies), with the routes' 400 / 409 refusals and the sidecar
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
pub use musaic_db::track_tags::VocabEntry;
use musaic_db::track_tags::{self as db, MergeResult, RenameResult};
use rusqlite::Connection;

use crate::error::OpError;
use crate::track_tags::{conflict, db_err, rejected};

/// The tag vocabulary: its entries in display order, the tags in use that
/// have no entry, and groups of entries a database written under the old
/// ASCII-only rules holds that now fold to one tag (empty when none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vocabulary {
    pub entries: Vec<VocabEntry>,
    /// `(tag, track rows)`.
    pub orphans: Vec<(String, i64)>,
    pub case_conflicts: Vec<Vec<String>>,
}

/// What a rename or a delete did to the track rows. `albums` are the albums
/// whose rows changed, sorted; `sidecars_not_written` those among them whose
/// sidecar could not be (a Missing album's folder is not touched).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagRewrite {
    /// A vocabulary row was renamed (deleted, for a delete).
    pub vocab: bool,
    /// Track rows rewritten (deleted, for a strip).
    pub tag_rows: usize,
    pub albums: Vec<String>,
    pub sidecars_not_written: Vec<String>,
}

/// What a merge did to the track rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagMerge {
    /// Rows actually inserted at the destination.
    pub moved: usize,
    pub albums: Vec<String>,
    pub sidecars_not_written: Vec<String>,
}

/// The vocabulary after a rename.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VocabularyRenamed {
    pub vocabulary: Vocabulary,
    pub renamed: TagRewrite,
}

/// The vocabulary after a merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VocabularyMerged {
    pub vocabulary: Vocabulary,
    pub merged: TagMerge,
}

/// The vocabulary after a delete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VocabularyDeleted {
    pub vocabulary: Vocabulary,
    pub deleted: TagRewrite,
}

/// `GET /api/vocabulary`.
pub fn list_vocabulary(conn: &Connection) -> Result<Vocabulary, OpError> {
    Ok(Vocabulary {
        entries: db::list_vocabulary(conn).map_err(db_err)?,
        orphans: db::find_orphan_tags(conn).map_err(db_err)?,
        case_conflicts: db::case_conflicts(conn).map_err(db_err)?,
    })
}

fn rewrite(r: RenameResult, sidecars_not_written: Vec<String>) -> TagRewrite {
    TagRewrite {
        vocab: r.vocab,
        tag_rows: r.tag_rows,
        albums: r.albums,
        sidecars_not_written,
    }
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
) -> Result<Vocabulary, OpError> {
    let tag = strip(tag);
    if tag.is_empty() {
        return Err(rejected("tag is required"));
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
) -> Result<Vocabulary, OpError> {
    let tag = strip(tag);
    if tag.is_empty() {
        return Err(rejected("tag is required"));
    }
    db::update_vocabulary_entry(conn, tag, description, color, sort_order)
        .map_err(db_err)?
        .map_err(|e| conflict(e.0))?;
    list_vocabulary(conn)
}

/// `POST /api/vocabulary/{tag}/rename`: refused when either is blank, in
/// conflict when another entry already has the new name.
pub fn rename_vocabulary_entry(
    conn: &mut Connection,
    tag: &str,
    new_tag: &str,
) -> Result<VocabularyRenamed, OpError> {
    let (tag, new_tag) = (strip(tag), strip(new_tag));
    if tag.is_empty() || new_tag.is_empty() {
        return Err(rejected("tag and new_tag are required"));
    }
    let r = db::rename_vocabulary_entry(conn, tag, new_tag)
        .map_err(db_err)?
        .map_err(|e| conflict(e.0))?;
    let not_written = resync_sidecars_for_albums(conn, &r.albums);
    Ok(VocabularyRenamed {
        vocabulary: list_vocabulary(conn)?,
        renamed: rewrite(r, not_written),
    })
}

/// `POST /api/vocabulary/{tag}/merge`: every refusal is a `Rejected`.
pub fn merge_vocabulary_entry(
    conn: &mut Connection,
    tag: &str,
    into_tag: &str,
) -> Result<VocabularyMerged, OpError> {
    let (tag, into_tag) = (strip(tag), strip(into_tag));
    if tag.is_empty() || into_tag.is_empty() {
        return Err(rejected("tag and into_tag are required"));
    }
    let MergeResult { moved, albums } = db::merge_vocabulary_entries(conn, tag, into_tag)
        .map_err(db_err)?
        .map_err(|e| rejected(e.0))?;
    let sidecars_not_written = resync_sidecars_for_albums(conn, &albums);
    Ok(VocabularyMerged {
        vocabulary: list_vocabulary(conn)?,
        merged: TagMerge {
            moved,
            albums,
            sidecars_not_written,
        },
    })
}

/// `DELETE /api/vocabulary/{tag}?strip_from_tracks=`.
pub fn delete_vocabulary_entry(
    conn: &mut Connection,
    tag: &str,
    strip_from_tracks: bool,
) -> Result<VocabularyDeleted, OpError> {
    let tag = strip(tag);
    if tag.is_empty() {
        return Err(rejected("tag is required"));
    }
    let r = db::delete_vocabulary_entry(conn, tag, strip_from_tracks)
        .map_err(db_err)?
        .map_err(|e| rejected(e.0))?;
    let not_written = if strip_from_tracks {
        resync_sidecars_for_albums(conn, &r.albums)
    } else {
        Vec::new()
    };
    Ok(VocabularyDeleted {
        vocabulary: list_vocabulary(conn)?,
        deleted: rewrite(r, not_written),
    })
}

/// `POST /api/vocabulary/orphans/{tag}/promote`: add with defaults.
pub fn promote_orphan_tag(
    conn: &mut Connection,
    now: &str,
    tag: &str,
) -> Result<Vocabulary, OpError> {
    let tag = strip(tag);
    if tag.is_empty() {
        return Err(rejected("tag is required"));
    }
    db::add_vocabulary_entry(conn, tag, "", None, None, now).map_err(db_err)?;
    list_vocabulary(conn)
}
