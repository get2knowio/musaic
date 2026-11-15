//! Tests against a database the Python app actually wrote
//! (testdata/db/v16-synthetic-offline.db, see testdata/db/README.md). Its
//! album paths are rewritten to a neutral root; the ids still hash the
//! recording machine's paths, so the id check reads the unmodified database
//! from the private test data.

use std::path::PathBuf;

use musaic_db::{BACKUP_FILE_NAME, Db, Error, Opened, SCHEMA_VERSION, album_id, settings};
use rusqlite::Connection;

/// A private copy of the v16 fixture in its own temp dir.
fn fixture_copy() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("library.db");
    std::fs::copy(musaic_testdata::public("db/v16-synthetic-offline.db"), &db).unwrap();
    (dir, db)
}

fn table_counts(conn: &Connection) -> Vec<(String, i64)> {
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap();
    let names: Vec<String> = stmt
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    names
        .into_iter()
        .map(|n| {
            let c = conn
                .query_row(&format!("SELECT COUNT(*) FROM \"{n}\""), [], |r| r.get(0))
                .unwrap();
            (n, c)
        })
        .collect()
}

/// Every album id the Python app stored equals ours for the stored path.
/// The fixture includes a directory whose name is stored NFD. The pairs are
/// checked on the database exactly as the Python app wrote it (private:
/// absolute paths of the recording machine).
#[test]
fn album_ids_match_the_python_app() {
    let (_dir, path) = fixture_copy();
    let (db, _) = Db::open(&path).unwrap();
    let albums = db.album_paths().unwrap();
    assert_eq!(albums.len(), 13);
    match musaic_testdata::private("db/v16-synthetic-offline.db") {
        Some(original) => {
            let dir = tempfile::tempdir().unwrap();
            let copy = dir.path().join("library.db");
            std::fs::copy(original, &copy).unwrap();
            let (db, _) = Db::open(&copy).unwrap();
            let stored = db.album_paths().unwrap();
            assert_eq!(stored.len(), 13);
            for (id, album_path) in &stored {
                assert_eq!(&album_id(album_path), id, "{album_path}");
            }
        }
        None => musaic_testdata::skip_private_cases(),
    }
    assert!(
        albums
            .iter()
            .any(|(_, p)| p.chars().any(|c| ('\u{0300}'..='\u{036f}').contains(&c))),
        "fixture should contain a decomposed (NFD) path"
    );
}

/// Backup first, then one-way migration; nothing else changes.
#[test]
fn v16_is_backed_up_then_migrated() {
    let (dir, path) = fixture_copy();
    let before = table_counts(&Connection::open(&path).unwrap());

    let (db, opened) = Db::open(&path).unwrap();
    let backup = dir.path().join(BACKUP_FILE_NAME);
    assert_eq!(
        opened,
        Opened::Migrated {
            from: 16,
            backup: Some(backup.clone())
        }
    );
    assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);

    // Every table has the rows it had; the one table v18 adds is empty
    // (enumerated delta).
    let mut want = before.clone();
    want.push(("fingerprint_rejects".into(), 0));
    want.sort();
    assert_eq!(table_counts(db.connection()), want);
    // Rows with a recording came from a completed lookup; the
    // others (a failure or a no-match, which the Python app could not tell
    // apart) have no outcome yet, so they get one lookup.
    let by_state: Vec<(Option<String>, bool, i64, i64)> = db
        .connection()
        .prepare(
            "SELECT acoustid_lookup_state, acoustid_recording_mbid IS NOT NULL, \
                    SUM(acoustid_lookup_failures), COUNT(*) \
             FROM track_fingerprints GROUP BY 1, 2 ORDER BY 1, 2",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let b = Connection::open(&backup).unwrap();
    let (with, without): (i64, i64) = b
        .query_row(
            "SELECT SUM(acoustid_recording_mbid IS NOT NULL), SUM(acoustid_recording_mbid IS NULL) \
             FROM track_fingerprints",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    drop(b);
    let mut want_states = Vec::new();
    if without > 0 {
        want_states.push((None, false, 0, without));
    }
    if with > 0 {
        want_states.push((Some(musaic_db::LOOKUP_DONE.to_owned()), true, 0, with));
    }
    assert_eq!(by_state, want_states);
    // Existing fingerprints are marked as the Python app's.
    let (rows, fpcalc): (i64, i64) = db
        .connection()
        .query_row(
            "SELECT COUNT(*), SUM(fingerprint_impl = 'fpcalc') FROM track_fingerprints",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((rows, fpcalc), (42, 42));

    // The backup is the untouched v16 database.
    let b = Connection::open(&backup).unwrap();
    assert_eq!(
        b.query_row("SELECT version FROM schema_version", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        16
    );
    assert_eq!(table_counts(&b), before);
    let has_col: i64 = b
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('track_fingerprints') WHERE name = 'fingerprint_impl'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(has_col, 0);
    assert!(!dir.path().join(format!("{BACKUP_FILE_NAME}.tmp")).exists());
}

/// Idempotency: a second open changes nothing and leaves the backup alone.
#[test]
fn reopening_is_a_no_op() {
    let (dir, path) = fixture_copy();
    drop(Db::open(&path).unwrap());
    let backup = dir.path().join(BACKUP_FILE_NAME);
    let backup_bytes = std::fs::read(&backup).unwrap();

    let (db, opened) = Db::open(&path).unwrap();
    assert_eq!(opened, Opened::Current);
    assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(std::fs::read(&backup).unwrap(), backup_bytes);
}

/// An interrupted first attempt may have left a backup behind while the
/// database is still v16. That backup is kept, not replaced.
#[test]
fn existing_backup_is_never_overwritten() {
    let (dir, path) = fixture_copy();
    let backup = dir.path().join(BACKUP_FILE_NAME);
    std::fs::write(&backup, b"earlier backup").unwrap();

    let (_db, opened) = Db::open(&path).unwrap();
    assert!(matches!(opened, Opened::Migrated { from: 16, .. }));
    assert_eq!(std::fs::read(&backup).unwrap(), b"earlier backup");
}

/// A database from a newer build is refused and left untouched.
#[test]
fn newer_schema_is_refused() {
    let (dir, path) = fixture_copy();
    Connection::open(&path)
        .unwrap()
        .execute("UPDATE schema_version SET version = 99", [])
        .unwrap();
    let bytes = std::fs::read(&path).unwrap();

    match Db::open(&path) {
        Err(Error::NewerSchema {
            found: 99,
            supported,
        }) => assert_eq!(supported, SCHEMA_VERSION),
        other => panic!("expected NewerSchema, got {:?}", other.map(|(_, o)| o)),
    }
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert!(!dir.path().join(BACKUP_FILE_NAME).exists());
}

/// A v16 file stamped v15 goes through the Python app's v16
/// step, whose `ADD COLUMN attach_cover` hits "duplicate column name" and is
/// skipped exactly as `db._migrate` skips it; then on to the current version,
/// backed up first under the version it was found at. The full per-version
/// comparison with the Python app is tests/old_schemas.rs.
#[test]
fn pre_v16_schema_is_migrated_through_the_python_chain() {
    let (dir, path) = fixture_copy();
    Connection::open(&path)
        .unwrap()
        .execute("UPDATE schema_version SET version = 15", [])
        .unwrap();
    let (db, opened) = Db::open(&path).unwrap();
    let backup = dir.path().join(musaic_db::backup_file_name(15));
    assert_eq!(
        opened,
        Opened::Migrated {
            from: 15,
            backup: Some(backup.clone())
        }
    );
    assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
    assert!(backup.is_file());
    assert!(!dir.path().join(BACKUP_FILE_NAME).exists());
}

/// A version row below v1 (no build wrote one) is refused, file untouched.
#[test]
fn version_below_one_is_refused() {
    let (dir, path) = fixture_copy();
    Connection::open(&path)
        .unwrap()
        .execute("UPDATE schema_version SET version = 0", [])
        .unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert!(matches!(
        Db::open(&path),
        Err(Error::TooOld {
            found: 0,
            oldest: 1
        })
    ));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert!(musaic_db::existing_backup(dir.path()).is_none());
}

#[test]
fn a_file_that_is_not_a_library_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("other.db");
    Connection::open(&path)
        .unwrap()
        .execute("CREATE TABLE t(x)", [])
        .unwrap();
    assert!(matches!(Db::open(&path), Err(Error::NotALibrary(_))));
}

/// Defaults are exactly the Python app's.
#[test]
fn settings_defaults_match_the_python_app() {
    let golden: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(musaic_testdata::public("golden/settings-defaults.json")).unwrap(),
    )
    .unwrap();
    let want: Vec<(String, String)> = golden["defaults"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
        .collect();
    let mut have: Vec<(String, String)> = settings::DEFAULTS
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    have.sort();
    assert_eq!(have, want);
    assert_eq!(golden["schema_version"], musaic_db::PYTHON_SCHEMA_VERSION);
}

/// Reads rows the Python app wrote, and round-trips writes.
#[test]
fn settings_read_and_write() {
    let (_dir, path) = fixture_copy();
    let (db, _) = Db::open(&path).unwrap();
    let c = db.connection();
    assert_eq!(
        settings::get_raw(c, "acoustid_api_key").unwrap().as_deref(),
        Some("migration-offline-dummy")
    );
    assert_eq!(settings::get_raw(c, "ai_queue_paused").unwrap(), None);
    assert!(!settings::get_bool(c, "ai_queue_paused", false).unwrap());
    assert_eq!(
        settings::get_int(c, "snapshot_retention_count", 10).unwrap(),
        10
    );

    settings::set_raw(c, "ai_queue_paused", " Yes ").unwrap();
    assert!(settings::get_bool(c, "ai_queue_paused", false).unwrap());
    settings::set_raw(c, "ai_queue_paused", "2").unwrap();
    assert!(!settings::get_bool(c, "ai_queue_paused", true).unwrap());
    settings::set_raw(c, "snapshot_retention_count", "many").unwrap();
    assert_eq!(
        settings::get_int(c, "snapshot_retention_count", 10).unwrap(),
        10
    );
    assert_eq!(settings::default_for("ai_max_parallel"), Some("1"));
}

/// A v17 database (this build's, before v18)
/// is migrated in place to v18 with no backup file; its rows are kept, a
/// row with a recording is marked as looked up, and the rejects table is
/// added. Reopening changes nothing.
#[test]
fn v17_is_migrated_to_v18_without_a_backup() {
    let (dir, path) = fixture_copy();
    // A v17 file as the previous build left it: the v16 fixture plus the
    // v17 step, and its pre-migration backup removed to see none is written.
    {
        let c = Connection::open(&path).unwrap();
        c.execute_batch(
            "ALTER TABLE track_fingerprints ADD COLUMN fingerprint_impl TEXT NOT NULL DEFAULT 'fpcalc';
             UPDATE track_fingerprints SET fingerprint_impl = 'rust-chromaprint';
             UPDATE schema_version SET version = 17;
             -- the fixture (an offline run) has no recordings: give three
             UPDATE track_fingerprints SET acoustid_recording_mbid = 'r-' || filename,
                    acoustid_score = 0.9
              WHERE rowid IN (SELECT rowid FROM track_fingerprints ORDER BY rowid LIMIT 3);",
        )
        .unwrap();
    }
    let before = table_counts(&Connection::open(&path).unwrap());
    let (db, opened) = Db::open(&path).unwrap();
    assert_eq!(
        opened,
        Opened::Migrated {
            from: 17,
            backup: None
        }
    );
    assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
    assert!(!dir.path().join(BACKUP_FILE_NAME).exists());
    let mut want = before;
    want.push(("fingerprint_rejects".into(), 0));
    want.sort();
    assert_eq!(table_counts(db.connection()), want);
    let (done, matched, impls): (i64, i64, i64) = db
        .connection()
        .query_row(
            "SELECT SUM(acoustid_lookup_state IS 'done'), \
                    SUM(acoustid_recording_mbid IS NOT NULL), \
                    SUM(fingerprint_impl = 'rust-chromaprint') FROM track_fingerprints",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!((done, matched, impls), (3, 3, 42));
    drop(db);
    let (_, opened) = Db::open(&path).unwrap();
    assert_eq!(opened, Opened::Current);
}

/// Remembered rejects follow an album's directory rename (the id
/// changes under deferred foreign keys) and go with a deleted album.
#[test]
fn rejects_follow_renames_and_deletes() {
    let (_dir, path) = fixture_copy();
    let (mut db, _) = Db::open(&path).unwrap();
    let (id, p): (String, String) = db
        .connection()
        .query_row("SELECT id, path FROM albums ORDER BY id LIMIT 1", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    let rec = musaic_db::fingerprints_ext::RejectRecord {
        filename: "02 - Bad.flac".into(),
        file_signature: "1:2:3".into(),
        fingerprint_impl: musaic_db::FINGERPRINT_IMPL_RUST.into(),
        rejected_at: "2001-01-01T00:00:00Z".into(),
    };
    musaic_db::fingerprints_ext::record_reject(db.connection(), &id, &rec).unwrap();
    let new_path = format!("{p} renamed");
    let new_id = album_id(&new_path);
    musaic_db::renames::rename_album_directory(
        db.connection_mut(),
        &id,
        &new_id,
        &new_path,
        "renamed",
    )
    .unwrap();
    assert!(
        musaic_db::fingerprints_ext::get_rejects(db.connection(), &id)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        musaic_db::fingerprints_ext::get_rejects(db.connection(), &new_id).unwrap(),
        vec![rec]
    );
    db.connection()
        .execute("DELETE FROM albums WHERE id = ?1", [&new_id])
        .unwrap();
    let n: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM fingerprint_rejects", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
}
