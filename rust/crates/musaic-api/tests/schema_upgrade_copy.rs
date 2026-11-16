//! Before a database this build wrote (v17+) is upgraded, startup takes
//! one copy of it as a snapshot (listed, restorable and pruned with the
//! others), with no secret row in it; when the copy cannot be written the
//! database is not upgraded and startup fails with a clear message.

use std::path::Path;
use std::sync::Arc;

use musaic_api::{AppState, Network};
use rusqlite::Connection;

/// A v18 database as the previous build left it, with a key row the OS
/// keyring has not taken yet.
fn make_v18(cfg: &Path) {
    std::fs::create_dir_all(cfg).unwrap();
    let p = cfg.join("library.db");
    drop(musaic_db::Db::open_or_create(&p).unwrap());
    let c = Connection::open(&p).unwrap();
    c.execute_batch(
        "DROP INDEX idx_track_fingerprints_inputs; DROP TABLE check_inputs; \
         UPDATE schema_version SET version = 18; \
         INSERT INTO settings(key, value) VALUES ('acoustid_api_key', 'SECRET-KEY-VALUE'); \
         INSERT INTO settings(key, value) VALUES ('snapshot_retention_count', '10');",
    )
    .unwrap();
}

fn version(p: &Path) -> i64 {
    Connection::open(p)
        .unwrap()
        .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
        .unwrap()
}

fn build(cfg: &Path) -> Result<AppState, String> {
    AppState::builder(cfg)
        .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
        .network(Network::Offline)
        .frontend_dir(None)
        .subscribers(false)
        .auto_snapshot(false)
        .build()
        .map_err(|e| e.to_string())
}

#[test]
fn a_copy_is_taken_before_the_upgrade() {
    let d = tempfile::tempdir().unwrap();
    let cfg = d.path().join("config");
    make_v18(&cfg);
    let st = build(&cfg).unwrap();
    assert_eq!(version(&cfg.join("library.db")), musaic_db::SCHEMA_VERSION);
    let snaps =
        musaic_db::backup::list_snapshots(&cfg.join(musaic_core::config::SNAPSHOT_DIR_NAME))
            .unwrap();
    assert_eq!(snaps.len(), 1, "{snaps:?}");
    let s = &snaps[0];
    assert!(s.filename.ends_with("-v18.db"), "{}", s.filename);
    assert!(s.valid);
    assert_eq!(s.schema_version, Some(18));
    let copy = cfg
        .join(musaic_core::config::SNAPSHOT_DIR_NAME)
        .join(&s.filename);
    assert_eq!(version(&copy), 18);
    // No secret in the copy, not even in free pages.
    let bytes = std::fs::read(&copy).unwrap();
    assert!(
        !bytes
            .windows(b"SECRET-KEY-VALUE".len())
            .any(|w| w == b"SECRET-KEY-VALUE"),
        "the copy holds a key"
    );
    drop(st);
    // Already current: no second copy.
    drop(build(&cfg).unwrap());
    assert_eq!(
        musaic_db::backup::list_snapshots(&cfg.join(musaic_core::config::SNAPSHOT_DIR_NAME))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn no_copy_no_upgrade() {
    let d = tempfile::tempdir().unwrap();
    let cfg = d.path().join("config");
    make_v18(&cfg);
    // The snapshot folder cannot be made: a file is in its place.
    std::fs::write(
        cfg.join(musaic_core::config::SNAPSHOT_DIR_NAME),
        b"not a folder",
    )
    .unwrap();
    let err = build(&cfg).err().expect("startup must fail");
    assert!(err.contains("before upgrading it from schema v18"), "{err}");
    assert!(err.contains("not changed"), "{err}");
    assert_eq!(version(&cfg.join("library.db")), 18);
}

// ---- follow-ups: what makes the copy trustworthy ----

use musaic_services::startup::{PreUpgradeCopy, PreUpgradeCopyFailed, snapshot_before_schema_step};
use std::time::{Duration, SystemTime};

fn snap_dir(cfg: &Path) -> std::path::PathBuf {
    cfg.join(musaic_core::config::SNAPSHOT_DIR_NAME)
}

fn names(cfg: &Path) -> Vec<String> {
    musaic_db::backup::list_snapshots(&snap_dir(cfg))
        .unwrap()
        .into_iter()
        .map(|s| s.filename)
        .collect()
}

fn set_mtime(p: &Path, t: SystemTime) {
    std::fs::File::options()
        .write(true)
        .open(p)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

/// A database that exists but cannot be read is an error: the upgrade that
/// follows must not run because the check for a copy was skipped.
#[test]
fn an_unreadable_database_is_an_error_not_nothing_to_copy() {
    let d = tempfile::tempdir().unwrap();
    let cfg = d.path().join("config");
    std::fs::create_dir_all(&cfg).unwrap();
    let db = cfg.join("library.db");
    std::fs::write(
        &db,
        b"this is not a SQLite database at all, but it is not empty",
    )
    .unwrap();
    let err = snapshot_before_schema_step(&db, &snap_dir(&cfg), SystemTime::now()).unwrap_err();
    assert!(matches!(err, PreUpgradeCopyFailed::Read { .. }), "{err}");
    let msg = err.to_string();
    assert!(msg.contains("library.db"), "names the file: {msg}");
    assert!(msg.contains("not changed"), "{msg}");
    // And startup stops with it.
    let err = build(&cfg).err().expect("startup must fail");
    assert!(err.contains("could not read the database"), "{err}");
    assert!(!snap_dir(&cfg).exists(), "no copy was made");
}

/// What is not a library is not this check's business: a blank SQLite
/// file is created over, one without a version is refused by the open.
#[test]
fn a_blank_or_foreign_sqlite_file_needs_no_copy() {
    let d = tempfile::tempdir().unwrap();
    let cfg = d.path().join("config");
    std::fs::create_dir_all(&cfg).unwrap();
    let blank = cfg.join("blank.db");
    drop(
        Connection::open(&blank)
            .unwrap()
            .execute_batch("PRAGMA user_version = 3"),
    );
    // A SQLite file with no schema objects is a few header bytes after a write.
    Connection::open(&blank)
        .unwrap()
        .execute_batch("CREATE TABLE t(x); DROP TABLE t;")
        .unwrap();
    let foreign = cfg.join("foreign.db");
    Connection::open(&foreign)
        .unwrap()
        .execute_batch("CREATE TABLE other(x)")
        .unwrap();
    let norow = cfg.join("norow.db");
    Connection::open(&norow)
        .unwrap()
        .execute_batch("CREATE TABLE schema_version(version INTEGER)")
        .unwrap();
    for p in [&blank, &foreign, &norow] {
        assert_eq!(
            snapshot_before_schema_step(p, &snap_dir(&cfg), SystemTime::now()).unwrap(),
            PreUpgradeCopy::NotNeeded,
            "{}",
            p.display()
        );
    }
    assert!(!snap_dir(&cfg).exists());
}

/// The read waits for a lock rather than failing at once on "locked".
#[test]
fn the_read_waits_out_a_short_lock() {
    let d = tempfile::tempdir().unwrap();
    let cfg = d.path().join("config");
    make_v18(&cfg);
    let db = cfg.join("library.db");
    // Rollback-journal mode so an exclusive lock blocks readers.
    let locker = Connection::open(&db).unwrap();
    locker
        .execute_batch("PRAGMA journal_mode = DELETE; BEGIN EXCLUSIVE;")
        .unwrap();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(700));
        locker.execute_batch("COMMIT").unwrap();
    });
    let r = snapshot_before_schema_step(&db, &snap_dir(&cfg), SystemTime::now());
    t.join().unwrap();
    assert!(matches!(r, Ok(PreUpgradeCopy::Taken(_))), "{r:?}");
}

/// Starting again after a start that did not finish upgrading does not take
/// another full copy (and so does not prune older snapshots for it), unless
/// the database changed since the copy.
#[test]
fn a_second_start_reuses_the_copy_unless_the_database_changed() {
    let d = tempfile::tempdir().unwrap();
    let cfg = d.path().join("config");
    make_v18(&cfg);
    let db = cfg.join("library.db");
    let now = SystemTime::now();
    let first = match snapshot_before_schema_step(&db, &snap_dir(&cfg), now).unwrap() {
        PreUpgradeCopy::Taken(n) => n,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        snapshot_before_schema_step(&db, &snap_dir(&cfg), now).unwrap(),
        PreUpgradeCopy::Existing(first.clone())
    );
    assert_eq!(names(&cfg), std::slice::from_ref(&first), "no second copy");

    // The database changed after the copy: the copy no longer holds it.
    set_mtime(
        &snap_dir(&cfg).join(&first),
        SystemTime::now() - Duration::from_secs(3600),
    );
    let second = match snapshot_before_schema_step(&db, &snap_dir(&cfg), now).unwrap() {
        PreUpgradeCopy::Taken(n) => n,
        other => panic!("{other:?}"),
    };
    assert_ne!(first, second);
    assert_eq!(names(&cfg).len(), 2);
}

/// A snapshot of that version that is not a database at that version is not
/// reused.
#[test]
fn a_damaged_or_mislabelled_copy_is_not_reused() {
    let d = tempfile::tempdir().unwrap();
    let cfg = d.path().join("config");
    make_v18(&cfg);
    let dir = snap_dir(&cfg);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("library-20990101-000000Z-v18.db"), b"garbage").unwrap();
    let mislabelled = dir.join("library-20990101-000001Z-v18.db");
    std::fs::copy(cfg.join("library.db"), &mislabelled).unwrap();
    Connection::open(&mislabelled)
        .unwrap()
        .execute_batch("UPDATE schema_version SET version = 17")
        .unwrap();
    let r = snapshot_before_schema_step(&cfg.join("library.db"), &dir, SystemTime::now());
    assert!(matches!(r, Ok(PreUpgradeCopy::Taken(_))), "{r:?}");
}

/// Retention 1 with no minimum interval: the startup snapshot taken after
/// the upgrade would prune the pre-upgrade copy; it is kept.
#[test]
fn the_pre_upgrade_copy_survives_retention() {
    let d = tempfile::tempdir().unwrap();
    let cfg = d.path().join("config");
    make_v18(&cfg);
    Connection::open(cfg.join("library.db"))
        .unwrap()
        .execute_batch(
            "UPDATE settings SET value = '1' WHERE key = 'snapshot_retention_count'; \
             INSERT INTO settings(key, value) VALUES ('snapshot_min_interval_hours', '0');",
        )
        .unwrap();
    let build = |cfg: &Path| {
        AppState::builder(cfg)
            .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
            .network(Network::Offline)
            .frontend_dir(None)
            .subscribers(false)
            .auto_snapshot(true)
            .build()
            .map_err(|e| e.to_string())
            .unwrap()
    };
    drop(build(&cfg));
    let after_first = names(&cfg);
    assert_eq!(after_first.len(), 2, "{after_first:?}");
    assert!(after_first.iter().any(|n| n.ends_with("-v18.db")));
    // More starts replace the current-version snapshot, never the copy.
    // (Whole-second names: step the clock by waiting out the second.)
    std::thread::sleep(Duration::from_millis(1100));
    drop(build(&cfg));
    let after_second = names(&cfg);
    assert_eq!(after_second.len(), 2, "{after_second:?}");
    assert!(
        after_second.iter().any(|n| n.ends_with("-v18.db")),
        "the pre-upgrade copy was pruned: {after_second:?}"
    );
}

/// Restoring an older snapshot needs no extra copy: the file that is migrated
/// is a copy at the live path, the snapshot itself is untouched, and the
/// database it replaces is set aside.
#[test]
fn restoring_an_older_snapshot_leaves_it_and_the_set_aside_file_intact() {
    let d = tempfile::tempdir().unwrap();
    let cfg = d.path().join("config");
    let src = d.path().join("src");
    make_v18(&src);
    let dir = snap_dir(&cfg);
    std::fs::create_dir_all(&dir).unwrap();
    let name = "library-20260101-000000Z-v18.db";
    std::fs::copy(src.join("library.db"), dir.join(name)).unwrap();
    let before = std::fs::read(dir.join(name)).unwrap();

    let (live, _) = musaic_db::Db::open_or_create(cfg.join("library.db")).unwrap();
    let (restored, info) = musaic_db::backup::restore_from_snapshot(
        live,
        &dir,
        name,
        "20260102-000000Z",
        SystemTime::now(),
    )
    .map_err(|e| e.error.to_string())
    .unwrap();
    assert!(
        matches!(info.opened, musaic_db::Opened::Migrated { from: 18, .. }),
        "{:?}",
        info.opened
    );
    assert_eq!(
        restored.schema_version().unwrap(),
        musaic_db::SCHEMA_VERSION
    );
    assert_eq!(
        std::fs::read(dir.join(name)).unwrap(),
        before,
        "snapshot untouched"
    );
    assert_eq!(version(&dir.join(name)), 18);
    assert!(!info.aside_paths.is_empty());
    let aside = &info.aside_paths[0];
    assert_eq!(
        version(aside),
        musaic_db::SCHEMA_VERSION,
        "the old live file is kept"
    );
}
