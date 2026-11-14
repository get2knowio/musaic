//! Databases older than v16, compared with what
//! the Python app at 6902c23 makes of them: testdata/golden/persistence/old-schemas/.
//! vNN.db is the input (GENUINE for v12..v16:
//! written by the Python code of the commit that introduced that version;
//! CONSTRUCTED for v1..v11, which have no code in git); vNN.json holds
//! the Python app's `db.bootstrap()` result on a copy (`python_app.after`), every cell as
//! SQLite's `quote()` text.
//!
//! `Db::open` must produce the Python app's result followed by this build's own
//! v16 -> v17 -> v18 steps, and nothing else. The deltas are enumerated in
//! [`expected_from_python`]; an unlisted difference fails, and so does an
//! enumerated delta that no fixture exercised.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use musaic_db::{
    Db, Error, FINGERPRINT_IMPL_FPCALC, LOOKUP_DONE, OLDEST_SCHEMA_VERSION, Opened, SCHEMA_VERSION,
    backup_file_name, existing_backup,
};
use rusqlite::Connection;
use serde_json::{Value, json};

const PLACEHOLDER: &str = "<MIGRATION_NOW>";

fn old_dir() -> PathBuf {
    musaic_testdata::public("golden/persistence/old-schemas")
}

fn golden(v: i64) -> Value {
    let p = old_dir().join(format!("v{v:02}.json"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

/// A private copy of vNN.db as `library.db` in its own temp dir.
fn fixture_copy(v: i64) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("library.db");
    std::fs::copy(old_dir().join(format!("v{v:02}.db")), &p).unwrap();
    (dir, p)
}

/// The generator's `dump()`, in the same JSON shape.
fn dump(conn: &Connection) -> Value {
    let master: Vec<Value> = conn
        .prepare("SELECT type, name, tbl_name, sql FROM sqlite_master ORDER BY rowid")
        .unwrap()
        .query_map([], |r| {
            Ok(json!([
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?
            ]))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut tables = serde_json::Map::new();
    for m in &master {
        if m[0] != "table" {
            continue;
        }
        let name = m[1].as_str().unwrap();
        let info: Vec<Value> = conn
            .prepare(&format!("PRAGMA table_info(\"{name}\")"))
            .unwrap()
            .query_map([], |r| {
                let dflt: Option<String> = r.get(4)?;
                Ok(json!([
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    dflt,
                    r.get::<_, i64>(5)?
                ]))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let cols: Vec<String> = info
            .iter()
            .map(|c| format!("quote(\"{}\")", c[1].as_str().unwrap()))
            .collect();
        let mut st = conn
            .prepare(&format!(
                "SELECT {} FROM \"{name}\" ORDER BY rowid",
                cols.join(", ")
            ))
            .unwrap();
        let n = cols.len();
        let rows: Vec<Value> = st
            .query_map([], |r| {
                Ok(Value::Array(
                    (0..n)
                        .map(|i| Value::String(r.get::<_, String>(i).unwrap()))
                        .collect(),
                ))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        tables.insert(name.to_string(), json!({"columns": info, "rows": rows}));
    }
    let versions: Vec<i64> = conn
        .prepare("SELECT version FROM schema_version")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut pragmas = serde_json::Map::new();
    for p in [
        "journal_mode",
        "page_size",
        "auto_vacuum",
        "user_version",
        "application_id",
        "encoding",
    ] {
        let v: rusqlite::types::Value = conn
            .query_row(&format!("PRAGMA {p}"), [], |r| r.get(0))
            .unwrap();
        let v = match v {
            rusqlite::types::Value::Integer(i) => json!(i),
            rusqlite::types::Value::Text(s) => json!(s),
            other => panic!("{other:?}"),
        };
        pragmas.insert(p.to_string(), v);
    }
    let integrity: Vec<String> = conn
        .prepare("PRAGMA integrity_check")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let fk: usize = conn
        .prepare("PRAGMA foreign_key_check")
        .unwrap()
        .query_map([], |_| Ok(()))
        .unwrap()
        .count();
    assert_eq!(fk, 0, "foreign_key_check");
    json!({
        "sqlite_master": master,
        "schema_version": versions,
        "pragmas": pragmas,
        "integrity_check": integrity,
        "foreign_key_check": [],
        "tables": tables,
    })
}

/// Cells the v14 step stamped with `strftime('%Y-%m-%dT%H:%M:%fZ','now')`:
/// in ours, each cell the golden holds as the placeholder must have that
/// format and lie in `[t0, t1]`; it is then replaced by the placeholder.
fn normalise_now(ours: &mut Value, theirs: &Value, t0: f64, t1: f64) {
    let g = &theirs["tables"]["check_findings"];
    let col = g["columns"]
        .as_array()
        .unwrap()
        .iter()
        .position(|c| c[1] == "resolved_at")
        .unwrap();
    for (i, row) in g["rows"].as_array().unwrap().iter().enumerate() {
        if row[col] != PLACEHOLDER {
            continue;
        }
        if ignored_finding_kept(g, i) {
            // Left unresolved here; compare_with_python's D5.
            continue;
        }
        let cell = &mut ours["tables"]["check_findings"]["rows"][i][col];
        let s = cell.as_str().unwrap().trim_matches('\'').to_string();
        let b = s.as_bytes();
        assert!(
            s.len() == 24 && b[4] == b'-' && b[10] == b'T' && b[19] == b'.' && s.ends_with('Z'),
            "{s}"
        );
        let secs = unix_seconds(&s);
        assert!(
            secs + 1.0 >= t0 && secs - 1.0 <= t1,
            "{s} not in run window"
        );
        *cell = Value::String(PLACEHOLDER.to_string());
    }
}

/// The five check ids the v14 step retires (musaic/db.py:522-540).
const RETIRED: [&str; 5] = [
    "'missing-fingerprints'",
    "'missing-mbid'",
    "'missing-core-tags'",
    "'missing-sort-tags'",
    "'mb-canonical-mismatch'",
];

/// Row `i` of the dumped `check_findings` `t` is an ignored finding
/// of a retired check, which this build's v14 step leaves unresolved.
fn ignored_finding_kept(t: &Value, i: usize) -> bool {
    let cols: Vec<&str> = t["columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c[1].as_str().unwrap())
        .collect();
    let at = |c: &str| cols.iter().position(|x| *x == c).unwrap();
    let row = &t["rows"][i];
    RETIRED.contains(&row[at("check_id")].as_str().unwrap()) && row[at("ignored_at")] != "NULL"
}

/// Seconds since the epoch for `YYYY-MM-DDTHH:MM:SS.fffZ` (UTC).
fn unix_seconds(s: &str) -> f64 {
    let n = |a: usize, b: usize| s[a..b].parse::<i64>().unwrap();
    let (y, m, d) = (n(0, 4), n(5, 7), n(8, 10));
    // Days from civil (Howard Hinnant's algorithm).
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    (days * 86_400 + n(11, 13) * 3600 + n(14, 16) * 60 + n(17, 19)) as f64
        + s[20..23].parse::<f64>().unwrap() / 1000.0
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

/// Open a copy of vNN.db with `Db::open`; its dump (v14 stamps normalised).
fn migrate(v: i64) -> (tempfile::TempDir, PathBuf, Opened, Value) {
    let (dir, p) = fixture_copy(v);
    let t0 = now();
    let (db, opened) = Db::open(&p).unwrap();
    let t1 = now();
    let mut ours = dump(db.connection());
    normalise_now(&mut ours, &golden(v)["python_app"]["after"], t0, t1);
    (dir, p, opened, ours)
}

/// Which delta a comparison used (each must be used by some fixture).
#[derive(Default)]
struct Used(BTreeMap<&'static str, usize>);

impl Used {
    fn hit(&mut self, k: &'static str) {
        *self.0.entry(k).or_default() += 1;
    }
}

/// The Python app's `after` plus this build's v16 -> v18 steps, enumerated:
///
/// * D1: `schema_version` 16 -> [`SCHEMA_VERSION`];
/// * D2: `track_fingerprints` gains
///   `fingerprint_impl` (every existing row `fpcalc`), `acoustid_lookup_state`
///   (`done` where `acoustid_recording_mbid` is set, else NULL),
///   `acoustid_lookup_failures` (0), `acoustid_retry_after` (NULL), in that
///   order after the Python app's columns; its `sqlite_master.sql` is
///   the Python app's with those column definitions spliced in (checked separately,
///   [`track_fingerprints_sql_is_the_python_apps_plus_the_v17_v18_columns`]);
/// * D3: table `fingerprint_rejects` (empty) and its primary-key
///   index appended to `sqlite_master`;
/// * D4: `journal_mode` is `wal` on both sides (no delta; listed
///   so a change shows up here);
/// * D5: the v14 step leaves an ignored
///   finding of a retired check unresolved (`resolved_at` stays NULL where
///   the Python app stamped it); files older than v14 only.
///
/// Everything else (every other table's columns and rows in rowid order,
/// `sqlite_master` order and text, stored pragmas, integrity) must be equal.
fn compare_with_python(v: i64, ours: &Value, used: &mut Used) {
    let theirs = &golden(v)["python_app"]["after"];
    // D1
    assert_eq!(theirs["schema_version"], json!([16]), "v{v}");
    assert_eq!(ours["schema_version"], json!([SCHEMA_VERSION]), "v{v}");
    used.hit("D1");
    // D4
    assert_eq!(ours["pragmas"], theirs["pragmas"], "v{v} pragmas");
    assert_eq!(ours["pragmas"]["journal_mode"], "wal");
    used.hit("D4");
    assert_eq!(ours["integrity_check"], json!(["ok"]), "v{v}");
    assert_eq!(theirs["integrity_check"], json!(["ok"]), "v{v}");

    // sqlite_master: same rows in the same order, then D3's two.
    let om = ours["sqlite_master"].as_array().unwrap();
    let tm = theirs["sqlite_master"].as_array().unwrap();
    assert_eq!(om.len(), tm.len() + 2, "v{v} sqlite_master length");
    for (o, t) in om.iter().zip(tm) {
        if t[1] == "track_fingerprints" && t[0] == "table" {
            assert_eq!(o.as_array().unwrap()[..3], t.as_array().unwrap()[..3]);
            used.hit("D2-sql");
            continue;
        }
        assert_eq!(o, t, "v{v} sqlite_master row");
    }
    assert_eq!(om[tm.len()][0], "table");
    assert_eq!(om[tm.len()][1], "fingerprint_rejects");
    assert_eq!(
        om[tm.len() + 1][1],
        "sqlite_autoindex_fingerprint_rejects_1"
    );
    used.hit("D3");

    // Tables.
    let ot = ours["tables"].as_object().unwrap();
    let tt = theirs["tables"].as_object().unwrap();
    let mut names: Vec<&String> = ot.keys().collect();
    names.retain(|n| *n != "fingerprint_rejects");
    let mut their_names: Vec<&String> = tt.keys().collect();
    names.sort();
    their_names.sort();
    assert_eq!(names, their_names, "v{v} tables");
    assert_eq!(ot["fingerprint_rejects"]["rows"], json!([]));
    for (name, t) in tt {
        let o = &ot[name];
        if name == "schema_version" {
            assert_eq!(o["columns"], t["columns"]);
            continue;
        }
        if name == "check_findings" {
            // D5: an ignored finding of a retired check the Python app's
            // v14 step stamped resolved stays unresolved (only files older
            // than v14 run that step).
            let mut want = t.clone();
            let col = t["columns"]
                .as_array()
                .unwrap()
                .iter()
                .position(|c| c[1] == "resolved_at")
                .unwrap();
            for i in 0..t["rows"].as_array().unwrap().len() {
                if t["rows"][i][col] == PLACEHOLDER && ignored_finding_kept(t, i) {
                    assert!(v < 14, "v{v}: the v14 step ran on a v14+ file");
                    want["rows"][i][col] = json!("NULL");
                    used.hit("D5");
                }
            }
            assert_eq!(o, &want, "v{v} table {name}");
            continue;
        }
        if name != "track_fingerprints" {
            assert_eq!(o, t, "v{v} table {name}");
            continue;
        }
        // D2
        let tcols = t["columns"].as_array().unwrap();
        let ocols = o["columns"].as_array().unwrap();
        assert_eq!(ocols[..tcols.len()], tcols[..], "v{v} track_fingerprints");
        let added: Vec<&str> = ocols[tcols.len()..]
            .iter()
            .map(|c| c[1].as_str().unwrap())
            .collect();
        assert_eq!(
            added,
            [
                "fingerprint_impl",
                "acoustid_lookup_state",
                "acoustid_lookup_failures",
                "acoustid_retry_after"
            ]
        );
        let mbid = tcols
            .iter()
            .position(|c| c[1] == "acoustid_recording_mbid")
            .unwrap();
        let trows = t["rows"].as_array().unwrap();
        let orows = o["rows"].as_array().unwrap();
        assert_eq!(orows.len(), trows.len());
        for (or, tr) in orows.iter().zip(trows) {
            let tr = tr.as_array().unwrap();
            let mut want = tr.clone();
            want.push(json!(format!("'{FINGERPRINT_IMPL_FPCALC}'")));
            want.push(if tr[mbid] == "NULL" {
                json!("NULL")
            } else {
                json!(format!("'{LOOKUP_DONE}'"))
            });
            want.push(json!("0"));
            want.push(json!("NULL"));
            assert_eq!(or.as_array().unwrap(), &want, "v{v} track_fingerprints row");
        }
        if !trows.is_empty() {
            used.hit("D2-rows");
        }
    }
}

/// Every schema version the Python app could have left on
/// disk (v1..v16) opens and ends exactly where the Python app's chain ends, plus
/// the enumerated v17 / v18 deltas.
#[test]
fn every_old_version_ends_where_the_python_ends_plus_the_v17_v18_deltas() {
    let mut used = Used::default();
    let mut seen_v14_stamp = false;
    for v in OLDEST_SCHEMA_VERSION..=16 {
        let (dir, _p, opened, ours) = migrate(v);
        assert_eq!(
            opened,
            Opened::Migrated {
                from: v,
                backup: Some(dir.path().join(backup_file_name(v)))
            },
            "v{v}"
        );
        compare_with_python(v, &ours, &mut used);
        seen_v14_stamp |= ours.to_string().contains(PLACEHOLDER);
    }
    for k in ["D1", "D2-sql", "D2-rows", "D3", "D4", "D5"] {
        assert!(used.0.contains_key(k), "delta {k} never exercised");
    }
    assert!(seen_v14_stamp, "the v14 step was never exercised");
}

/// The rows are the Python app's: the fixtures are not empty (a guard against a
/// generator that silently wrote nothing). Counts are read from the golden.
#[test]
fn fixtures_carry_user_data() {
    let g = golden(12);
    let t = &g["python_app"]["before"]["tables"];
    for name in [
        "albums",
        "settings",
        "track_tags",
        "tag_vocabulary",
        "track_fingerprints",
        "ai_jobs",
        "check_findings",
    ] {
        assert!(
            !t[name]["rows"].as_array().unwrap().is_empty(),
            "v12 {name} empty"
        );
    }
    let settings = t["settings"]["rows"].to_string();
    assert!(settings.contains("acoustid_api_key"));
}

/// D2's SQL text: the Python app's `track_fingerprints` CREATE text with the
/// v17 / v18 columns spliced in by SQLite's ADD COLUMN. Derived without
/// typing the text: the Python app's statement replayed in a scratch database,
/// then the ADD COLUMNs that reproduce this build's columns (from
/// `PRAGMA table_info` of our result), must give our text exactly.
#[test]
fn track_fingerprints_sql_is_the_python_apps_plus_the_v17_v18_columns() {
    for v in OLDEST_SCHEMA_VERSION..=16 {
        let (_d, _p, _o, ours) = migrate(v);
        let theirs = &golden(v)["python_app"]["after"];
        let sql = |d: &Value| -> String {
            d["sqlite_master"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r[0] == "table" && r[1] == "track_fingerprints")
                .unwrap()[3]
                .as_str()
                .unwrap()
                .to_string()
        };
        let scratch = Connection::open_in_memory().unwrap();
        scratch.execute_batch(&sql(theirs)).unwrap();
        let n = theirs["tables"]["track_fingerprints"]["columns"]
            .as_array()
            .unwrap()
            .len();
        for c in &ours["tables"]["track_fingerprints"]["columns"]
            .as_array()
            .unwrap()[n..]
        {
            let mut def = format!("{} {}", c[1].as_str().unwrap(), c[2].as_str().unwrap());
            if c[3] == 1 {
                def.push_str(" NOT NULL");
            }
            if let Some(d) = c[4].as_str() {
                def.push_str(&format!(" DEFAULT {d}"));
            }
            scratch
                .execute_batch(&format!("ALTER TABLE track_fingerprints ADD COLUMN {def}"))
                .unwrap();
        }
        let replayed: String = scratch
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name = 'track_fingerprints'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sql(&ours), replayed, "v{v}");
    }
}

/// Backup rule for old versions: one backup, named for the version
/// found, a self-contained rollback-journal file holding the untouched
/// input (same dump as the fixture), nothing beside it, no temp file left.
#[test]
fn old_versions_are_backed_up_untouched_under_their_own_version() {
    for v in [1, 6, 11, 12, 14, 15] {
        let (dir, _p, _o, _ours) = migrate(v);
        let backup = dir.path().join(backup_file_name(v));
        let mut names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("before-rust"))
            .collect();
        names.sort();
        assert_eq!(names, [backup_file_name(v)], "v{v}");
        let header = std::fs::read(&backup).unwrap();
        assert_eq!((header[18], header[19]), (1, 1), "v{v} rollback-journal");
        let conn = Connection::open_with_flags(&backup, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
        let theirs = &golden(v)["python_app"]["before"];
        assert_eq!(dump(&conn), *theirs, "v{v} backup is the input");
        drop(conn);
        assert_eq!(existing_backup(dir.path()), Some(backup));
    }
}

/// Adds a trigger that aborts the migration when `schema_version` is set to
/// `at`: a failure injected at a step boundary without a hook in the code.
fn inject_failure_at(p: &Path, at: i64) {
    Connection::open(p)
        .unwrap()
        .execute_batch(&format!(
            "CREATE TRIGGER inject_failure BEFORE UPDATE ON schema_version \
             WHEN NEW.version = {at} BEGIN SELECT RAISE(ABORT, 'injected'); END;"
        ))
        .unwrap();
}

fn remove_injection(p: &Path) {
    Connection::open(p)
        .unwrap()
        .execute_batch("DROP TRIGGER inject_failure")
        .unwrap();
}

/// A migration interrupted at any step boundary either leaves the
/// original content intact (the Python app's chain is one transaction: a
/// failure at its last statement rolls every earlier one back) or leaves a
/// file at a later version that the next open resumes from, reusing the
/// backup the first attempt took (no second backup appears). The end state
/// equals an uninterrupted migration's.
#[test]
fn an_interrupted_migration_is_resumable_and_never_takes_a_second_backup() {
    for v in [3, 12, 15] {
        let (_d, _p, _o, clean) = migrate(v);
        for at in [16, 17, SCHEMA_VERSION] {
            let (dir, p) = fixture_copy(v);
            inject_failure_at(&p, at);
            let before = {
                let c = Connection::open(&p).unwrap();
                dump(&c)
            };
            let err = Db::open(&p).err().expect("injected failure");
            assert!(err.to_string().contains("injected"), "{err}");
            let backup = dir.path().join(backup_file_name(v));
            assert!(backup.is_file(), "v{v}@{at}: backup taken first");
            {
                let c = Connection::open(&p).unwrap();
                let mid = dump(&c);
                let reached: i64 = c
                    .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
                    .unwrap();
                assert_eq!(reached, if at == 16 { v } else { at - 1 }, "v{v}@{at}");
                if at == 16 {
                    // Only the journal mode changed (as the Python app sets WAL
                    // before its transaction: v12.json failure_injection).
                    let mut b = before.clone();
                    b["pragmas"]["journal_mode"] = json!("wal");
                    assert_eq!(mid, b, "v{v}: chain rolled back entirely");
                }
            }
            remove_injection(&p);
            let t0 = now();
            let (db, opened) = Db::open(&p).unwrap();
            let t1 = now();
            assert_eq!(
                opened,
                Opened::Migrated {
                    from: if at == 16 { v } else { at - 1 },
                    backup: if at <= 17 { Some(backup.clone()) } else { None },
                },
                "v{v}@{at}"
            );
            let mut ours = dump(db.connection());
            normalise_now(
                &mut ours,
                &golden(v)["python_app"]["after"],
                t0.min(t1) - 60.0,
                t1,
            );
            assert_eq!(ours, clean, "v{v}@{at}: same end state");
            let backups: Vec<String> = std::fs::read_dir(dir.path())
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|n| n.contains("before-rust"))
                .collect();
            assert_eq!(backups, [backup_file_name(v)], "v{v}@{at}: one backup");
        }
    }
}

/// A newer file than this build knows is still refused and left untouched,
/// whatever older backups sit beside it.
#[test]
fn a_newer_file_is_still_refused_untouched() {
    let (dir, p) = fixture_copy(12);
    Connection::open(&p)
        .unwrap()
        .execute(
            "UPDATE schema_version SET version = ?1",
            [SCHEMA_VERSION + 1],
        )
        .unwrap();
    let bytes = std::fs::read(&p).unwrap();
    assert!(matches!(Db::open(&p), Err(Error::NewerSchema { .. })));
    assert_eq!(std::fs::read(&p).unwrap(), bytes);
    assert!(existing_backup(dir.path()).is_none());
}

/// A leftover temp copy from a backup interrupted mid-copy (any version's
/// name) is removed before the backup is written (it holds keys).
#[test]
fn leftover_temp_backups_of_any_version_are_removed() {
    let (dir, p) = fixture_copy(12);
    let leftovers: Vec<PathBuf> = [5, 12, 16]
        .iter()
        .flat_map(|v| {
            ["", "-journal", "-wal", "-shm"]
                .map(|s| dir.path().join(format!("{}.tmp{s}", backup_file_name(*v))))
        })
        .collect();
    for l in &leftovers {
        std::fs::write(l, b"acoustid_api_key leftover").unwrap();
    }
    Db::open(&p).unwrap();
    for l in &leftovers {
        assert!(!l.exists(), "{}", l.display());
    }
}

/// musaic-secrets finds the backup through `existing_backup`, so the
/// v12-named backup is the one it scrubs (see
/// musaic-secrets/tests/old_schema_backup.rs).
#[test]
fn the_backup_of_an_old_database_is_where_musaic_secrets_looks() {
    let (dir, p) = fixture_copy(12);
    let (_db, _) = Db::open(&p).unwrap();
    assert_eq!(
        existing_backup(dir.path()),
        Some(dir.path().join(musaic_db::backup_file_name(12)))
    );
}
