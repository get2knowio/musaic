//! A crash inside the pre-migration backup can leave
//! `library.v16.before-rust.db.tmp` and its `-journal` / `-wal` / `-shm`
//! beside the database. They hold whatever the v16 database held, settings
//! rows with API keys included, so the next open removes all four before it
//! redoes the backup (and also when an earlier attempt's backup is kept).
//!
//! Starting state: a copy of testdata/db/v16-synthetic-offline.db (written by
//! the Python app). The leftovers are simulated: the temp copy is a real
//! SQLite copy of that database carrying a secret settings row, the sidecars
//! are files carrying the same value.

use std::path::{Path, PathBuf};

use musaic_db::{BACKUP_FILE_NAME, Db, Opened};
use rusqlite::Connection;

const SECRET: &str = "sk-leftover-in-tmp-0123456789";

fn fixture_copy() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("library.db");
    std::fs::copy(musaic_testdata::public("db/v16-synthetic-offline.db"), &db).unwrap();
    (dir, db)
}

fn leftovers(dir: &Path) -> Vec<PathBuf> {
    ["", "-journal", "-wal", "-shm"]
        .iter()
        .map(|s| dir.join(format!("{BACKUP_FILE_NAME}.tmp{s}")))
        .collect()
}

/// What a crash mid-backup leaves: the temp copy (with a key row) and its
/// three sidecars (with the key's bytes).
fn plant_leftovers(dir: &Path, db: &Path) {
    let files = leftovers(dir);
    std::fs::copy(db, &files[0]).unwrap();
    {
        let c = Connection::open(&files[0]).unwrap();
        c.execute(
            "INSERT OR REPLACE INTO settings(key, value) VALUES ('ai_api_key', ?1)",
            [SECRET],
        )
        .unwrap();
    }
    for f in &files[1..] {
        std::fs::write(f, format!("leftover page holding {SECRET}")).unwrap();
    }
    for f in &files {
        assert!(f.exists(), "{}", f.display());
    }
}

fn contains(path: &Path, needle: &str) -> bool {
    let bytes = std::fs::read(path).unwrap();
    bytes.windows(needle.len()).any(|w| w == needle.as_bytes())
}

// The leftovers are gone after the next open, which
// writes a fresh backup of the untouched v16 file (no key value in it).
#[test]
fn leftover_tmp_and_sidecars_are_removed_before_the_backup_is_redone() {
    let (dir, path) = fixture_copy();
    assert!(!contains(&path, SECRET));
    plant_leftovers(dir.path(), &path);

    let (_db, opened) = Db::open(&path).unwrap();
    let backup = dir.path().join(BACKUP_FILE_NAME);
    // (The shape of `Opened::Migrated` is another agent's to change; only
    // "migrated from 16" is pinned here.)
    assert!(
        matches!(opened, Opened::Migrated { from: 16, .. }),
        "{opened:?}"
    );
    for f in leftovers(dir.path()) {
        assert!(!f.exists(), "left behind: {}", f.display());
    }
    assert!(!contains(&backup, SECRET));
    let b = Connection::open(&backup).unwrap();
    let v: i64 = b
        .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, 16);
}

// An earlier attempt's backup is kept (never overwritten),
// and the leftovers of a later interrupted attempt are removed all the same.
#[test]
fn leftovers_are_removed_when_the_backup_is_kept() {
    let (dir, path) = fixture_copy();
    let backup = dir.path().join(BACKUP_FILE_NAME);
    std::fs::write(&backup, b"earlier backup").unwrap();
    plant_leftovers(dir.path(), &path);

    let (_db, opened) = Db::open(&path).unwrap();
    assert!(matches!(opened, Opened::Migrated { from: 16, .. }));
    assert_eq!(std::fs::read(&backup).unwrap(), b"earlier backup");
    for f in leftovers(dir.path()) {
        assert!(!f.exists(), "left behind: {}", f.display());
    }
}

// The helper itself: missing files are not an error; every sidecar goes.
#[test]
fn remove_db_with_sidecars_tolerates_missing_files() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("x.db");
    musaic_db::backup::remove_db_with_sidecars(&p).unwrap();
    std::fs::write(dir.path().join("x.db-wal"), b"w").unwrap();
    musaic_db::backup::remove_db_with_sidecars(&p).unwrap();
    assert!(!dir.path().join("x.db-wal").exists());
}
