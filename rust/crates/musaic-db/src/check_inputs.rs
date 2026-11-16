//! `check_inputs` (v19): what each health check last ran against.
//!
//! One row per (check, library, album) for a check whose result for an
//! album depends only on that album, or one row per (check, library) with
//! `album_id = ''` ([`LIBRARY_ROW`]) for a check that reads the whole
//! library. `digest` is opaque here (musaic-checks computes it); `checked_at`
//! is the run's start, ISO-8601 seconds in UTC.

use rusqlite::{Connection, params};

use crate::Result;

/// The `album_id` of a whole-library row.
pub const LIBRARY_ROW: &str = "";

/// One recorded row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputRow {
    pub check_id: String,
    pub album_id: String,
    pub digest: String,
    pub checked_at: String,
}

/// Every row of `library_path`, ordered by check then album.
pub fn rows_for_library(conn: &Connection, library_path: &str) -> Result<Vec<InputRow>> {
    let mut stmt = conn.prepare(
        "SELECT check_id, album_id, digest, checked_at FROM check_inputs \
         WHERE library_path = ?1 ORDER BY check_id, album_id",
    )?;
    let rows = stmt.query_map([library_path], |r| {
        Ok(InputRow {
            check_id: r.get(0)?,
            album_id: r.get(1)?,
            digest: r.get(2)?,
            checked_at: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Record that `check_id` ran against these `(album_id, digest)` inputs at
/// `checked_at` (replacing earlier rows of the same keys), in one
/// transaction.
pub fn record(
    conn: &Connection,
    check_id: &str,
    library_path: &str,
    digests: &[(String, String)],
    checked_at: &str,
) -> Result<()> {
    if digests.is_empty() {
        return Ok(());
    }
    let tx = crate::immediate(conn)?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO check_inputs(check_id, library_path, album_id, digest, checked_at) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(check_id, library_path, album_id) \
             DO UPDATE SET digest = excluded.digest, checked_at = excluded.checked_at",
        )?;
        for (album_id, digest) in digests {
            stmt.execute(params![
                check_id,
                library_path,
                album_id,
                digest,
                checked_at
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Forget every row of `check_id` (every library): the check is stale
/// everywhere. Returns the number of rows removed.
pub fn forget_check(conn: &Connection, check_id: &str) -> Result<usize> {
    Ok(conn.execute("DELETE FROM check_inputs WHERE check_id = ?1", [check_id])?)
}

/// Remove album rows whose album no longer exists (deleted, or renamed to a
/// new id). Missing albums keep theirs: their rows are still in `albums`.
pub fn prune_orphans(conn: &Connection) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM check_inputs WHERE album_id <> '' \
         AND NOT EXISTS (SELECT 1 FROM albums a WHERE a.id = check_inputs.album_id)",
        [],
    )?)
}
