//! A row written before the folder-name parser knew `Album CD1` has no disc
//! number and a title ending in "CD1". The scan notices that a fresh parse
//! of the stored folder name differs from the row and reads that album again
//! (and only that one), once. The album id is the path's hash and does not
//! change.

mod common;

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, Mutex};

use common::{connect, flac_bytes, fresh_db, recording_bus};
use musaic_scan::TrackOrder;
use musaic_services::scan::{ScanDeps, ScanService, StartScan};

const SET_DISC: &str = "Test Artist - Boxed Set CD1 [FLAC]";
const PLAIN: &str = "Test Artist - Plain [FLAC]";

fn album(lib: &Path, dir: &str) {
    let d = lib.join(dir);
    std::fs::create_dir_all(&d).unwrap();
    for i in 0..2 {
        let tags: Vec<(String, String)> = vec![
            ("ARTIST".into(), "Test Artist".into()),
            ("ALBUM".into(), dir.into()),
            ("TITLE".into(), format!("Song {i}")),
            ("TRACKNUMBER".into(), (i + 1).to_string()),
        ];
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

type Row = (
    String,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<i64>,
);

fn row(conn: &rusqlite::Connection, dir: &str) -> Row {
    conn.query_row(
        "SELECT id, dir_album, title, disc_number, disc_total FROM albums WHERE dir_name = ?1",
        [dir],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )
    .unwrap()
}

#[test]
fn rows_parsed_before_the_marker_was_known_are_read_again_once() {
    let td = tempfile::tempdir().unwrap();
    let root = musaic_scan::py_resolve(td.path());
    let lib = root.join("lib");
    std::fs::create_dir(&lib).unwrap();
    album(&lib, SET_DISC);
    album(&lib, PLAIN);
    let db = fresh_db(&root);
    let lib_s = lib.to_string_lossy().into_owned();
    scan(&db, &lib_s);

    let conn = connect(&db);
    let before = row(&conn, SET_DISC);
    assert_eq!(before.1.as_deref(), Some("Boxed Set"));
    assert_eq!((before.3, before.4), (Some(1), None));

    // The row as the earlier parser wrote it.
    conn.execute(
        "UPDATE albums SET dir_album = 'Boxed Set CD1', title = 'Boxed Set CD1', \
         disc_number = NULL WHERE dir_name = ?1",
        [SET_DISC],
    )
    .unwrap();
    let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>();
    assert_eq!(
        scan(&db, &lib_s),
        names(&[SET_DISC]),
        "only the folder whose parse differs is read"
    );
    assert_eq!(row(&conn, SET_DISC), before, "same id, parsed fields back");
    assert_eq!(scan(&db, &lib_s), BTreeSet::new(), "then nothing is read");
}
