//! `checkpoint_on_close`: the last thing shutdown does to the database.
//! Fresh databases in temp dirs only.

use std::time::Duration;

use musaic_db::{CloseCheckpoint, Db, checkpoint_on_close};

#[test]
fn folds_the_wal_into_the_main_file() {
    let td = tempfile::tempdir().unwrap();
    let path = td.path().join("library.db");
    let (db, _) = Db::open_or_create(&path).unwrap();
    db.connection()
        .execute_batch(
            "PRAGMA wal_autocheckpoint = 0;
             INSERT INTO settings (key, value) VALUES ('invented', 'value');",
        )
        .unwrap();
    let wal = td.path().join("library.db-wal");
    assert!(std::fs::metadata(&wal).unwrap().len() > 0);
    // `db` is still open but idle: the checkpoint completes and truncates.
    assert_eq!(
        checkpoint_on_close(&path, Duration::from_secs(1)).unwrap(),
        CloseCheckpoint::Done
    );
    assert_eq!(std::fs::metadata(&wal).unwrap().len(), 0);
    drop(db);
    let main =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let v: String = main
        .query_row(
            "SELECT value FROM settings WHERE key = 'invented'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(v, "value");
}

#[test]
fn a_reader_in_a_transaction_is_reported_as_blocked() {
    let td = tempfile::tempdir().unwrap();
    let path = td.path().join("library.db");
    let (db, _) = Db::open_or_create(&path).unwrap();
    let reader = rusqlite::Connection::open(&path).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    let _: i64 = reader
        .query_row("SELECT COUNT(*) FROM settings", [], |r| r.get(0))
        .unwrap();
    db.connection()
        .execute_batch(
            "PRAGMA wal_autocheckpoint = 0;
             INSERT INTO settings (key, value) VALUES ('invented', 'value');",
        )
        .unwrap();
    assert_eq!(
        checkpoint_on_close(&path, Duration::from_millis(50)).unwrap(),
        CloseCheckpoint::Blocked
    );
    reader.execute_batch("COMMIT").unwrap();
    assert_eq!(
        checkpoint_on_close(&path, Duration::from_millis(50)).unwrap(),
        CloseCheckpoint::Done
    );
}

#[test]
fn no_database_is_not_created() {
    let td = tempfile::tempdir().unwrap();
    let path = td.path().join("library.db");
    assert_eq!(
        checkpoint_on_close(&path, Duration::from_millis(50)).unwrap(),
        CloseCheckpoint::NoDatabase
    );
    assert!(!path.exists());
}
