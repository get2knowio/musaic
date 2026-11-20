//! A scan adopts a vanished album's row into the folder it found under
//! another spelling, when the filesystem says they are the same folder
//! (`src/scan_adopt.rs`), and refuses in every unclear case.
//!
//! "Respelled" here means: the folder is now listed under another
//! spelling, and its old spelling still opens it. On macOS (default APFS)
//! and Windows (NTFS) that is a case-only rename, a real case-insensitive
//! lookup. Linux volumes tell names apart byte for byte, so there the old
//! spelling is kept opening the folder by a symlink, which discovery lists
//! but does not count as an album folder; the real Linux behaviour (the old
//! spelling does not open, nothing is adopted) has its own tests.

#[path = "../../musaic-services/tests/common/mod.rs"]
mod common;
mod scan_wire;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use common::{connect, flac_bytes, fresh_db, recording_bus};
use musaic_events::{Bus, Event, EventKind, Subscriber, Topic};
use musaic_scan::TrackOrder;
use musaic_services::scan::{AlbumHook, ScanDeps, ScanService, StartScan};
use musaic_services::scan_adopt::{Found, plan};
use rusqlite::Connection;
use serde_json::{Value, json};

/// Invented albums: "<Artist> - <Album> [FLAC]" with two tracks each.
const ALBUMS: &[(&str, &str)] = &[
    ("Velvet Harbor", "Night Tides"),
    ("Paper Lanterns", "Slow Orbit"),
    ("Quiet Meridian", "Salt and Cedar"),
    ("Juniper Static", "Low Frequencies"),
    ("Marble Coast", "Afterhours"),
    ("Copper Wren", "Field Notes"),
    ("Hollow Pines", "Weathervane"),
    ("Saffron Tide", "Open Water"),
    ("Iron Orchard", "First Frost"),
    ("Lumen Drift", "Northbound"),
];

fn dir_name(i: usize) -> String {
    format!("{} - {} [FLAC]", ALBUMS[i].0, ALBUMS[i].1)
}

fn build_named(lib: &Path, name: &str, artist: &str, album: &str) {
    let d = lib.join(name);
    std::fs::create_dir_all(&d).unwrap();
    for t in 1..=2u32 {
        let tags = [
            ("ARTIST", artist.to_string()),
            ("ALBUMARTIST", artist.to_string()),
            ("ALBUM", album.to_string()),
            ("TITLE", format!("Track {t}")),
            ("TRACKNUMBER", t.to_string()),
            ("DATE", "2021".to_string()),
        ]
        .map(|(k, v)| (k.to_string(), v));
        std::fs::write(
            d.join(format!("0{t} - Track {t}.flac")),
            flac_bytes(&tags, 2, 0),
        )
        .unwrap();
    }
}

fn build(lib: &Path, i: usize) {
    build_named(lib, &dir_name(i), ALBUMS[i].0, ALBUMS[i].1);
}

/// The lower-case spelling a respelled folder is listed under.
fn respelled(i: usize) -> String {
    dir_name(i).to_lowercase()
}

/// Make `lib/from` listed as `lib/to` while `lib/from` still opens it (see
/// the module docs). `false` when this volume cannot (a case-sensitive
/// volume on macOS): the caller skips.
fn respell(lib: &Path, from: &str, to: &str) -> bool {
    std::fs::rename(lib.join(from), lib.join(to)).unwrap();
    #[cfg(target_os = "linux")]
    std::os::unix::fs::symlink(to, lib.join(from)).unwrap();
    let opens = lib.join(from).is_dir();
    if !opens {
        eprintln!("skipped: this volume tells {from:?} and {to:?} apart");
    }
    opens
}

struct World {
    _td: tempfile::TempDir,
    root: PathBuf,
    lib: PathBuf,
    db: PathBuf,
    bus: Arc<Bus>,
    events: Arc<Mutex<Vec<Event>>>,
    /// `in_bulk_run` as each `AlbumRenamed` subscriber saw it.
    renamed_in_bulk: Arc<Mutex<Vec<bool>>>,
    tick: Arc<AtomicUsize>,
}

impl World {
    fn new(albums: usize) -> World {
        let td = tempfile::tempdir().unwrap();
        let root = musaic_testdata::canonical(td.path());
        let lib = root.join("lib");
        std::fs::create_dir(&lib).unwrap();
        for i in 0..albums {
            build(&lib, i);
        }
        let db = fresh_db(&root);
        let (bus, events) = recording_bus();
        let renamed_in_bulk: Arc<Mutex<Vec<bool>>> = Arc::default();
        let seen = Arc::clone(&renamed_in_bulk);
        bus.register(
            Topic::Kind(EventKind::AlbumRenamed),
            Subscriber::from_fn(move |_| {
                seen.lock()
                    .unwrap()
                    .push(musaic_events::context::in_bulk_run());
                Ok(())
            })
            .name("renamed_context"),
        );
        World {
            _td: td,
            root,
            lib,
            db,
            bus,
            events,
            renamed_in_bulk,
            tick: Arc::default(),
        }
    }

    fn lib_s(&self) -> String {
        self.lib.to_string_lossy().into_owned()
    }

    fn path_of(&self, name: &str) -> String {
        self.lib.join(name).to_string_lossy().into_owned()
    }

    fn deps(&self, on_album: Option<AlbumHook>) -> ScanDeps {
        let db = self.db.clone();
        let tick = Arc::clone(&self.tick);
        ScanDeps {
            open_db: Arc::new(move || Ok(connect(&db))),
            bus: Arc::clone(&self.bus),
            clock: Arc::new(move || {
                let n = tick.fetch_add(1, Ordering::SeqCst);
                format!("2030-01-01T00:{:02}:{:02}Z", n / 60, n % 60)
            }),
            track_order: TrackOrder::DiscTrack,
            after_scan: None,
            on_album,
            on_worker_done: None,
            log: None,
        }
    }

    /// A scan run to its end; returns the service (its buffer and state).
    fn scan(&self, on_album: Option<AlbumHook>) -> ScanService {
        let svc = ScanService::new();
        let StartScan::Started(_, job) = svc.start_scan(&self.lib_s(), false, self.deps(on_album))
        else {
            panic!("a scan was already running")
        };
        job.run();
        svc
    }

    fn take_events(&self) -> Vec<Value> {
        let mut l = self.events.lock().unwrap();
        let out = l.iter().map(common::event_json).collect();
        l.clear();
        self.renamed_in_bulk.lock().unwrap().clear();
        out
    }

    fn conn(&self) -> Connection {
        connect(&self.db)
    }

    fn id_at(&self, path: &str) -> Option<String> {
        self.conn()
            .query_row("SELECT id FROM albums WHERE path = ?1", [path], |r| {
                r.get(0)
            })
            .ok()
    }

    fn missing_since(&self, id: &str) -> Option<String> {
        self.conn()
            .query_row(
                "SELECT missing_since FROM albums WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap()
    }

    fn album_count(&self) -> i64 {
        self.conn()
            .query_row("SELECT COUNT(*) FROM albums", [], |r| r.get(0))
            .unwrap()
    }

    /// Every table, every row, as text: what a scan that must write nothing
    /// is compared against.
    fn dump(&self) -> Value {
        let c = self.conn();
        let mut out = serde_json::Map::new();
        for table in [
            "albums",
            "libraries",
            "track_fingerprints",
            "fingerprint_rejects",
            "track_tags",
            "check_findings",
            "check_inputs",
            "ai_jobs",
            "album_blocked_mb_release_groups",
        ] {
            let mut st = c
                .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
                .or_else(|_| c.prepare(&format!("SELECT * FROM {table}")))
                .unwrap();
            let n = st.column_count();
            let rows: Vec<Value> = st
                .query_map([], |r| {
                    Ok(Value::Array(
                        (0..n)
                            .map(|i| json!(format!("{:?}", r.get_ref(i).unwrap())))
                            .collect(),
                    ))
                })
                .unwrap()
                .map(Result::unwrap)
                .collect();
            out.insert(table.into(), Value::Array(rows));
        }
        Value::Object(out)
    }
}

fn names(svc: &ScanService) -> Vec<&'static str> {
    svc.buffer().iter().map(|e| scan_wire::name(e)).collect()
}

fn types(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|e| e["type"].as_str().unwrap().to_string())
        .collect()
}

/// Everything that hangs off an album by id, one or two rows each, plus
/// its MusicBrainz anchor (columns on the row).
fn add_dependents(c: &Connection, id: &str, path: &str) {
    let lib: String = c
        .query_row("SELECT library_path FROM albums WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .unwrap();
    c.execute(
        "UPDATE albums SET mb_release_group_mbid = 'rg-1', mb_release_mbid = 'rel-1', \
         mb_match_score = 97, mb_last_checked_at = '2030-01-01T00:00:00Z' WHERE id = ?1",
        [id],
    )
    .unwrap();
    c.execute(
        "INSERT INTO track_fingerprints (album_id, filename, fingerprint, duration_s, \
         last_computed_at) VALUES (?1, '01 - Track 1.flac', 'AQAA', 2.0, '2030-01-01T00:00:00Z')",
        [id],
    )
    .unwrap();
    c.execute(
        "INSERT INTO fingerprint_rejects (album_id, filename, file_signature, \
         fingerprint_impl, rejected_at) VALUES (?1, '02 - Track 2.flac', 'sig', \
         'rust-chromaprint', '2030-01-01T00:00:00Z')",
        [id],
    )
    .unwrap();
    c.execute(
        "INSERT INTO track_tags (album_id, filename, tag, created_at) \
         VALUES (?1, '01 - Track 1.flac', 'favourite', '2030-01-01T00:00:00Z')",
        [id],
    )
    .unwrap();
    c.execute(
        "INSERT INTO check_findings (check_id, album_id, summary, first_seen_at, last_seen_at) \
         VALUES ('no-cover-art', ?1, 'No cover art', 't', 't')",
        [id],
    )
    .unwrap();
    c.execute(
        "INSERT INTO check_findings (check_id, album_id, summary, first_seen_at, last_seen_at, \
         ignored_at, ignored_reason) VALUES ('track-gaps', ?1, 'Gap', 't', 't', 't', 'on purpose')",
        [id],
    )
    .unwrap();
    c.execute(
        "INSERT INTO ai_jobs (id, album_id, album_path, job_type, status, created_at) \
         VALUES ('job1', ?1, ?2, 'metadata', 'completed', 't')",
        [id, path],
    )
    .unwrap();
    c.execute(
        "INSERT INTO album_blocked_mb_release_groups (album_id, release_group_mbid, blocked_at) \
         VALUES (?1, 'rg-blocked', 't')",
        [id],
    )
    .unwrap();
    c.execute(
        "INSERT INTO check_inputs (check_id, library_path, album_id, digest, checked_at) \
         VALUES ('no-cover-art', ?1, ?2, 'd', 't')",
        [lib.as_str(), id],
    )
    .unwrap();
}

/// Rows per table under `id`.
fn dependents(c: &Connection, id: &str) -> Value {
    let n = |sql: &str| -> i64 { c.query_row(sql, [id], |r| r.get(0)).unwrap() };
    json!({
        "track_fingerprints": n("SELECT COUNT(*) FROM track_fingerprints WHERE album_id = ?1"),
        "fingerprint_rejects": n("SELECT COUNT(*) FROM fingerprint_rejects WHERE album_id = ?1"),
        "track_tags": n("SELECT COUNT(*) FROM track_tags WHERE album_id = ?1"),
        "check_findings": n("SELECT COUNT(*) FROM check_findings WHERE album_id = ?1"),
        "ignored_findings": n("SELECT COUNT(*) FROM check_findings WHERE album_id = ?1 \
                               AND ignored_at IS NOT NULL AND ignored_reason = 'on purpose'"),
        "ai_jobs": n("SELECT COUNT(*) FROM ai_jobs WHERE album_id = ?1"),
        "blocklist": n("SELECT COUNT(*) FROM album_blocked_mb_release_groups WHERE album_id = ?1"),
        "anchor": n("SELECT COUNT(*) FROM albums WHERE id = ?1 AND mb_release_group_mbid = 'rg-1' \
                     AND mb_release_mbid = 'rel-1' AND mb_match_score = 97"),
    })
}

fn all_dependents() -> Value {
    json!({
        "track_fingerprints": 1, "fingerprint_rejects": 1, "track_tags": 1,
        "check_findings": 2, "ignored_findings": 1, "ai_jobs": 1, "blocklist": 1,
        "anchor": 1,
    })
}

fn no_dependents() -> Value {
    json!({
        "track_fingerprints": 0, "fingerprint_rejects": 0, "track_tags": 0,
        "check_findings": 0, "ignored_findings": 0, "ai_jobs": 0, "blocklist": 0,
        "anchor": 0,
    })
}

fn foreign_key_violations(c: &Connection) -> i64 {
    let mut st = c.prepare("PRAGMA foreign_key_check").unwrap();
    st.query_map([], |_| Ok(())).unwrap().count() as i64
}

/// The rule's main case: the row, and everything on it, moves to the id and
/// path of the folder as now listed; no Missing mark, no new row, one
/// `AlbumRenamed`, published with the per-album reruns suppressed. A scan
/// after it changes nothing.
#[test]
fn a_respelled_folder_keeps_its_row_and_everything_on_it() {
    let w = World::new(4);
    w.scan(None);
    w.take_events();
    let old_path = w.path_of(&dir_name(0));
    let old_id = w.id_at(&old_path).unwrap();
    add_dependents(&w.conn(), &old_id, &old_path);
    if !respell(&w.lib, &dir_name(0), &respelled(0)) {
        return;
    }
    let new_path = w.path_of(&respelled(0));
    let new_id = musaic_core::album_id(&new_path);
    assert_ne!(old_id, new_id);

    let svc = w.scan(None);
    assert_eq!(svc.state().status.as_str(), "done");
    assert_eq!(svc.state().removed_count, 0);
    let c = w.conn();
    assert_eq!(w.album_count(), 4, "no row added, none left behind");
    assert_eq!(w.id_at(&old_path), None);
    assert_eq!(w.id_at(&new_path).as_deref(), Some(new_id.as_str()));
    assert_eq!(w.missing_since(&new_id), None);
    let dir: String = c
        .query_row(
            "SELECT dir_name FROM albums WHERE id = ?1",
            [&new_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(dir, respelled(0));
    assert_eq!(dependents(&c, &new_id), all_dependents());
    assert_eq!(dependents(&c, &old_id), no_dependents());
    assert_eq!(foreign_key_violations(&c), 0);

    let ev = w.take_events();
    assert_eq!(types(&ev), ["AlbumRenamed", "LibraryScanCompleted"]);
    assert_eq!(ev[0]["old_album_id"], old_id);
    assert_eq!(ev[0]["new_album_id"], new_id);

    let stream = names(&svc);
    assert!(!stream.contains(&"marked_missing"), "{stream:?}");
    assert_eq!(stream.iter().filter(|n| **n == "adopted").count(), 1);
    let buffer = svc.buffer();
    let adopted = buffer
        .iter()
        .find(|e| scan_wire::name(e) == "adopted")
        .unwrap();
    assert_eq!(
        scan_wire::data(adopted).to_value(),
        json!({"old_id": old_id, "id": new_id, "old_path": old_path, "path": new_path})
    );
    let done = buffer.last().unwrap();
    assert_eq!(scan_wire::name(done), "done");
    assert_eq!(scan_wire::data(done).to_value()["marked_missing_count"], 0);
    assert_eq!(scan_wire::data(done).to_value()["newly_missing_count"], 0);

    // Idempotent: the next scan finds the row at the listed spelling.
    let again = w.scan(None);
    assert!(!names(&again).contains(&"adopted"));
    assert_eq!(types(&w.take_events()), ["LibraryScanCompleted"]);
    assert_eq!(w.id_at(&new_path).as_deref(), Some(new_id.as_str()));
}

/// The adoption's `AlbumRenamed` is published with `in_bulk_run` set, so
/// the rename subscribers skip their per-album check reruns (the
/// library-wide one on `LibraryScanCompleted` follows).
#[test]
fn adopted_renames_do_not_rerun_checks_per_album() {
    let w = World::new(4);
    w.scan(None);
    w.take_events();
    if !respell(&w.lib, &dir_name(1), &respelled(1))
        || !respell(&w.lib, &dir_name(2), &respelled(2))
    {
        return;
    }
    w.scan(None);
    assert_eq!(*w.renamed_in_bulk.lock().unwrap(), [true, true]);
    assert_eq!(
        types(&w.take_events()),
        ["AlbumRenamed", "AlbumRenamed", "LibraryScanCompleted"]
    );
    // Outside the scan the flag is not left set.
    assert!(!musaic_events::context::in_bulk_run());
}

/// An unmounted drive: the stored paths do not open, so nothing is
/// adopted and the sanity check aborts exactly as before.
#[test]
fn an_absent_library_root_still_aborts() {
    let w = World::new(10);
    w.scan(None);
    w.take_events();
    let before = w.dump();
    std::fs::rename(&w.lib, w.root.join("away")).unwrap();
    let svc = w.scan(None);
    assert_eq!(svc.state().status.as_str(), "aborted");
    let last = scan_wire::data(svc.buffer().last().unwrap()).to_value();
    assert_eq!(last["vanish_count"], 10);
    assert_eq!(last["cached_count"], 10);
    assert_eq!(w.dump(), before);
    assert_eq!(
        types(&w.take_events()),
        ["LibraryScanStopped"],
        "no AlbumRenamed, no Missing transition"
    );
}

/// A respelled share is not an outage: adopted rows are neither vanished
/// nor new, so respelling half the library saves instead of aborting.
#[test]
fn respelling_half_the_library_does_not_abort() {
    let w = World::new(10);
    w.scan(None);
    w.take_events();
    for i in 0..5 {
        if !respell(&w.lib, &dir_name(i), &respelled(i)) {
            return;
        }
    }
    let svc = w.scan(None);
    assert_eq!(svc.state().status.as_str(), "done");
    assert_eq!(w.album_count(), 10);
    let ev = types(&w.take_events());
    assert_eq!(ev.iter().filter(|t| *t == "AlbumRenamed").count(), 5);
    assert!(!ev.contains(&"AlbumMarkedMissing".to_string()));
    for i in 0..5 {
        assert!(w.id_at(&w.path_of(&respelled(i))).is_some());
    }
}

/// A row already Missing is never adopted, even when its old spelling opens
/// the folder found: it stays Missing and the folder gets a new row.
#[test]
fn a_missing_row_is_not_adopted() {
    let w = World::new(4);
    w.scan(None);
    let old_path = w.path_of(&dir_name(0));
    let old_id = w.id_at(&old_path).unwrap();
    std::fs::rename(w.lib.join(dir_name(0)), w.root.join("parked")).unwrap();
    w.scan(None);
    assert!(w.missing_since(&old_id).is_some());
    std::fs::rename(w.root.join("parked"), w.lib.join(dir_name(0))).unwrap();
    if !respell(&w.lib, &dir_name(0), &respelled(0)) {
        return;
    }
    w.take_events();
    let svc = w.scan(None);
    assert!(!names(&svc).contains(&"adopted"));
    assert!(!types(&w.take_events()).contains(&"AlbumRenamed".to_string()));
    assert!(w.missing_since(&old_id).is_some(), "still Missing");
    let new_id = w.id_at(&w.path_of(&respelled(0))).unwrap();
    assert_ne!(new_id, old_id);
    assert_eq!(w.album_count(), 5);
}

/// A folder that already has a row of its own does not take over another
/// row: the vanished row goes Missing as it always did.
#[test]
fn a_folder_with_its_own_row_is_not_adopted() {
    let w = World::new(4);
    w.scan(None);
    let old_path = w.path_of(&dir_name(0));
    let old_id = w.id_at(&old_path).unwrap();
    if !respell(&w.lib, &dir_name(0), &respelled(0)) {
        return;
    }
    let new_path = w.path_of(&respelled(0));
    // A row for the listed spelling, as an earlier scan of it would have
    // left.
    let fresh = musaic_scan::scan_album(Path::new(&new_path), TrackOrder::DiscTrack).unwrap();
    let mut c = w.conn();
    musaic_db::albums::upsert_album(&mut c, &w.lib_s(), "t", &fresh).unwrap();
    w.take_events();
    let svc = w.scan(None);
    assert!(!names(&svc).contains(&"adopted"));
    assert!(names(&svc).contains(&"marked_missing"));
    assert_eq!(
        types(&w.take_events()),
        ["AlbumMarkedMissing", "LibraryScanCompleted"]
    );
    assert!(w.missing_since(&old_id).is_some());
    assert_eq!(w.missing_since(&fresh.id), None);
}

/// Rows left under the new id by an album that was removed (here a stale
/// finding) would be merged into the adopted album: refused.
#[test]
fn a_new_id_that_other_tables_still_hold_is_not_adopted() {
    let w = World::new(4);
    w.scan(None);
    let old_id = w.id_at(&w.path_of(&dir_name(0))).unwrap();
    if !respell(&w.lib, &dir_name(0), &respelled(0)) {
        return;
    }
    let new_id = musaic_core::album_id(&w.path_of(&respelled(0)));
    w.conn()
        .execute(
            "INSERT INTO check_findings (check_id, album_id, summary, first_seen_at, \
             last_seen_at) VALUES ('no-cover-art', ?1, 'left over', 't', 't')",
            [&new_id],
        )
        .unwrap();
    w.take_events();
    let svc = w.scan(None);
    assert!(!names(&svc).contains(&"adopted"));
    assert!(w.missing_since(&old_id).is_some());
    assert_eq!(w.missing_since(&new_id), None);
}

/// The planner itself, on the cases a real listing rarely produces: two
/// folders found that are both the vanished one, and two vanished rows
/// that are both the folder found. Neither is adopted. The spellings all
/// open one directory (natively on a case-insensitive volume, through
/// symlinks elsewhere).
#[test]
fn ambiguous_matches_are_not_adopted() {
    let td = tempfile::tempdir().unwrap();
    let root = musaic_testdata::canonical(td.path());
    let lib = root.join("lib");
    std::fs::create_dir(&lib).unwrap();
    build_named(&lib, "velvet harbor - x [FLAC]", "Velvet Harbor", "X");
    for alias in ["Velvet Harbor - X [FLAC]", "VELVET HARBOR - X [FLAC]"] {
        if !lib.join(alias).is_dir() {
            #[cfg(unix)]
            std::os::unix::fs::symlink("velvet harbor - x [FLAC]", lib.join(alias)).unwrap();
        }
        if !lib.join(alias).is_dir() {
            eprintln!("skipped: cannot make {alias:?} open the folder");
            return;
        }
    }
    let p = |n: &str| lib.join(n).to_string_lossy().into_owned();
    let real = p("velvet harbor - x [FLAC]");
    let upper = p("VELVET HARBOR - X [FLAC]");
    let title = p("Velvet Harbor - X [FLAC]");
    let read = |s: &str| musaic_scan::scan_album(Path::new(s), TrackOrder::DiscTrack).unwrap();
    let (a_real, a_upper) = (read(&real), read(&upper));
    let db = fresh_db(&root);
    let conn = connect(&db);
    let gone = |s: &str| ("old-id".to_string(), s.to_string());
    let gone2 = |s: &str| ("other-id".to_string(), s.to_string());

    // Control: one vanished row, one folder found that is it.
    let f_real = Found {
        path: &real,
        album: Some(&a_real),
        has_row: false,
    };
    let got = plan(&conn, &[gone(&title)], std::slice::from_ref(&f_real));
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].old_id, "old-id");
    assert_eq!(got[0].new_id, a_real.id);
    assert_eq!(got[0].new_path, real);

    // Two folders found are both the vanished one.
    let f_upper = Found {
        path: &upper,
        album: Some(&a_upper),
        has_row: false,
    };
    let both = [
        Found {
            path: &real,
            album: Some(&a_real),
            has_row: false,
        },
        f_upper,
    ];
    assert!(plan(&conn, &[gone(&title)], &both).is_empty());

    // Two vanished rows are both the folder found.
    assert!(
        plan(
            &conn,
            &[gone(&title), gone2(&upper)],
            std::slice::from_ref(&f_real)
        )
        .is_empty()
    );

    // The folder found has a row of its own, or gave no album.
    let with_row = Found {
        path: &real,
        album: Some(&a_real),
        has_row: true,
    };
    assert!(plan(&conn, &[gone(&title)], &[with_row]).is_empty());
    let unread = Found {
        path: &real,
        album: None,
        has_row: false,
    };
    assert!(plan(&conn, &[gone(&title)], &[unread]).is_empty());

    // A stored path that does not open is never compared.
    let absent = p("Velvet Harbor - Y [FLAC]");
    assert!(plan(&conn, &[gone(&absent)], std::slice::from_ref(&f_real)).is_empty());
}

/// A cancelled scan adopts nothing (the re-key is part of the save it never
/// reached); the next scan does.
#[test]
fn a_cancelled_scan_adopts_nothing() {
    let w = World::new(4);
    w.scan(None);
    let old_path = w.path_of(&dir_name(0));
    let old_id = w.id_at(&old_path).unwrap();
    add_dependents(&w.conn(), &old_id, &old_path);
    if !respell(&w.lib, &dir_name(0), &respelled(0)) {
        return;
    }
    w.take_events();
    let before = w.dump();
    let svc = ScanService::new();
    let svc2 = svc.clone();
    let hook: AlbumHook = Arc::new(move |_album, _fresh| {
        let _ = svc2.cancel();
    });
    let StartScan::Started(_, job) = svc.start_scan(&w.lib_s(), false, w.deps(Some(hook))) else {
        panic!()
    };
    job.run();
    assert_eq!(svc.state().status.as_str(), "cancelled");
    assert!(!names(&svc).contains(&"adopted"));
    assert_eq!(w.dump(), before, "a cancelled scan writes nothing");
    assert_eq!(types(&w.take_events()), ["LibraryScanStopped"]);

    w.scan(None);
    let new_id = w.id_at(&w.path_of(&respelled(0))).unwrap();
    assert_eq!(dependents(&w.conn(), &new_id), all_dependents());
    assert_eq!(
        types(&w.take_events()),
        ["AlbumRenamed", "LibraryScanCompleted"]
    );
}

/// The save re-checks what the database says: an adoption whose row went
/// Missing (or whose new id was taken) after the plan writes nothing.
#[test]
fn a_stale_adoption_saves_nothing() {
    let w = World::new(2);
    w.scan(None);
    let old_path = w.path_of(&dir_name(0));
    let old_id = w.id_at(&old_path).unwrap();
    if !respell(&w.lib, &dir_name(0), &respelled(0)) {
        return;
    }
    let new_path = w.path_of(&respelled(0));
    let album = musaic_scan::scan_album(Path::new(&new_path), TrackOrder::DiscTrack).unwrap();
    let ad = musaic_db::albums::Adoption {
        old_id: old_id.clone(),
        old_path: old_path.clone(),
        new_id: album.id.clone(),
        new_path: new_path.clone(),
        new_dir_name: album.dir_name.clone(),
    };
    let mut c = w.conn();
    musaic_db::albums::mark_album_missing(&c, &old_id, "t").unwrap();
    let before = w.dump();
    let lib = w.lib_s();
    let err = musaic_db::albums::save_snapshot_adopting(
        &mut c,
        &lib,
        "t2",
        "t2",
        std::slice::from_ref(&album),
        std::slice::from_ref(&ad),
    )
    .unwrap_err();
    assert!(
        matches!(err, musaic_db::Error::AdoptionStale { .. }),
        "{err}"
    );
    assert_eq!(w.dump(), before);

    // Present again, but the new path got a row of its own meanwhile.
    musaic_db::albums::clear_album_missing(&c, &old_id).unwrap();
    musaic_db::albums::upsert_album(&mut c, &lib, "t", &album).unwrap();
    let before = w.dump();
    let err = musaic_db::albums::save_snapshot_adopting(
        &mut c,
        &lib,
        "t2",
        "t2",
        std::slice::from_ref(&album),
        std::slice::from_ref(&ad),
    )
    .unwrap_err();
    assert!(
        matches!(err, musaic_db::Error::AdoptionStale { .. }),
        "{err}"
    );
    assert_eq!(w.dump(), before);
}

/// A scan of an unchanged library against the database the previous scan
/// wrote keeps every id and adopts nothing.
#[test]
fn an_unchanged_synthetic_library_keeps_every_id() {
    let lib = musaic_testdata::synthetic_library();
    let td = tempfile::tempdir().unwrap();
    let db = fresh_db(&musaic_testdata::canonical(td.path()));
    let (bus, events) = recording_bus();
    let deps = ScanDeps {
        open_db: {
            let db = db.clone();
            Arc::new(move || Ok(connect(&db)))
        },
        bus,
        clock: Arc::new(|| "2030-01-01T00:00:00Z".to_string()),
        track_order: TrackOrder::DiscTrack,
        after_scan: None,
        on_album: None,
        on_worker_done: None,
        log: None,
    };
    let lib_s = lib.to_string_lossy().into_owned();
    let ids = || -> Vec<(String, String)> {
        let c = connect(&db);
        let mut st = c
            .prepare("SELECT id, path FROM albums ORDER BY id")
            .unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let run = || {
        let svc = ScanService::new();
        let StartScan::Started(_, job) = svc.start_scan(&lib_s, false, deps.clone()) else {
            panic!()
        };
        job.run();
        assert_eq!(svc.state().status.as_str(), "done");
        svc
    };
    run();
    let first = ids();
    assert!(!first.is_empty());
    events.lock().unwrap().clear();
    let svc = run();
    assert_eq!(ids(), first);
    assert!(!names(&svc).contains(&"adopted"));
    assert!(!names(&svc).contains(&"marked_missing"));
    let ev: Vec<Value> = events
        .lock()
        .unwrap()
        .iter()
        .map(common::event_json)
        .collect();
    assert_eq!(types(&ev), ["LibraryScanCompleted"]);
}

/// A row stored in NFC for a folder now listed in NFD (as a rename made
/// before the app read back the listed spelling left it). APFS looks names
/// up regardless of normalisation, so the NFC spelling opens the folder and
/// the row is adopted; ext4 and NTFS keep both spellings apart, so the NFC
/// one does not open and nothing is adopted (Missing plus a new row, as
/// before).
#[test]
fn an_nfc_row_and_an_nfd_folder() {
    let nfc = "Caf\u{e9} Society - Blue [FLAC]";
    let nfd = "Cafe\u{301} Society - Blue [FLAC]";
    let w = World::new(1);
    build_named(&w.lib, nfc, "Caf\u{e9} Society", "Blue");
    w.scan(None);
    let old_id = w.id_at(&w.path_of(nfc)).unwrap();
    std::fs::rename(w.lib.join(nfc), w.lib.join(nfd)).unwrap();
    w.take_events();
    let svc = w.scan(None);
    let ev = types(&w.take_events());
    let new_id = musaic_core::album_id(&w.path_of(nfd));
    if cfg!(target_os = "macos") {
        if !w.lib.join(nfc).is_dir() {
            eprintln!("skipped: this volume tells NFC and NFD apart");
            return;
        }
        assert_eq!(ev, ["AlbumRenamed", "LibraryScanCompleted"]);
        assert!(names(&svc).contains(&"adopted"));
        assert_eq!(w.id_at(&w.path_of(nfd)).as_deref(), Some(new_id.as_str()));
        assert_eq!(w.album_count(), 2);
    } else {
        assert!(!w.lib.join(nfc).exists());
        assert_eq!(ev, ["AlbumMarkedMissing", "LibraryScanCompleted"]);
        assert!(!names(&svc).contains(&"adopted"));
        assert!(w.missing_since(&old_id).is_some());
        assert_eq!(w.missing_since(&new_id), None);
        assert_eq!(w.album_count(), 3);
    }
}

/// On Linux a case variant is another name: the old spelling does not open
/// the folder, so nothing is adopted.
#[cfg(target_os = "linux")]
#[test]
fn a_case_variant_on_linux_is_another_folder() {
    let w = World::new(2);
    w.scan(None);
    let old_id = w.id_at(&w.path_of(&dir_name(0))).unwrap();
    std::fs::rename(w.lib.join(dir_name(0)), w.lib.join(respelled(0))).unwrap();
    w.take_events();
    let svc = w.scan(None);
    assert!(!names(&svc).contains(&"adopted"));
    assert_eq!(
        types(&w.take_events()),
        ["AlbumMarkedMissing", "LibraryScanCompleted"]
    );
    assert!(w.missing_since(&old_id).is_some());
    assert_eq!(w.album_count(), 3);
}

/// An HFS+ disk image mounted under a temp dir, detached when dropped.
#[cfg(target_os = "macos")]
struct HfsVolume {
    mount: PathBuf,
    _dir: tempfile::TempDir,
}

#[cfg(target_os = "macos")]
impl HfsVolume {
    fn new() -> HfsVolume {
        use std::process::Command;
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("volume.dmg");
        let mount = dir.path().join("mnt");
        std::fs::create_dir(&mount).unwrap();
        let run = |cmd: &mut Command| {
            let out = cmd.output().expect("hdiutil");
            assert!(
                out.status.success(),
                "{cmd:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(Command::new("hdiutil")
            .args(["create", "-quiet", "-fs", "HFS+", "-size", "20m"])
            .args(["-volname", "musaic-test"])
            .arg(&image));
        run(Command::new("hdiutil")
            .args(["attach", "-quiet", "-nobrowse", "-mountpoint"])
            .arg(&mount)
            .arg(&image));
        HfsVolume { mount, _dir: dir }
    }
}

#[cfg(target_os = "macos")]
impl Drop for HfsVolume {
    fn drop(&mut self) {
        use std::process::Command;
        let detach = |force: bool| {
            let mut cmd = Command::new("hdiutil");
            cmd.args(["detach", "-quiet"]).arg(&self.mount);
            if force {
                cmd.arg("-force");
            }
            cmd.status().is_ok_and(|s| s.success())
        };
        if !detach(false) && !detach(true) {
            eprintln!("could not detach {}", self.mount.display());
        }
    }
}

/// A real HFS+ volume lists every name decomposed. A row stored under the
/// composed spelling (as renames before the app read back the listed
/// spelling stored it) is adopted into the listed one with everything on
/// it. Needs `hdiutil` (no root); run by an explicit CI step on macOS.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "mounts an HFS+ disk image; run with --ignored on macOS"]
fn an_hfs_plus_volume_adopts_the_composed_row() {
    let volume = HfsVolume::new();
    let tmp = tempfile::tempdir().unwrap();
    let lib = musaic_testdata::canonical(&volume.mount).join("lib");
    std::fs::create_dir(&lib).unwrap();
    let nfc = "Caf\u{e9} Society - Blue [FLAC]";
    let nfd = "Cafe\u{301} Society - Blue [FLAC]";
    build_named(&lib, nfc, "Caf\u{e9} Society", "Blue");
    let listed: Vec<String> = std::fs::read_dir(&lib)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(listed, [nfd], "HFS+ lists the name decomposed");
    let lib_s = lib.to_string_lossy().into_owned();
    let nfc_path = lib.join(nfc).to_string_lossy().into_owned();
    let nfd_path = lib.join(nfd).to_string_lossy().into_owned();

    // The row as an older rename stored it: composed id and path.
    let db = fresh_db(tmp.path());
    let mut album = musaic_scan::scan_album(&lib.join(nfd), TrackOrder::DiscTrack).unwrap();
    assert_eq!(album.path, nfd_path);
    album.id = musaic_core::album_id(&nfc_path);
    album.path = nfc_path.clone();
    album.dir_name = nfc.to_string();
    {
        let mut c = connect(&db);
        musaic_db::albums::save_snapshot(&mut c, &lib_s, "t", "t", std::slice::from_ref(&album))
            .unwrap();
        add_dependents(&c, &album.id, &nfc_path);
    }

    let (bus, events) = recording_bus();
    let deps = ScanDeps {
        open_db: {
            let db = db.clone();
            Arc::new(move || Ok(connect(&db)))
        },
        bus,
        clock: Arc::new(|| "2030-01-01T00:00:00Z".to_string()),
        track_order: TrackOrder::DiscTrack,
        after_scan: None,
        on_album: None,
        on_worker_done: None,
        log: None,
    };
    let svc = ScanService::new();
    let StartScan::Started(_, job) = svc.start_scan(&lib_s, false, deps) else {
        panic!()
    };
    job.run();
    assert_eq!(svc.state().status.as_str(), "done");
    let ev: Vec<Value> = events
        .lock()
        .unwrap()
        .iter()
        .map(common::event_json)
        .collect();
    assert_eq!(types(&ev), ["AlbumRenamed", "LibraryScanCompleted"]);
    let c = connect(&db);
    let new_id = musaic_core::album_id(&nfd_path);
    let rows: Vec<(String, String, Option<String>)> = c
        .prepare("SELECT id, path, missing_since FROM albums")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(rows, [(new_id.clone(), nfd_path, None)]);
    assert_eq!(dependents(&c, &new_id), all_dependents());
    assert_eq!(foreign_key_violations(&c), 0);
    drop(volume);
}
