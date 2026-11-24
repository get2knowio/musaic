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
    /// Research moved with the album (requests in `delegated_work` and old
    /// job history records in `research_history`), counted in jobs: one per
    /// history record plus one per request no record names
    /// ([`moved_research`]). The name is the fix response's wire key.
    pub ai_jobs: usize,
    /// Not repointed by the Python app; always 0 in its results.
    pub album_blocked_mb_release_groups: usize,
    /// An assistant's proposals and change records moved with the album
    /// ([`moved_agent_rows`]). Not part of the fix response.
    pub agent_proposals: usize,
    pub agent_changes: usize,
}

/// Move an album row to a new id + path + dir_name and repoint every table
/// that refers to the album by id, in one transaction with foreign keys
/// deferred to COMMIT.
///
/// Tables holding an album id (schema v24). This is the one inventory: a
/// table added later that refers to albums by id belongs here, in
/// [`ALBUM_ID_HOLDERS`], and in the merge column with a decision.
///
/// | table | FK to albums(id) | repointed here | merge ([`crate::merge`]) |
/// |---|---|---|---|
/// | track_fingerprints | yes, ON DELETE CASCADE | yes | per file, [`crate::merge::move_track_rows`]; the fold refuses while a from-album has any |
/// | track_tags | yes, ON DELETE CASCADE | yes | per file, as above (union with the target's tags) |
/// | fingerprint_rejects (v18) | yes, ON UPDATE / DELETE CASCADE | by the cascade | per file, as above |
/// | album_blocked_mb_release_groups | yes, ON DELETE CASCADE | yes | union onto the survivor |
/// | check_findings | no (plain column) | yes | not moved: unresolved ones (open or ignored) resolved in place under the dead id, kept as history |
/// | delegated_work (v22) | no (plain column) | yes, in this transaction | finished requests repointed; an unresolved one refuses the fold |
/// | research_history (v24) | no (plain column) | yes (`album_path` is a display copy and keeps the old text) | repointed |
/// | agent_proposals (v21) | no (plain column) | yes, and expired ([`moved_agent_rows`]) | repointed and expired, as in a rename |
/// | agent_changes (v21) | no (plain column; the after-image's `albumId` too) | yes, both | repointed, both |
/// | check_inputs (v19) | no | no: the new id has no inputs, so the sweep checks it again, and prunes the old id's rows | deleted for every member and the survivor |
/// | check_runs.scope | no (JSON text of ids) | no: a historical record of what a run covered | no, for the same reason |
///
/// Per-album columns of `albums` that a scan does not own (the
/// MusicBrainz anchor, `metadata_ai_acted_on_at`, `missing_since`) move
/// with the row here; for a merge see [`crate::merge::merge_album_rows`].
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
        ai_jobs: moved_research(tx, old_id, new_id)?,
        album_blocked_mb_release_groups: moved("album_blocked_mb_release_groups")?,
        ..moved_agent_rows(tx, old_id, new_id)?
    })
}

/// Moves an assistant's proposals and change records to `new_id`, as a
/// [`DirMigration`] with only the two agent counts set.
///
/// A change record names its album twice: the `album_id` column and the
/// after-image's `albumId` (the id once the change finished, which is what
/// the desktop's Undo looks the album up by). Both follow the album, so an
/// Undo still finds it under its new id; whether the files still allow the
/// Undo is the Undo's own check of the folder and files.
///
/// A proposal is bound to the id and the album revision it was made
/// against (its digest covers both), and the revision covers the album
/// row, path included, so a proposal can never be applied once its album
/// changed id. It is repointed so its approval card still names the album,
/// and expired in the same statement (its expiry set back to its creation
/// time, a time already past, much as a library restore expires them), so
/// applying it asks the assistant to preview again rather than reporting a
/// broken digest.
pub(crate) fn moved_agent_rows(
    conn: &Connection,
    old_id: &str,
    new_id: &str,
) -> Result<DirMigration> {
    let agent_proposals = conn.execute(
        "UPDATE agent_proposals SET album_id = ?1, expires_at = min(expires_at, created_at) \
         WHERE album_id = ?2",
        [new_id, old_id],
    )?;
    let agent_changes = conn.execute(
        "UPDATE agent_changes SET \
            album_id = CASE WHEN album_id = ?2 THEN ?1 ELSE album_id END, \
            after = CASE WHEN json_valid(after) AND json_extract(after, '$.albumId') = ?2 \
                         THEN json_set(after, '$.albumId', ?1) ELSE after END \
         WHERE album_id = ?2 \
            OR (json_valid(after) AND json_extract(after, '$.albumId') = ?2)",
        [new_id, old_id],
    )?;
    Ok(DirMigration {
        agent_proposals,
        agent_changes,
        ..DirMigration::default()
    })
}

/// Moves an album's research (requests and old job history records) to
/// `new_id`; how many jobs moved, counted as the old job table counted them:
/// one per history record, plus one per request made after the fold (a
/// request that names a history record is that record's, not one more job).
pub(crate) fn moved_research(conn: &Connection, old_id: &str, new_id: &str) -> Result<usize> {
    let native = conn.execute(
        "UPDATE delegated_work SET album_id = ?1 WHERE album_id = ?2 AND legacy_job_id IS NULL",
        [new_id, old_id],
    )?;
    conn.execute(
        "UPDATE delegated_work SET album_id = ?1 WHERE album_id = ?2",
        [new_id, old_id],
    )?;
    let history = conn.execute(
        "UPDATE research_history SET album_id = ?1 WHERE album_id = ?2",
        [new_id, old_id],
    )?;
    Ok(native + history)
}

/// Every table besides `albums` that refers to an album by id. A row in one
/// of them under an id no album has is left over from an album that was
/// removed; moving another album onto that id would merge the two.
pub const ALBUM_ID_HOLDERS: [&str; 10] = [
    "track_fingerprints",
    "track_tags",
    "album_blocked_mb_release_groups",
    "fingerprint_rejects",
    "check_findings",
    "delegated_work",
    "research_history",
    "check_inputs",
    "agent_proposals",
    "agent_changes",
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
    let moved = move_file_rows(&tx, album_id, old_filename, album_id, new_filename)?;
    tx.commit()?;
    Ok(TrackRowsMoved {
        track_tags: moved.track_tags,
        track_fingerprints: moved.track_fingerprints,
    })
}

/// The tables whose rows belong to one file of an album: keyed by
/// `(album_id, filename)`.
pub const FILE_ROW_TABLES: [&str; 3] = ["track_fingerprints", "track_tags", "fingerprint_rejects"];

/// Rows moved per table by one file's move.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FileRowsMoved {
    /// Source `track_tags` rows removed (moved, or merged into a tag the
    /// target name already had).
    pub track_tags: usize,
    /// `track_fingerprints` rows moved (0 or 1).
    pub track_fingerprints: usize,
    /// `fingerprint_rejects` rows moved (0 or 1).
    pub fingerprint_rejects: usize,
}

impl FileRowsMoved {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Every per-file row of `(from_album, from_name)` to `(into_album,
/// into_name)`, on the caller's transaction. One rule for a rename inside
/// an album ([`rename_track_files`]) and a move between albums
/// ([`crate::merge::move_track_rows`]):
///
/// * `track_tags`: the union. Tags already on the target name are kept
///   (with their own `created_at`), the source's are added, the source rows
///   deleted.
/// * `track_fingerprints` and `fingerprint_rejects`: a row describes the
///   audio of the file that now carries the target name, so a source row
///   replaces any target row (that one described a file that is no longer
///   there). Without a source row the target row is left alone, which also
///   makes a second call a no-op.
pub(crate) fn move_file_rows(
    tx: &Connection,
    from_album: &str,
    from_name: &str,
    into_album: &str,
    into_name: &str,
) -> Result<FileRowsMoved> {
    let p = rusqlite::named_params! {
        ":fa": from_album, ":fn": from_name, ":ia": into_album, ":in": into_name,
    };
    tx.execute(
        "INSERT OR IGNORE INTO track_tags (album_id, filename, tag, created_at) \
         SELECT :ia, :in, tag, created_at FROM track_tags \
         WHERE album_id = :fa AND filename = :fn",
        p,
    )?;
    let track_tags = tx.execute(
        "DELETE FROM track_tags WHERE album_id = :fa AND filename = :fn",
        rusqlite::named_params! { ":fa": from_album, ":fn": from_name },
    )?;
    let moved = |table: &str| -> Result<usize> {
        tx.execute(
            &format!(
                "DELETE FROM {table} WHERE album_id = :ia AND filename = :in \
                 AND EXISTS (SELECT 1 FROM {table} WHERE album_id = :fa AND filename = :fn)"
            ),
            p,
        )?;
        Ok(tx.execute(
            &format!(
                "UPDATE {table} SET album_id = :ia, filename = :in \
                 WHERE album_id = :fa AND filename = :fn"
            ),
            p,
        )?)
    };
    let track_fingerprints = moved("track_fingerprints")?;
    // A reject records that this file's audio could not be decoded; it
    // follows the file like its fingerprint.
    let fingerprint_rejects = moved("fingerprint_rejects")?;
    Ok(FileRowsMoved {
        track_tags,
        track_fingerprints,
        fingerprint_rejects,
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
