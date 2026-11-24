//! Under WAL: after the migration no file in the
//! config directory holds a secret key's bytes, migrated (moved to the
//! keyring) or retired (scrubbed, never moved).
//!
//! Every real user database is WAL (the Python app's `bootstrap()` switches it on
//! every start, and so does `Db::open`), so the key rows the Python app wrote
//! may still sit in `library.db-wal`, uncheckpointed, and older values it
//! overwrote may sit in free space. Each test starts from the Python app fixture
//! (testdata/db/v16-synthetic-offline.db), writes the keys with the Python app's
//! setter semantics through a WAL connection, and copies `library.db` +
//! `library.db-wal` as a crashed or still-running Python app leaves them. Then
//! it scans every file in the directory (live db, -wal, -shm, the
//! pre-migration backup and anything beside it, temp copies) byte for byte,
//! while the database is still open and again after it is closed.
//!
//! A checkpoint another connection blocks must not be reported as success:
//! the migration returns an error and the next call finishes the job.

mod common;

use std::path::{Path, PathBuf};

use common::names;
use musaic_db::{BACKUP_FILE_NAME, Db, settings};
use musaic_secrets::migrate::SCRUB_TMP_SUFFIX;
use musaic_secrets::{MemoryStore, SecretStore, migrate_settings_secrets};
use rusqlite::Connection;

/// Obviously fake, with the length and shape of a real Anthropic key (108 chars).
const KEY: &str = "sk-ant-api03-FAKE-TEST-ONLY-not-a-real-key-0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRS-TESTAA";
/// The value the Python app held before the user replaced it (left in free space).
const OLD: &str = "sk-ant-api03-OLD-FAKE-TEST-ONLY-replaced-key-9876543210zyxwvutsrqponmlkjihgfedcbaZYXWVUTSRQPONMLKJ-TESTBB";
/// A FireCrawl-shaped fake key.
const FIRECRAWL: &str = "fc-FAKE-TEST-ONLY-0a1b2c3d4e5f60718293a4b5c6d7e8f9";
/// The AcoustID dummy the Python app fixture already holds.
const DUMMY: &str = "migration-offline-dummy";

/// Whole values plus distinctive fragments (a later cell can overwrite part of
/// a freed one, leaving only a tail or a head).
const NEEDLES: [&str; 9] = [
    KEY,
    OLD,
    FIRECRAWL,
    DUMMY,
    "FAKE-TEST-ONLY-not-a-real-key",
    "uvwxyzABCDEFGHIJKLMNOPQRS-TESTAA",
    "OLD-FAKE-TEST-ONLY-replaced",
    "SRQPONMLKJ-TESTBB",
    "0a1b2c3d4e5f60718293",
];

fn assert_precondition(cond: bool, what: &str) {
    assert!(cond, "precondition: {what}");
}

/// Every (file, needle) pair under `dir`, recursively.
fn leaks(dir: &Path) -> Vec<(PathBuf, &'static str)> {
    let mut hits = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let bytes = std::fs::read(&p).unwrap();
            for n in NEEDLES {
                if common::contains(&bytes, n) {
                    hits.push((p.clone(), n));
                }
            }
        }
    }
    hits.sort();
    hits
}

fn assert_no_trace(dir: &Path) {
    assert_eq!(leaks(dir), Vec::new());
}

fn wal_len(dir: &Path) -> u64 {
    std::fs::metadata(dir.join("library.db-wal")).map_or(0, |m| m.len())
}

fn copy_db_and_wal(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for s in ["", "-wal"] {
        let src = from.join(format!("library.db{s}"));
        if src.exists() {
            std::fs::copy(&src, to.join(format!("library.db{s}"))).unwrap();
        }
    }
}

/// `<tmp>/cfg/library.db` + `-wal` as the Python app leaves them after a crash
/// (or while it still runs): v16 in WAL mode, the keys written through
/// the Python app's upsert with autocheckpoint off, so the newest rows exist only in
/// the WAL and the overwritten `OLD` value sits in free space.
fn python_wal_state() -> (tempfile::TempDir, PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let root = musaic_testdata::canonical(t.path());
    let stage = root.join("stage");
    std::fs::create_dir_all(&stage).unwrap();
    std::fs::copy(
        musaic_testdata::public("db/v16-synthetic-offline.db"),
        stage.join("library.db"),
    )
    .unwrap();
    let w = Connection::open(stage.join("library.db")).unwrap();
    let mode: String = w
        .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    w.execute_batch("PRAGMA wal_autocheckpoint = 0").unwrap();
    settings::set_raw(&w, "ai_api_key", OLD).unwrap();
    settings::set_raw(&w, "ai_api_key", KEY).unwrap();
    settings::set_raw(&w, "firecrawl_api_key", FIRECRAWL).unwrap();
    let cfg = root.join("cfg");
    copy_db_and_wal(&stage, &cfg);
    drop(w);
    std::fs::remove_dir_all(&stage).unwrap();
    assert_precondition(
        common::file_contains(&cfg.join("library.db-wal"), KEY),
        "the key is in the WAL",
    );
    assert_precondition(
        !common::file_contains(&cfg.join("library.db"), KEY),
        "the key is only in the WAL",
    );
    (t, cfg)
}

fn open(cfg: &Path) -> Db {
    let (db, _) = Db::open(cfg.join("library.db")).unwrap();
    let mode: String = db
        .connection()
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    assert_precondition(
        common::file_contains(&cfg.join(BACKUP_FILE_NAME), KEY),
        "the pre-migration backup holds the key",
    );
    db
}

fn assert_rows_gone(db: &Db) {
    for k in ["ai_api_key", "firecrawl_api_key", "acoustid_api_key"] {
        assert_eq!(settings::get_raw(db.connection(), k).unwrap(), None, "{k}");
    }
}

/// The migrated key moved; the retired ones never reach the store.
fn assert_store_holds(store: &MemoryStore) {
    assert_eq!(store.get("ai_api_key").unwrap(), None);
    assert_eq!(store.get("firecrawl_api_key").unwrap(), None);
    assert_eq!(
        store.get("acoustid_api_key").unwrap().as_deref(),
        Some(DUMMY)
    );
}

/// The backup is a self-contained rollback-journal v16 file with nothing beside it.
fn assert_backup_clean(cfg: &Path) {
    let b = cfg.join(BACKUP_FILE_NAME);
    let c = Connection::open_with_flags(&b, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mode: String = c
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "delete");
    let check: String = c
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(check, "ok");
    let v: i64 = c
        .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, musaic_db::PYTHON_SCHEMA_VERSION);
    drop(c);
    for s in ["-wal", "-shm", "-journal", SCRUB_TMP_SUFFIX] {
        assert!(!Path::new(&format!("{}{s}", b.display())).exists(), "{s}");
    }
}

/// Close the database and check what is left: no trace, no stray files.
fn close_and_check(db: Db, cfg: &Path) {
    drop(db);
    assert_no_trace(cfg);
    assert_eq!(names(cfg), ["library.db", BACKUP_FILE_NAME]);
}

/// Positive: keys only in the WAL, an overwritten value in
/// free space, no other connection. Nothing holds them afterwards, while the
/// database is open (WAL truncated) and after it is closed.
#[test]
fn wal_database_keys_leave_no_trace() {
    let (_t, cfg) = python_wal_state();
    let db = open(&cfg);
    let store = MemoryStore::new();
    let r = migrate_settings_secrets(&db, &store).unwrap();
    assert_eq!(
        r.removed_rows(),
        ["acoustid_api_key", "ai_api_key", "firecrawl_api_key"]
    );
    assert_eq!(r.backup_error, None);
    assert_store_holds(&store);
    assert_rows_gone(&db);
    assert_eq!(wal_len(&cfg), 0);
    assert_no_trace(&cfg);
    assert_backup_clean(&cfg);
    close_and_check(db, &cfg);
}

/// Another connection that is open but idle (the API's own
/// connections) does not block the checkpoint; the result is the same.
#[test]
fn idle_second_connection_does_not_block() {
    let (_t, cfg) = python_wal_state();
    let db = open(&cfg);
    let other = musaic_db::open_connection(cfg.join("library.db")).unwrap();
    let n: i64 = other
        .query_row("SELECT COUNT(*) FROM settings", [], |r| r.get(0))
        .unwrap();
    assert!(n > 0);
    let store = MemoryStore::new();
    migrate_settings_secrets(&db, &store).unwrap();
    assert_rows_gone(&db);
    assert_eq!(wal_len(&cfg), 0);
    assert_no_trace(&cfg);
    drop(other);
    close_and_check(db, &cfg);
}

/// Fail-safe: a reader holding a snapshot that still needs the WAL
/// blocks the checkpoint before anything is deleted. The migration reports an
/// error and deletes nothing (the keys are in the keyring, the rows stay; no
/// new copy of a key is made). Once the reader is gone, the next call
/// finishes and leaves no trace.
#[test]
fn reader_blocking_the_first_checkpoint_fails_safe_then_retries() {
    let (_t, cfg) = python_wal_state();
    let db = open(&cfg);
    assert_precondition(wal_len(&cfg) > 0, "the WAL holds frames");
    let reader = musaic_db::open_connection(cfg.join("library.db")).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    let _: i64 = reader
        .query_row("SELECT COUNT(*) FROM settings", [], |r| r.get(0))
        .unwrap();
    let store = MemoryStore::new();
    let err = migrate_settings_secrets(&db, &store).unwrap_err();
    assert!(err.to_string().contains("checkpoint"), "{err}");
    assert_eq!(
        settings::get_raw(db.connection(), "ai_api_key")
            .unwrap()
            .as_deref(),
        Some(KEY),
        "nothing deleted"
    );
    assert_store_holds(&store);

    reader.execute_batch("COMMIT").unwrap();
    let r = migrate_settings_secrets(&db, &store).unwrap();
    assert_eq!(r.kept_keyring, ["acoustid_api_key"]);
    assert_eq!(r.dropped_retired, ["ai_api_key", "firecrawl_api_key"]);
    assert_rows_gone(&db);
    assert_eq!(wal_len(&cfg), 0);
    assert_no_trace(&cfg);
    drop(reader);
    assert_backup_clean(&cfg);
    close_and_check(db, &cfg);
}

/// Fail-safe: a reader whose snapshot predates the delete (taken on a
/// checkpointed, empty WAL) lets the first checkpoint through but blocks the
/// last one, so the old pages holding the key stay in library.db. The
/// migration must not report success; the next call (with no rows left to
/// remove) completes the checkpoint and the trace is gone.
#[test]
fn reader_blocking_the_last_checkpoint_is_an_error_until_it_completes() {
    let (_t, cfg) = python_wal_state();
    let db = open(&cfg);
    db.connection()
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
        .unwrap();
    assert_precondition(wal_len(&cfg) == 0, "the WAL is empty");
    let reader = musaic_db::open_connection(cfg.join("library.db")).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    let _: i64 = reader
        .query_row("SELECT COUNT(*) FROM settings", [], |r| r.get(0))
        .unwrap();
    let store = MemoryStore::new();
    let err = migrate_settings_secrets(&db, &store).unwrap_err();
    assert!(err.to_string().contains("checkpoint"), "{err}");
    assert_rows_gone(&db);
    assert_store_holds(&store);
    // The reason the error is warranted: the old pages are still there.
    assert!(common::file_contains(&cfg.join("library.db"), KEY));

    // Still blocked: still an error, never a silent success.
    assert!(migrate_settings_secrets(&db, &store).is_err());

    reader.execute_batch("COMMIT").unwrap();
    let r = migrate_settings_secrets(&db, &store).unwrap();
    assert!(r.removed_rows().is_empty(), "{r:?}");
    assert_eq!(wal_len(&cfg), 0);
    assert_no_trace(&cfg);
    drop(reader);
    assert_backup_clean(&cfg);
    close_and_check(db, &cfg);
}

/// Simulate a crash of the process holding `db`: copy the live file and its WAL
/// to a new directory (with the backup), as the files are at that instant, and
/// open the copy. The old connection is closed only after the copy.
fn crash_and_reopen(db: Db, cfg: &Path, to: &Path) -> Db {
    copy_db_and_wal(cfg, to);
    for s in ["", SCRUB_TMP_SUFFIX] {
        let p = cfg.join(format!("{BACKUP_FILE_NAME}{s}"));
        if p.exists() {
            std::fs::copy(&p, to.join(format!("{BACKUP_FILE_NAME}{s}"))).unwrap();
        }
    }
    drop(db);
    Db::open(to.join("library.db")).unwrap().0
}

/// Cancellation under WAL: interrupted after the keyring write (of the
/// migrated key; a retired one is never written), before any scrub. The rerun
/// keeps the keyring's value and leaves no trace.
#[test]
fn wal_resume_after_keyring_write() {
    let (_t, cfg) = python_wal_state();
    let db = open(&cfg);
    let store = MemoryStore::new();
    store.set("acoustid_api_key", DUMMY).unwrap();
    let r = migrate_settings_secrets(&db, &store).unwrap();
    assert_eq!(r.moved, Vec::<&str>::new());
    assert_eq!(r.kept_keyring, ["acoustid_api_key"]);
    assert_store_holds(&store);
    assert_rows_gone(&db);
    assert_no_trace(&cfg);
    close_and_check(db, &cfg);
}

/// Cancellation under WAL: interrupted after the rows were deleted
/// (secure_delete, committed) but before the WAL was checkpointed. The crash
/// leaves the old pages in library.db and the new ones in the WAL. The rerun
/// has no rows to remove but must still finish the checkpoint.
#[test]
fn wal_resume_after_delete_before_checkpoint() {
    let (t, cfg) = python_wal_state();
    let db = open(&cfg);
    let store = MemoryStore::new();
    store.set("acoustid_api_key", DUMMY).unwrap();
    db.connection()
        .execute_batch(
            "PRAGMA wal_autocheckpoint = 0;
             PRAGMA secure_delete = ON;
             VACUUM;
             DELETE FROM settings
              WHERE key IN ('ai_api_key', 'firecrawl_api_key', 'acoustid_api_key');",
        )
        .unwrap();
    let crashed = musaic_testdata::canonical(t.path()).join("crashed");
    let db = crash_and_reopen(db, &cfg, &crashed);
    assert_precondition(!leaks(&crashed).is_empty(), "the crash left a trace");
    let r = migrate_settings_secrets(&db, &store).unwrap();
    assert!(r.removed_rows().is_empty(), "{r:?}");
    assert_eq!(wal_len(&crashed), 0);
    assert_no_trace(&crashed);
    close_and_check(db, &crashed);
}

/// Cancellation under WAL: the live scrub finished, the process died
/// in the backup scrub, leaving the temp copy (still holding every key) and a
/// rollback journal beside it. The rerun removes them and scrubs the backup.
#[test]
fn wal_resume_after_live_scrub_during_backup_scrub() {
    let (t, cfg) = python_wal_state();
    let db = open(&cfg);
    let b = cfg.join(BACKUP_FILE_NAME);
    let backup_before = std::fs::read(&b).unwrap();
    let store = MemoryStore::new();
    migrate_settings_secrets(&db, &store).unwrap();
    // Put back the unscrubbed backup and a half-done scrub beside it.
    std::fs::write(&b, &backup_before).unwrap();
    let stale = format!("{}{SCRUB_TMP_SUFFIX}", b.display());
    std::fs::write(&stale, &backup_before).unwrap();
    std::fs::write(format!("{stale}-journal"), KEY).unwrap();
    let crashed = musaic_testdata::canonical(t.path()).join("crashed");
    let db = crash_and_reopen(db, &cfg, &crashed);
    std::fs::copy(
        format!("{stale}-journal"),
        crashed.join(format!("{BACKUP_FILE_NAME}{SCRUB_TMP_SUFFIX}-journal")),
    )
    .unwrap();
    assert_precondition(leaks(&crashed).len() >= 3, "backup + temp + journal");
    let r = migrate_settings_secrets(&db, &store).unwrap();
    assert!(r.removed_rows().is_empty());
    assert_eq!(
        r.backup_scrubbed,
        ["ai_api_key", "acoustid_api_key", "firecrawl_api_key"]
    );
    assert_no_trace(&crashed);
    assert_backup_clean(&crashed);
    close_and_check(db, &crashed);
}

/// A backup left in WAL mode with its newest rows (the keys) still in
/// a `-wal` beside it (as a build that did not convert the backup to
/// rollback-journal mode could leave it). The scrub must see those rows, and
/// leave a self-contained rollback-journal file with nothing beside it.
#[test]
fn wal_mode_backup_with_its_own_wal_is_scrubbed() {
    let (t, cfg) = python_wal_state();
    let db = open(&cfg);
    // Replace the backup with a WAL-mode copy of the Python state.
    let (_t2, py) = python_wal_state();
    let b = cfg.join(BACKUP_FILE_NAME);
    std::fs::copy(py.join("library.db"), &b).unwrap();
    std::fs::copy(py.join("library.db-wal"), format!("{}-wal", b.display())).unwrap();
    let store = MemoryStore::new();
    let r = migrate_settings_secrets(&db, &store).unwrap();
    assert_eq!(r.backup_error, None);
    assert_eq!(
        r.backup_scrubbed,
        ["ai_api_key", "acoustid_api_key", "firecrawl_api_key"]
    );
    assert_no_trace(&cfg);
    assert_backup_clean(&cfg);
    close_and_check(db, &cfg);
    drop(t);
}

/// Junk beside the backup (a `-journal` that is not a valid journal
/// but holds a key, a `-shm`) is not part of the scrubbed file and must not
/// survive it, nor be applied to it.
#[test]
fn junk_beside_the_backup_is_removed() {
    let (_t, cfg) = python_wal_state();
    let db = open(&cfg);
    let b = cfg.join(BACKUP_FILE_NAME);
    std::fs::write(format!("{}-journal", b.display()), KEY).unwrap();
    std::fs::write(format!("{}-shm", b.display()), FIRECRAWL).unwrap();
    let store = MemoryStore::new();
    let r = migrate_settings_secrets(&db, &store).unwrap();
    assert_eq!(r.backup_error, None);
    assert_no_trace(&cfg);
    assert_backup_clean(&cfg);
    close_and_check(db, &cfg);
}
