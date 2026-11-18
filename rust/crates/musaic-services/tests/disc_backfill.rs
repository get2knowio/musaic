//! After the v20 upgrade every album row's per-file disc columns are NULL
//! ("not read yet"). A scan whose folders are unchanged reads again only the
//! rows where disc numbers can matter (repeated track numbers, or a disc in
//! the folder name); every other row keeps its NULL until it is read for
//! another reason. A second scan reads nothing.
//!
//! Also: the album dict the API serves does not carry the new columns, and
//! a NULL in them leaves a check's inputs digest as it was before the
//! columns existed.

mod common;

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, Mutex};

use common::{connect, flac_bytes, fresh_db, recording_bus};
use musaic_scan::TrackOrder;
use musaic_services::scan::{ScanDeps, ScanService, StartScan};
use rusqlite::Connection;

/// One file's (DISCNUMBER, TRACKNUMBER).
type T = (Option<&'static str>, &'static str);

const PLAIN: &str = "Test Artist - Plain [FLAC]";
const REPEATS: &str = "Test Artist - Repeats [FLAC]";
const FOLDER_DISC: &str = "Test Artist - Folder Disc (disc 2 of 2) [FLAC]";
const CONTINUOUS: &str = "Test Artist - Continuous [FLAC]";

fn album(lib: &Path, dir: &str, tracks: &[T]) {
    let d = lib.join(dir);
    std::fs::create_dir_all(&d).unwrap();
    for (i, (disc, track)) in tracks.iter().enumerate() {
        let mut tags: Vec<(String, String)> = vec![
            ("ARTIST".into(), "Test Artist".into()),
            ("ALBUM".into(), dir.into()),
            ("TITLE".into(), format!("Song {i}")),
            ("TRACKNUMBER".into(), (*track).into()),
        ];
        if let Some(disc) = disc {
            tags.push(("DISCNUMBER".into(), (*disc).into()));
        }
        std::fs::write(d.join(format!("{i:02}.flac")), flac_bytes(&tags, 1, 0)).unwrap();
    }
}

/// Runs one scan; returns the folder names it read from disk ("fresh").
fn scan(db: &Path, lib: &str) -> BTreeSet<String> {
    let (bus, _events) = recording_bus();
    let fresh = Arc::new(Mutex::new(BTreeSet::new()));
    let seen = fresh.clone();
    let opener = db.to_path_buf();
    let deps = ScanDeps {
        open_db: Arc::new(move || Ok(connect(&opener))),
        bus,
        clock: Arc::new(|| "2030-01-01T00:00:00Z".to_string()),
        track_order: TrackOrder::DiscTrack,
        after_scan: None,
        on_album: Some(Arc::new(move |dict, is_fresh| {
            assert!(dict.get("track_discs").is_none(), "{}", dict.dumps());
            assert!(dict.get("track_discs_json").is_none(), "{}", dict.dumps());
            assert!(dict.get("tag_disc_total").is_none(), "{}", dict.dumps());
            if is_fresh {
                let name = dict.get("dir_name").and_then(|v| v.as_str()).unwrap();
                seen.lock().unwrap().insert(name.to_string());
            }
        })),
        on_worker_done: None,
        log: None,
    };
    let svc = ScanService::new();
    let StartScan::Started(_, job) = svc.start_scan(lib, false, deps) else {
        panic!("scan already running")
    };
    job.run();
    assert_eq!(svc.state().status.as_str(), "done");
    fresh.lock().unwrap().clone()
}

fn disc_columns(conn: &Connection, dir: &str) -> (Option<String>, Option<i64>) {
    conn.query_row(
        "SELECT track_discs_json, tag_disc_total FROM albums WHERE dir_name = ?1",
        [dir],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .unwrap()
}

fn names(v: &[&str]) -> BTreeSet<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn after_the_upgrade_only_the_rows_that_can_matter_are_read_again() {
    let td = tempfile::tempdir().unwrap();
    let root = musaic_scan::py_resolve(td.path());
    let lib = root.join("lib");
    std::fs::create_dir(&lib).unwrap();
    album(&lib, PLAIN, &[(None, "1"), (None, "2"), (None, "3")]);
    album(
        &lib,
        REPEATS,
        &[
            (Some("1/2"), "1"),
            (Some("1/2"), "2"),
            (Some("2/2"), "1"),
            (Some("2/2"), "2"),
        ],
    );
    album(&lib, FOLDER_DISC, &[(None, "1"), (None, "2")]);
    // Disc tags with continuous track numbers: nothing in the row says so
    // until its files are read, so it is not read again now.
    album(
        &lib,
        CONTINUOUS,
        &[
            (Some("1"), "1"),
            (Some("1"), "2"),
            (Some("2"), "3"),
            (Some("2"), "4"),
        ],
    );
    let db = fresh_db(&root);
    let lib_s = lib.to_string_lossy().into_owned();
    let all = names(&[PLAIN, REPEATS, FOLDER_DISC, CONTINUOUS]);
    assert_eq!(scan(&db, &lib_s), all, "a first scan reads every folder");

    let conn = connect(&db);
    assert_eq!(
        disc_columns(&conn, REPEATS),
        (Some("[1, 1, 2, 2]".into()), Some(2))
    );
    assert_eq!(
        disc_columns(&conn, CONTINUOUS),
        (Some("[1, 1, 2, 2]".into()), None)
    );
    assert_eq!(
        disc_columns(&conn, PLAIN),
        (Some("[null, null, null]".into()), None)
    );
    // The album dict served for a row is the one it always was.
    let id: String = conn
        .query_row(
            "SELECT id FROM albums WHERE dir_name = ?1",
            [REPEATS],
            |r| r.get(0),
        )
        .unwrap();
    let row = musaic_services::albums::get_album_by_id(&conn, &id)
        .unwrap()
        .unwrap();
    assert!(row.json.get("track_discs").is_none());
    assert!(row.json.get("track_discs_json").is_none());
    assert!(row.json.get("tag_disc_total").is_none());
    assert_eq!(
        row.album.track_discs,
        Some(vec![Some(1), Some(1), Some(2), Some(2)])
    );

    // The state a v19 row is in after the migration.
    conn.execute(
        "UPDATE albums SET track_discs_json = NULL, tag_disc_total = NULL",
        [],
    )
    .unwrap();
    assert_eq!(
        scan(&db, &lib_s),
        names(&[REPEATS, FOLDER_DISC]),
        "repeated track numbers, or a disc in the folder name"
    );
    assert_eq!(
        disc_columns(&conn, REPEATS),
        (Some("[1, 1, 2, 2]".into()), Some(2))
    );
    assert_eq!(
        disc_columns(&conn, FOLDER_DISC),
        (Some("[null, null]".into()), None)
    );
    assert_eq!(disc_columns(&conn, PLAIN), (None, None));
    assert_eq!(disc_columns(&conn, CONTINUOUS), (None, None));

    assert_eq!(scan(&db, &lib_s), BTreeSet::new(), "nothing left to read");
    assert_eq!(disc_columns(&conn, PLAIN), (None, None));

    // A NULL leaves an album's inputs digest as it was before the columns
    // existed, so the upgrade itself reruns no check; a filled row's digest
    // does take its values.
    let library: String = conn
        .query_row("SELECT path FROM libraries", [], |r| r.get(0))
        .unwrap();
    let with_columns = musaic_checks::inputs::album_digests(&conn, &library, None).unwrap();
    let copy = td.path().join("without.db");
    conn.execute("VACUUM INTO ?1", [copy.to_string_lossy()])
        .unwrap();
    let old = Connection::open(&copy).unwrap();
    old.execute_batch(
        "ALTER TABLE albums DROP COLUMN tag_disc_total; \
         ALTER TABLE albums DROP COLUMN track_discs_json;",
    )
    .unwrap();
    let without_columns = musaic_checks::inputs::album_digests(&old, &library, None).unwrap();
    let id_of = |dir: &str| -> String {
        conn.query_row("SELECT id FROM albums WHERE dir_name = ?1", [dir], |r| {
            r.get(0)
        })
        .unwrap()
    };
    for dir in [PLAIN, CONTINUOUS] {
        assert_eq!(
            with_columns[&id_of(dir)],
            without_columns[&id_of(dir)],
            "{dir}"
        );
    }
    for dir in [REPEATS, FOLDER_DISC] {
        assert_ne!(
            with_columns[&id_of(dir)],
            without_columns[&id_of(dir)],
            "{dir}"
        );
    }
}
