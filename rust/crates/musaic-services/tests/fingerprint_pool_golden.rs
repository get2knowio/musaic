//! The fingerprint pool against testdata/golden/fingerprint/pool.json,
//! recorded by a generator that drove the
//! Python app's own `musaic.services.fingerprint_pool` and the
//! `/api/fingerprint-pool/{status,stream}` route functions
//! (payload and frame shapes for the host to wire).
//!
//! Compared per scenario: what `warm_library` returned, the state at every
//! change notification (the Python app's `_notify_changed` calls), the
//! inter-album sleeps, the bus events, the final state, the fingerprint rows
//! per album, and the candidates before and after. Timestamps come from the
//! same sequenced clock the generator installed. Sleeps are recorded, never
//! slept.
//!
//! Deliberate changes, as enumerated deltas ([`Delta`]: scenario, path,
//! old -> new, decision; checked against the Python app's value first, unused
//! ones fail):
//! - the Missing album ("Legacy Folder Name") is not a candidate.
//!   New values are the Python app's records without it: its two state changes
//!   and its pause removed, `total` one less, its error not counted, and it
//!   no longer left over afterwards.
//! - an album holding rows from another fingerprint
//!   implementation is a candidate (its own test below).
//! - with lookups possible, an album with a lookup due is
//!   a candidate, and a pass looks it up without fingerprinting again (own
//!   tests below; not in the Python app's records, where every lookup it made
//!   completed).

mod fp_common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fp_common::*;
use musaic_net::AcoustIdClient;
use musaic_services::fingerprint::{CancelToken, FingerprintEnv, fingerprint_album};
use musaic_services::fingerprint_pool::{
    AlbumJob, FingerprintPool, JobError, KEEPALIVE_AFTER, KEEPALIVE_FRAME, PoolDeps, PoolSleeper,
    PoolState, PoolStatus,
};
use serde_json::{Value, json};

fn pool_golden() -> Value {
    golden("pool.json")
}

fn scenario(g: &Value, name: &str) -> Value {
    g["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == name)
        .unwrap_or_else(|| panic!("{name}"))
        .clone()
}

/// `2026-11-11T00:00:01Z`, `…02Z`, … like the generator's `_utc_iso_now`.
fn seq_clock() -> musaic_services::clock::Clock {
    let n = Arc::new(AtomicUsize::new(0));
    Arc::new(move || {
        let i = n.fetch_add(1, Ordering::SeqCst) + 1;
        format!("2026-11-11T00:00:{i:02}Z")
    })
}

struct Sleeps {
    log: Mutex<Vec<f64>>,
    cancel_on: Option<usize>,
}

impl PoolSleeper for Sleeps {
    fn sleep(&self, d: Duration, _cancel: &CancelToken) -> bool {
        let mut l = self.log.lock().unwrap();
        l.push(d.as_secs_f64());
        self.cancel_on != Some(l.len())
    }
}

/// Every change notification records the state, like the generator's
/// wrapped `_notify_changed`.
fn snapshot_listener(pool: &FingerprintPool) -> Arc<Mutex<Vec<PoolState>>> {
    let snaps: Arc<Mutex<Vec<PoolState>>> = Arc::default();
    let (s, p) = (Arc::clone(&snaps), pool.clone());
    pool.changed()
        .add_listener(Arc::new(move || s.lock().unwrap().push(p.state())));
    snaps
}

fn state_json(lib: Option<&Lib>, s: &PoolState) -> Value {
    let v = s.to_json().to_value();
    lib.map_or(v.clone(), |l| l.norm_value(&v))
}

fn deps(
    lib: &Lib,
    job: AlbumJob,
    sleeper: Arc<dyn PoolSleeper>,
    availability: Result<(), String>,
) -> PoolDeps {
    let db = lib.db.clone();
    PoolDeps {
        open_db: Arc::new(move || {
            let c = rusqlite::Connection::open(&db).map_err(|e| e.to_string())?;
            c.pragma_update(None, "foreign_keys", "ON")
                .map_err(|e| e.to_string())?;
            Ok(c)
        }),
        availability: Arc::new(move || availability.clone()),
        job,
        clock: seq_clock(),
        sleeper,
        fingerprint_impl: musaic_db::FINGERPRINT_IMPL_RUST.into(),
    }
}

/// `fingerprint_album` over the golden fingerprints and replayed lookups,
/// with a recording bus.
/// (The job of an app with a key set: it can look up.)
fn real_job(fps: Arc<GoldenFingerprints>, net: Arc<TestNet>, bus: musaic_events::Bus) -> AlbumJob {
    AlbumJob::new(move |conn, album_id, path, cancel| {
        let client = AcoustIdClient::new(net.as_ref(), "golden-test-key");
        let env = FingerprintEnv {
            conn,
            fingerprinter: fps.as_ref(),
            acoustid: Some(&client),
            bus: &bus,
            now_iso: "2001-01-01T00:00:00Z",
            cancel: Some(cancel),
        };
        match fingerprint_album(&env, album_id, path, false) {
            Ok(_) => Ok(()),
            Err(musaic_services::fingerprint::FingerprintError::Cancelled { .. }) => {
                Err(JobError::Cancelled)
            }
            Err(e) => Err(JobError::Failed(e.to_string())),
        }
    })
    .with_lookups(Arc::new(|| true))
}

fn synthetic_files() -> Value {
    album_golden()["libraries"][0]["fpcalc_files"].clone()
}

fn fp_counts(lib: &Lib) -> Value {
    let conn = lib.conn();
    let mut st = conn
        .prepare(
            "SELECT album_id, COUNT(*) FROM track_fingerprints GROUP BY album_id ORDER BY album_id",
        )
        .unwrap();
    let rows = st
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .map(|(a, n)| (lib.norm(&a), json!(n)))
        .collect::<serde_json::Map<_, _>>();
    Value::Object(rows)
}

/// The pool's candidates; `due_at`: lookups possible (a key set), as of then.
fn candidates(lib: &Lib, current_impl: &str, due_at: Option<&str>) -> Value {
    let c = musaic_db::fingerprints_ext::pool_candidates(
        &lib.conn(),
        &lib.lib_str(),
        current_impl,
        due_at,
    )
    .unwrap();
    json!(
        c.iter()
            .map(|(a, p)| json!([lib.norm(a), lib.norm(p)]))
            .collect::<Vec<_>>()
    )
}

// Status / stream shapes: the idle record, its key order, and the
// stream's first frame; the keepalive the stream sends.
#[test]
fn idle_state_status_and_first_frame() {
    let g = pool_golden();
    let pool = FingerprintPool::new();
    let s = pool.state();
    assert_eq!(s.to_json().to_value(), g["idle"]["status_route"]);
    let keys: Vec<String> = match s.to_json() {
        musaic_services::Json::Obj(p) => p.into_iter().map(|(k, _)| k).collect(),
        _ => unreachable!(),
    };
    assert_eq!(json!(keys), g["keys_in_order"]);
    assert_eq!(json!(s.sse_frame()), g["idle"]["stream_first_frame"]);
    assert_eq!(json!(KEEPALIVE_FRAME), g["stream"]["keepalive"]);
    assert_eq!(
        json!(KEEPALIVE_AFTER.as_secs_f64()),
        g["stream"]["keepalive_after_s"]
    );
    assert_eq!(g["stream"]["event"], "state");
}

// Unavailable (now means "the fingerprinter cannot run"; the record
// is the Python app's): status, both timestamps in order, skip_reason, no task,
// one notification, the event left set, the SSE frame.
#[test]
fn unavailable_matches_the_python_app() {
    let g = pool_golden();
    let lib = Lib::build(&json!([]), &json!({}));
    for sc in g["scenarios"].as_array().unwrap() {
        let name = sc["name"].as_str().unwrap();
        let Some(reason) = name.strip_prefix("unavailable: ") else {
            continue;
        };
        let pool = FingerprintPool::new();
        let snaps = snapshot_listener(&pool);
        let sleeps = Arc::new(Sleeps {
            log: Mutex::default(),
            cancel_on: None,
        });
        let job = AlbumJob::new(|_, _, _, _| panic!("no job may run"));
        let ret = pool.warm_library(
            sc["library_dir"].as_str().unwrap(),
            deps(&lib, job, sleeps.clone(), Err(reason.to_owned())),
        );
        pool.join();
        assert_eq!(state_json(None, &ret), sc["returned"], "{name}");
        let snaps: Vec<Value> = snaps
            .lock()
            .unwrap()
            .iter()
            .map(|s| state_json(None, s))
            .collect();
        assert_eq!(json!(snaps), sc["snapshots"], "{name}");
        assert_eq!(json!(*sleeps.log.lock().unwrap()), sc["sleeps"], "{name}");
        assert_eq!(
            pool.changed().is_set(),
            sc["changed_set"].as_bool().unwrap()
        );
        assert_eq!(state_json(None, &pool.state()), sc["final"], "{name}");
        assert_eq!(
            state_json(None, &pool.state()),
            sc["status_route"],
            "{name}"
        );
        assert_eq!(json!(pool.state().sse_frame()), sc["sse_frame"], "{name}");
    }
}

// The Python app's stream, opened after a change it has not consumed,
// sends the state twice (the event is still set), then the event is clear.
#[test]
fn stream_on_connect_after_change_sends_state_twice() {
    let g = pool_golden();
    let want = &g["stream_on_connect_after_change"];
    let lib = Lib::build(&json!([]), &json!({}));
    let pool = FingerprintPool::new();
    let job = AlbumJob::new(|_, _, _, _| panic!("no job may run"));
    let sleeps = Arc::new(Sleeps {
        log: Mutex::default(),
        cancel_on: None,
    });
    // The generator's last unavailable scenario left this state.
    pool.warm_library(
        "/some/library",
        deps(
            &lib,
            job,
            sleeps,
            Err("AcoustID API key not set (configure in Settings)".into()),
        ),
    );
    assert_eq!(
        json!(pool.changed().is_set()),
        want["event_set_before_connect"]
    );
    // The handler: one frame on connect, then wait (30 s) / clear / frame.
    let mut frames = vec![pool.state().sse_frame()];
    if pool.changed().wait_timeout(Duration::from_secs(2)) {
        pool.changed().clear();
        frames.push(pool.state().sse_frame());
    }
    assert_eq!(json!(frames), want["frames"]);
    assert_eq!(json!(pool.changed().is_set()), want["event_set_after"]);
}

const MISSING: &str = "<ID:Legacy Folder Name>";

/// The skip-Missing-albums deltas for a scenario of the Python app's in which "Legacy Folder
/// Name" is Missing: the album taken out of its candidates, state changes,
/// pauses, counts and leftovers. None where it is present.
fn missing_deltas(sc: &Value) -> Vec<Delta> {
    let name = sc["name"].as_str().unwrap();
    let cands = sc["candidates"].as_array().unwrap();
    let pos = cands
        .iter()
        .position(|c| c[0] == MISSING)
        .expect("recorded includes it");
    let missing_row = sc["albums"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == MISSING)
        .unwrap();
    if missing_row["missing_since"].is_null() {
        return Vec::new();
    }
    let n = cands.len() as u64;
    let mut d = vec![Delta::new(
        name,
        "/candidates",
        Some(json!(cands)),
        Some(json!(
            cands.iter().filter(|c| c[0] != MISSING).collect::<Vec<_>>()
        )),
        "pool-skips-missing",
    )];
    // Was it reached (an error counted after its two state changes)?
    let snaps = sc["snapshots"].as_array().unwrap();
    let reached = snaps.iter().any(|s| s["current_album_id"] == MISSING);
    let fix = |s: &Value| {
        let mut s = s.clone();
        if s["total"] == json!(n) {
            s["total"] = json!(n - 1);
        }
        if reached {
            // Its failure was the only error of the pass.
            assert!(s["errors"].as_u64().unwrap() <= 1);
            s["errors"] = json!(0);
        }
        s
    };
    let new_snaps: Vec<Value> = snaps
        .iter()
        .filter(|s| s["current_album_id"] != MISSING)
        .map(fix)
        .collect();
    d.push(Delta::new(
        name,
        "/snapshots",
        Some(json!(snaps)),
        Some(json!(new_snaps)),
        "pool-skips-missing",
    ));
    d.push(Delta::new(
        name,
        "/final",
        Some(sc["final"].clone()),
        Some(fix(&sc["final"])),
        "pool-skips-missing",
    ));
    if reached {
        let sleeps = sc["sleeps"].as_array().unwrap();
        assert!(pos < sleeps.len());
        let mut new_sleeps = sleeps.clone();
        new_sleeps.remove(pos);
        d.push(Delta::new(
            name,
            "/sleeps",
            Some(json!(sleeps)),
            Some(json!(new_sleeps)),
            "pool-skips-missing",
        ));
    }
    if let Some(after) = sc.get("candidates_after") {
        let a = after.as_array().unwrap();
        d.push(Delta::new(
            name,
            "/candidates_after",
            Some(after.clone()),
            Some(json!(
                a.iter().filter(|c| c[0] != MISSING).collect::<Vec<_>>()
            )),
            "pool-skips-missing",
        ));
    }
    d
}

/// The run scenario's library: synthetic stand-ins, the golden's album
/// table, First Light fingerprinted beforehand, Legacy Folder Name gone and
/// Missing.
fn run_world(sc: &Value) -> (Lib, Arc<GoldenFingerprints>, Arc<TestNet>) {
    let files = synthetic_files();
    let lib = Lib::build(&sc["albums"], &files);
    lib.insert_albums(&lib.conn(), &sc["albums"]);
    let fps = Arc::new(GoldenFingerprints::new(&lib.root, &files));
    let net = Arc::new(TestNet::default());
    (lib, fps, net)
}

// A whole pass. Candidates (SQLite's
// order), every state change, one 0.5 s sleep per album, TrackFingerprinted
// per album, the rows written, and nothing left to do afterwards. The
// Python app also walked the Missing (vanished) album, counted it in errors and
// left it over: deltas (Missing albums are skipped).
#[test]
fn a_full_pass_matches_the_python_app() {
    musaic_testdata::require_private!("replay");
    let g = pool_golden();
    let recorded = scenario(&g, "run");
    let list = missing_deltas(&recorded);
    // candidates, snapshots, final, sleeps, candidates_after
    assert_eq!(list.len(), 5);
    let mut deltas = Deltas::new(list);
    let mut sc = recorded.clone();
    deltas.apply("run", &mut sc);
    deltas.assert_all_used();
    let due_at = sc["returned"]["started_at"].as_str().unwrap().to_owned();
    let (lib, fps, net) = run_world(&sc);
    // Setup as the generator did it.
    {
        let conn = lib.conn();
        let (bus, _) = recording_bus();
        let client = AcoustIdClient::new(net.as_ref(), "k");
        let env = FingerprintEnv {
            conn: &conn,
            fingerprinter: fps.as_ref(),
            acoustid: Some(&client),
            bus: &bus,
            now_iso: "2001-01-01T00:00:00Z",
            cancel: None,
        };
        let first = lib.real("<ID:Aurora Fields - First Light [FLAC]>");
        let path = lib.real("<LIB>/Aurora Fields - First Light [FLAC]");
        fingerprint_album(&env, &first, &path, false).unwrap();
        std::fs::remove_dir_all(lib.root.join("Legacy Folder Name")).unwrap();
        net.take_log();
    }
    assert_eq!(fp_counts(&lib), sc["rows_per_album_before"]);
    assert_eq!(
        candidates(&lib, musaic_db::FINGERPRINT_IMPL_RUST, Some(&due_at)),
        sc["candidates"]
    );

    let pool = FingerprintPool::new();
    let snaps = snapshot_listener(&pool);
    let (bus, events) = recording_bus();
    let sleeps = Arc::new(Sleeps {
        log: Mutex::default(),
        cancel_on: None,
    });
    let ret = pool.warm_library(
        &lib.lib_str(),
        deps(
            &lib,
            real_job(fps, net.clone(), bus),
            sleeps.clone(),
            Ok(()),
        ),
    );
    pool.join();
    assert_eq!(state_json(Some(&lib), &ret), sc["returned"]);
    let snaps: Vec<Value> = snaps
        .lock()
        .unwrap()
        .iter()
        .map(|s| state_json(Some(&lib), s))
        .collect();
    assert_eq!(json!(snaps), sc["snapshots"]);
    assert_eq!(json!(*sleeps.log.lock().unwrap()), sc["sleeps"]);
    let ev: Vec<Value> = events
        .lock()
        .unwrap()
        .iter()
        .map(|e| lib.norm_value(&event_json(e)))
        .collect();
    assert_eq!(json!(ev), sc["events"]);
    assert_eq!(
        net.take_log().len() as u64,
        sc["requests_count"].as_u64().unwrap()
    );
    assert_eq!(state_json(Some(&lib), &pool.state()), sc["final"]);
    assert_eq!(fp_counts(&lib), sc["rows_per_album"]);
    assert_eq!(
        candidates(&lib, musaic_db::FINGERPRINT_IMPL_RUST, Some(&due_at)),
        sc["candidates_after"]
    );
}

// A second warm_library while one runs returns the running state
// and starts nothing.
#[test]
fn already_running_returns_the_running_state() {
    let g = pool_golden();
    let sc = scenario(&g, "already running");
    let lib = Lib::build(&json!([]), &json!({}));
    let pool = FingerprintPool::new();
    let snaps = snapshot_listener(&pool);
    pool.set_state(PoolState {
        status: PoolStatus::Running,
        library_dir: Some("/lib".into()),
        ..PoolState::default()
    });
    let job = AlbumJob::new(|_, _, _, _| panic!("no job may run"));
    let sleeps = Arc::new(Sleeps {
        log: Mutex::default(),
        cancel_on: None,
    });
    let ret = pool.warm_library("/lib", deps(&lib, job, sleeps, Ok(())));
    assert_eq!(state_json(None, &ret), sc["returned"]);
    assert_eq!(
        json!(snaps.lock().unwrap().len()),
        json!(sc["snapshots"].as_array().unwrap().len())
    );
    assert!(!sc["task_spawned"].as_bool().unwrap());
}

// Cancelled in the sleep after the
// second album: two albums done and counted, status "cancelled",
// completed_at stamped, current album cleared.
#[test]
fn cancelled_in_the_sleep_matches_the_python_app() {
    musaic_testdata::require_private!("replay");
    let g = pool_golden();
    let name = "cancelled in the sleep after the second album";
    let recorded = scenario(&g, name);
    // Skipping Missing albums does not apply: the album is present in this scenario.
    let mut deltas = Deltas::new(missing_deltas(&recorded));
    let mut sc = recorded.clone();
    deltas.apply(name, &mut sc);
    deltas.assert_all_used();
    let due_at = sc["returned"]["started_at"].as_str().unwrap().to_owned();
    let (lib, fps, net) = run_world(&sc);
    assert_eq!(
        candidates(&lib, musaic_db::FINGERPRINT_IMPL_RUST, Some(&due_at)),
        sc["candidates"]
    );
    let pool = FingerprintPool::new();
    let snaps = snapshot_listener(&pool);
    let (bus, events) = recording_bus();
    let sleeps = Arc::new(Sleeps {
        log: Mutex::default(),
        cancel_on: Some(2),
    });
    let ret = pool.warm_library(
        &lib.lib_str(),
        deps(&lib, real_job(fps, net, bus), sleeps.clone(), Ok(())),
    );
    pool.join();
    assert_eq!(state_json(Some(&lib), &ret), sc["returned"]);
    let snaps: Vec<Value> = snaps
        .lock()
        .unwrap()
        .iter()
        .map(|s| state_json(Some(&lib), s))
        .collect();
    assert_eq!(json!(snaps), sc["snapshots"]);
    assert_eq!(json!(*sleeps.log.lock().unwrap()), sc["sleeps"]);
    let ev: Vec<Value> = events
        .lock()
        .unwrap()
        .iter()
        .map(|e| lib.norm_value(&event_json(e)))
        .collect();
    assert_eq!(json!(ev), sc["events"]);
    assert_eq!(state_json(Some(&lib), &pool.state()), sc["final"]);
    assert_eq!(fp_counts(&lib), sc["rows_per_album"]);
}

// Stop() with nothing running changes nothing and notifies nobody.
#[test]
fn stop_with_nothing_running_is_a_no_op() {
    let g = pool_golden();
    let sc = scenario(&g, "stop with nothing running");
    let pool = FingerprintPool::new();
    let snaps = snapshot_listener(&pool);
    pool.stop();
    assert_eq!(state_json(None, &pool.state()), sc["final"]);
    assert_eq!(
        json!(snaps.lock().unwrap().len()),
        json!(sc["snapshots"].as_array().unwrap().len())
    );
    assert!(sc["task"].is_null());
}

// Cancellation through stop() while an album is being fingerprinted (not
// recorded from the Python app: its task is cancelled at the `to_thread` await,
// fingerprint_pool.py:187-192, so the album is not counted and the status is
// "cancelled"; the abandoned thread keeps writing, which this build does not
// do). Pinned against the Python app's
// cancelled record: same final shape, the in-flight album not counted.
#[test]
fn stop_mid_album_returns_after_the_thread_is_gone() {
    musaic_testdata::require_private!("replay");
    let g = pool_golden();
    let sc = scenario(&g, "cancelled in the sleep after the second album");
    let (lib, _, _) = run_world(&sc);
    let started = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let s2 = Arc::clone(&started);
    let job = AlbumJob::new(move |_, _, _, cancel| {
        *s2.0.lock().unwrap() = true;
        s2.1.notify_all();
        while !cancel.is_cancelled() {
            std::thread::sleep(Duration::from_millis(2));
        }
        Err(JobError::Cancelled)
    });
    let pool = FingerprintPool::new();
    let sleeps = Arc::new(Sleeps {
        log: Mutex::default(),
        cancel_on: None,
    });
    pool.warm_library(&lib.lib_str(), deps(&lib, job, sleeps.clone(), Ok(())));
    let s = started.0.lock().unwrap();
    let (s, timeout) = started
        .1
        .wait_timeout_while(s, Duration::from_secs(10), |s| !*s)
        .unwrap();
    assert!(!timeout.timed_out(), "the pool never started an album");
    drop(s);
    pool.stop();
    let st = pool.state();
    // The Python app's cancelled record, less what differs by construction (the
    // count of albums finished before the cancel: 2 there, 0 here).
    let mut want = sc["final"].clone();
    want["completed"] = json!(0);
    assert_eq!(state_json(Some(&lib), &st), want);
    assert!(sleeps.log.lock().unwrap().is_empty());
    assert!(!pool.is_running());
}

// Deliberate change: an album whose rows came from fpcalc is a
// candidate again, everything else as the Python app's candidate list (less the
// Missing album).
#[test]
fn fpcalc_rows_make_an_album_a_candidate() {
    musaic_testdata::require_private!("replay");
    let g = pool_golden();
    let mut sc = scenario(&g, "run");
    let mut deltas = Deltas::new(missing_deltas(&sc));
    deltas.apply("run", &mut sc);
    let (lib, fps, net) = run_world(&sc);
    let conn = lib.conn();
    let (bus, _) = recording_bus();
    let client = AcoustIdClient::new(net.as_ref(), "k");
    let env = FingerprintEnv {
        conn: &conn,
        fingerprinter: fps.as_ref(),
        acoustid: Some(&client),
        bus: &bus,
        now_iso: "2001-01-01T00:00:00Z",
        cancel: None,
    };
    let first = lib.real("<ID:Aurora Fields - First Light [FLAC]>");
    let path = lib.real("<LIB>/Aurora Fields - First Light [FLAC]");
    fingerprint_album(&env, &first, &path, false).unwrap();
    std::fs::remove_dir_all(lib.root.join("Legacy Folder Name")).unwrap();
    let recorded = sc["candidates"].as_array().unwrap().clone();
    assert!(
        !recorded
            .iter()
            .any(|c| c[0] == "<ID:Aurora Fields - First Light [FLAC]>")
    );
    conn.execute(
        "UPDATE track_fingerprints SET fingerprint_impl = 'fpcalc' WHERE album_id = ?1",
        [&first],
    )
    .unwrap();
    let got = candidates(&lib, musaic_db::FINGERPRINT_IMPL_RUST, None);
    let got = got.as_array().unwrap();
    let without: Vec<Value> = got
        .iter()
        .filter(|c| c[0] != "<ID:Aurora Fields - First Light [FLAC]>")
        .cloned()
        .collect();
    assert_eq!(
        without, recorded,
        "the Python app's candidates, in its order"
    );
    assert_eq!(got.len(), recorded.len() + 1, "plus the fpcalc album");
    // ... and fingerprint_album recomputes it rather than serving the cache.
    let again = fingerprint_album(&env, &first, &path, false).unwrap();
    assert!(!again.from_cache);
}

/// `fingerprint_album` as the app's job runs it, with or without a key, its
/// rows stamped `now`.
fn job_at(
    fps: Arc<GoldenFingerprints>,
    net: Arc<TestNet>,
    bus: musaic_events::Bus,
    key: bool,
    now: &'static str,
) -> AlbumJob {
    AlbumJob::new(move |conn, album_id, path, cancel| {
        let client = AcoustIdClient::new(net.as_ref(), "golden-test-key");
        let env = FingerprintEnv {
            conn,
            fingerprinter: fps.as_ref(),
            acoustid: key.then_some(&client as &dyn musaic_services::fingerprint::AcoustIdLookup),
            bus: &bus,
            now_iso: now,
            cancel: Some(cancel),
        };
        fingerprint_album(&env, album_id, path, false)
            .map(|_| ())
            .map_err(|e| JobError::Failed(e.to_string()))
    })
    .with_lookups(Arc::new(move || key))
}

/// The "run" world with nothing fingerprinted yet and the Missing album's
/// directory gone; the candidates: the Python app's (less the Missing album,
/// which is skipped) plus First Light, which the Python app had fingerprinted first.
fn present_world() -> (Lib, Arc<GoldenFingerprints>, Arc<TestNet>, Vec<Value>) {
    let g = pool_golden();
    let mut sc = scenario(&g, "run");
    Deltas::new(missing_deltas(&sc)).apply("run", &mut sc);
    let (lib, fps, net) = run_world(&sc);
    std::fs::remove_dir_all(lib.root.join("Legacy Folder Name")).unwrap();
    let cands = candidates(&lib, musaic_db::FINGERPRINT_IMPL_RUST, None);
    let cands = cands.as_array().unwrap().clone();
    let first = "<ID:Aurora Fields - First Light [FLAC]>";
    let others: Vec<&Value> = cands.iter().filter(|c| c[0] != first).collect();
    assert_eq!(json!(others), sc["candidates"]);
    assert_eq!(cands.len(), others.len() + 1);
    (lib, fps, net, cands)
}

fn run_pass(lib: &Lib, job: AlbumJob) -> PoolState {
    let pool = FingerprintPool::new();
    let sleeps = Arc::new(Sleeps {
        log: Mutex::default(),
        cancel_on: None,
    });
    pool.warm_library(&lib.lib_str(), deps(lib, job, sleeps, Ok(())));
    pool.join();
    pool.state()
}

fn all_states(lib: &Lib, cands: &[Value]) -> Vec<(String, Option<String>, i64, Option<String>)> {
    cands
        .iter()
        .flat_map(|c| lookup_states(&lib.conn(), &lib.real(c[0].as_str().unwrap())))
        .collect()
}

// Key added later: a pass without a key stores every
// fingerprint with no lookup outcome; without a key such albums are not
// candidates again; once a key is set they are, and the pass looks every
// stored fingerprint up without fingerprinting anything again. Afterwards
// nothing is left to do. Expected requests and rows: the Python app's own (the
// same fingerprints, the recorded answers).
#[test]
fn a_key_added_later_looks_up_stored_fingerprints() {
    musaic_testdata::require_private!("replay");
    let (lib, fps, net, cands) = present_world();
    let (bus, events) = recording_bus();
    let st = run_pass(
        &lib,
        job_at(
            fps.clone(),
            net.clone(),
            bus.clone(),
            false,
            "2026-11-11T00:00:00Z",
        ),
    );
    assert_eq!(
        (st.total, st.completed, st.errors),
        (cands.len(), cands.len(), 0)
    );
    assert!(net.take_log().is_empty(), "no key: nothing asked");
    let rows: usize = all_states(&lib, &cands).len();
    assert!(all_states(&lib, &cands).iter().all(|s| s.1.is_none()));
    assert_eq!(
        candidates(&lib, musaic_db::FINGERPRINT_IMPL_RUST, None),
        json!([])
    );
    assert_eq!(
        candidates(
            &lib,
            musaic_db::FINGERPRINT_IMPL_RUST,
            Some("2026-11-11T00:00:01Z")
        ),
        json!(cands)
    );
    let fp_calls = fps.calls.lock().unwrap().len();
    events.lock().unwrap().clear();

    let st = run_pass(
        &lib,
        job_at(fps.clone(), net.clone(), bus, true, "2026-11-11T00:00:00Z"),
    );
    assert_eq!(
        (st.total, st.completed, st.errors),
        (cands.len(), cands.len(), 0)
    );
    assert_eq!(
        fps.calls.lock().unwrap().len(),
        fp_calls,
        "nothing fingerprinted again"
    );
    let log = net.take_log();
    assert_eq!(log.len(), rows, "every stored fingerprint asked once");
    assert!(log.iter().all(|r| r["served"] == "recorded"));
    assert_eq!(
        events.lock().unwrap().len(),
        cands.len(),
        "one TrackFingerprinted per album"
    );
    assert!(
        all_states(&lib, &cands)
            .iter()
            .all(|s| s.1.as_deref() == Some(musaic_db::LOOKUP_DONE))
    );
    assert_eq!(
        candidates(
            &lib,
            musaic_db::FINGERPRINT_IMPL_RUST,
            Some("2026-11-11T00:00:01Z")
        ),
        json!([])
    );
}

// Retry storm guard: with AcoustID down, a pass asks each file
// once (each album visited once, then the pause). The next pass asks again
// (due at once after a first failure); after the second failure the rows
// wait an hour, so a third pass at the same time has no candidates; once
// the hour has passed the album is a candidate again.
#[test]
fn failed_lookups_are_retried_on_later_passes_with_backoff() {
    musaic_testdata::require_private!("replay");
    let (lib, fps, net, cands) = present_world();
    for f in album_golden()["libraries"][0]["fpcalc_files"]
        .as_object()
        .unwrap()
        .values()
    {
        if let Some(sha) = f["fingerprint_sha256"].as_str() {
            net.inject
                .lock()
                .unwrap()
                .insert(sha.into(), "connection-error".into());
        }
    }
    let (bus, _) = recording_bus();
    let now = "2026-11-11T00:00:00Z";
    let st = run_pass(
        &lib,
        job_at(fps.clone(), net.clone(), bus.clone(), true, now),
    );
    assert_eq!(st.total, cands.len());
    let rows = all_states(&lib, &cands);
    let log = net.take_log();
    assert_eq!(log.len(), rows.len(), "each file asked once in the pass");
    assert!(
        rows.iter()
            .all(|s| s.1.as_deref() == Some(musaic_db::LOOKUP_FAILED)
                && s.2 == 1
                && s.3.as_deref() == Some(now))
    );

    let fp_calls = fps.calls.lock().unwrap().len();
    let st = run_pass(
        &lib,
        job_at(fps.clone(), net.clone(), bus.clone(), true, now),
    );
    assert_eq!(st.total, cands.len(), "due again at once after one failure");
    assert_eq!(net.take_log().len(), rows.len());
    assert_eq!(
        fps.calls.lock().unwrap().len(),
        fp_calls,
        "looked up, not fingerprinted"
    );
    assert!(
        all_states(&lib, &cands)
            .iter()
            .all(|s| s.2 == 2 && s.3.as_deref() == Some("2026-11-11T01:00:00Z"))
    );

    // The pool's clock reads 2026-11-11T00:00:01Z: an hour has not passed.
    let st = run_pass(
        &lib,
        job_at(fps.clone(), net.clone(), bus.clone(), true, now),
    );
    assert_eq!((st.total, st.completed), (0, 0));
    assert!(net.take_log().is_empty());
    assert_eq!(
        candidates(
            &lib,
            musaic_db::FINGERPRINT_IMPL_RUST,
            Some("2026-11-11T01:00:00Z")
        ),
        json!(cands)
    );
}
