//! Older copies of the database in the config directory (snapshots under
//! `backups/`, files a restore moved aside) lose their API key rows too, are
//! verified byte for byte, keep their modification time, and a copy that
//! cannot be cleaned is reported and left unchanged. Invented keys and
//! databases only; in-memory secrets store.

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use common::{file_contains, files_containing};
use musaic_db::{Db, backup};
use musaic_secrets::migrate::SCRUB_TMP_SUFFIX;
use musaic_secrets::{MemoryStore, SecretStore, migrate_settings_secrets};
use rusqlite::Connection;

const KEY: &str = "invented-acoustid-key-0001";
const OLDER: &str = "invented-acoustid-key-older-0000";

/// A config dir with a fresh live database (no key rows) and a `backups/`.
fn config() -> (tempfile::TempDir, PathBuf, Db) {
    let t = tempfile::tempdir().unwrap();
    let dir = musaic_testdata::canonical(t.path());
    let (db, _) = Db::open_or_create(dir.join("library.db")).unwrap();
    std::fs::create_dir_all(dir.join("backups")).unwrap();
    (t, dir, db)
}

/// A self-contained database copy at `path` (what a snapshot is), holding
/// `value` as its AcoustID key. With `overwritten`, the row first held that
/// older value (padded so it spills onto overflow pages) and was then
/// updated, as the Python app's setter does without `secure_delete`, so the
/// older value sits in free pages too.
fn snapshot_with_key(db: &Db, path: &Path, value: &str, overwritten: Option<&str>) {
    let c = db.connection();
    c.execute(&format!("VACUUM INTO '{}'", path.display()), [])
        .unwrap();
    let w = Connection::open(path).unwrap();
    w.execute_batch("PRAGMA journal_mode = DELETE").unwrap();
    if let Some(old) = overwritten {
        w.execute(
            "INSERT OR REPLACE INTO settings(key, value) VALUES ('acoustid_api_key', ?1)",
            [format!("{old}{}", " ".repeat(6000))],
        )
        .unwrap();
    }
    w.execute(
        "INSERT OR REPLACE INTO settings(key, value) VALUES ('acoustid_api_key', ?1)",
        [value],
    )
    .unwrap();
    w.execute(
        "INSERT OR REPLACE INTO settings(key, value) VALUES ('theme', 'dark')",
        [],
    )
    .unwrap();
}

fn set_mtime(p: &Path, t: SystemTime) {
    std::fs::OpenOptions::new()
        .write(true)
        .open(p)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

fn mtime(p: &Path) -> SystemTime {
    std::fs::metadata(p).unwrap().modified().unwrap()
}

fn ago(days: u64) -> SystemTime {
    // Whole seconds: some filesystems keep no finer modification times.
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    SystemTime::UNIX_EPOCH + Duration::from_secs(now - days * 86_400)
}

fn row(p: &Path, key: &str) -> Option<String> {
    let c = Connection::open_with_flags(p, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    c.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
        r.get(0)
    })
    .ok()
}

fn store_with_key() -> MemoryStore {
    let s = MemoryStore::new();
    s.set("acoustid_api_key", KEY).unwrap();
    s
}

#[test]
fn snapshots_lose_their_keys_and_keep_their_order() {
    let (_t, dir, db) = config();
    let snaps = dir.join("backups");
    let a = snaps.join("library-20260101-000000Z-v16.db");
    let b = snaps.join("library-20260201-000000Z-v16.db");
    snapshot_with_key(&db, &a, KEY, Some(OLDER));
    snapshot_with_key(&db, &b, KEY, None);
    set_mtime(&a, ago(60));
    set_mtime(&b, ago(30));
    let before = (mtime(&a), mtime(&b));
    let order_before: Vec<String> = backup::list_snapshots(&snaps)
        .unwrap()
        .into_iter()
        .map(|e| e.filename)
        .collect();
    assert!(
        file_contains(&a, OLDER),
        "precondition: older value in free space"
    );

    let store = store_with_key();
    let r = migrate_settings_secrets(&db, &store).unwrap();
    assert_eq!(r.copies_scrubbed, vec![a.clone(), b.clone()]);
    assert!(r.copies_left.is_empty(), "{:?}", r.copies_left);
    drop(db);

    for needle in [KEY, OLDER] {
        assert!(
            files_containing(&dir, needle).is_empty(),
            "{needle}: {:?}",
            files_containing(&dir, needle)
        );
    }
    for p in [&a, &b] {
        assert_eq!(row(p, "acoustid_api_key"), None);
        assert_eq!(row(p, "theme").as_deref(), Some("dark"), "other rows kept");
        let c = Connection::open(p).unwrap();
        let ok: String = c
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ok, "ok");
    }
    assert_eq!((mtime(&a), mtime(&b)), before, "modification times kept");
    let listed = backup::list_snapshots(&snaps).unwrap();
    assert!(listed.iter().all(|e| e.valid));
    let order: Vec<String> = listed.into_iter().map(|e| e.filename).collect();
    assert_eq!(order, order_before);
    let names = common::names(&snaps);
    assert!(
        !names.iter().any(|n| n.contains(SCRUB_TMP_SUFFIX)),
        "{names:?}"
    );

    // A second run finds nothing and rewrites nothing.
    let (db, _) = Db::open(dir.join("library.db")).unwrap();
    let bytes = std::fs::read(&a).unwrap();
    let r = migrate_settings_secrets(&db, &store).unwrap();
    assert!(
        r.copies_scrubbed.is_empty() && r.copies_left.is_empty(),
        "{r:?}"
    );
    assert_eq!(std::fs::read(&a).unwrap(), bytes);
}

/// The key moved out of the live database in the same run is also removed
/// from snapshots that hold it (the usual first launch after an upgrade).
#[test]
fn first_launch_moves_the_key_and_cleans_old_snapshots() {
    let (_t, dir, db) = config();
    let snap = dir.join("backups/library-20260101-000000Z-v16.db");
    snapshot_with_key(&db, &snap, KEY, None);
    db.connection()
        .execute(
            "INSERT INTO settings(key, value) VALUES ('acoustid_api_key', ?1)",
            [KEY],
        )
        .unwrap();
    let store = MemoryStore::new();
    let r = migrate_settings_secrets(&db, &store).unwrap();
    assert_eq!(r.moved, vec!["acoustid_api_key"]);
    assert_eq!(r.copies_scrubbed, vec![snap.clone()]);
    drop(db);
    assert!(files_containing(&dir, KEY).is_empty());
}

/// A file a restore moved aside, with the WAL moved aside beside it (the
/// Python app moved the files under open connections), whose key row exists
/// only in that WAL: the WAL is folded into a copy, the copy scrubbed and
/// verified, then it replaces the aside and the WAL / shm asides go.
#[test]
fn restore_aside_with_its_wal_is_scrubbed() {
    let (_t, dir, db) = config();
    let aside = dir.join("library.db.bak-20260101-000000Z");
    {
        let c = db.connection();
        c.execute(&format!("VACUUM INTO '{}'", aside.display()), [])
            .unwrap();
    }
    // Make a WAL-mode copy whose newest rows are only in its WAL.
    let work = dir.join("work-src.db");
    std::fs::copy(&aside, &work).unwrap();
    let w = Connection::open(&work).unwrap();
    w.execute_batch(
        "PRAGMA journal_mode = WAL; PRAGMA wal_autocheckpoint = 0;
         INSERT INTO settings(key, value) VALUES ('theme', 'dark');",
    )
    .unwrap();
    w.execute(
        "INSERT INTO settings(key, value) VALUES ('acoustid_api_key', ?1)",
        [KEY],
    )
    .unwrap();
    // Copy the three files while the connection is open (the WAL is not
    // checkpointed), as a restore under open connections would move them.
    std::fs::copy(&work, &aside).unwrap();
    std::fs::copy(
        dir.join("work-src.db-wal"),
        dir.join("library.db-wal.bak-20260101-000000Z"),
    )
    .unwrap();
    std::fs::copy(
        dir.join("work-src.db-shm"),
        dir.join("library.db-shm.bak-20260101-000000Z"),
    )
    .unwrap();
    drop(w);
    for ext in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{ext}", work.display()));
    }
    assert!(file_contains(
        &dir.join("library.db-wal.bak-20260101-000000Z"),
        KEY
    ));

    let r = migrate_settings_secrets(&db, &store_with_key()).unwrap();
    assert_eq!(r.copies_scrubbed, vec![aside.clone()], "{r:?}");
    assert!(r.copies_left.is_empty(), "{:?}", r.copies_left);
    drop(db);
    assert!(files_containing(&dir, KEY).is_empty());
    assert!(!dir.join("library.db-wal.bak-20260101-000000Z").exists());
    assert!(!dir.join("library.db-shm.bak-20260101-000000Z").exists());
    assert_eq!(
        row(&aside, "theme").as_deref(),
        Some("dark"),
        "WAL folded in"
    );
    assert_eq!(row(&aside, "acoustid_api_key"), None);
}

/// A snapshot that cannot be opened (damaged) is reported, left byte for
/// byte as it was, and does not stop the others or make the run fail.
#[test]
fn an_unreadable_snapshot_is_reported_and_left_alone() {
    let (_t, dir, db) = config();
    let good = dir.join("backups/library-20260101-000000Z-v16.db");
    snapshot_with_key(&db, &good, KEY, None);
    let bad = dir.join("backups/library-20260102-000000Z-v16.db");
    let mut junk = b"not a database; acoustid_api_key=".to_vec();
    junk.extend_from_slice(KEY.as_bytes());
    std::fs::write(&bad, &junk).unwrap();

    let r = migrate_settings_secrets(&db, &store_with_key()).unwrap();
    assert_eq!(r.copies_scrubbed, vec![good.clone()]);
    assert_eq!(r.copies_left.len(), 1, "{:?}", r.copies_left);
    assert_eq!(r.copies_left[0].0, bad);
    assert!(
        r.copies_left[0].1.contains("could not be scrubbed"),
        "{:?}",
        r.copies_left
    );
    assert!(!r.copies_left[0].1.contains(KEY), "no value in the report");
    assert_eq!(std::fs::read(&bad).unwrap(), junk, "left unchanged");
    assert!(!file_contains(&good, KEY));
}

/// Never reported clean while the bytes remain: a copy that also holds the
/// key outside the settings table cannot be cleaned by deleting rows, so the
/// scrubbed copy fails the byte check and the original stays as it was.
#[test]
fn bytes_left_after_scrubbing_are_reported_not_hidden() {
    let (_t, dir, db) = config();
    let snap = dir.join("backups/library-20260101-000000Z-v16.db");
    snapshot_with_key(&db, &snap, KEY, None);
    {
        let w = Connection::open(&snap).unwrap();
        w.execute_batch("CREATE TABLE notes (body TEXT)").unwrap();
        w.execute("INSERT INTO notes VALUES (?1)", [format!("pasted {KEY}")])
            .unwrap();
    }
    let before = std::fs::read(&snap).unwrap();
    let r = migrate_settings_secrets(&db, &store_with_key()).unwrap();
    assert!(r.copies_scrubbed.is_empty(), "{r:?}");
    assert_eq!(r.copies_left.len(), 1);
    assert!(
        r.copies_left[0].1.contains("still in the file"),
        "{:?}",
        r.copies_left
    );
    assert_eq!(std::fs::read(&snap).unwrap(), before);
    let names = common::names(&dir.join("backups"));
    assert_eq!(names, vec!["library-20260101-000000Z-v16.db".to_string()]);
}

/// A key the keyring holds no value for keeps its row in the snapshot (the
/// same rule as the database file and its backup), and that is reported.
#[test]
fn a_key_the_keyring_does_not_hold_is_kept_and_reported() {
    let (_t, dir, db) = config();
    let snap = dir.join("backups/library-20260101-000000Z-v16.db");
    snapshot_with_key(&db, &snap, KEY, None);
    let before = std::fs::read(&snap).unwrap();
    let r = migrate_settings_secrets(&db, &MemoryStore::new()).unwrap();
    assert!(r.copies_scrubbed.is_empty());
    assert_eq!(r.copies_left.len(), 1);
    assert!(
        r.copies_left[0].1.contains("keeps acoustid_api_key"),
        "{:?}",
        r.copies_left
    );
    assert_eq!(std::fs::read(&snap).unwrap(), before);
}

/// Snapshots taken by this build from a scrubbed database hold no key and
/// are not rewritten.
#[test]
fn clean_snapshots_are_not_touched() {
    let (_t, dir, db) = config();
    let snaps = dir.join("backups");
    backup::create_snapshot(&db, &snaps, 10, "20260101-000000Z", SystemTime::now()).unwrap();
    let p = backup::list_snapshots(&snaps).unwrap()[0].filename.clone();
    let p = snaps.join(p);
    set_mtime(&p, ago(3));
    let before = (std::fs::read(&p).unwrap(), mtime(&p));
    let r = migrate_settings_secrets(&db, &store_with_key()).unwrap();
    assert!(
        r.copies_scrubbed.is_empty() && r.copies_left.is_empty(),
        "{r:?}"
    );
    assert_eq!((std::fs::read(&p).unwrap(), mtime(&p)), before);
}
