//! A fresh install creates its database directly, compared
//! with what the Python app's `db.bootstrap()` writes
//! (testdata/golden/persistence/fresh-db.json)
//! and with the Python app's own empty v16 file migrated by `Db::open`
//! (testdata/db/v16-empty.db, written by the Python app).

use std::path::Path;

use musaic_db::{BACKUP_FILE_NAME, Db, Error, Opened, SCHEMA_VERSION};
use rusqlite::Connection;
use serde_json::Value;

fn golden() -> Value {
    let p = musaic_testdata::public("golden/persistence/fresh-db.json");
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

type MasterRow = (String, String, String, Option<String>);

fn master(conn: &Connection) -> Vec<MasterRow> {
    let mut st = conn
        .prepare("SELECT type, name, tbl_name, sql FROM sqlite_master ORDER BY rowid")
        .unwrap();
    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn golden_master(g: &Value) -> Vec<MasterRow> {
    g["sqlite_master"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r[0].as_str().unwrap().to_string(),
                r[1].as_str().unwrap().to_string(),
                r[2].as_str().unwrap().to_string(),
                r[3].as_str().map(str::to_string),
            )
        })
        .collect()
}

fn dir_names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// The Python app-written empty v16 database, copied and migrated by `Db::open`.
fn migrated_python_empty() -> (tempfile::TempDir, Vec<MasterRow>) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("library.db");
    std::fs::copy(musaic_testdata::public("db/v16-empty.db"), &p).unwrap();
    let (db, opened) = Db::open(&p).unwrap();
    assert!(matches!(opened, Opened::Migrated { from: 16, .. }));
    let m = master(db.connection());
    (dir, m)
}

/// A fresh database has exactly the schema of the Python app's
/// empty v16 database after the v17, v18 and v19 migrations, row for row in
/// sqlite_master.
#[test]
fn fresh_schema_equals_the_migrated_python_database() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("library.db");
    let (db, opened) = Db::open_or_create(&p).unwrap();
    assert_eq!(opened, Opened::Created);
    assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
    let (_d, expected) = migrated_python_empty();
    assert_eq!(master(db.connection()), expected);
}

/// Compared with what `db.bootstrap()` itself writes. The only
/// differences are the migrations' (v17: `fingerprint_impl`; v18: AcoustID
/// lookup outcome columns and `fingerprint_rejects`; v19: `check_inputs`
/// and the fingerprint inputs index), enumerated here; an unlisted difference or an
/// unused delta fails.
#[test]
fn fresh_database_equals_the_python_bootstrap_plus_the_migration_deltas() {
    let g = golden();
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("library.db");
    let (db, _) = Db::open_or_create(&p).unwrap();
    let conn = db.connection();

    // sqlite_master: the Python app's rows in the same order, then the rows v18
    // and v19 add. Changed: the track_fingerprints table's SQL (ALTER TABLE ADD
    // COLUMN fingerprint_impl (v17); acoustid_lookup_state,
    // acoustid_lookup_failures, acoustid_retry_after (v18)). Added: the
    // fingerprint_rejects table and its primary-key index (v18); the
    // check_inputs table, its primary-key index and the fingerprint inputs
    // index (v19). Every new
    // text is the one the migrations produce on the Python app's own file.
    let ours = master(conn);
    let theirs = golden_master(&g);
    let (_d, migrated) = migrated_python_empty();
    // (type, name, words the new SQL must contain, decision)
    let changed: &[(&str, &str, &[&str], &str)] = &[(
        "table",
        "track_fingerprints",
        &[
            "fingerprint_impl",
            "acoustid_lookup_state",
            "acoustid_lookup_failures",
            "acoustid_retry_after",
        ],
        "fingerprint implementation, AcoustID lookup outcome",
    )];
    // (type, name, table)
    let added: &[(&str, &str, &str)] = &[
        ("table", "fingerprint_rejects", "fingerprint_rejects"),
        (
            "index",
            "sqlite_autoindex_fingerprint_rejects_1",
            "fingerprint_rejects",
        ),
        ("table", "check_inputs", "check_inputs"),
        ("index", "sqlite_autoindex_check_inputs_1", "check_inputs"),
        (
            "index",
            "idx_track_fingerprints_inputs",
            "track_fingerprints",
        ),
    ];
    let mut used = vec![false; changed.len()];
    assert_eq!(ours.len(), theirs.len() + added.len());
    for (o, t) in ours.iter().zip(&theirs) {
        assert_eq!(
            (&o.0, &o.1, &o.2),
            (&t.0, &t.1, &t.2),
            "row order/type/name"
        );
        if o.3 == t.3 {
            continue;
        }
        let i = changed
            .iter()
            .position(|(ty, n, _, _)| *ty == o.0 && *n == o.1)
            .unwrap_or_else(|| panic!("unlisted schema difference in {} {}", o.0, o.1));
        used[i] = true;
        let m = migrated.iter().find(|m| m.1 == o.1).unwrap();
        assert_eq!(o.3, m.3, "delta value must be the migration's own text");
        for w in changed[i].2 {
            assert!(o.3.as_deref().unwrap().contains(w), "{w}: {:?}", o.3);
        }
    }
    assert!(
        used.iter().all(|u| *u),
        "unused delta: {changed:?} {used:?}"
    );
    for (o, (ty, name, table)) in ours[theirs.len()..].iter().zip(added) {
        assert_eq!((o.0.as_str(), o.1.as_str()), (*ty, *name));
        assert_eq!(o.2, *table);
        let m = migrated.iter().find(|m| m.1 == o.1).unwrap();
        assert_eq!(o, m, "added row must be the migration's own");
    }

    // schema_version: [16] -> [19] (v17, v18, then v19).
    let versions: Vec<i64> = conn
        .prepare("SELECT version FROM schema_version")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(g["schema_version"], serde_json::json!([16]));
    assert_eq!(versions, vec![SCHEMA_VERSION]);

    // Pragmas stored in the file: identical (journal_mode=wal, page size, ...).
    for (k, v) in g["persistent_pragmas"].as_object().unwrap() {
        let ours: Value = conn
            .query_row(&format!("PRAGMA {k}"), [], |r| {
                Ok(match r.get_ref(0)? {
                    rusqlite::types::ValueRef::Integer(i) => Value::from(i),
                    rusqlite::types::ValueRef::Text(t) => {
                        Value::from(String::from_utf8_lossy(t).into_owned())
                    }
                    other => panic!("{other:?}"),
                })
            })
            .unwrap();
        assert_eq!(&ours, v, "PRAGMA {k}");
    }
    // foreign_keys on the connection we hand out, as on the Python app's.
    let fk: i64 = conn
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
        .unwrap();
    assert_eq!(Value::from(fk), g["connection_pragmas"]["foreign_keys"]);
    drop(db);

    // No side files: the Python app leaves only library.db; so do we (no
    // library.v16.before-rust.db, no -wal/-shm after the last close).
    assert_eq!(Value::from(dir_names(dir.path())), g["files"]);
}

/// Opening the created database again changes nothing and writes no backup.
#[test]
fn reopening_a_created_database_is_current() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("library.db");
    drop(Db::open_or_create(&p).unwrap());
    let (_, opened) = Db::open_or_create(&p).unwrap();
    assert_eq!(opened, Opened::Current);
    let (_, opened) = Db::open(&p).unwrap();
    assert_eq!(opened, Opened::Current);
    assert!(!dir.path().join(BACKUP_FILE_NAME).exists());
}

/// Cancellation: an interrupted creation leaves an empty file, or a
/// SQLite header with no schema objects; both are created again from scratch.
#[test]
fn interrupted_creation_is_redone() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("library.db");
    std::fs::write(&p, b"").unwrap();
    let (_, opened) = Db::open_or_create(&p).unwrap();
    assert_eq!(opened, Opened::Created);

    let p2 = dir.path().join("other.db");
    {
        let c = Connection::open(&p2).unwrap();
        c.pragma_update(None, "journal_mode", "WAL").unwrap();
    }
    assert!(std::fs::metadata(&p2).unwrap().len() > 0);
    let (db, opened) = Db::open_or_create(&p2).unwrap();
    assert_eq!(opened, Opened::Created);
    let (_d, expected) = migrated_python_empty();
    assert_eq!(master(db.connection()), expected);
}

/// Negative: a file that is a database but not a library is still refused.
#[test]
fn a_foreign_database_is_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("library.db");
    {
        let c = Connection::open(&p).unwrap();
        c.execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES (1);")
            .unwrap();
    }
    assert!(matches!(Db::open_or_create(&p), Err(Error::NotALibrary(_))));
    let c = Connection::open(&p).unwrap();
    let n: i64 = c
        .query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
}

/// `drop_snapshot` forgets the snapshot without deleting anything.
/// Compared with the Python app: testdata/db/v16-synthetic-offline.db holds the
/// rows the Python app wrote; after drop_snapshot every album row is unchanged
/// except dir_mtime = 0, libraries.scanned_at = '', and children are kept.
#[test]
fn drop_snapshot_keeps_rows_and_children() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("library.db");
    std::fs::copy(musaic_testdata::public("db/v16-synthetic-offline.db"), &p).unwrap();
    let (mut db, _) = Db::open(&p).unwrap();
    let dump = |c: &Connection, sql: &str| -> Vec<Vec<String>> {
        let mut st = c.prepare(sql).unwrap();
        let n = st.column_count();
        st.query_map([], |r| {
            Ok((0..n)
                .map(|i| format!("{:?}", r.get_ref(i).unwrap()))
                .collect())
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    };
    let lib: String = db
        .connection()
        .query_row("SELECT path FROM libraries", [], |r| r.get(0))
        .unwrap();
    let other_cols = "id, library_path, path, title, mb_release_group_mbid, missing_since";
    let before = dump(
        db.connection(),
        &format!("SELECT {other_cols} FROM albums ORDER BY id"),
    );
    let fps = dump(
        db.connection(),
        "SELECT * FROM track_fingerprints ORDER BY 1, 2",
    );
    musaic_db::albums::drop_snapshot(db.connection_mut(), &lib).unwrap();
    let c = db.connection();
    assert_eq!(
        dump(c, &format!("SELECT {other_cols} FROM albums ORDER BY id")),
        before
    );
    assert_eq!(
        dump(c, "SELECT * FROM track_fingerprints ORDER BY 1, 2"),
        fps
    );
    assert!(!fps.is_empty());
    let mtimes: Vec<f64> = c
        .prepare("SELECT dir_mtime FROM albums")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(mtimes.len(), 13);
    assert!(mtimes.iter().all(|m| *m == 0.0));
    let scanned: String = c
        .query_row("SELECT scanned_at FROM libraries", [], |r| r.get(0))
        .unwrap();
    assert_eq!(scanned, "");
}

fn pragma_value(conn: &Connection, name: &str) -> Value {
    conn.query_row(&format!("PRAGMA {name}"), [], |r| {
        Ok(match r.get_ref(0)? {
            rusqlite::types::ValueRef::Integer(i) => Value::from(i),
            rusqlite::types::ValueRef::Text(t) => {
                Value::from(String::from_utf8_lossy(t).into_owned())
            }
            other => panic!("{other:?}"),
        })
    })
    .unwrap()
}

/// Every per-connection pragma the generator recorded, on `conn`, against
/// the Python app's `expected` map (an unlisted or extra pragma fails).
fn assert_connection_pragmas(conn: &Connection, expected: &Value, what: &str) {
    let expected = expected.as_object().unwrap();
    assert!(expected.len() >= 13, "golden too small: {expected:?}");
    for (k, v) in expected {
        assert_eq!(&pragma_value(conn, k), v, "{what}: PRAGMA {k}");
    }
    assert!(
        conn.is_autocommit(),
        "{what}: autocommit (isolation_level=None)"
    );
}

/// Every connection musaic-db hands out has the per-connection state
/// of the Python app's `db.connect()` connection (busy_timeout 30000,
/// foreign_keys=1, synchronous=2 (FULL, not bootstrap's NORMAL),
/// journal_mode=wal, and SQLite defaults for the rest), whichever way the
/// database was opened: created, migrated from the Python app's v16 file, opened
/// when already current, or as a further connection.
#[test]
fn every_handed_out_connection_matches_the_python_connection() {
    let g = golden();
    let expected = &g["connection_pragmas"];
    assert_eq!(g["connection_autocommit"], Value::Bool(true));
    // The recorded difference this test exists for: bootstrap's own
    // connection is NORMAL, the working connections are not.
    assert_eq!(
        g["bootstrap_connection_pragmas"]["synchronous"],
        Value::from(1)
    );
    assert_eq!(expected["synchronous"], Value::from(2));

    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("library.db");
    let (db, opened) = Db::open_or_create(&p).unwrap();
    assert_eq!(opened, Opened::Created);
    assert_connection_pragmas(db.connection(), expected, "open_or_create (created)");
    drop(db);

    let (db, opened) = Db::open(&p).unwrap();
    assert_eq!(opened, Opened::Current);
    assert_connection_pragmas(db.connection(), expected, "open (current)");
    let extra = musaic_db::open_connection(&p).unwrap();
    assert_connection_pragmas(&extra, expected, "open_connection");
    drop((db, extra));

    let p2 = dir.path().join("sub");
    std::fs::create_dir(&p2).unwrap();
    let p2 = p2.join("library.db");
    std::fs::copy(musaic_testdata::public("db/v16-empty.db"), &p2).unwrap();
    let (db, opened) = Db::open(&p2).unwrap();
    assert!(matches!(opened, Opened::Migrated { from: 16, .. }));
    assert_connection_pragmas(db.connection(), expected, "open (migrated)");
}

/// An existing library database left in rollback-journal mode is
/// switched to WAL when opened, as the Python app's `bootstrap()` (run on every
/// process start) switches it; the connection then matches the Python app's
/// (golden `existing_db_bootstrap`).
#[test]
fn an_existing_database_not_in_wal_is_switched_to_wal() {
    let g = golden();
    let e = &g["existing_db_bootstrap"];
    assert_eq!(e["journal_mode_before"], Value::from("delete"));
    for fixture in ["db/v16-empty.db", "db/v16-synthetic-offline.db"] {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("library.db");
        std::fs::copy(musaic_testdata::public(fixture), &p).unwrap();
        {
            let c = Connection::open(&p).unwrap();
            c.pragma_update(None, "journal_mode", "DELETE").unwrap();
            assert_eq!(pragma_value(&c, "journal_mode"), e["journal_mode_before"]);
        }
        let (db, _) = Db::open(&p).unwrap();
        assert_connection_pragmas(db.connection(), &e["connection_pragmas"], fixture);
        drop(db);
        let ro =
            Connection::open_with_flags(&p, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        assert_eq!(
            pragma_value(&ro, "journal_mode"),
            e["journal_mode_after"],
            "{fixture}"
        );
    }
}
