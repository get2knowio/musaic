//! Cancelling an agent's check run or analysis part-way (#349): the check
//! stops before its next album and the analysis before its next file. Each
//! test holds the work at a known item (a resolver or an analyzer that waits
//! at a door on its second call), cancels while it is held, then lets it go:
//! the progress is observed, not timed.

mod common;

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use common::{AgentWorld, FakeClock, app_quiet_pool, configure, wait_for_fingerprints};
use musaic_app::AppBuilder;
use musaic_app::ops::agent::Principal;
use musaic_checks::{Resolution, ResolutionSource};
use musaic_commands::{Decision, PermissionKind as K, ToolError, ToolOutput};
use musaic_events::{Event, EventKind, Subscriber, Topic};
use musaic_secrets::MemoryStore;
use rusqlite::Connection;
use serde_json::{Value, json};

/// A held door: the thread that reaches `wait` blocks until `open`.
#[derive(Default)]
struct Door {
    open: Mutex<bool>,
    cv: Condvar,
    entered: AtomicUsize,
}

impl Door {
    fn wait(&self) {
        self.entered.fetch_add(1, Ordering::SeqCst);
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.cv.wait(open).unwrap();
        }
    }
    fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.cv.notify_all();
    }
}

/// Counts its calls; the second one waits at the door.
#[derive(Default)]
struct HeldSecond {
    calls: AtomicUsize,
    door: Door,
}

impl HeldSecond {
    fn step(&self) {
        if self.calls.fetch_add(1, Ordering::SeqCst) + 1 == 2 {
            self.door.wait();
        }
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

struct HeldResolver(Arc<HeldSecond>);

impl ResolutionSource for HeldResolver {
    fn resolve(&self, _conn: &Connection, _album_id: &str) -> Result<Resolution, String> {
        self.0.step();
        Ok(Resolution::default())
    }
}

struct HeldAnalyzer(Arc<HeldSecond>);

impl musaic_services::analyze::Analyzer for HeldAnalyzer {
    fn bpm(&self, _path: &Path) -> Result<f64, String> {
        self.0.step();
        Ok(120.0)
    }
    fn loudness(&self, _path: &Path) -> Result<(f64, f64), String> {
        self.0.step();
        Ok((-14.0, 0.5))
    }
}

/// A scanned agent world built with `more`, and a principal allowed to scan,
/// analyse and write tags.
fn world(more: impl FnOnce(AppBuilder) -> AppBuilder + 'static) -> (AgentWorld, Principal) {
    let clock = FakeClock::new();
    let store = Arc::new(MemoryStore::new());
    let (c2, s2) = (clock.clone(), store.clone());
    let (app, scratch) = app_quiet_pool(move |b| more(b.clock(c2.wall()).agent_secrets(s2)));
    let w = AgentWorld {
        app,
        scratch,
        clock,
        store,
    };
    configure(&w.app, &w.scratch);
    assert!(w.app.ops().scan().subscribe(false).unwrap().count() > 2);
    eventually("the scan to let go of the gate", || {
        w.app.gate().snapshot() == (0, false)
    });
    wait_for_fingerprints(&w.app);
    let kinds = [
        K::LibraryRead,
        K::ComputeScan,
        K::ComputeAnalyze,
        K::TagsWrite,
    ];
    let id = w.pair("cancel", &kinds).pairing.id;
    for k in [K::ComputeScan, K::ComputeAnalyze, K::TagsWrite] {
        w.app
            .ops()
            .agent()
            .set_policy(&id, &[(k, Decision::Allow)])
            .unwrap();
    }
    let p = w.login(&id);
    (w, p)
}

fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for {what}");
}

fn out_json(r: Result<ToolOutput, ToolError>) -> Value {
    r.unwrap_or_else(|e| panic!("{e}")).to_json()
}

fn finished(w: &AgentWorld, p: &Principal, op: &str) -> Value {
    let mut last = json!(null);
    eventually("the operation to finish", || {
        last = out_json(w.call(p, "musaic_operation_get", json!({"operationId": op})));
        !matches!(
            last["state"].as_str(),
            Some("running") | Some("awaiting_approval")
        )
    });
    last
}

/// Every event of `kind` published from now on.
fn record(w: &AgentWorld, kind: EventKind) -> Arc<Mutex<Vec<Event>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    w.app.bus().register(
        Topic::Kind(kind),
        Subscriber::from_fn(move |e| {
            s.lock().unwrap().push(e.clone());
            Ok(())
        })
        .key("agent_cancel_test")
        .priority(500)
        .name("agent_cancel_test"),
    );
    seen
}

/// Hold the work at its second item, cancel through the tool, let it go, and
/// return the finished operation.
fn cancel_at_second(w: &AgentWorld, p: &Principal, held: &HeldSecond, op: &str) -> Value {
    eventually("the work to reach its second item", || {
        held.door.entered.load(Ordering::SeqCst) == 1
    });
    let c = out_json(w.call(p, "musaic_operation_cancel", json!({"operationId": op})));
    assert_eq!(
        (c["cancellation"].as_str(), c["state"].as_str()),
        (Some("requested"), Some("running"))
    );
    held.door.open();
    finished(w, p, op)
}

const CHECK: &str = "tags-out-of-sync";

#[test]
fn a_check_run_stops_before_its_next_album_and_records_no_inputs() {
    let held = Arc::new(HeldSecond::default());
    let h2 = held.clone();
    let (w, p) = world(move |b| b.checks_resolution(Arc::new(HeldResolver(h2))));
    let conn = musaic_db::open_connection(w.app.db_path()).unwrap();
    // Anchored, so the check asks the resolver once per album as it goes.
    let anchored = conn
        .execute(
            "UPDATE albums SET mb_release_group_mbid = 'rg-' || id WHERE missing_since IS NULL",
            [],
        )
        .unwrap();
    assert!(anchored > 2, "enough albums to stop part-way");
    let reruns = record(&w, EventKind::ChecksRerunFinished);

    let h = out_json(w.call(
        &p,
        "musaic_checks_start",
        json!({"checkIds": [CHECK], "idempotencyKey": "key-cancel-check-1"}),
    ));
    let op = h["operationId"].as_str().unwrap().to_string();
    let done = cancel_at_second(&w, &p, &held, &op);

    assert_eq!(done["state"], "cancelled", "{done}");
    assert_eq!(
        held.calls(),
        2,
        "the album in hand was finished and no further one was started"
    );
    assert_eq!(done["counts"], json!({"done": 0, "total": 1, "failed": 0}));
    // The partial count: the findings written for the two albums it saw.
    let open: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM check_findings WHERE check_id = ?1 AND resolved_at IS NULL",
            [CHECK],
            |r| r.get(0),
        )
        .unwrap();
    assert!(open <= 2);
    assert_eq!(
        done["result"]["checks"],
        json!([{"checkId": CHECK, "findings": open, "failed": true}])
    );
    let (status, error): (String, Option<String>) = conn
        .query_row(
            "SELECT status, error FROM check_runs WHERE check_id = ?1 ORDER BY id DESC LIMIT 1",
            [CHECK],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (status.as_str(), error.as_deref()),
        ("failed", Some("check run cancelled"))
    );
    // No album's inputs recorded (not the two it saw, not the ones it did
    // not reach): the background sweep runs the check again for all of them.
    let inputs: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM check_inputs WHERE check_id = ?1",
            [CHECK],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(inputs, 0);
    // The batch is still reported, with the check among the failed runs.
    let got = reruns.lock().unwrap().clone();
    assert_eq!(got.len(), 1, "{got:?}");
    let Event::ChecksRerunFinished { checks, failed, .. } = &got[0] else {
        unreachable!()
    };
    assert_eq!(checks, &[CHECK.to_string()]);
    assert_eq!(failed, &[CHECK.to_string()]);
    // The worker records the outcome just after it ends the operation.
    eventually("the cancelled outcome in the activity", || {
        let act = w.app.ops().agent().activity(&p.pairing_id).unwrap();
        act.iter()
            .any(|a| a.tool == "musaic_checks_start" && a.outcome == "cancelled")
    });
}

/// An album of the library with at least three FLACs: (id, folder, files).
fn album_with_three_files(w: &AgentWorld) -> (String, String, Vec<std::path::PathBuf>) {
    w.app
        .ops()
        .library()
        .list_albums(false)
        .unwrap()
        .albums
        .into_iter()
        .map(|a| {
            let files = musaic_scan::list_flacs(Path::new(a.path()));
            (a.id().to_string(), a.path().to_string(), files)
        })
        .find(|(_, _, files)| files.len() >= 3)
        .expect("an album with three files")
}

fn tag(path: &Path, key: &str) -> Option<String> {
    musaic_tags::FlacFile::open(path)
        .unwrap()
        .tags()
        .and_then(|t| t.first(key).map(str::to_string))
}

fn tags_changed_for(events: &Mutex<Vec<Event>>, album: &str) -> Vec<Vec<String>> {
    events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            Event::AlbumTagsChanged { album_id, fields } if album_id == album => {
                Some(fields.clone())
            }
            _ => None,
        })
        .collect()
}

#[test]
fn an_analysis_stops_before_its_next_file_and_publishes_for_the_files_it_wrote() {
    let held = Arc::new(HeldSecond::default());
    let h2 = held.clone();
    let (w, p) = world(move |b| b.analyzer(Arc::new(HeldAnalyzer(h2))));
    let (album, _, files) = album_with_three_files(&w);
    let before: Vec<Option<String>> = files.iter().map(|f| tag(f, "bpm")).collect();
    let changed = record(&w, EventKind::AlbumTagsChanged);

    let h = out_json(w.call(
        &p,
        "musaic_analysis_start",
        json!({"albumIds": [album], "kinds": ["bpm"], "idempotencyKey": "key-cancel-bpm-01"}),
    ));
    let op = h["operationId"].as_str().unwrap().to_string();
    let done = cancel_at_second(&w, &p, &held, &op);

    assert_eq!(done["state"], "cancelled", "{done}");
    assert_eq!(
        held.calls(),
        2,
        "the file in hand was finished, no further one started"
    );
    assert_eq!(done["counts"], json!({"done": 0, "total": 1, "failed": 0}));
    assert_eq!(
        done["result"]["analysis"],
        json!([{"albumId": album, "kind": "bpm", "status": "failed"}]),
        "the album was not finished"
    );
    // The two files it got to are written whole; the rest are untouched.
    for (i, f) in files.iter().enumerate() {
        let now = tag(f, "bpm");
        if i < 2 {
            assert_eq!(now.as_deref(), Some("120.00"), "{}", f.display());
        } else {
            assert_eq!(now, before[i], "{} untouched", f.display());
        }
    }
    // The files it wrote are announced, as for a finished analysis.
    assert_eq!(
        tags_changed_for(&changed, &album),
        [vec!["bpm".to_string()]]
    );
}

#[test]
fn a_cancelled_replaygain_writes_track_gains_but_no_album_gain() {
    let held = Arc::new(HeldSecond::default());
    let h2 = held.clone();
    let (w, p) = world(move |b| b.analyzer(Arc::new(HeldAnalyzer(h2))));
    let (album, _, files) = album_with_three_files(&w);
    let album_gain_before: Vec<Option<String>> = files
        .iter()
        .map(|f| tag(f, "replaygain_album_gain"))
        .collect();
    let track_gain_before: Vec<Option<String>> = files
        .iter()
        .map(|f| tag(f, "replaygain_track_gain"))
        .collect();
    let changed = record(&w, EventKind::AlbumTagsChanged);

    let h = out_json(w.call(
        &p,
        "musaic_analysis_start",
        json!({"albumIds": [album], "kinds": ["replaygain"], "idempotencyKey": "key-cancel-rg-001"}),
    ));
    let op = h["operationId"].as_str().unwrap().to_string();
    let done = cancel_at_second(&w, &p, &held, &op);

    assert_eq!(done["state"], "cancelled", "{done}");
    assert_eq!(held.calls(), 2);
    for (i, f) in files.iter().enumerate() {
        if i < 2 {
            assert!(tag(f, "replaygain_track_gain").is_some(), "{}", f.display());
        } else {
            assert_eq!(tag(f, "replaygain_track_gain"), track_gain_before[i]);
        }
        assert_eq!(
            tag(f, "replaygain_album_gain"),
            album_gain_before[i],
            "no album gain from part of the album: {}",
            f.display()
        );
    }
    assert_eq!(
        tags_changed_for(&changed, &album),
        [vec!["replaygain".to_string()]]
    );
}
