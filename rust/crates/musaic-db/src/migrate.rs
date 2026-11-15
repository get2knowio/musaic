//! One-way migrations: v16 -> v17 (`fingerprint_impl`),
//! v17 -> v18 (AcoustID lookup outcome per fingerprint row; files the
//! fingerprinter rejected), and the backup taken before the first.
//!
//! Backup rule: a database the Python app wrote (v16, or any older version
//! [`crate::python_chain`] brings to v16 first) is copied, to
//! `library.v<N>.before-rust.db` ([`backup_file_name`], N its version as
//! found; [`BACKUP_FILE_NAME`] for v16), before its first migration step;
//! that file is the rollback for the Python app, and musaic-secrets scrubs
//! keys from it. One backup only: if any `library.v<N>.before-rust.db`
//! is already there ([`existing_backup`]), an earlier interrupted attempt took
//! it from the untouched file, and no second one is written (an attempt
//! interrupted after the `v<N> -> v16` step finds a v16 file next time).
//! A v17 database was written by this build: the v17 -> v18 step
//! is additive and runs in one transaction (an interruption leaves the v17
//! file as it was), and no further backup file is written, so no new copy
//! of plaintext settings rows appears beside the database.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, backup::Backup};

use crate::{PYTHON_SCHEMA_VERSION, Result, SCHEMA_VERSION};

/// Written next to the database before the first migration. Never overwritten:
/// if it already exists, an earlier (interrupted) attempt took it from the
/// still-untouched v16 database, and that copy is the one to keep.
pub const BACKUP_FILE_NAME: &str = "library.v16.before-rust.db";

/// The pre-migration backup's name for a database found at `version`
/// (`library.v<version>.before-rust.db`; [`BACKUP_FILE_NAME`] for v16).
pub fn backup_file_name(version: i64) -> String {
    format!("library.v{version}.before-rust.db")
}

/// The pre-migration backup already in `dir`, if any: the
/// `library.v<N>.before-rust.db` with the lowest N in
/// [`crate::OLDEST_SCHEMA_VERSION`]..=v16 (there is only ever one unless a
/// file was copied in by hand; the lowest is the oldest original). This is
/// the file musaic-secrets must scrub, whatever N is.
pub fn existing_backup(dir: &Path) -> Option<PathBuf> {
    (crate::OLDEST_SCHEMA_VERSION..=PYTHON_SCHEMA_VERSION)
        .map(|v| dir.join(backup_file_name(v)))
        .find(|p| p.is_file())
}

/// `track_fingerprints.fingerprint_impl` for rows the Python app computed with fpcalc.
pub const FINGERPRINT_IMPL_FPCALC: &str = "fpcalc";
/// ... and for rows this build computes in-process (never compared with fpcalc rows).
pub const FINGERPRINT_IMPL_RUST: &str = "rust-chromaprint";

/// `track_fingerprints.acoustid_lookup_state` (v18): the last AcoustID
/// lookup of the row's fingerprint completed (a match, or "no match" when
/// `acoustid_recording_mbid` is NULL). Not asked again unless forced or the
/// fingerprint changes.
pub const LOOKUP_DONE: &str = "done";
/// ... the last lookup did not complete (network error, timeout, HTTP error,
/// rate limit, unreadable answer). Retried once `acoustid_retry_after` has
/// passed. NULL means no lookup has completed or failed for this fingerprint
/// yet (computed without a key, or a Python-app row with no recording).
pub const LOOKUP_FAILED: &str = "failed";

/// The v17 -> v18 statements, in order (also the text a fresh database gets).
pub(crate) const V18_STATEMENTS: &[&str] = &[
    "ALTER TABLE track_fingerprints ADD COLUMN acoustid_lookup_state TEXT",
    "ALTER TABLE track_fingerprints ADD COLUMN acoustid_lookup_failures INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE track_fingerprints ADD COLUMN acoustid_retry_after TEXT",
    // A Python-app row with a recording came from a completed lookup. One
    // without is a failure or a no-match the Python app could not tell apart:
    // left NULL (not yet looked up), so it gets one lookup.
    "UPDATE track_fingerprints SET acoustid_lookup_state = 'done' \
     WHERE acoustid_recording_mbid IS NOT NULL",
    // A file the fingerprinter rejected, remembered by its size and
    // times so it is not decoded again until it changes (or the
    // implementation does). ON UPDATE CASCADE: rows follow an album's id
    // when its directory is renamed (musaic-db renames.rs updates albums.id
    // under deferred foreign keys); ON DELETE CASCADE: they go with the album.
    "CREATE TABLE fingerprint_rejects (
        album_id          TEXT NOT NULL,
        filename          TEXT NOT NULL,
        file_signature    TEXT NOT NULL,
        fingerprint_impl  TEXT NOT NULL,
        rejected_at       TEXT NOT NULL,
        PRIMARY KEY (album_id, filename),
        FOREIGN KEY (album_id) REFERENCES albums(id) ON DELETE CASCADE ON UPDATE CASCADE
    )",
];

pub(crate) fn backup_before_migration(
    conn: &Connection,
    db_path: &Path,
    found: i64,
) -> Result<PathBuf> {
    let dir = db_path.parent().unwrap_or_else(|| Path::new("."));
    let dest = dir.join(backup_file_name(found));
    // Write to a temp name and rename, so a crash mid-copy never leaves a
    // truncated file under the real name (which would then be "kept"). A
    // crash leaves the temp copy and its -journal / -wal / -shm, which hold
    // the source's settings rows (API keys): removed on every call
    // before anything else (crate::backup::remove_db_with_sidecars).
    // Leftovers are removed for every version's name, not only `found`'s.
    for v in crate::OLDEST_SCHEMA_VERSION..=PYTHON_SCHEMA_VERSION {
        crate::backup::remove_db_with_sidecars(&dir.join(format!("{}.tmp", backup_file_name(v))))?;
    }
    let tmp = dir.join(format!("{}.tmp", backup_file_name(found)));
    if let Some(existing) = existing_backup(dir) {
        return Ok(existing);
    }
    {
        let mut out = Connection::open(&tmp)?;
        let backup = Backup::new(conn, &mut out)?;
        backup.run_to_completion(256, Duration::ZERO, None)?;
        drop(backup);
        // A copy of a WAL-mode database is itself marked WAL; make the backup
        // a self-contained rollback-journal file, so opening it later leaves
        // no -wal / -shm beside it (it is never written to again).
        out.pragma_update(None, "journal_mode", "DELETE")?;
    }
    std::fs::rename(&tmp, &dest)?;
    Ok(dest)
}

/// A fresh database, as the Python app's `db.bootstrap()` creates one,
/// then the same steps a v16 file goes through ([`upgrade`]). WAL first (it cannot change inside a
/// transaction), then `synchronous=NORMAL` for this connection, then every
/// statement plus the version row in one transaction: an interruption leaves
/// a file with no schema objects, which [`crate::Db::open_or_create`] treats
/// as absent and creates again.
pub(crate) fn create_fresh(conn: &mut Connection) -> Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    let tx = crate::immediate(conn)?;
    for stmt in crate::schema_v16::STATEMENTS {
        tx.execute(stmt, [])?;
    }
    tx.execute(
        "INSERT INTO schema_version(version) VALUES (?1)",
        [PYTHON_SCHEMA_VERSION],
    )?;
    tx.commit()?;
    upgrade(conn, PYTHON_SCHEMA_VERSION)
}

/// Every step from `found` up to [`SCHEMA_VERSION`], each in its own
/// transaction (each re-checks the version, so a step another process
/// already ran is skipped): the Python app's own chain to v16 for an older file
/// ([`crate::python_chain`], one transaction as in the Python app), then v17, v18.
pub(crate) fn upgrade(conn: &mut Connection, found: i64) -> Result<()> {
    if found < PYTHON_SCHEMA_VERSION {
        crate::python_chain::to_python_version(conn)?;
    }
    if found <= PYTHON_SCHEMA_VERSION {
        v16_to_v17(conn)?;
    }
    if found <= 17 {
        v17_to_v18(conn)?;
    }
    Ok(())
}

pub(crate) fn v16_to_v17(conn: &mut Connection) -> Result<()> {
    let tx = crate::immediate(conn)?;
    // Re-check inside the transaction: another process may have migrated
    // between our version read and here.
    let current: i64 = tx.query_row("SELECT version FROM schema_version LIMIT 1", [], |r| {
        r.get(0)
    })?;
    if current == PYTHON_SCHEMA_VERSION {
        tx.execute_batch(&format!(
            "ALTER TABLE track_fingerprints
                 ADD COLUMN fingerprint_impl TEXT NOT NULL DEFAULT '{FINGERPRINT_IMPL_FPCALC}';
             UPDATE schema_version SET version = 17;"
        ))?;
    }
    tx.commit()?;
    Ok(())
}

/// See [`V18_STATEMENTS`].
pub(crate) fn v17_to_v18(conn: &mut Connection) -> Result<()> {
    let tx = crate::immediate(conn)?;
    let current: i64 = tx.query_row("SELECT version FROM schema_version LIMIT 1", [], |r| {
        r.get(0)
    })?;
    if current == 17 {
        for stmt in V18_STATEMENTS {
            tx.execute(stmt, [])?;
        }
        tx.execute("UPDATE schema_version SET version = ?1", [SCHEMA_VERSION])?;
    }
    tx.commit()?;
    Ok(())
}
