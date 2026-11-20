//! Cancelling a library scan (issue #193), and how a scan that saved
//! nothing ends. The Python app could not cancel a scan, so there is no
//! recording: these pin the contract in `src/scan.rs` ("Cancelling") on
//! invented albums.
//!
//! - A cancel at any point before the save writes nothing: the database is
//!   byte-for-byte what the previous scan left (rows, `missing_since`,
//!   `scanned_at`); nothing is marked Missing because of the partial walk.
//! - The scan ends `cancelled` with a `cancelled` event (after `cancelling`),
//!   publishes `LibraryScanStopped(cancelled)`, never `LibraryScanCompleted`,
//!   and does not kick off the fingerprint pool.
//! - Past the commit point a cancel is `too_late` and the scan ends `done`.
//! - A scan started right after a cancelled one runs normally.
//! - A subscriber joining late replays the whole buffer and ends.

#[path = "../../musaic-services/tests/common/mod.rs"]
mod common;
mod scan_wire;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{connect, flac_bytes, fresh_db, recording_bus};
use musaic_events::{Bus, Event, Subscriber, Topic};
use musaic_scan::TrackOrder;
use musaic_services::scan::{
    AlbumHook, CancelScan, Poll, ScanDeps, ScanService, StartScan, discover_album_dirs_until,
};
use serde_json::{Value, json};

/// Invented albums: "<Artist> - <Album> [FLAC]" with two tracks each.
const ALBUMS: &[(&str, &str)] = &[
    ("Velvet Harbor", "Night Tides"),
    ("Paper Lanterns", "Slow Orbit"),
    ("Quiet Meridian", "Salt and Cedar"),
    ("Juniper Static", "Low Frequencies"),
    ("Marble Coast", "Afterhours"),
];

fn dir_name(i: usize) -> String {
    format!("{} - {} [FLAC]", ALBUMS[i].0, ALBUMS[i].1)
}

fn build(lib: &Path, i: usize) {
    let d = lib.join(dir_name(i));
    std::fs::create_dir_all(&d).unwrap();
    for t in 1..=2u32 {
        let tags = [
            ("ARTIST", ALBUMS[i].0.to_string()),
            ("ALBUMARTIST", ALBUMS[i].0.to_string()),
            ("ALBUM", ALBUMS[i].1.to_string()),
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

struct World {
    _td: tempfile::TempDir,
    lib: PathBuf,
    db: PathBuf,
    bus: Arc<Bus>,
    events: Arc<Mutex<Vec<Event>>>,
    after_scan: Arc<AtomicUsize>,
    worker_done: Arc<AtomicUsize>,
    tick: Arc<AtomicUsize>,
}

impl World {
    /// A library with albums 0..4 (album 4 is added by the tests).
    fn new() -> World {
        let td = tempfile::tempdir().unwrap();
        let root = musaic_scan::py_resolve(td.path());
        let lib = root.join("lib");
        std::fs::create_dir(&lib).unwrap();
        for i in 0..4 {
            build(&lib, i);
        }
        let db = fresh_db(&root);
        let (bus, events) = recording_bus();
        World {
            _td: td,
            lib,
            db,
            bus,
            events,
            after_scan: Arc::default(),
            worker_done: Arc::default(),
            tick: Arc::default(),
        }
    }

    fn lib_s(&self) -> String {
        self.lib.to_string_lossy().into_owned()
    }

    /// Every timestamp differs, so a save that should not happen would show
    /// in `scanned_at` / `missing_since`.
    fn deps(&self, on_album: Option<AlbumHook>) -> ScanDeps {
        let db = self.db.clone();
        let after = Arc::clone(&self.after_scan);
        let done = Arc::clone(&self.worker_done);
        let tick = Arc::clone(&self.tick);
        ScanDeps {
            open_db: Arc::new(move || Ok(connect(&db))),
            bus: Arc::clone(&self.bus),
            clock: Arc::new(move || {
                let n = tick.fetch_add(1, Ordering::SeqCst);
                format!("2030-01-01T00:{:02}:{:02}Z", n / 60, n % 60)
            }),
            track_order: TrackOrder::DiscTrack,
            after_scan: Some(Arc::new(move |_lib: &str| {
                after.fetch_add(1, Ordering::SeqCst);
            })),
            on_album,
            on_worker_done: Some(Arc::new(move || {
                done.fetch_add(1, Ordering::SeqCst);
            })),
            log: None,
        }
    }

    fn take_events(&self) -> Vec<Value> {
        let mut l = self.events.lock().unwrap();
        let out = l.iter().map(common::event_json).collect();
        l.clear();
        out
    }

    /// Every album row (all columns) and the libraries table.
    fn dump(&self) -> Value {
        let c = connect(&self.db);
        let rows = |sql: &str| -> Vec<Value> {
            let mut st = c.prepare(sql).unwrap();
            let n = st.column_count();
            st.query_map([], |r| {
                Ok(Value::Array(
                    (0..n)
                        .map(|i| json!(format!("{:?}", r.get_ref(i).unwrap())))
                        .collect(),
                ))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect()
        };
        json!({
            "albums": rows("SELECT * FROM albums ORDER BY rowid"),
            "libraries": rows("SELECT * FROM libraries ORDER BY path"),
            "track_tags": rows("SELECT * FROM track_tags ORDER BY album_id, filename, tag"),
        })
    }

    /// A plain scan to completion.
    fn scan_done(&self, svc: &ScanService) {
        let StartScan::Started(_, job) = svc.start_scan(&self.lib_s(), false, self.deps(None))
        else {
            panic!("a scan was already running")
        };
        job.run();
        assert_eq!(svc.state().status.as_str(), "done");
    }
}

fn names(svc: &ScanService) -> Vec<&'static str> {
    svc.buffer().iter().map(|e| scan_wire::name(e)).collect()
}

/// Drain a subscription to its end (generous deadline per event).
fn drain(svc: &ScanService, from: u64) -> Vec<&'static str> {
    let mut sub = svc.subscribe(from);
    let mut got = Vec::new();
    loop {
        match sub.next_timeout(Duration::from_secs(30)) {
            Poll::Event(e) => got.push(scan_wire::name(&e)),
            Poll::Finished => return got,
            Poll::Pending => panic!("subscriber stalled after {got:?}"),
        }
    }
}

/// Cancel in the middle of the per-album work (from the album hook, so on
/// the scan thread, deterministically after the first album): two albums
/// vanished and one is new, yet nothing is written and nobody goes Missing.
/// The next scan then runs normally and does the work.
#[test]
fn cancel_during_albums_writes_nothing_and_the_next_scan_runs() {
    let w = World::new();
    let svc = ScanService::new();
    w.scan_done(&svc);
    w.take_events();
    assert_eq!(w.after_scan.load(Ordering::SeqCst), 1);
    let before = w.dump();

    std::fs::remove_dir_all(w.lib.join(dir_name(1))).unwrap();
    std::fs::remove_dir_all(w.lib.join(dir_name(2))).unwrap();
    build(&w.lib, 4);

    let svc2 = svc.clone();
    let outcome: Arc<Mutex<Option<CancelScan>>> = Arc::default();
    let o2 = Arc::clone(&outcome);
    let hook: AlbumHook = Arc::new(move |_album, _fresh| {
        let mut o = o2.lock().unwrap();
        if o.is_none() {
            *o = Some(svc2.cancel());
            // While the scan still runs, it reports `cancelling`.
            assert_eq!(
                scan_wire::state(&svc2.state())
                    .get("status")
                    .unwrap()
                    .as_str(),
                Some("cancelling")
            );
            assert!(svc2.is_running());
        }
    });
    let StartScan::Started(_, job) = svc.start_scan(&w.lib_s(), false, w.deps(Some(hook))) else {
        panic!()
    };
    // A live subscriber from the start sees the whole scan, then ends.
    let live = {
        let svc = svc.clone();
        std::thread::spawn(move || drain(&svc, 0))
    };
    job.run();
    let live = live.join().unwrap();

    assert!(matches!(
        outcome.lock().unwrap().as_ref(),
        Some(CancelScan::Requested(_))
    ));
    let st = svc.state();
    assert_eq!(st.status.as_str(), "cancelled");
    assert_eq!(
        scan_wire::state(&st).get("status").unwrap().as_str(),
        Some("cancelled")
    );
    assert_eq!(st.scanned, 1, "the album being read is finished, no more");
    assert_eq!(st.removed_count, 0);
    assert!(st.completed_at.is_some());
    assert_eq!(st.error, None);
    assert!(!svc.is_running());

    assert_eq!(
        names(&svc),
        ["start", "album", "progress", "cancelling", "cancelled"]
    );
    assert_eq!(live, names(&svc), "the live subscriber saw every event");
    let last = scan_wire::data(svc.buffer().last().unwrap()).to_value();
    assert_eq!(last["reason"], "cancelled");
    assert_eq!(last["phase"], "albums");
    assert_eq!(last["scanned"], 1);
    assert_eq!(last["total"], 3);
    assert!(
        last["detail"]
            .as_str()
            .unwrap()
            .contains("Nothing was saved"),
        "{last}"
    );

    assert_eq!(w.dump(), before, "a cancelled scan writes nothing");
    assert_eq!(
        w.take_events(),
        [json!({"type": "LibraryScanStopped", "reason": "cancelled", "album_count": 1})],
        "no completion, no Missing transitions"
    );
    assert_eq!(w.after_scan.load(Ordering::SeqCst), 1, "no pool kick-off");
    assert_eq!(
        w.worker_done.load(Ordering::SeqCst),
        2,
        "thumbnails drained"
    );

    // A late subscriber replays the cancelled scan and ends.
    assert_eq!(drain(&svc, 0), names(&svc));
    // Cancelling a finished scan changes nothing.
    assert!(matches!(svc.cancel(), CancelScan::NotRunning(_)));
    assert_eq!(svc.state().status.as_str(), "cancelled");

    // A scan started right after runs normally and does what was skipped.
    let StartScan::Started(state, job) = svc.start_scan(&w.lib_s(), false, w.deps(None)) else {
        panic!("the cancelled scan still counts as running")
    };
    assert_eq!(state.status.as_str(), "running");
    assert!(!state.cancel_requested);
    job.run();
    assert_eq!(svc.state().status.as_str(), "done");
    assert!(!names(&svc).contains(&"cancelled"));
    let ev = w.take_events();
    let types: Vec<&str> = ev.iter().map(|e| e["type"].as_str().unwrap()).collect();
    assert_eq!(
        types,
        [
            "AlbumMarkedMissing",
            "AlbumMarkedMissing",
            "LibraryScanCompleted"
        ]
    );
    assert_eq!(w.after_scan.load(Ordering::SeqCst), 2);
    let c = connect(&w.db);
    let missing: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM albums WHERE missing_since IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let total: i64 = c
        .query_row("SELECT COUNT(*) FROM albums", [], |r| r.get(0))
        .unwrap();
    assert_eq!((missing, total), (2, 5));
}

/// A cancel accepted before the job ran (the discovery phase): no `start`,
/// no database connection needed, nothing written; a second cancel is
/// `already_requested` and adds no event.
#[test]
fn cancel_before_discovery() {
    let w = World::new();
    let svc = ScanService::new();
    w.scan_done(&svc);
    w.take_events();
    let before = w.dump();
    let StartScan::Started(_, job) = svc.start_scan(&w.lib_s(), false, w.deps(None)) else {
        panic!()
    };
    let CancelScan::Requested(s) = svc.cancel() else {
        panic!("cancel refused")
    };
    assert_eq!(
        scan_wire::state(&s).get("status").unwrap().as_str(),
        Some("cancelling")
    );
    assert!(matches!(svc.cancel(), CancelScan::AlreadyRequested(_)));
    // A start while it is stopping attaches to it, as to any running scan.
    assert!(matches!(
        svc.start_scan(&w.lib_s(), true, w.deps(None)),
        StartScan::AlreadyRunning(_)
    ));
    job.run();
    assert_eq!(names(&svc), ["cancelling", "cancelled"]);
    let last = scan_wire::data(svc.buffer().last().unwrap()).to_value();
    assert_eq!(last["phase"], "discovery");
    assert_eq!(last["scanned"], 0);
    assert_eq!(svc.state().status.as_str(), "cancelled");
    assert_eq!(w.dump(), before);
    assert_eq!(
        w.take_events(),
        [json!({"type": "LibraryScanStopped", "reason": "cancelled", "album_count": 0})]
    );
    assert_eq!(w.after_scan.load(Ordering::SeqCst), 1);
}

/// The discovery walk stops at the next directory once asked.
#[test]
fn discovery_walk_stops_at_the_next_directory() {
    let w = World::new();
    let listed = AtomicUsize::new(0);
    let list = |d: &Path| {
        listed.fetch_add(1, Ordering::SeqCst);
        std::fs::read_dir(d)
            .ok()
            .map(|rd| rd.flatten().map(|e| e.file_name()).collect())
    };
    let all = discover_album_dirs_until(&w.lib, &list, &|| false).unwrap();
    assert_eq!(all.len(), 4);
    let walked = listed.swap(0, Ordering::SeqCst);
    assert_eq!(walked, 5, "the root and four albums");
    // Stop once two directories were listed.
    let stopped = discover_album_dirs_until(&w.lib, &list, &|| listed.load(Ordering::SeqCst) >= 2);
    assert_eq!(stopped, None);
    assert_eq!(listed.load(Ordering::SeqCst), 2);
}

/// Past the commit point (the save has begun) a cancel is refused as
/// `too_late`: the scan ends `done`, saves, announces completion and kicks
/// off the pool. The cancel is attempted from a subscriber of the Missing
/// transition, which the scan publishes right after its commit.
#[test]
fn cancel_after_the_commit_point_is_too_late() {
    let w = World::new();
    let svc = ScanService::new();
    w.scan_done(&svc);
    w.take_events();
    std::fs::remove_dir_all(w.lib.join(dir_name(3))).unwrap();
    let svc2 = svc.clone();
    let seen: Arc<Mutex<Vec<&'static str>>> = Arc::default();
    let s2 = Arc::clone(&seen);
    w.bus.register(
        Topic::Kind(musaic_events::EventKind::AlbumMarkedMissing),
        Subscriber::from_fn(move |_| {
            s2.lock().unwrap().push(svc2.cancel().as_str());
            Ok(())
        })
        .name("late_cancel"),
    );
    w.scan_done(&svc);
    assert_eq!(*seen.lock().unwrap(), ["too_late"]);
    assert_eq!(names(&svc).last(), Some(&"done"));
    assert!(!names(&svc).contains(&"cancelling"));
    let types: Vec<String> = w
        .take_events()
        .iter()
        .map(|e| e["type"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(types, ["AlbumMarkedMissing", "LibraryScanCompleted"]);
    assert_eq!(w.after_scan.load(Ordering::SeqCst), 2);
}

/// Nothing to cancel on a fresh service.
#[test]
fn cancel_with_no_scan_is_not_running() {
    let svc = ScanService::new();
    let out = svc.cancel();
    assert_eq!(out.as_str(), "not_running");
    assert_eq!(out.state().status.as_str(), "idle");
    assert!(svc.buffer().is_empty(), "no event for a refused cancel");
    assert!(svc.wait_until_stopped(Duration::from_millis(1)));
}

/// `wait_until_stopped` returns once a cancelled scan running on another
/// thread has ended (the scan is parked in its first album until the
/// cancel arrives, so the cancel lands mid-scan).
#[test]
fn wait_until_stopped_after_a_cancel_from_another_thread() {
    let w = World::new();
    let svc = ScanService::new();
    let svc2 = svc.clone();
    let hook: AlbumHook = Arc::new(move |_album, _fresh| {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !svc2.state().cancel_requested {
            assert!(std::time::Instant::now() < deadline, "no cancel arrived");
            std::thread::sleep(Duration::from_millis(2));
        }
    });
    let StartScan::Started(_, job) = svc.start_scan(&w.lib_s(), false, w.deps(Some(hook))) else {
        panic!()
    };
    let t = std::thread::spawn(move || job.run());
    // Wait for the first album to be under way.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while svc.state().scanned == 0 {
        assert!(std::time::Instant::now() < deadline, "scan never started");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(matches!(svc.cancel(), CancelScan::Requested(_)));
    assert!(svc.wait_until_stopped(Duration::from_secs(30)));
    t.join().unwrap();
    assert_eq!(svc.state().status.as_str(), "cancelled");
    assert_eq!(svc.state().scanned, 1);
    // A first scan cancelled: the database has no album and no scan stamp.
    let c = connect(&w.db);
    let n: i64 = c
        .query_row("SELECT COUNT(*) FROM albums", [], |r| r.get(0))
        .unwrap();
    let libs: i64 = c
        .query_row("SELECT COUNT(*) FROM libraries", [], |r| r.get(0))
        .unwrap();
    assert_eq!((n, libs), (0, 0));
}
