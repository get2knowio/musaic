//! The database side of file and directory renames.
//!
//! Ports of the Python app's `cache.rename_album_directory`,
//! `cache.rename_track_filename`, `cache.get_album_track_tags`, and the two
//! album-row reads `checks/filename_mismatch.py` makes.

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, Transaction};

use crate::Result;

/// Rows moved per table by [`rename_album_directory`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DirMigration {
    pub albums: usize,
    pub track_fingerprints: usize,
    pub track_tags: usize,
    pub check_findings: usize,
    pub ai_jobs: usize,
    /// Not repointed by the Python app; always 0 in its results.
    pub album_blocked_mb_release_groups: usize,
}

/// Move an album row to a new id + path + dir_name and repoint every table
/// that refers to the album by id, in one transaction with foreign keys
/// deferred to COMMIT.
///
/// Tables holding an album id (schema v20):
///
/// | table | FK to albums(id) | repointed |
/// |---|---|---|
/// | track_fingerprints | yes, ON DELETE CASCADE | yes |
/// | track_tags | yes, ON DELETE CASCADE | yes |
/// | album_blocked_mb_release_groups | yes, ON DELETE CASCADE | yes |
/// | fingerprint_rejects (v18) | yes, ON UPDATE / DELETE CASCADE | by the cascade |
/// | check_findings | no (plain column) | yes |
/// | ai_jobs | no (plain column) | yes (`album_path` is a display copy and keeps the old text) |
/// | check_inputs (v19) | no | no: the new id has no inputs, so the sweep checks it again, and prunes the old id's rows |
/// | check_runs.scope | no (JSON text of ids) | no: a historical record of what a run covered |
///
/// Deliberate change: the Python app did not repoint
/// `album_blocked_mb_release_groups`, so an album with a blocklist row failed
/// the deferred foreign-key check at COMMIT and could never be renamed.
/// An error still rolls the whole transaction back and is returned (the
/// caller undoes the on-disk rename).
pub fn rename_album_directory(
    conn: &mut Connection,
    old_id: &str,
    new_id: &str,
    new_path: &str,
    new_dir_name: &str,
) -> Result<DirMigration> {
    let tx = crate::immediate(conn)?;
    let out = move_album_rows(&tx, old_id, new_id, new_path, new_dir_name)?;
    tx.commit()?;
    Ok(out)
}

/// [`rename_album_directory`] on the caller's transaction, so the re-key
/// commits (or rolls back) together with the caller's other writes: a
/// library scan adopting a row does it inside its snapshot save
/// (`albums::save_snapshot_adopting`). Foreign keys are deferred to that
/// transaction's COMMIT; `fingerprint_rejects` follows through its
/// `ON UPDATE CASCADE`.
pub fn move_album_rows(
    tx: &Transaction<'_>,
    old_id: &str,
    new_id: &str,
    new_path: &str,
    new_dir_name: &str,
) -> Result<DirMigration> {
    tx.execute_batch("PRAGMA defer_foreign_keys = ON")?;
    let albums = tx.execute(
        "UPDATE albums SET id = ?1, path = ?2, dir_name = ?3 WHERE id = ?4",
        [new_id, new_path, new_dir_name, old_id],
    )?;
    let moved = |table: &str| {
        tx.execute(
            &format!("UPDATE {table} SET album_id = ?1 WHERE album_id = ?2"),
            [new_id, old_id],
        )
    };
    Ok(DirMigration {
        albums,
        track_fingerprints: moved("track_fingerprints")?,
        track_tags: moved("track_tags")?,
        check_findings: moved("check_findings")?,
        ai_jobs: moved("ai_jobs")?,
        album_blocked_mb_release_groups: moved("album_blocked_mb_release_groups")?,
    })
}

/// Every table besides `albums` that refers to an album by id. A row in one
/// of them under an id no album has is left over from an album that was
/// removed; moving another album onto that id would merge the two.
const ALBUM_ID_HOLDERS: [&str; 7] = [
    "track_fingerprints",
    "track_tags",
    "album_blocked_mb_release_groups",
    "fingerprint_rejects",
    "check_findings",
    "ai_jobs",
    "check_inputs",
];

/// Whether an album row already has `id` or `path`, or any table that
/// refers to albums by id holds a row under `id`.
pub fn album_key_in_use(conn: &Connection, id: &str, path: &str) -> Result<bool> {
    let row: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM albums WHERE id = ?1 OR path = ?2)",
        [id, path],
        |r| r.get(0),
    )?;
    if row {
        return Ok(true);
    }
    for table in ALBUM_ID_HOLDERS {
        let held: bool = conn.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE album_id = ?1)"),
            [id],
            |r| r.get(0),
        )?;
        if held {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Follow a file rename in `track_tags`: copy the old filename's
/// rows to the new one with INSERT OR IGNORE (so tags already on the new name
/// are kept and the sets are OR-ed), then delete the old rows. Returns the
/// number of old rows deleted; 0 without touching the database when either
/// name is empty or they are equal.
pub fn rename_track_filename(
    conn: &mut Connection,
    album_id: &str,
    old_filename: &str,
    new_filename: &str,
) -> Result<usize> {
    if old_filename.is_empty() || new_filename.is_empty() || old_filename == new_filename {
        return Ok(0);
    }
    let tx = crate::immediate(conn)?;
    tx.execute(
        "INSERT OR IGNORE INTO track_tags (album_id, filename, tag, created_at) \
         SELECT album_id, ?1, tag, created_at FROM track_tags \
         WHERE album_id = ?2 AND filename = ?3",
        [new_filename, album_id, old_filename],
    )?;
    let n = tx.execute(
        "DELETE FROM track_tags WHERE album_id = ?1 AND filename = ?2",
        [album_id, old_filename],
    )?;
    tx.commit()?;
    Ok(n)
}

/// Rows moved by [`rename_track_files`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TrackRowsMoved {
    /// Old-name `track_tags` rows removed (moved or merged).
    pub track_tags: usize,
    /// `track_fingerprints` rows moved (0 or 1).
    pub track_fingerprints: usize,
}

/// Follow a file rename with every per-file row, in one transaction:
/// `track_tags` exactly as [`rename_track_filename`] does (union, old rows
/// deleted), `track_fingerprints` (deliberate change;
/// the Python app left the row under the old name) and `fingerprint_rejects`
/// the same way as the fingerprint.
///
/// A fingerprint describes the audio of the file that now carries the new
/// name, so when the old name has a row it replaces any row already stored
/// under the new name (that row described a file that is no longer there).
/// When the old name has no row, an existing new-name row is left alone.
/// No-op (all zero) when either name is empty or they are equal.
pub fn rename_track_files(
    conn: &mut Connection,
    album_id: &str,
    old_filename: &str,
    new_filename: &str,
) -> Result<TrackRowsMoved> {
    if old_filename.is_empty() || new_filename.is_empty() || old_filename == new_filename {
        return Ok(TrackRowsMoved::default());
    }
    let tx = crate::immediate(conn)?;
    tx.execute(
        "INSERT OR IGNORE INTO track_tags (album_id, filename, tag, created_at) \
         SELECT album_id, ?1, tag, created_at FROM track_tags \
         WHERE album_id = ?2 AND filename = ?3",
        [new_filename, album_id, old_filename],
    )?;
    let track_tags = tx.execute(
        "DELETE FROM track_tags WHERE album_id = ?1 AND filename = ?2",
        [album_id, old_filename],
    )?;
    tx.execute(
        "DELETE FROM track_fingerprints WHERE album_id = ?1 AND filename = ?2 \
         AND EXISTS (SELECT 1 FROM track_fingerprints WHERE album_id = ?1 AND filename = ?3)",
        [album_id, new_filename, old_filename],
    )?;
    let track_fingerprints = tx.execute(
        "UPDATE track_fingerprints SET filename = ?1 WHERE album_id = ?2 AND filename = ?3",
        [new_filename, album_id, old_filename],
    )?;
    // A reject records that this file's audio could not be decoded; it
    // follows the file like its fingerprint (a row under the new name
    // described a file that is no longer there).
    tx.execute(
        "DELETE FROM fingerprint_rejects WHERE album_id = ?1 AND filename = ?2 \
         AND EXISTS (SELECT 1 FROM fingerprint_rejects WHERE album_id = ?1 AND filename = ?3)",
        [album_id, new_filename, old_filename],
    )?;
    tx.execute(
        "UPDATE fingerprint_rejects SET filename = ?1 WHERE album_id = ?2 AND filename = ?3",
        [new_filename, album_id, old_filename],
    )?;
    tx.commit()?;
    Ok(TrackRowsMoved {
        track_tags,
        track_fingerprints,
    })
}

/// `get_album_track_tags`: `{filename: [tag, ...]}` ordered by filename, tag
/// (SQLite's default BINARY collation, i.e. UTF-8 byte order).
pub fn album_track_tags(
    conn: &Connection,
    album_id: &str,
) -> Result<BTreeMap<String, Vec<String>>> {
    let mut stmt = conn.prepare(
        "SELECT filename, tag FROM track_tags WHERE album_id = ?1 ORDER BY filename, tag",
    )?;
    let rows = stmt.query_map([album_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for row in rows {
        let (filename, tag) = row?;
        out.entry(filename).or_default().push(tag);
    }
    Ok(out)
}

/// The album columns the filename-mismatch check reads.
#[derive(Debug, Clone, PartialEq)]
pub struct RenamePlanRow {
    pub id: String,
    pub path: Option<String>,
    pub dir_name: Option<String>,
    pub album_artist: Option<String>,
    pub artist: Option<String>,
    pub dir_artist: Option<String>,
    pub title: Option<String>,
    pub year: Option<String>,
    pub is_compilation: bool,
    pub disc_number: Option<i64>,
    pub disc_total: Option<i64>,
}

/// `derive_plan`'s row read: the album, unless it is Missing.
pub fn rename_plan_row(conn: &Connection, album_id: &str) -> Result<Option<RenamePlanRow>> {
    Ok(conn
        .query_row(
            "SELECT id, path, dir_name, album_artist, artist, dir_artist, title, year, \
                    is_compilation, disc_number, disc_total \
             FROM albums WHERE id = ?1 AND missing_since IS NULL",
            [album_id],
            |r| {
                Ok(RenamePlanRow {
                    id: r.get(0)?,
                    path: r.get(1)?,
                    dir_name: r.get(2)?,
                    album_artist: r.get(3)?,
                    artist: r.get(4)?,
                    dir_artist: r.get(5)?,
                    title: r.get(6)?,
                    year: r.get(7)?,
                    is_compilation: r.get::<_, Option<i64>>(8)?.unwrap_or(0) != 0,
                    disc_number: r.get(9)?,
                    disc_total: r.get(10)?,
                })
            },
        )
        .optional()?)
}

/// Where an album row says it lives: what `execute_renames` reads before it
/// touches any file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumLocation {
    pub path: String,
    /// The library the row belongs to.
    pub library_path: String,
    /// `missing_since` as stored (`None` when NULL).
    pub missing_since: Option<String>,
}

/// The album's path, library and Missing state; `None` without a row.
pub fn album_location(conn: &Connection, album_id: &str) -> Result<Option<AlbumLocation>> {
    Ok(conn
        .query_row(
            "SELECT path, library_path, missing_since FROM albums WHERE id = ?1",
            [album_id],
            |r| {
                Ok(AlbumLocation {
                    path: r.get(0)?,
                    library_path: r.get(1)?,
                    missing_since: r.get(2)?,
                })
            },
        )
        .optional()?)
}

/// The album's path, Missing or not.
pub fn album_path(conn: &Connection, album_id: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT path FROM albums WHERE id = ?1", [album_id], |r| {
            r.get(0)
        })
        .optional()?)
}
