//! Backup, restore and named snapshots, against
//! testdata/golden/persistence/backup.json (the Python app's db_backup.py run
//! with a scripted clock).
//!
//! Where this build deliberately differs (see src/backup.rs) the test says so:
//! file names and versions carry v17 where the Python app wrote v16.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use musaic_db::backup::{self, RestoreError};
use musaic_db::{Db, SCHEMA_VERSION, settings};
use rusqlite::Connection;
use serde_json::{Value, json};

fn golden() -> Value {
    let p = musaic_testdata::public("golden/persistence/backup.json");
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

/// The Python app wrote v16 into names and versions; this build writes v17.
fn v17(s: &str) -> String {
    s.replace("-v16", &format!("-v{SCHEMA_VERSION}"))
}

/// A config dir holding a private copy of a fixture as library.db, opened.
fn live(fixture: &str) -> (tempfile::TempDir, PathBuf, Db) {
    let t = tempfile::tempdir().unwrap();
    let cfg = musaic_testdata::canonical(t.path());
    let db_path = cfg.join("library.db");
    std::fs::copy(musaic_testdata::public("db").join(fixture), &db_path).unwrap();
    let (db, _) = Db::open(&db_path).unwrap();
    (t, cfg, db)
}

fn at(secs: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs as u64)
}

fn set_mtime(p: &Path, secs: i64) {
    std::fs::File::options()
        .write(true)
        .open(p)
        .unwrap()
        .set_modified(at(secs))
        .unwrap();
}

fn listing(dir: &Path) -> Value {
    Value::Array(
        backup::list_snapshots(dir)
            .unwrap()
            .into_iter()
            .map(|e| {
                json!({
                    "filename": e.filename,
                    "valid": e.valid,
                    "schema_version": e.schema_version,
                    "counts": e.counts,
                    "modified_at": e.modified_at,
                })
            })
            .collect(),
    )
}

/// The Python app listing with this build's version substituted.
fn python_listing(v: &Value) -> Value {
    Value::Array(
        v.as_array()
            .unwrap()
            .iter()
            .map(|e| {
                let mut e = e.clone();
                e["filename"] = json!(v17(e["filename"].as_str().unwrap()));
                if e["schema_version"] == json!(16) {
                    e["schema_version"] = json!(SCHEMA_VERSION);
                }
                e
            })
            .collect(),
    )
}

/// Row counts per table equal the Python app's _gather_counts on the same fixture.
#[test]
fn counts_match_the_python_app() {
    let g = golden();
    let (_t, _cfg, db) = live("v16-synthetic-offline.db");
    assert_eq!(
        json!(backup::gather_counts(db.connection())),
        g["fixture_counts"]["counts"]
    );
    let info = backup::db_info(&db);
    assert!(info.exists);
    assert_eq!(info.schema_version, Some(SCHEMA_VERSION));
    assert_eq!(info.current_schema, SCHEMA_VERSION);
    assert_eq!(json!(info.counts), g["fixture_counts"]["counts"]);
}

/// Download name and the named backup file.
#[test]
fn backup_files_are_named_like_the_python_app() {
    let g = golden();
    let s = &g["suggested_backup_filename"];
    assert_eq!(
        backup::suggested_backup_filename(s["tag"].as_str().unwrap()),
        v17(s["name"].as_str().unwrap())
    );
    let (_t, cfg, db) = live("v16-empty.db");
    let m = &g["make_backup_dir"];
    let p = backup::make_backup_in(&db, &cfg.join("exports"), m["tag"].as_str().unwrap()).unwrap();
    assert_eq!(
        p.file_name().unwrap().to_str().unwrap(),
        m["name"].as_str().unwrap()
    );
    let header = std::fs::read(&p).unwrap()[..16].to_vec();
    assert_eq!(
        String::from_utf8_lossy(&header),
        m["header"].as_str().unwrap()
    );
    assert_eq!(
        PathBuf::from(format!("{}-wal", p.display())).exists(),
        m["wal_beside"].as_bool().unwrap()
    );
    // Same name again replaces the file rather than failing.
    backup::make_backup_in(&db, &cfg.join("exports"), m["tag"].as_str().unwrap()).unwrap();
}

/// Positive + malformed: the scripted snapshot sequence (same-second
/// clashes, pruning, foreign files), name validation and delete, step by step.
///
/// Deliberate (issue #213, edited in the golden with a note): only names the
/// app writes are snapshots, and same-second snapshots order by their clash
/// suffix, so the Python app's listings and pruned lists differ.
#[test]
fn snapshot_sequence_matches_the_python_app() {
    let g = golden();
    let (_t, cfg, db) = live("v16-empty.db");
    let dir = cfg.join("backups");
    for step in g["snapshot_steps"].as_array().unwrap() {
        let op = step["op"].as_str().unwrap();
        if op == "create" {
            let r = backup::create_snapshot(
                &db,
                &dir,
                step["retention"].as_i64().unwrap(),
                step["tag"].as_str().unwrap(),
                SystemTime::now(),
            )
            .unwrap();
            set_mtime(&dir.join(&r.filename), step["mtime"].as_i64().unwrap());
            let want = &step["result"];
            assert_eq!(
                r.filename,
                v17(want["filename"].as_str().unwrap()),
                "{step}"
            );
            let pruned: Vec<String> = want["pruned"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| v17(p.as_str().unwrap()))
                .collect();
            assert_eq!(r.pruned, pruned, "{step}");
            assert_eq!(r.schema_version, SCHEMA_VERSION);
        } else {
            let m0 = step["mtime"].as_i64().unwrap();
            std::fs::write(dir.join("library-junk.db"), b"not a database at all").unwrap();
            set_mtime(&dir.join("library-junk.db"), m0);
            std::fs::create_dir(dir.join("library-dir.db")).unwrap();
            std::fs::write(dir.join("notes.txt"), b"x").unwrap();
            std::fs::write(dir.join("LIBRARY-upper.DB"), b"x").unwrap();
            std::fs::write(dir.join("library-.db"), b"").unwrap();
            set_mtime(&dir.join("library-.db"), m0);
        }
        assert_eq!(listing(&dir), python_listing(&step["listing"]), "{op}");
    }

    // Name validation, same directory state as the Python app's.
    #[cfg(unix)]
    let have_link = {
        std::os::unix::fs::symlink(cfg.join("library.db"), dir.join("library-escape.db")).unwrap();
        true
    };
    // Creating a symlink on Windows needs a privilege (or developer mode).
    #[cfg(windows)]
    let have_link = match std::os::windows::fs::symlink_file(
        cfg.join("library.db"),
        dir.join("library-escape.db"),
    ) {
        Ok(()) => true,
        Err(e) => {
            println!("skipped case library-escape.db: cannot create a symlink here ({e})");
            false
        }
    };
    std::fs::write(dir.join("plain-file"), b"x").unwrap();
    for case in g["resolve_snapshot"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        if !have_link && name.contains("escape") {
            continue;
        }
        let name = v17(name);
        let got = backup::resolve_snapshot(&dir, &name);
        if let Some(ok) = case["ok"].as_str() {
            let p = got.unwrap_or_else(|e| panic!("{name:?}: {e}"));
            assert_eq!(p.file_name().unwrap().to_str().unwrap(), v17(ok));
        } else if let Some(msg) = case["restore_error"].as_str() {
            assert_eq!(got.unwrap_err().to_string(), msg, "{name:?}");
        } else {
            // Deliberate: the Python app raises ValueError (HTTP 500) on NUL.
            assert_eq!(case["raises"], "ValueError");
            assert!(matches!(got, Err(RestoreError::InvalidName(_))), "{name:?}");
        }
    }
    if have_link {
        std::fs::remove_file(dir.join("library-escape.db")).unwrap();
    }

    // Deliberate (issue #213): the Python app deleted `plain-file`; a file
    // whose name the app never writes is refused and left on disk.
    let d = &g["delete_snapshot"];
    let name = d["name"].as_str().unwrap();
    for attempt in ["first", "again"] {
        assert_eq!(
            backup::delete_snapshot(&dir, name).unwrap_err().to_string(),
            d[attempt]["restore_error"].as_str().unwrap(),
            "{attempt}"
        );
    }
    assert!(dir.join(name).is_file(), "a stray file is never deleted");
    assert_eq!(listing(&dir), python_listing(&d["listing_after"]));
    assert!(
        cfg.join("library.db").is_file(),
        "traversal must not have touched the live db"
    );
}

/// The startup auto-snapshot gate.
#[test]
fn auto_snapshot_gate_matches_the_python_app() {
    let g = golden();
    let (_t, cfg, db) = live("v16-empty.db");
    let dir = cfg.join("backups");
    let cases = g["auto_snapshot"].as_array().unwrap();
    let now = SystemTime::now();
    let tags = ["20260104-000000Z", "20260104-000001Z", "20260104-000002Z"];
    let r = backup::maybe_auto_snapshot(&db, &dir, 0, 24.0, "unused", now).unwrap();
    assert_eq!(r.is_some(), cases[0]["taken"].as_bool().unwrap());
    let r = backup::maybe_auto_snapshot(&db, &dir, 5, 24.0, tags[0], now).unwrap();
    assert_eq!(r.is_some(), cases[1]["taken"].as_bool().unwrap());
    let first = r.unwrap().filename;
    assert_eq!(first, v17(cases[1]["filename"].as_str().unwrap()));
    let hour_ago = now.duration_since(UNIX_EPOCH).unwrap().as_secs() as i64 - 3600;
    set_mtime(&dir.join(&first), hour_ago);
    let r = backup::maybe_auto_snapshot(&db, &dir, 5, 24.0, "unused", now).unwrap();
    assert_eq!(r.is_some(), cases[2]["taken"].as_bool().unwrap());
    let r = backup::maybe_auto_snapshot(&db, &dir, 5, 0.5, tags[1], now).unwrap();
    assert_eq!(
        r.map(|s| s.filename),
        cases[3]["filename"].as_str().map(v17)
    );
    let r = backup::maybe_auto_snapshot(&db, &dir, 5, 0.0, tags[2], now).unwrap();
    assert_eq!(
        r.map(|s| s.filename),
        cases[4]["filename"].as_str().map(v17)
    );
}

/// Positive: restore from a snapshot brings back the snapshot's rows
/// and moves the previous database aside under the Python app's name.
#[test]
fn restore_from_snapshot_matches_the_python_app() {
    let g = golden();
    let want = &g["restore_snapshot"];
    let (_t, cfg, db) = live("v16-empty.db");
    let dir = cfg.join("backups");
    settings::set_raw(db.connection(), "marker", "before").unwrap();
    let snap_tag = "20260105-000000Z";
    let s = backup::create_snapshot(&db, &dir, 10, snap_tag, SystemTime::now()).unwrap();
    assert_eq!(s.filename, v17(want["snapshot"].as_str().unwrap()));
    settings::set_raw(db.connection(), "marker", "after").unwrap();
    let tag = want["aside_tag"].as_str().unwrap();
    let (db, info) =
        backup::restore_from_snapshot(db, &dir, &s.filename, tag, SystemTime::now()).unwrap();
    // The snapshot is this build's v17 file; the Python app's was its v16.
    assert_eq!(want["result"]["schema_version"], 16);
    assert_eq!(info.schema_version, SCHEMA_VERSION);
    assert_eq!(json!(info.counts), want["result"]["counts"]);
    let aside: Vec<String> = info
        .aside_paths
        .iter()
        .map(|p| p.file_name().unwrap().to_str().unwrap().to_owned())
        .collect();
    assert_eq!(json!(aside), want["result"]["aside_paths"]);
    assert_eq!(
        json!(settings::get_raw(db.connection(), "marker").unwrap()),
        want["marker_after_restore"]
    );
    let a = Connection::open(cfg.join(&aside[0])).unwrap();
    let m: String = a
        .query_row("SELECT value FROM settings WHERE key='marker'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(json!(m), want["marker_in_aside"]);
    // The Python app lists the directory with every connection closed (it opens
    // one per call); since this build an open database is in WAL mode and has
    // -wal/-shm beside it, so close ours first.
    drop(a);
    drop(db);
    let mut dbfiles: Vec<String> = std::fs::read_dir(&cfg)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.starts_with("library.db"))
        .collect();
    dbfiles.sort();
    let python: Vec<&str> = want["config_dir_after"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .filter(|n| n.starts_with("library.db"))
        .collect();
    assert_eq!(dbfiles, python);
}

/// The live connection is checkpointed and closed before the move, so
/// the aside file ALONE (no -wal next to it) holds the last committed write.
#[test]
fn restore_closes_the_wal_before_moving_the_live_db() {
    let (_t, cfg, db) = live("v16-empty.db");
    let dir = cfg.join("backups");
    let mode: String = db
        .connection()
        .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    let s = backup::create_snapshot(&db, &dir, 10, "20260105-000000Z", SystemTime::now()).unwrap();
    db.connection()
        .execute_batch("PRAGMA wal_autocheckpoint = 0")
        .unwrap();
    settings::set_raw(db.connection(), "marker", "only-in-wal").unwrap();
    assert!(cfg.join("library.db-wal").exists());
    let (_db, info) =
        backup::restore_from_snapshot(db, &dir, &s.filename, "20260105-000001Z", SystemTime::now())
            .unwrap();
    assert_eq!(info.aside_paths.len(), 1, "{:?}", info.aside_paths);
    let lone = cfg.join("lone");
    std::fs::create_dir(&lone).unwrap();
    std::fs::copy(&info.aside_paths[0], lone.join("old.db")).unwrap();
    let c = Connection::open(lone.join("old.db")).unwrap();
    assert_eq!(
        settings::get_raw(&c, "marker").unwrap().as_deref(),
        Some("only-in-wal")
    );
}

fn make_upload(path: &Path, ddl: &[&str]) {
    let c = Connection::open(path).unwrap();
    for d in ddl {
        c.execute_batch(d).unwrap();
    }
}

/// Malformed: upload validation, message for message.
#[test]
fn upload_validation_matches_the_python_app() {
    let g = golden();
    let t = tempfile::tempdir().unwrap();
    let up = musaic_testdata::canonical(t.path());
    std::fs::write(up.join("tiny.db"), b"SQLite").unwrap();
    std::fs::write(
        up.join("text.db"),
        b"this is definitely not a sqlite database file",
    )
    .unwrap();
    let base = [
        "CREATE TABLE schema_version (version INTEGER)",
        "CREATE TABLE libraries (path TEXT)",
        "CREATE TABLE albums (id TEXT)",
    ];
    make_upload(
        &up.join("foreign.db"),
        &["CREATE TABLE moz_cookies (id INTEGER)"],
    );
    make_upload(&up.join("partial.db"), &[base[0], base[2]]);
    make_upload(&up.join("empty-version.db"), &base);
    make_upload(
        &up.join("no-version-column.db"),
        &["CREATE TABLE schema_version (v INTEGER)", base[1], base[2]],
    );
    make_upload(
        &up.join("newer.db"),
        &[
            base[0],
            base[1],
            base[2],
            "INSERT INTO schema_version VALUES (99)",
        ],
    );
    make_upload(
        &up.join("v17.db"),
        &[
            base[0],
            base[1],
            base[2],
            "INSERT INTO schema_version VALUES (17)",
        ],
    );
    make_upload(
        &up.join("minimal-v16.db"),
        &[
            base[0],
            base[1],
            "CREATE TABLE albums (id TEXT, mb_release_group_mbid TEXT)",
            "INSERT INTO schema_version VALUES (16)",
            "INSERT INTO albums VALUES ('a', NULL)",
        ],
    );
    std::fs::copy(
        musaic_testdata::public("db/v16-synthetic-offline.db"),
        up.join("fixture.db"),
    )
    .unwrap();
    let root_s = up.to_str().unwrap();
    for case in g["validate_upload"].as_array().unwrap() {
        let name = case["upload"].as_str().unwrap();
        let got = backup::validate_upload(&up.join(name));
        match name {
            // Deliberate: this build reads v17 (its own backups).
            "v17.db" => assert_eq!(got.unwrap().schema_version, 17),
            // Deliberate: the supported version in the message is ours.
            "newer.db" => assert_eq!(
                got.unwrap_err().to_string(),
                case["restore_error"]
                    .as_str()
                    .unwrap()
                    .replace("up to v16", &format!("up to v{SCHEMA_VERSION}"))
            ),
            _ => {
                if let Some(ok) = case.get("ok") {
                    let v = got.unwrap_or_else(|e| panic!("{name}: {e}"));
                    assert_eq!(json!(v.schema_version), ok["schema_version"], "{name}");
                    assert_eq!(json!(v.counts), ok["counts"], "{name}");
                } else {
                    let msg = musaic_testdata::replace_root(
                        &got.unwrap_err().to_string(),
                        root_s,
                        "<ROOT>/uploads",
                    );
                    assert_eq!(msg, case["restore_error"].as_str().unwrap(), "{name}");
                }
            }
        }
    }
}

/// Malformed (deliberate change): an upload that validates but cannot
/// be opened is backed out; the previous database is live again and unchanged.
/// The Python app leaves the broken file in place.
#[test]
fn failed_open_after_swap_puts_the_old_database_back() {
    let (_t, cfg, db) = live("v16-empty.db");
    settings::set_raw(db.connection(), "marker", "kept").unwrap();
    let upload = cfg.join("minimal-v16.db");
    make_upload(
        &upload,
        &[
            "CREATE TABLE schema_version (version INTEGER)",
            "CREATE TABLE libraries (path TEXT)",
            "CREATE TABLE albums (id TEXT, mb_release_group_mbid TEXT)",
            "INSERT INTO schema_version VALUES (16)",
        ],
    );
    backup::validate_upload(&upload).unwrap();
    let failed = backup::restore_from(db, &upload, "20260106-000000Z", SystemTime::now())
        .err()
        .expect("v16 file without track_fingerprints cannot be migrated");
    assert!(
        matches!(failed.error, RestoreError::OpenFailed(_)),
        "{}",
        failed.error
    );
    let db = failed.db.expect("old database reopened");
    assert_eq!(
        settings::get_raw(db.connection(), "marker")
            .unwrap()
            .as_deref(),
        Some("kept")
    );
    assert!(!cfg.join("library.db.bak-20260106-000000Z").exists());
}

/// Malformed: a rejected upload leaves the live database open and untouched.
#[test]
fn rejected_upload_touches_nothing() {
    let (_t, cfg, db) = live("v16-empty.db");
    std::fs::write(
        cfg.join("junk.db"),
        b"this is definitely not a sqlite database file",
    )
    .unwrap();
    let failed = backup::restore_from(
        db,
        &cfg.join("junk.db"),
        "20260106-000000Z",
        SystemTime::now(),
    )
    .err()
    .unwrap();
    assert!(matches!(failed.error, RestoreError::NotSqlite));
    assert!(failed.db.is_some());
    assert!(!cfg.join("library.db.bak-20260106-000000Z").exists());
}

/// An upload older than v16 is accepted, as
/// the Python app's `validate_upload` accepts it (db_backup.py: only a version newer
/// than its own is refused), and migrated on reopen through the Python app's
/// chain; the genuine v12 database the v12 Python code wrote
/// (testdata/golden/persistence/old-schemas/v12.db). Only a version below v1, which no build
/// wrote, is refused before anything moves.
#[test]
fn pre_v16_upload_is_accepted_and_migrated() {
    let (_t, cfg, db) = live("v16-empty.db");
    let upload = cfg.join("upload-v12.db");
    std::fs::copy(
        musaic_testdata::public("golden/persistence/old-schemas/v12.db"),
        &upload,
    )
    .unwrap();
    assert_eq!(backup::validate_upload(&upload).unwrap().schema_version, 12);
    let (db, info) =
        backup::restore_from(db, &upload, "20260106-000000Z", SystemTime::now()).unwrap();
    assert_eq!(info.schema_version, 12);
    assert!(matches!(
        info.opened,
        musaic_db::Opened::Migrated { from: 12, .. }
    ));
    assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
    let albums: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM albums", [], |r| r.get(0))
        .unwrap();
    assert_eq!(albums, 4);

    let zero = cfg.join("v0.db");
    std::fs::copy(musaic_testdata::public("db/v16-empty.db"), &zero).unwrap();
    Connection::open(&zero)
        .unwrap()
        .execute_batch("UPDATE schema_version SET version = 0")
        .unwrap();
    assert!(matches!(
        backup::validate_upload(&zero),
        Err(RestoreError::Older {
            found: 0,
            oldest: 1
        })
    ));
}

/// Issue #213: files in `backups/` that the app did not write, even ones that
/// are valid Musaic databases under a `library-*.db` name, are not listed,
/// not pruned, and cannot be restored from or deleted through the snapshot
/// API.
#[test]
fn stray_files_in_the_backup_dir_are_left_alone() {
    let (_t, cfg, db) = live("v16-empty.db");
    let dir = cfg.join("backups");
    std::fs::create_dir_all(&dir).unwrap();
    let strays = [
        "plain-file",
        "library-old-copy.db",
        "library-20260101-000000Z.db",
        "library-20260101-000000Z-v16.db.bak",
        "library-2026010-000000Z-v16.db",
        "library-20260101-000000Z-v.db",
        "library-20260101-000000Z-v16..db",
        "library-20260101-000000Z-v16.x.db",
    ];
    for s in strays {
        std::fs::copy(musaic_testdata::public("db/v16-empty.db"), dir.join(s)).unwrap();
        set_mtime(&dir.join(s), 1_000_000_000);
    }
    for (i, tag) in ["20260101-000000Z", "20260102-000000Z", "20260103-000000Z"]
        .iter()
        .enumerate()
    {
        let s = backup::create_snapshot(&db, &dir, 1, tag, SystemTime::now()).unwrap();
        set_mtime(&dir.join(&s.filename), 1_767_225_600 + i as i64 * 86_400);
        assert!(
            s.pruned.iter().all(|p| !strays.contains(&p.as_str())),
            "{:?}",
            s.pruned
        );
    }
    for s in strays {
        assert!(dir.join(s).is_file(), "{s} must survive pruning");
    }
    let names: Vec<String> = backup::list_snapshots(&dir)
        .unwrap()
        .into_iter()
        .map(|e| e.filename)
        .collect();
    assert_eq!(
        names,
        vec![format!("library-20260103-000000Z-v{SCHEMA_VERSION}.db")]
    );
    let age = backup::latest_snapshot_age_seconds(&dir, at(1_767_398_400)).unwrap();
    assert_eq!(age, Some(0.0), "a stray file never counts as the newest");

    let mut db = db;
    for s in strays {
        assert!(
            matches!(
                backup::resolve_snapshot(&dir, s),
                Err(RestoreError::InvalidName(_))
            ),
            "{s}"
        );
        assert!(
            matches!(
                backup::delete_snapshot(&dir, s),
                Err(RestoreError::InvalidName(_))
            ),
            "{s}"
        );
        let failed =
            backup::restore_from_snapshot(db, &dir, s, "20260106-000000Z", SystemTime::now())
                .err()
                .unwrap_or_else(|| panic!("{s} restored"));
        assert!(
            matches!(failed.error, RestoreError::InvalidName(_)),
            "{s}: {}",
            failed.error
        );
        db = failed.db.expect("live database still open");
        assert!(dir.join(s).is_file(), "{s}");
        assert!(!cfg.join("library.db.bak-20260106-000000Z").exists());
    }
}

/// Issue #213: same-second snapshots are newest by clash suffix (`.11` after
/// `.10` after `.2` after the plain name), so retention prunes the oldest
/// of them first; the suffix compares as a number, not as text.
#[test]
fn same_second_snapshots_prune_the_oldest_first() {
    let (_t, cfg, db) = live("v16-empty.db");
    let dir = cfg.join("backups");
    let tag = "20260101-000000Z";
    let mut made = Vec::new();
    for _ in 0..12 {
        let s = backup::create_snapshot(&db, &dir, 0, tag, SystemTime::now()).unwrap();
        set_mtime(&dir.join(&s.filename), 1_767_225_600);
        made.push(s.filename);
    }
    let base = format!("library-{tag}-v{SCHEMA_VERSION}");
    assert_eq!(made[0], format!("{base}.db"));
    assert_eq!(made[11], format!("{base}.11.db"));
    let listed: Vec<String> = backup::list_snapshots(&dir)
        .unwrap()
        .into_iter()
        .map(|e| e.filename)
        .collect();
    let mut newest_first = made.clone();
    newest_first.reverse();
    assert_eq!(listed, newest_first);

    // One more in the same second (same mtime): the 13 are pruned to 4.
    let s = backup::create_snapshot(&db, &dir, 4, tag, SystemTime::now()).unwrap();
    assert_eq!(s.filename, format!("{base}.12.db"));
    set_mtime(&dir.join(&s.filename), 1_767_225_600);
    let kept: Vec<String> = backup::list_snapshots(&dir)
        .unwrap()
        .into_iter()
        .map(|e| e.filename)
        .collect();
    assert_eq!(
        kept,
        [12, 11, 10, 9]
            .iter()
            .map(|n| format!("{base}.{n}.db"))
            .collect::<Vec<_>>()
    );
    assert!(s.pruned.contains(&format!("{base}.db")), "{:?}", s.pruned);
}

/// A snapshot file at a given schema version and modification time.
fn snapshot_at(dir: &Path, name: &str, version: i64, mtime: i64) {
    let p = dir.join(name);
    let c = Connection::open(&p).unwrap();
    c.execute_batch(
        "CREATE TABLE albums(id); CREATE TABLE libraries(path); CREATE TABLE schema_version(version INTEGER);",
    )
    .unwrap();
    c.execute("INSERT INTO schema_version VALUES (?1)", [version])
        .unwrap();
    drop(c);
    set_mtime(&p, mtime);
}

/// The newest copy taken before a schema upgrade outlives the retention
/// count; the next upgrade's copy supersedes it, and then it is pruned like
/// any other.
#[test]
fn the_newest_pre_upgrade_copy_is_not_pruned_until_superseded() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join("backups");
    std::fs::create_dir_all(&dir).unwrap();
    let v = SCHEMA_VERSION;
    // v16 is the Python app's: not a pre-upgrade copy of this build.
    snapshot_at(&dir, "library-20260101-000000Z-v16.db", 16, 1_000);
    snapshot_at(
        &dir,
        &format!("library-20260102-000000Z-v{}.db", v - 2),
        v - 2,
        2_000,
    );
    snapshot_at(
        &dir,
        &format!("library-20260103-000000Z-v{}.db", v - 1),
        v - 1,
        3_000,
    );
    snapshot_at(&dir, &format!("library-20260104-000000Z-v{v}.db"), v, 4_000);
    snapshot_at(
        &dir,
        &format!("library-20260105-000000Z-v{v}.1.db"),
        v,
        5_000,
    );
    let pruned = backup::prune_snapshots(&dir, 1).unwrap();
    let mut left: Vec<String> = backup::list_snapshots(&dir)
        .unwrap()
        .into_iter()
        .map(|e| e.filename)
        .collect();
    left.sort();
    // Kept: the newest (retention 1) and the newest pre-upgrade copy (v - 1);
    // the older copy was superseded by it.
    assert_eq!(
        left,
        [
            format!("library-20260103-000000Z-v{}.db", v - 1),
            format!("library-20260105-000000Z-v{v}.1.db")
        ]
    );
    assert_eq!(pruned.len(), 3, "{pruned:?}");
    // Retention 0 still means no pruning at all.
    assert!(backup::prune_snapshots(&dir, 0).unwrap().is_empty());
    // The user can still delete it on purpose.
    assert!(
        backup::delete_snapshot(&dir, &format!("library-20260103-000000Z-v{}.db", v - 1)).unwrap()
    );
}

/// A connection another thread still has open when a restore starts (a read
/// in flight) delays the swap; it does not fail it. Windows refuses to move
/// a file that has any handle open, so this is where it matters; elsewhere
/// it passes without waiting.
#[test]
fn restore_waits_for_a_reader_that_is_still_open() {
    let (_t, cfg, db) = live("v16-empty.db");
    let upload = cfg.join("upload.db");
    std::fs::copy(musaic_testdata::public("db").join("v16-empty.db"), &upload).unwrap();
    let reader = musaic_db::open_connection(db.path()).unwrap();
    let closer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        drop(reader);
    });
    backup::restore_from(db, &upload, "20260106-000000Z", SystemTime::now())
        .map_err(|f| f.error.to_string())
        .expect("restore");
    closer.join().unwrap();
}
