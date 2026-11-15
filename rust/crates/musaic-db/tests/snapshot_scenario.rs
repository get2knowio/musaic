//! save_snapshot and friends, replayed against a scenario the Python app ran
//! (testdata/golden/snapshot-scenario.json). After every step the whole
//! albums table must equal the Python app's.

use std::collections::HashMap;

use musaic_db::{Db, albums};
use musaic_scan::{TrackOrder, py_resolve, scan_library};
use rusqlite::Connection;
use rusqlite::types::ValueRef;
use serde_json::{Map, Value, json};

struct Norm {
    lib: String,
    ids: HashMap<String, String>,
}

impl Norm {
    fn text(&self, s: &str) -> String {
        match self.ids.get(s) {
            Some(dir) => format!("<ID:{dir}>"),
            None => musaic_testdata::replace_root(s, &self.lib, "<LIB>"),
        }
    }

    fn ids(&self, v: &[String]) -> Vec<String> {
        let mut out: Vec<String> = v.iter().map(|s| self.text(s)).collect();
        out.sort();
        out
    }

    /// The database in the shape the golden's `dump()` writes.
    fn dump(&self, conn: &Connection) -> Value {
        let mut stmt = conn
            .prepare("SELECT * FROM albums ORDER BY dir_name")
            .unwrap();
        let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let albums: Vec<Value> = stmt
            .query_map([], |r| {
                let mut o = Map::new();
                for (i, name) in names.iter().enumerate() {
                    let v = match r.get_ref(i)? {
                        ValueRef::Null => Value::Null,
                        ValueRef::Integer(n) => json!(n),
                        ValueRef::Real(f) => json!(f),
                        ValueRef::Text(t) => {
                            let s = String::from_utf8_lossy(t);
                            if name == "missing_since" || name == "mb_last_checked_at" {
                                json!("<NOW>")
                            } else {
                                json!(self.text(&s))
                            }
                        }
                        ValueRef::Blob(_) => unreachable!("no blob columns"),
                    };
                    o.insert(name.clone(), v);
                }
                Ok(Value::Object(o))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let libraries: Vec<Value> = conn
            .prepare("SELECT path, scanned_at FROM libraries ORDER BY path")
            .unwrap()
            .query_map([], |r| Ok(json!({"path": self.text(&r.get::<_, String>(0)?), "scanned_at": r.get::<_, String>(1)?})))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let fps: Vec<Value> = conn
            .prepare("SELECT album_id, filename FROM track_fingerprints ORDER BY 1, 2")
            .unwrap()
            .query_map([], |r| Ok(json!({"album_id": self.text(&r.get::<_, String>(0)?), "filename": r.get::<_, String>(1)?})))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        json!({"albums": albums, "libraries": libraries, "track_fingerprints": fps})
    }
}

/// The Python app's dump has no `fingerprint_impl`-style v17 additions on the
/// albums table, so the two dumps compare directly.
fn assert_db(norm: &Norm, conn: &Connection, step: &Value) {
    let have = norm.dump(conn);
    let want = &step["db"];
    // Rows come `ORDER BY` album id, and an id hashes the library's absolute
    // location: compare them in the order of their normalised form, which
    // does not depend on where the library lives.
    let sorted = |v: &Value| {
        let mut rows = v.as_array().unwrap().clone();
        rows.sort_by_key(|r| r.to_string());
        rows
    };
    for table in ["libraries", "track_fingerprints"] {
        assert_eq!(
            sorted(&have[table]),
            sorted(&want[table]),
            "{} / {table}",
            step["step"]
        );
    }
    let (h, w) = (
        have["albums"].as_array().unwrap(),
        want["albums"].as_array().unwrap(),
    );
    assert_eq!(h.len(), w.len(), "{} / album count", step["step"]);
    for (a, b) in h.iter().zip(w) {
        assert_eq!(a, b, "{} / {}", step["step"], b["dir_name"]);
    }
}

/// Vanished albums are marked Missing, never deleted; deletion cascades;
/// mark/clear are idempotent; plus the row encoding of album rows.
#[test]
fn snapshot_scenario_matches_the_python_app() {
    let lib_dir = musaic_testdata::synthetic_library();
    let g: Value = musaic_testdata::golden("snapshot-scenario.json");
    let steps = g["steps"].as_array().unwrap();
    let step = |prefix: &str| {
        steps
            .iter()
            .find(|s| s["step"].as_str().unwrap().starts_with(prefix))
            .unwrap()
    };
    let scanned_at: Vec<&str> = g["scanned_at"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    let name = |k: &str| g[k].as_str().unwrap().to_string();
    let gone: Vec<String> = g["gone"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();

    // Scan in the Python app's own track order so rows are comparable field for field.
    let all = scan_library(&lib_dir, TrackOrder::Filename);
    assert_eq!(all.len(), 13);
    let lib = py_resolve(&lib_dir).to_string_lossy().into_owned();
    let norm = Norm {
        lib: lib.clone(),
        ids: all
            .iter()
            .map(|a| (a.id.clone(), a.dir_name.clone()))
            .collect(),
    };
    let id_of = |dir: &str| all.iter().find(|a| a.dir_name == dir).unwrap().id.clone();

    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("library.db");
    std::fs::copy(musaic_testdata::public("db/v16-empty.db"), &path).unwrap();
    let (mut db, _) = Db::open(&path).unwrap();

    // 1 save all
    let r = albums::save_snapshot(
        db.connection_mut(),
        &lib,
        scanned_at[0],
        "2026-01-01T00:00:01Z",
        &all,
    )
    .unwrap();
    let s = step("1 save all");
    assert_eq!(
        json!({"upserted_count": r.upserted_count, "newly_missing": norm.ids(&r.newly_missing), "returned": norm.ids(&r.returned)}),
        s["result"]
    );
    assert_db(&norm, db.connection(), s);

    // 1b other subsystems write an anchor and fingerprints (their Rust ports do not exist yet)
    let c = db.connection();
    c.execute(
        "UPDATE albums SET mb_release_group_mbid = '11111111-1111-4111-8111-111111111111', mb_release_mbid = NULL, mb_match_score = 88, mb_last_checked_at = 'x' WHERE id = ?1",
        [id_of(&name("anchored"))],
    )
    .unwrap();
    c.execute(
        "UPDATE albums SET mb_release_group_mbid = '22222222-2222-4222-8222-222222222222', mb_release_mbid = '33333333-3333-4333-8333-333333333333', mb_match_score = 100, mb_last_checked_at = 'x' WHERE id = ?1",
        [id_of(&gone[0])],
    )
    .unwrap();
    for dir in [&gone[0], &name("deleted")] {
        c.execute(
            "INSERT INTO track_fingerprints(album_id, filename, fingerprint, duration_s, last_computed_at) VALUES (?1, '01.flac', 'AQAA', 10.0, 'x')",
            [id_of(dir)],
        )
        .unwrap();
    }
    assert_db(&norm, db.connection(), step("1b"));

    // 2 two albums vanish: marked Missing, rows, anchor and fingerprints kept
    let reduced: Vec<_> = all
        .iter()
        .filter(|a| !gone.contains(&a.dir_name))
        .cloned()
        .collect();
    let r = albums::save_snapshot(
        db.connection_mut(),
        &lib,
        scanned_at[1],
        "2026-01-02T00:00:01Z",
        &reduced,
    )
    .unwrap();
    let s = step("2 ");
    assert_eq!(
        json!({"upserted_count": r.upserted_count, "newly_missing": norm.ids(&r.newly_missing), "returned": norm.ids(&r.returned)}),
        s["result"]
    );
    assert_db(&norm, db.connection(), s);
    let first_seen_missing = albums::missing_since(db.connection(), &id_of(&gone[0])).unwrap();
    assert_eq!(first_seen_missing.as_deref(), Some("2026-01-02T00:00:01Z"));

    // 3 the same reduced scan again: nothing transitions and the stamp is not bumped
    let r = albums::save_snapshot(
        db.connection_mut(),
        &lib,
        scanned_at[2],
        "2026-01-03T00:00:01Z",
        &reduced,
    )
    .unwrap();
    let s = step("3 ");
    assert_eq!(
        json!({"upserted_count": r.upserted_count, "newly_missing": norm.ids(&r.newly_missing), "returned": norm.ids(&r.returned)}),
        s["result"]
    );
    assert_db(&norm, db.connection(), s);
    assert_eq!(s["missing_since_unchanged"], json!(true));
    assert_eq!(
        albums::missing_since(db.connection(), &id_of(&gone[0])).unwrap(),
        first_seen_missing
    );

    // 4 everything is back, one album retitled: returned, anchors survive the rescan
    let retitled: Vec<_> = all
        .iter()
        .cloned()
        .map(|mut a| {
            if a.dir_name == name("retitled") {
                a.title = Some("Retitled".into());
            }
            a
        })
        .collect();
    let r = albums::save_snapshot(
        db.connection_mut(),
        &lib,
        scanned_at[3],
        "2026-01-04T00:00:01Z",
        &retitled,
    )
    .unwrap();
    let s = step("4 ");
    assert_eq!(
        json!({"upserted_count": r.upserted_count, "newly_missing": norm.ids(&r.newly_missing), "returned": norm.ids(&r.returned)}),
        s["result"]
    );
    assert_db(&norm, db.connection(), s);

    // 5 mark / clear by hand are idempotent
    let s = step("5 ");
    let aid = id_of(&name("anchored"));
    let c = db.connection();
    let have = json!({
        "mark_first": albums::mark_album_missing(c, &aid, "2026-02-01T00:00:00Z").unwrap(),
        "mark_second": albums::mark_album_missing(c, &aid, "2026-02-02T00:00:00Z").unwrap(),
        "missing_since_after_marks": albums::missing_since(c, &aid).unwrap(),
        "clear_first": albums::clear_album_missing(c, &aid).unwrap(),
        "clear_second": albums::clear_album_missing(c, &aid).unwrap(),
        "mark_unknown_id": albums::mark_album_missing(c, "0000000000000000", "2026-02-03T00:00:00Z").unwrap(),
        "clear_unknown_id": albums::clear_album_missing(c, "0000000000000000").unwrap(),
    });
    assert_eq!(have, s["result"]);
    assert_db(&norm, db.connection(), s);

    // 6 the one destructive path: the row and its fingerprints go
    assert!(albums::delete_album(db.connection(), &id_of(&name("deleted"))).unwrap());
    assert!(!albums::delete_album(db.connection(), &id_of(&name("deleted"))).unwrap());
    assert_db(&norm, db.connection(), step("6 "));
}

/// More than 30% of a cache of at least 10 albums vanishing aborts the scan.
/// Thresholds and message format from `services/scan.py`; no capture exercises this yet.
#[test]
fn sanity_check_thresholds() {
    assert!(albums::scan_sanity_check(9, 9, "/lib").is_none()); // cache too small to judge
    assert!(albums::scan_sanity_check(10, 3, "/lib").is_none()); // exactly 30% is allowed
    assert!(albums::scan_sanity_check(0, 0, "/lib").is_none());
    let a = albums::scan_sanity_check(10, 4, "/lib").unwrap();
    assert_eq!((a.vanish_count, a.cached_count, a.threshold), (4, 10, 0.30));
    assert_eq!(a.vanish_ratio, 0.4);
    assert_eq!(
        a.detail,
        "Scan aborted: 4 of 10 cached album(s) (40%) appear to be missing from disk. This looks like a drive \
         outage or a mount problem rather than real deletions. No changes were written. Check that /lib is \
         mounted and readable, then re-run the scan."
    );
    assert!(
        albums::scan_sanity_check(273, 273, "/srv/music")
            .unwrap()
            .detail
            .contains("(100%)")
    );
}
