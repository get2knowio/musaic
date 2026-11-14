//! The fingerprint pool behind `GET /api/fingerprint-pool/status` and
//! `/stream`, its start after every scan, its stop on restore and shutdown,
//! and the scan job's panic recovery.
//!
//! Compared with testdata/golden/fingerprint/pool.json (the Python app's own
//! `fingerprint_pool` module and stream handler)
//! and the Python app source it records (musaic/services/scan.py:234-236 for
//! "stop, then warm, after a scan that did not fail"; musaic/main.py:76-84
//! for the stop on shutdown; musaic/api/library.py:161-193 for the stream).
//! One wake-up per connection (enumerated deltas below).

mod common;

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use common::{Sandbox, call, golden as api_golden};
use http_body_util::BodyExt;
use musaic_api::{AppState, Fingerprinting, Network, router};
use musaic_services::fingerprint::{CancelToken, Fingerprinter};
use musaic_services::fingerprint_pool::{KEEPALIVE_FRAME, PoolSleeper, PoolStatus};
use regex::Regex;
use serde_json::{Value, json};
use tower::ServiceExt;

fn pool_golden() -> Value {
    let p = musaic_testdata::public("golden/fingerprint/pool.json");
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

/// A fingerprinter that answers every file with the same print.
struct Fixed;

impl Fingerprinter for Fixed {
    fn fingerprint(&self, _path: &Path) -> Result<(String, u64), String> {
        Ok(("AQAAstub".into(), 3))
    }
    fn implementation(&self) -> &str {
        musaic_db::FINGERPRINT_IMPL_RUST
    }
}

/// No pause between albums.
struct NoSleep;

impl PoolSleeper for NoSleep {
    fn sleep(&self, _d: Duration, cancel: &CancelToken) -> bool {
        !cancel.is_cancelled()
    }
}

/// Parks in the first pause until cancelled.
#[derive(Default)]
struct Park {
    entered: AtomicUsize,
}

impl PoolSleeper for Park {
    fn sleep(&self, _d: Duration, cancel: &CancelToken) -> bool {
        self.entered.fetch_add(1, Ordering::SeqCst);
        while !cancel.is_cancelled() {
            std::thread::sleep(Duration::from_millis(2));
        }
        false
    }
}

fn base(sb: &Sandbox) -> musaic_api::AppStateBuilder {
    AppState::builder(&sb.cfg)
        .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
        .network(Network::Offline)
        .frontend_dir(None)
        .subscribers(false)
        .auto_snapshot(false)
        .ping_interval(Duration::from_millis(300))
}

async fn configure(app: &axum::Router, sb: &Sandbox) {
    let r = call(
        app,
        "POST",
        "/api/config",
        Some(&json!({"library_dir": Sandbox::s(&sb.lib)})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
}

async fn scan(app: &axum::Router) -> String {
    let r = call(app, "GET", "/api/scan/stream", None).await;
    assert_eq!(r.status, 200);
    r.text()
}

async fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..1000 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("timed out waiting for {what}");
}

/// An open SSE response, read frame by frame.
struct Stream {
    body: Body,
    buf: String,
}

impl Stream {
    async fn open(app: &axum::Router, path: &str) -> (Stream, Vec<(String, String)>) {
        let resp = app.clone().oneshot(common::get(path)).await.unwrap();
        assert_eq!(resp.status(), 200);
        let headers = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap().to_string()))
            .collect();
        (
            Stream {
                body: resp.into_body(),
                buf: String::new(),
            },
            headers,
        )
    }

    /// The next `\n\n`-terminated frame, or `None` after `wait`.
    async fn next(&mut self, wait: Duration) -> Option<String> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            if let Some(i) = self.buf.find("\n\n") {
                let f = self.buf[..i + 2].to_string();
                self.buf.drain(..i + 2);
                return Some(f);
            }
            let left = deadline.checked_duration_since(tokio::time::Instant::now())?;
            match tokio::time::timeout(left, self.body.frame()).await {
                Ok(Some(Ok(f))) => {
                    if let Ok(d) = f.into_data() {
                        self.buf.push_str(&String::from_utf8_lossy(&d));
                    }
                }
                _ => return None,
            }
        }
    }
}

/// ISO timestamps and the library path, normalised on both sides.
fn norm_frame(f: &str, lib: &str) -> String {
    let iso = Regex::new(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ").unwrap();
    iso.replace_all(&f.replace(lib, "<LIB>"), "<NOW>")
        .into_owned()
}

// The idle pool (no scan yet): status route body and key
// order, the stream's first frame and headers, as the Python app's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn idle_pool_status_and_first_frame() {
    let g = pool_golden();
    let sb = Sandbox::new();
    let app = router(base(&sb).build().unwrap());
    let r = call(&app, "GET", "/api/fingerprint-pool/status", None).await;
    assert_eq!(r.status, 200);
    assert_eq!(r.json(), g["idle"]["status_route"]);
    let keys: Vec<String> = Regex::new(r#""([a-z_]+)":"#)
        .unwrap()
        .captures_iter(&r.text())
        .map(|c| c[1].to_string())
        .collect();
    assert_eq!(json!(keys), g["keys_in_order"]);
    let (mut s, headers) = Stream::open(&app, "/api/fingerprint-pool/stream").await;
    for (k, v) in g["idle"]["stream_headers"].as_object().unwrap() {
        let got = headers
            .iter()
            .find(|(h, _)| h == k)
            .map(|(_, x)| x.as_str());
        assert_eq!(got, v.as_str(), "header {k}");
    }
    let first = s.next(Duration::from_secs(5)).await.unwrap();
    assert_eq!(json!(first), g["idle"]["stream_first_frame"]);
}

// After a scan the
// pool is `unavailable` (the Python app's no-key reason configured, see
// common::PYTHON_NO_KEY) and its change event is left set. The Python app's
// stream opened now sends the state twice (pool.json
// `stream_on_connect_after_change`) and clears the event. Enumerated
// deltas: `frames` [state, state] -> [state] (the change before
// connecting is already in the first frame; the next frame is the
// keepalive), `event_set_after` false -> true (a stream no longer clears
// the pool's event). Then the keepalive after the ping interval.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unconsumed_change_is_sent_once_then_keepalive() {
    let g = pool_golden();
    let mut want = g["stream_on_connect_after_change"].clone();
    // Deltas for the one-wake-up-per-connection change, checked against the
    // Python app's values first.
    let frames = want["frames"].as_array().unwrap().clone();
    assert_eq!(frames.len(), 2, "the Python app's value");
    assert_eq!(frames[0], frames[1], "the Python app's value");
    want["frames"] = json!([frames[0]]);
    assert_eq!(
        want["event_set_after"],
        json!(false),
        "the Python app's value"
    );
    want["event_set_after"] = json!(true);

    let sb = Sandbox::new();
    let st = base(&sb)
        .fingerprinting(common::no_key_fingerprinting())
        .build()
        .unwrap();
    let app = router(st.clone());
    configure(&app, &sb).await;
    scan(&app).await;
    assert_eq!(st.pool().state().status, PoolStatus::Unavailable);
    assert_eq!(
        json!(st.pool().changed().is_set()),
        want["event_set_before_connect"]
    );
    let (mut s, _) = Stream::open(&app, "/api/fingerprint-pool/stream").await;
    let a = s.next(Duration::from_secs(5)).await.unwrap();
    let lib = Sandbox::s(&sb.lib);
    let golden_frames: Vec<String> = want["frames"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| norm_frame(&f.as_str().unwrap().replace("/some/library", &lib), &lib))
        .collect();
    assert_eq!(vec![norm_frame(&a, &lib)], golden_frames);
    // `asyncio.wait_for(..., timeout=30.0)` -> `": ping\n\n"`.
    assert_eq!(json!(KEEPALIVE_FRAME), g["stream"]["keepalive"]);
    let k = s.next(Duration::from_secs(5)).await.unwrap();
    assert_eq!(k, KEEPALIVE_FRAME);
    assert_eq!(json!(st.pool().changed().is_set()), want["event_set_after"]);
}

// A change wakes every stream waiting at that moment, each sends
// the new state once (the shared event is set once, cleared by whichever
// runs first; asyncio wakes every waiter regardless).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_change_wakes_every_waiting_stream() {
    let sb = Sandbox::new();
    let st = base(&sb)
        .fingerprinting(common::no_key_fingerprinting())
        .ping_interval(Duration::from_secs(30))
        .build()
        .unwrap();
    let app = router(st.clone());
    let (mut s1, _) = Stream::open(&app, "/api/fingerprint-pool/stream").await;
    let (mut s2, _) = Stream::open(&app, "/api/fingerprint-pool/stream").await;
    for s in [&mut s1, &mut s2] {
        let f = s.next(Duration::from_secs(5)).await.unwrap();
        assert!(f.contains("\"status\": \"idle\""), "{f}");
    }
    // Both are now waiting; a scan makes the pool `unavailable` (one set).
    configure(&app, &sb).await;
    scan(&app).await;
    for s in [&mut s1, &mut s2] {
        let f = s.next(Duration::from_secs(5)).await.expect("woken");
        assert!(f.starts_with("event: state\n"), "{f}");
        assert!(f.contains("\"status\": \"unavailable\""), "{f}");
    }
}

/// The candidate albums of the scanned library (track_count > 0).
fn albums_with_tracks(st: &AppState) -> usize {
    let c = rusqlite::Connection::open(st.db_path()).unwrap();
    c.query_row(
        "SELECT COUNT(*) FROM albums WHERE track_count > 0",
        [],
        |r| r.get::<_, i64>(0),
    )
    .unwrap() as usize
}

// scan.py:234-236: a scan that ends cleanly starts a
// pool pass over the scanned library; with a working fingerprinter every
// candidate album is fingerprinted (rows stored, no AcoustID key so no
// lookups) and the pass ends `done`. The status route shows the Python app's
// record shape (pool.json `run`: total = candidates, completed + errors =
// total, current_album_id cleared, both times set).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_scan_starts_a_pool_pass() {
    let g = pool_golden();
    let sb = Sandbox::new();
    let st = base(&sb)
        .fingerprinting(Fingerprinting::Local(Arc::new(Fixed)))
        .pool_sleeper(Arc::new(NoSleep))
        .build()
        .unwrap();
    let app = router(st.clone());
    configure(&app, &sb).await;
    assert_eq!(st.pool().state().status, PoolStatus::Idle);
    scan(&app).await;
    eventually("pool done", || st.pool().state().status == PoolStatus::Done).await;
    let r = call(&app, "GET", "/api/fingerprint-pool/status", None)
        .await
        .json();
    let run = g["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "run")
        .unwrap();
    let keys = |v: &Value| -> Vec<String> { v.as_object().unwrap().keys().cloned().collect() };
    assert_eq!(keys(&r), keys(&run["final"]));
    assert_eq!(r["status"], "done");
    assert_eq!(r["library_dir"], json!(Sandbox::s(&sb.lib)));
    let n = albums_with_tracks(&st);
    assert!(n >= 10, "{n}");
    assert_eq!(r["total"], json!(n));
    assert_eq!(r["completed"], json!(n));
    assert_eq!(r["errors"], 0);
    assert_eq!(r["current_album_id"], Value::Null);
    assert!(r["started_at"].is_string() && r["completed_at"].is_string());
    assert_eq!(r["skip_reason"], Value::Null);
    let rows: i64 = rusqlite::Connection::open(st.db_path())
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM track_fingerprints WHERE fingerprint_impl = ?1",
            [musaic_db::FINGERPRINT_IMPL_RUST],
            |r| r.get(0),
        )
        .unwrap();
    assert!(rows > 0);
    // Nothing left to do: a further scan starts a pass with no candidates.
    scan(&app).await;
    eventually("second pass done", || {
        let s = st.pool().state();
        s.status == PoolStatus::Done && s.total == 0
    })
    .await;
}

// scan.py:235-236: the pass in flight is stopped (cancelled, its thread
// gone) before the next scan's pass starts.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_scan_stops_the_pass_in_flight_then_warms() {
    let sb = Sandbox::new();
    let park = Arc::new(Park::default());
    let st = base(&sb)
        .fingerprinting(Fingerprinting::Local(Arc::new(Fixed)))
        .pool_sleeper(park.clone())
        .build()
        .unwrap();
    let statuses: Arc<Mutex<Vec<&'static str>>> = Arc::default();
    {
        let (p, s) = (st.pool().clone(), statuses.clone());
        st.pool().changed().add_listener(Arc::new(move || {
            s.lock().unwrap().push(p.state().status.as_str());
        }));
    }
    let app = router(st.clone());
    configure(&app, &sb).await;
    scan(&app).await;
    eventually("first album done", || {
        park.entered.load(Ordering::SeqCst) == 1
    })
    .await;
    assert_eq!(st.pool().state().completed, 1);
    statuses.lock().unwrap().clear();
    scan(&app).await;
    eventually("second pass parked", || {
        park.entered.load(Ordering::SeqCst) == 2
    })
    .await;
    let seen = statuses.lock().unwrap().clone();
    let c = seen
        .iter()
        .position(|s| *s == "cancelled")
        .expect("cancelled");
    let r = seen.iter().rposition(|s| *s == "running").unwrap();
    assert!(c < r, "{seen:?}");
    st.shutdown();
}

// Pool work never holds the restore gate (see state.rs). A
// restore is not refused while a pass runs; it stops the pass (cancelled,
// thread gone) before swapping the file, and does not restart it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restore_stops_the_pool_instead_of_waiting_for_it() {
    let sb = Sandbox::new();
    let park = Arc::new(Park::default());
    let st = base(&sb)
        .fingerprinting(Fingerprinting::Local(Arc::new(Fixed)))
        .pool_sleeper(park.clone())
        .build()
        .unwrap();
    let app = router(st.clone());
    configure(&app, &sb).await;
    scan(&app).await;
    eventually("pool parked", || park.entered.load(Ordering::SeqCst) == 1).await;
    assert_eq!(st.pool().state().status, PoolStatus::Running);
    assert_eq!(st.gate().snapshot(), (0, false), "the pool holds no guard");
    let snap = call(&app, "POST", "/api/db/snapshots", None).await;
    assert_eq!(snap.status, 200, "{}", snap.text());
    let name = snap.json()["filename"].as_str().unwrap().to_string();
    let r = call(
        &app,
        "POST",
        &format!("/api/db/snapshots/{name}/restore"),
        None,
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(st.pool().state().status, PoolStatus::Cancelled);
    assert!(!st.pool().is_running());
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(park.entered.load(Ordering::SeqCst), 1, "not restarted");
}

// main.py:76-84: shutdown stops the pool and returns once its thread is
// gone.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_stops_the_pool() {
    let sb = Sandbox::new();
    let park = Arc::new(Park::default());
    let st = base(&sb)
        .fingerprinting(Fingerprinting::Local(Arc::new(Fixed)))
        .pool_sleeper(park.clone())
        .build()
        .unwrap();
    let app = router(st.clone());
    configure(&app, &sb).await;
    scan(&app).await;
    eventually("pool parked", || park.entered.load(Ordering::SeqCst) == 1).await;
    let db = st.db_path().to_path_buf();
    let wal = db.with_file_name("library.db-wal");
    assert!(wal.exists(), "the pass wrote through the WAL");
    let s2 = st.clone();
    let report = tokio::task::spawn_blocking(move || s2.shutdown())
        .await
        .unwrap();
    assert_eq!(st.pool().state().status, PoolStatus::Cancelled);
    assert!(st.pool().state().completed_at.is_some());
    // Then the database is checkpointed and closed: nothing is left in the
    // WAL, and the rows the pass wrote are in the main file.
    assert!(report.pool_stopped);
    assert_eq!(report.checkpoint, Ok(musaic_db::CloseCheckpoint::Done));
    assert!(
        !wal.exists() || std::fs::metadata(&wal).unwrap().len() == 0,
        "WAL left behind"
    );
    let main_only =
        rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let rows: i64 = main_only
        .query_row("SELECT COUNT(*) FROM track_fingerprints", [], |r| r.get(0))
        .unwrap();
    assert!(rows > 0);
}

/// Blocks every file until released, ignoring cancellation (a slow file).
struct Stuck {
    entered: AtomicUsize,
    release: AtomicBool,
}

impl Fingerprinter for Stuck {
    fn fingerprint(&self, _path: &Path) -> Result<(String, u64), String> {
        self.entered.fetch_add(1, Ordering::SeqCst);
        while !self.release.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(("AQAAstub".into(), 3))
    }
    fn implementation(&self) -> &str {
        musaic_db::FINGERPRINT_IMPL_RUST
    }
}

// Shutdown waits for the pool only so long: a pass stuck inside one file
// does not hold the process; shutdown says so in the log, still checkpoints
// the database, and the pass ends on its own at the next file boundary.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_waits_for_a_stuck_pool_only_so_long() {
    let sb = Sandbox::new();
    let stuck = Arc::new(Stuck {
        entered: AtomicUsize::new(0),
        release: AtomicBool::new(false),
    });
    let log: Arc<Mutex<Vec<String>>> = Arc::default();
    let l2 = log.clone();
    let st = base(&sb)
        .fingerprinting(Fingerprinting::Local(stuck.clone()))
        .pool_sleeper(Arc::new(NoSleep))
        .shutdown_pool_wait(Duration::from_millis(100))
        .log(Arc::new(move |line: &str| {
            l2.lock().unwrap().push(line.to_string())
        }))
        .build()
        .unwrap();
    let app = router(st.clone());
    configure(&app, &sb).await;
    scan(&app).await;
    eventually("a file in progress", || {
        stuck.entered.load(Ordering::SeqCst) == 1
    })
    .await;
    let s2 = st.clone();
    let t0 = std::time::Instant::now();
    let report = tokio::task::spawn_blocking(move || s2.shutdown())
        .await
        .unwrap();
    assert!(t0.elapsed() < Duration::from_secs(5), "{:?}", t0.elapsed());
    assert!(!report.pool_stopped);
    assert!(report.checkpoint.is_ok(), "{report:?}");
    assert!(
        log.lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("fingerprint pool still busy after 0.1 s")),
        "{:?}",
        log.lock().unwrap()
    );
    // Released, the pass sees the cancel at the next file and ends.
    stuck.release.store(true, Ordering::SeqCst);
    eventually("pass ended", || !st.pool().is_running()).await;
    assert_eq!(st.pool().state().status, PoolStatus::Cancelled);
    assert_eq!(stuck.entered.load(Ordering::SeqCst), 1);
}

// scan.py:184-206, 400-404: a panic inside the scan job is the
// Python app's exception path: an `error` event with the detail, status
// `error` (never stuck at `running`), `LibraryScanCompleted` published, no
// pool pass started (only clean scans warm it), the restore gate released,
// and the next scan runs normally.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_panicking_scan_ends_in_error_and_frees_everything() {
    let sb = Sandbox::new();
    let boom = Arc::new(AtomicBool::new(true));
    let b2 = boom.clone();
    let st = base(&sb)
        .fingerprinting(Fingerprinting::Local(Arc::new(Fixed)))
        .pool_sleeper(Arc::new(NoSleep))
        .on_scan_album(Arc::new(move |_album, _fresh| {
            if b2.load(Ordering::SeqCst) {
                panic!("injected scan failure");
            }
        }))
        .build()
        .unwrap();
    let completed = Arc::new(AtomicUsize::new(0));
    {
        let c = completed.clone();
        st.bus().register(
            musaic_events::Topic::Event,
            musaic_events::Subscriber::from_fn(move |ev| {
                if matches!(ev, musaic_events::Event::LibraryScanCompleted { .. }) {
                    c.fetch_add(1, Ordering::SeqCst);
                }
                Ok(())
            }),
        );
    }
    let app = router(st.clone());
    configure(&app, &sb).await;
    let text = tokio::time::timeout(Duration::from_secs(20), scan(&app))
        .await
        .expect("the stream ends");
    assert!(
        text.contains(
            "event: error\ndata: {\"detail\": \"scan worker panicked: injected scan failure\"}"
        ),
        "{text}"
    );
    let s = call(&app, "GET", "/api/scan/status", None).await.json();
    assert_eq!(s["status"], "error");
    assert_eq!(s["error"], "scan worker panicked: injected scan failure");
    assert!(s["completed_at"].is_string());
    assert_eq!(completed.load(Ordering::SeqCst), 1);
    assert_eq!(st.gate().snapshot(), (0, false), "guard released");
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        st.pool().state().status,
        PoolStatus::Idle,
        "no pass after an error"
    );
    boom.store(false, Ordering::SeqCst);
    let text = scan(&app).await;
    assert!(text.contains("event: done"), "{text}");
    let s = call(&app, "GET", "/api/scan/status", None).await.json();
    assert_eq!(s["status"], "done");
    eventually("pool done", || st.pool().state().status == PoolStatus::Done).await;
}

// The Python app's recorded first frame after its scans (golden_cases
// covers it in sequence); here the frame's skip_reason under the harness
// configuration is the Python app's text.
#[test]
fn harness_reason_is_the_python_apps() {
    let g = api_golden("sse_first_frames.json");
    let f = g["streams"]["/api/fingerprint-pool/stream"]["first_frame"]
        .as_str()
        .unwrap();
    assert!(f.contains(common::PYTHON_NO_KEY), "{f}");
}

/// Each fingerprint row's `(acoustid_lookup_state, acoustid_lookup_failures)`.
fn lookup_states(st: &AppState) -> Vec<(Option<String>, i64)> {
    let c = rusqlite::Connection::open(st.db_path()).unwrap();
    let mut q = c
        .prepare("SELECT acoustid_lookup_state, acoustid_lookup_failures FROM track_fingerprints")
        .unwrap();
    q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

async fn pass_after_scan(
    app: &axum::Router,
    st: &AppState,
) -> musaic_services::fingerprint_pool::PoolState {
    scan(app).await;
    eventually("pool done", || st.pool().state().status == PoolStatus::Done).await;
    st.pool().state()
}

// Failed AcoustID lookups and their retry, through the app's own pool wiring: a library
// fingerprinted without a key is not walked again while there is none; once
// a key is set the next pass visits every album to look its stored
// fingerprints up, without fingerprinting again. AcoustID unreachable
// (offline network): every lookup fails, is retried by the next pass (due
// at once after a first failure), and after the second failure waits (an
// hour), so the pass after that has nothing to do.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_key_set_later_looks_up_and_failures_back_off() {
    use std::sync::atomic::AtomicUsize;
    struct Counting(AtomicUsize);
    impl Fingerprinter for Counting {
        fn fingerprint(&self, _path: &Path) -> Result<(String, u64), String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(("AQAAstub".into(), 3))
        }
        fn implementation(&self) -> &str {
            musaic_db::FINGERPRINT_IMPL_RUST
        }
    }
    let sb = Sandbox::new();
    let secrets = Arc::new(musaic_secrets::MemoryStore::new());
    let fp = Arc::new(Counting(AtomicUsize::new(0)));
    let st = base(&sb)
        .secrets(secrets.clone())
        .fingerprinting(Fingerprinting::Local(fp.clone()))
        .pool_sleeper(Arc::new(NoSleep))
        .build()
        .unwrap();
    let app = router(st.clone());
    configure(&app, &sb).await;
    let first = pass_after_scan(&app, &st).await;
    let n = albums_with_tracks(&st);
    assert!(n >= 10, "{n}");
    assert_eq!((first.total, first.completed), (n, n));
    let rows = lookup_states(&st);
    assert!(!rows.is_empty() && rows.iter().all(|r| *r == (None, 0)));
    assert_eq!(
        pass_after_scan(&app, &st).await.total,
        0,
        "no key: nothing to do"
    );

    let computed = fp.0.load(Ordering::SeqCst);
    musaic_secrets::set_secret(secrets.as_ref(), "acoustid_api_key", "test-key").unwrap();
    let with_key = pass_after_scan(&app, &st).await;
    assert_eq!((with_key.total, with_key.completed), (n, n));
    assert_eq!(
        fp.0.load(Ordering::SeqCst),
        computed,
        "looked up, not fingerprinted"
    );
    let failed = Some(musaic_db::LOOKUP_FAILED.to_owned());
    assert!(lookup_states(&st).iter().all(|r| *r == (failed.clone(), 1)));

    assert_eq!(
        pass_after_scan(&app, &st).await.total,
        n,
        "retried by the next pass"
    );
    assert!(lookup_states(&st).iter().all(|r| *r == (failed.clone(), 2)));
    assert_eq!(pass_after_scan(&app, &st).await.total, 0, "then it waits");
    assert!(lookup_states(&st).iter().all(|r| *r == (failed.clone(), 2)));
    assert_eq!(fp.0.load(Ordering::SeqCst), computed);
}
