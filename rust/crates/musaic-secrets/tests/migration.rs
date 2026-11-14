//! Moving secrets out of SQLite into the OS keyring.
//!
//! Deliberate changes with no the Python app behaviour to compare against: these tests
//! check the outcomes the decisions state (a row only fills an empty keyring
//! slot; the keyring wins; a row goes only when the keyring demonstrably holds
//! a value; the pre-migration backup is scrubbed too, stays a valid v16 file,
//! and is never left half-written; with no keyring nothing changes). Inputs
//! are an Python app-written database (testdata/db/v16-synthetic-offline.db, whose
//! one settings row is the offline capture's dummy AcoustID key) and rows
//! written with the Python app's setter semantics. Byte-level checks scan every
//! file left in the database's directory.
//!
//! `Db::open` switches the database to WAL (as the Python app's bootstrap does), so
//! while it is open a `library.db-wal` and `-shm` sit beside it: key scans run
//! with the database open (those files included); file-name listings, which
//! check that the migration leaves no temp files, run after it is closed.
//! tests/wal_no_trace.rs covers databases that start in WAL mode with the keys
//! still in the WAL.

mod common;

use std::path::Path;
use std::time::SystemTime;

use common::{dump, file_contains, files_containing, names, open_copy, settings_rows};
use musaic_db::{BACKUP_FILE_NAME, Db, backup, settings};
use musaic_secrets::migrate::SCRUB_TMP_SUFFIX;
use musaic_secrets::{
    MemoryStore, SecretStore, StoreError, clear_all, migrate_settings_secrets, secret,
    secret_status, set_secret,
};
use rusqlite::{Connection, OpenFlags};

const DUMMY: &str = "migration-offline-dummy";
const FAKE_AI: &str = "test-key-not-real";
const NEWER: &str = "test-newer-key-not-real";

fn ro(p: &Path) -> Connection {
    Connection::open_with_flags(p, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
}

/// The scrubbed backup equals the Python app's fixture except for the removed rows,
/// passes an integrity check, and is still schema v16.
fn assert_backup_is_fixture_minus(dir: &Path, removed: &[&str]) {
    let b = dir.join(BACKUP_FILE_NAME);
    let c = ro(&b);
    let check: String = c
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(check, "ok");
    let v: i64 = c
        .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, musaic_db::PYTHON_SCHEMA_VERSION);
    let t = tempfile::tempdir().unwrap();
    let f = t.path().join("fixture.db");
    std::fs::copy(musaic_testdata::public("db/v16-synthetic-offline.db"), &f).unwrap();
    {
        let w = Connection::open(&f).unwrap();
        for k in removed {
            w.execute("DELETE FROM settings WHERE key = ?1", [k])
                .unwrap();
        }
    }
    assert_eq!(dump(&c), dump(&ro(&f)));
    for s in ["-wal", "-shm", "-journal", SCRUB_TMP_SUFFIX] {
        assert!(!Path::new(&format!("{}{s}", b.display())).exists(), "{s}");
    }
}

/// Positive: the Python app's database key moves to the empty
/// keyring slot and no file left on disk holds it: live db, wal, the
/// pre-migration backup, temp files, a backup and a snapshot taken afterwards.
#[test]
fn python_database_secret_moves_and_leaves_no_trace() {
    let (_t, dir, db) = open_copy("v16-synthetic-offline.db");
    assert!(
        file_contains(&dir.join(BACKUP_FILE_NAME), DUMMY),
        "precondition"
    );
    let store = MemoryStore::new();
    let report = migrate_settings_secrets(&db, &store).unwrap();
    assert_eq!(report.moved, vec!["acoustid_api_key"]);
    assert_eq!(report.backup_scrubbed, vec!["acoustid_api_key"]);
    assert!(report.failed.is_empty() && report.backup_error.is_none());
    assert_eq!(
        settings::get_raw(db.connection(), "acoustid_api_key").unwrap(),
        None
    );
    assert_eq!(secret(&store, "acoustid_api_key").unwrap(), DUMMY);

    backup::make_backup_in(&db, &dir.join("exports"), "20260101-000000Z").unwrap();
    backup::create_snapshot(
        &db,
        &dir.join("backups"),
        10,
        "20260101-000000Z",
        SystemTime::now(),
    )
    .unwrap();
    assert_eq!(
        files_containing(&dir, DUMMY),
        Vec::<std::path::PathBuf>::new()
    );
    assert_eq!(
        std::fs::metadata(dir.join("library.db-wal")).unwrap().len(),
        0
    );
    assert_backup_is_fixture_minus(&dir, &["acoustid_api_key"]);
    drop(db);
    assert!(files_containing(&dir, DUMMY).is_empty());
    assert_eq!(
        names(&dir),
        ["backups", "exports", "library.db", BACKUP_FILE_NAME]
    );
}

/// Values the Python app overwrote earlier (no secure_delete) do not
/// survive in free space either.
#[test]
fn overwritten_values_are_scrubbed_too() {
    let (_t, dir, db) = open_copy("v16-empty.db");
    let conn = db.connection();
    let old = "test-old-key-not-real-and-longer-than-the-new-one";
    settings::set_raw(conn, "ai_api_key", old).unwrap();
    settings::set_raw(conn, "ai_api_key", FAKE_AI).unwrap();
    let store = MemoryStore::new();
    migrate_settings_secrets(&db, &store).unwrap();
    // Whole values and the distinctive tail of the overwritten one (a later
    // cell can land on part of the freed one).
    for needle in [old, FAKE_AI, "longer-than-the-new-one", "test-old-key"] {
        assert!(!file_contains(&dir.join("library.db"), needle), "{needle}");
    }
}

/// Idempotency: a second and third run do nothing, and leave the
/// scrubbed backup byte-identical.
#[test]
fn rerun_is_a_noop() {
    let (_t, dir, db) = open_copy("v16-synthetic-offline.db");
    let store = MemoryStore::new();
    migrate_settings_secrets(&db, &store).unwrap();
    let entries = store.entries();
    let backup_bytes = std::fs::read(dir.join(BACKUP_FILE_NAME)).unwrap();
    for _ in 0..2 {
        let r = migrate_settings_secrets(&db, &store).unwrap();
        assert!(r.is_noop(), "{r:?}");
        assert_eq!(store.entries(), entries);
        assert_eq!(
            std::fs::read(dir.join(BACKUP_FILE_NAME)).unwrap(),
            backup_bytes
        );
    }
}

/// Cancellation: stopped after the keyring write, before any delete.
/// The re-run writes nothing and removes the rows (live and backup).
#[test]
fn resume_after_interruption_between_write_and_delete() {
    let (_t, dir, db) = open_copy("v16-synthetic-offline.db");
    let store = MemoryStore::new();
    store.set("acoustid_api_key", DUMMY).unwrap();
    let r = migrate_settings_secrets(&db, &store).unwrap();
    assert_eq!(r.kept_keyring, vec!["acoustid_api_key"]);
    assert!(r.moved.is_empty());
    assert!(!store.calls().iter().skip(1).any(|c| c.starts_with("set:")));
    assert_eq!(
        settings::get_raw(db.connection(), "acoustid_api_key").unwrap(),
        None
    );
    assert!(files_containing(&dir, DUMMY).is_empty());
}

/// Cancellation: stopped after the live rows were deleted and the file
/// rebuilt but before the final WAL checkpoint (so the old pages holding the
/// key are still in library.db) and before the backup scrub; and a crash
/// mid-scrub left a temp copy that still holds the key. The next run, with no
/// live rows left to remove, still completes the checkpoint, finishes the
/// backup and removes the stale copy.
#[test]
fn resume_after_interruption_between_live_and_backup_scrub() {
    let (_t, dir, db) = open_copy("v16-synthetic-offline.db");
    let store = MemoryStore::new();
    store.set("acoustid_api_key", DUMMY).unwrap();
    // The state the live scrub leaves (row gone, file rebuilt).
    db.connection()
        .execute_batch(
            "PRAGMA secure_delete = ON; DELETE FROM settings WHERE key = 'acoustid_api_key'; VACUUM;",
        )
        .unwrap();
    let b = dir.join(BACKUP_FILE_NAME);
    let stale = format!("{}{SCRUB_TMP_SUFFIX}", b.display());
    std::fs::copy(&b, &stale).unwrap();
    std::fs::write(format!("{stale}-journal"), DUMMY).unwrap();
    let r = migrate_settings_secrets(&db, &store).unwrap();
    assert!(r.removed_rows().is_empty());
    assert_eq!(r.backup_scrubbed, vec!["acoustid_api_key"]);
    assert!(files_containing(&dir, DUMMY).is_empty());
    assert_backup_is_fixture_minus(&dir, &["acoustid_api_key"]);
}

/// The backup is only replaced after the scrubbed copy verifies; a
/// failed scrub (here: the backup is not a database) leaves it as it was.
#[test]
fn backup_that_cannot_be_scrubbed_is_left_untouched() {
    let (_t, dir, db) = open_copy("v16-synthetic-offline.db");
    let b = dir.join(BACKUP_FILE_NAME);
    std::fs::write(&b, b"not a database").unwrap();
    let store = MemoryStore::new();
    let r = migrate_settings_secrets(&db, &store).unwrap();
    assert_eq!(r.moved, vec!["acoustid_api_key"]);
    assert!(r.backup_error.is_some());
    assert_eq!(std::fs::read(&b).unwrap(), b"not a database");
    drop(db);
    assert_eq!(names(&dir), ["library.db", BACKUP_FILE_NAME]);
}

/// A failed write into an empty slot keeps the row in the
/// live db AND the backup; the other keys still move.
#[test]
fn failed_write_keeps_the_row() {
    let (_t, dir, db) = open_copy("v16-synthetic-offline.db");
    let conn = db.connection();
    settings::set_raw(conn, "firecrawl_api_key", "test-firecrawl-key-not-real").unwrap();
    let store = MemoryStore::new();
    store.fail_set_for("acoustid_api_key");
    let r = migrate_settings_secrets(&db, &store).unwrap();
    assert_eq!(r.moved, vec!["firecrawl_api_key"]);
    assert_eq!(r.failed.len(), 1);
    assert!(store.calls().contains(&"set:acoustid_api_key".to_owned()));
    assert_eq!(
        settings::get_raw(conn, "acoustid_api_key")
            .unwrap()
            .as_deref(),
        Some(DUMMY)
    );
    assert_eq!(settings::get_raw(conn, "firecrawl_api_key").unwrap(), None);
    assert!(r.backup_scrubbed.is_empty());
    assert!(file_contains(&dir.join(BACKUP_FILE_NAME), DUMMY));
}

/// The read-back check catches a store that mangles the value.
#[test]
fn read_back_mismatch_keeps_the_row() {
    let (_t, dir, db) = open_copy("v16-synthetic-offline.db");
    let store = MemoryStore::new();
    store.corrupt_set_for("acoustid_api_key");
    let r = migrate_settings_secrets(&db, &store).unwrap();
    assert_eq!(
        r.failed,
        vec![(
            "acoustid_api_key",
            StoreError::ReadBackMismatch("acoustid_api_key".into())
        )]
    );
    assert_eq!(
        settings::get_raw(db.connection(), "acoustid_api_key")
            .unwrap()
            .as_deref(),
        Some(DUMMY)
    );
    // The mangled entry is removed again, so the slot is empty, the backup
    // keeps its copy, and a re-run still refuses to drop the row.
    assert_eq!(store.get("acoustid_api_key").unwrap(), None);
    assert!(r.backup_scrubbed.is_empty());
    assert!(file_contains(&dir.join(BACKUP_FILE_NAME), DUMMY));
    let r = migrate_settings_secrets(&db, &store).unwrap();
    assert_eq!(r.failed.len(), 1);
    assert!(
        settings::get_raw(db.connection(), "acoustid_api_key")
            .unwrap()
            .is_some()
    );
}

/// With no keyring nothing changes anywhere: live rows
/// (blank ones too), backup bytes, and no temp files.
#[test]
fn unavailable_keyring_changes_nothing() {
    let (_t, dir, db) = open_copy("v16-synthetic-offline.db");
    settings::set_raw(db.connection(), "ai_api_key", "  ").unwrap();
    let rows = settings_rows(db.connection());
    let backup_bytes = std::fs::read(dir.join(BACKUP_FILE_NAME)).unwrap();
    let store = MemoryStore::unavailable();
    let r = migrate_settings_secrets(&db, &store).unwrap();
    assert_eq!(r.failed.len(), 2);
    assert!(r.removed_rows().is_empty() && r.backup_scrubbed.is_empty());
    assert_eq!(settings_rows(db.connection()), rows);
    assert_eq!(
        std::fs::read(dir.join(BACKUP_FILE_NAME)).unwrap(),
        backup_bytes
    );
    drop(db);
    assert_eq!(names(&dir), ["library.db", BACKUP_FILE_NAME]);
}

/// The keyring wins. The row's different value is dropped (live and
/// backup), nothing is written, no `.previous` copy appears.
#[test]
fn keyring_wins_over_a_different_row() {
    let (_t, dir, db) = open_copy("v16-synthetic-offline.db");
    let store = MemoryStore::new();
    store.set("acoustid_api_key", NEWER).unwrap();
    let r = migrate_settings_secrets(&db, &store).unwrap();
    assert_eq!(r.kept_keyring, vec!["acoustid_api_key"]);
    assert_eq!(r.backup_scrubbed, vec!["acoustid_api_key"]);
    assert_eq!(
        store.entries().into_iter().collect::<Vec<_>>(),
        vec![("acoustid_api_key".to_owned(), NEWER.to_owned())]
    );
    assert_eq!(
        settings::get_raw(db.connection(), "acoustid_api_key").unwrap(),
        None
    );
    assert!(files_containing(&dir, DUMMY).is_empty());
}

/// Restoring an old backup never brings an old key back.
#[test]
fn restore_of_an_old_backup_does_not_bring_the_old_key_back() {
    let (_t, dir, db) = open_copy("v16-empty.db");
    let store = MemoryStore::new();
    set_secret(&store, "acoustid_api_key", NEWER).unwrap();
    let upload = musaic_testdata::public("db/v16-synthetic-offline.db");
    let (db, _) = backup::restore_from(db, &upload, "20260107-000000Z", SystemTime::now()).unwrap();
    assert!(
        settings::get_raw(db.connection(), "acoustid_api_key")
            .unwrap()
            .is_some()
    );
    let r = migrate_settings_secrets(&db, &store).unwrap();
    assert_eq!(r.kept_keyring, vec!["acoustid_api_key"]);
    assert_eq!(secret(&store, "acoustid_api_key").unwrap(), NEWER);
    assert!(!file_contains(&dir.join("library.db"), DUMMY));
}

/// A blank row has nothing to keep and does not clear the store.
#[test]
fn blank_row_is_dropped_without_touching_the_store() {
    let (_t, _dir, db) = open_copy("v16-empty.db");
    let conn = db.connection();
    settings::set_raw(conn, "firecrawl_api_key", "   ").unwrap();
    let store = MemoryStore::new();
    store
        .set("firecrawl_api_key", "test-firecrawl-key-not-real")
        .unwrap();
    let r = migrate_settings_secrets(&db, &store).unwrap();
    assert_eq!(r.dropped_blank, vec!["firecrawl_api_key"]);
    assert_eq!(settings::get_raw(conn, "firecrawl_api_key").unwrap(), None);
    assert_eq!(
        secret(&store, "firecrawl_api_key").unwrap(),
        "test-firecrawl-key-not-real"
    );
}

/// A database with no pre-migration backup beside it (already v17) migrates
/// without creating one.
#[test]
fn no_backup_beside_the_database_is_fine() {
    let (_t, dir, db) = open_copy("v16-synthetic-offline.db");
    let p = db.path().to_path_buf();
    drop(db);
    std::fs::remove_file(dir.join(BACKUP_FILE_NAME)).unwrap();
    let (db, _) = Db::open(&p).unwrap();
    let r = migrate_settings_secrets(&db, &MemoryStore::new()).unwrap();
    assert_eq!(r.moved, vec!["acoustid_api_key"]);
    assert!(r.backup_error.is_none());
    drop(db);
    assert_eq!(names(&dir), ["library.db"]);
}

/// Teardown: clearing a secret removes the keyring entry; clear_all
/// removes every entry the app owns, and is repeatable.
#[test]
fn clearing_removes_entries() {
    let store = MemoryStore::new();
    set_secret(&store, "ai_api_key", FAKE_AI).unwrap();
    set_secret(&store, "ai_api_key", "  ").unwrap();
    assert!(store.entries().is_empty());
    set_secret(&store, "acoustid_api_key", DUMMY).unwrap();
    clear_all(&store).unwrap();
    clear_all(&store).unwrap();
    assert!(store.entries().is_empty());
    assert!(secret_status(&store).set.iter().all(|(_, s)| !s));
    assert!(secret_status(&store).unavailable.is_none());
    assert!(
        secret_status(&MemoryStore::unavailable())
            .unavailable
            .is_some()
    );
    assert!(
        set_secret(&store, "ai_model_id", "x").is_err(),
        "not a secret key"
    );
}

/// Cancellation: a crash after the copy and before the rename leaves a
/// temp copy that still holds the key. Even when the backup itself has nothing
/// left to scrub, the next run deletes it.
#[test]
fn stale_scrub_copy_is_removed_on_the_next_run() {
    let (_t, dir, db) = open_copy("v16-synthetic-offline.db");
    let store = MemoryStore::new();
    migrate_settings_secrets(&db, &store).unwrap();
    let stale = format!("{}{SCRUB_TMP_SUFFIX}", dir.join(BACKUP_FILE_NAME).display());
    std::fs::copy(
        musaic_testdata::public("db/v16-synthetic-offline.db"),
        &stale,
    )
    .unwrap();
    assert_eq!(files_containing(&dir, DUMMY).len(), 1);
    let r = migrate_settings_secrets(&db, &store).unwrap();
    assert!(r.backup_scrubbed.is_empty());
    assert!(files_containing(&dir, DUMMY).is_empty());
    drop(db);
    assert_eq!(names(&dir), ["library.db", BACKUP_FILE_NAME]);
}
