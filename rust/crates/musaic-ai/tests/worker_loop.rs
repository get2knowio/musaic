//! The dispatcher loop (`services.worker.worker_loop`):
//! startup recovery of rows left `running`, oldest-first dispatch, nothing
//! dispatched while `ai_queue_paused`, the pause set by any failure (and
//! kept until a resume), `ai_max_parallel`, and (deliberate) the restore
//! gate: no dispatch while a restore holds it, and a
//! job holds a work guard for as long as it runs.
//!
//! The loop is not something a golden can record (the Python app's is an
//! asyncio task woken by an Event); the rules are read from
//! musaic/services/worker.py:488-568 and musaic/jobs.py:226-286, and each
//! test names the line it pins.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use common::{StubEnv, replay};
use musaic_ai::provider::replay::{Ending, ReplayProvider, Script};
use musaic_ai::provider::{ProviderError, ProviderEvent};
use musaic_ai::{Provider, Py, RunOptions, Signal, Worker, WorkerEnv};
use musaic_db::ai_jobs::{self, NewJob};

const OK: &str = r#"{"suggestions": {"genre": "Pop"}}"#;

fn env(n: usize) -> (tempfile::TempDir, Arc<StubEnv>) {
    let tmp = tempfile::tempdir().unwrap();
    let ids: Vec<String> = (0..n).map(|i| format!("a{i}")).collect();
    let db = common::fresh_db(tmp.path(), &ids);
    let albums: BTreeMap<String, Py> = ids
        .iter()
        .map(|id| {
            (
                id.clone(),
                Py::from_value(&serde_json::json!({"id": id, "path": format!("<LIB>/{id}")})),
            )
        })
        .collect();
    let e = StubEnv::new(db, albums, BTreeMap::new());
    // Unanchored: every metadata job goes to the provider.
    *e.resolution.lock().unwrap() =
        Some(serde_json::json!({"tracks": [], "unresolved_track_count": 1}));
    (tmp, Arc::new(e))
}

fn enqueue(env: &StubEnv, album: &str, n: u64) -> String {
    let mut c = env.open_db().unwrap();
    ai_jobs::enqueue(
        &mut c,
        &NewJob {
            album_id: album,
            album_path: "",
            album_title: album,
            job_type: "metadata",
            hint: "",
            attach_cover: false,
        },
        || format!("{n:016x}"),
        &env.now_iso(),
    )
    .unwrap()
    .unwrap()
    .id
}

fn ok_script() -> Script {
    Script {
        events: vec![ProviderEvent::TurnComplete { text: OK.into() }],
        ending: Ending::Done,
    }
}

fn hang() -> Script {
    Script {
        events: vec![],
        ending: Ending::Hang,
    }
}

fn fail() -> Script {
    Script {
        events: vec![],
        ending: Ending::Fail(ProviderError::Transient("503".into())),
    }
}

fn status(env: &StubEnv, id: &str) -> String {
    ai_jobs::get_job(&env.open_db().unwrap(), id)
        .unwrap()
        .unwrap()
        .status
}

/// Everything a job's progress changes: each job's status and timestamps,
/// the queue's paused flag and the number of work guards held.
type Progress = (
    Vec<(String, String, Option<String>, Option<String>)>,
    bool,
    u64,
);

fn progress(env: &StubEnv) -> Progress {
    let c = env.open_db().unwrap();
    let mut jobs: Vec<_> = ai_jobs::list_jobs(&c, None, 1000)
        .unwrap()
        .into_iter()
        .map(|j| (j.id, j.status, j.started_at, j.completed_at))
        .collect();
    jobs.sort();
    let paused = musaic_ai::jobs::queue_paused(&c).unwrap();
    (jobs, paused, env.guards.load(Ordering::SeqCst))
}

/// Waits until `f` holds, failing only when nothing the worker records has
/// changed for `STALL` (a hang) or after a backstop. Each job state is a
/// commit, and a runner whose disk stalls makes any one of them slow, so a
/// fixed deadline per wait failed for the disk rather than the worker. The
/// waits here are mostly for a single transition, so the signal is coarse:
/// it helps a wait that covers several jobs, and for a single transition
/// `STALL` is the whole allowance, which is why it is generous.
fn wait_for(env: &StubEnv, what: &str, mut f: impl FnMut() -> bool) {
    const STALL: Duration = Duration::from_secs(30);
    const CAP: Duration = Duration::from_secs(600);
    let begun = Instant::now();
    let mut last = progress(env);
    let mut since = Instant::now();
    loop {
        if f() {
            return;
        }
        let now = progress(env);
        if now != last {
            last = now;
            since = Instant::now();
        }
        assert!(
            since.elapsed() < STALL,
            "no progress for {STALL:?} waiting for {what}: {last:?}"
        );
        assert!(begun.elapsed() < CAP, "never reached {what}: {last:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

struct Running {
    worker: Worker,
    signal: Arc<Signal>,
    provider: ReplayProvider,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.provider.release();
        self.worker.stop();
    }
}

/// The dispatcher with any provider (no replay handle to release).
fn start_with(env: &Arc<StubEnv>, provider: Arc<dyn Provider>) -> (Worker, Arc<Signal>) {
    let signal = Signal::new();
    let wired: Arc<dyn WorkerEnv> = Arc::new(Wired {
        inner: env.clone(),
        signal: signal.clone(),
    });
    let worker = Worker::start(
        wired,
        provider,
        signal.clone(),
        RunOptions {
            call_timeout: Duration::from_secs(10),
            idle_wait: Duration::from_millis(200),
        },
    );
    (worker, signal)
}

fn start(env: &Arc<StubEnv>, provider: ReplayProvider, call_timeout: Duration) -> Running {
    let signal = Signal::new();
    let e2 = env.clone();
    // The env's notify is the signal (as the HTTP layer wires it).
    let s2 = signal.clone();
    let wired: Arc<dyn WorkerEnv> = Arc::new(Wired {
        inner: e2,
        signal: s2,
    });
    let p: Arc<dyn Provider> = Arc::new(provider.clone());
    let worker = Worker::start(
        wired,
        p,
        signal.clone(),
        RunOptions {
            call_timeout,
            idle_wait: Duration::from_millis(200),
        },
    );
    Running {
        worker,
        signal,
        provider,
    }
}

/// The stub env with `notify` bumping the loop's signal.
struct Wired {
    inner: Arc<StubEnv>,
    signal: Arc<Signal>,
}

impl WorkerEnv for Wired {
    fn open_db(&self) -> Result<rusqlite::Connection, String> {
        self.inner.open_db()
    }
    fn now_iso(&self) -> String {
        self.inner.now_iso()
    }
    fn notify(&self) {
        self.inner.notify();
        self.signal.notify();
    }
    fn log(&self, line: &str) {
        self.inner.log(line);
    }
    fn album(&self, conn: &rusqlite::Connection, id: &str) -> Option<Py> {
        self.inner.album(conn, id)
    }
    fn read_tracks(&self, p: &str) -> Py {
        self.inner.read_tracks(p)
    }
    fn resolve(
        &self,
        conn: &rusqlite::Connection,
        id: &str,
        hint: &str,
    ) -> Result<Arc<musaic_resolve::AlbumResolution>, String> {
        self.inner.resolve(conn, id, hint)
    }
    fn cover_for_prompt(&self, a: &Py) -> Option<musaic_ai::provider::ImagePart> {
        self.inner.cover_for_prompt(a)
    }
    fn firecrawl_key(&self) -> Option<String> {
        None
    }
    fn verify_cover_url(&self, u: &str) -> (bool, String) {
        self.inner.verify_cover_url(u)
    }
    fn find_cover(&self, c: &rusqlite::Connection, a: &Py, h: &str) -> Result<Py, String> {
        self.inner.find_cover(c, a, h)
    }
    fn verify_release_group(&self, m: &str) -> Result<u16, String> {
        self.inner.verify_release_group(m)
    }
    fn anchor_set(&self, a: &str) {
        self.inner.anchor_set(a)
    }
    fn begin_work(&self) -> Option<Box<dyn Send>> {
        self.inner.begin_work()
    }
}

fn paused(env: &StubEnv) -> bool {
    musaic_ai::jobs::queue_paused(&env.open_db().unwrap()).unwrap()
}

// worker.py:498-505 / jobs.py:272-286: rows left `running` by a
// previous process are requeued at startup (started_at cleared) and then
// dispatched.
#[test]
fn startup_requeues_running_jobs_then_runs_them() {
    let (_t, env) = env(1);
    let id = enqueue(&env, "a0", 1);
    let c = env.open_db().unwrap();
    ai_jobs::mark_running(&c, &id, "2001-01-01T00:00:00Z").unwrap();
    let p = replay();
    p.push(ok_script());
    let _r = start(&env, p, Duration::from_secs(10));
    wait_for(&env, "completed", || status(&env, &id) == "completed");
    let row = ai_jobs::get_job(&c, &id).unwrap().unwrap();
    assert_ne!(row.started_at.as_deref(), Some("2001-01-01T00:00:00Z"));
}

// jobs.py:226-233: the oldest queued job first; one at a time with
// ai_max_parallel 1 (the default).
#[test]
fn dispatches_oldest_first_one_at_a_time() {
    let (_t, env) = env(3);
    let ids: Vec<String> = (0..3)
        .map(|i| enqueue(&env, &format!("a{i}"), i + 1))
        .collect();
    let p = replay();
    p.push(hang());
    let r = start(&env, p.clone(), Duration::from_secs(10));
    wait_for(&env, "first running", || status(&env, &ids[0]) == "running");
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(status(&env, &ids[1]), "queued", "max_parallel 1");
    assert_eq!(status(&env, &ids[2]), "queued");
    assert_eq!(p.take_requests().len(), 1);
    drop(r);
}

// worker.py:534-541: up to ai_max_parallel jobs run at once.
#[test]
fn max_parallel_bounds_concurrent_jobs() {
    let (_t, env) = env(3);
    musaic_db::settings::set_raw(&env.open_db().unwrap(), "ai_max_parallel", "2").unwrap();
    let ids: Vec<String> = (0..3)
        .map(|i| enqueue(&env, &format!("a{i}"), i + 1))
        .collect();
    let p = replay();
    for _ in 0..3 {
        p.push(hang());
    }
    let r = start(&env, p.clone(), Duration::from_secs(10));
    wait_for(&env, "two running", || {
        ids.iter().filter(|i| status(&env, i) == "running").count() == 2
    });
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(status(&env, &ids[2]), "queued");
    drop(r);
}

// worker.py:460-483, 526-532: a failed job pauses the queue (setting
// "1"); nothing more is dispatched until a resume clears it and wakes the
// loop.
#[test]
fn a_failure_pauses_the_queue_until_resumed() {
    let (_t, env) = env(2);
    let a = enqueue(&env, "a0", 1);
    let b = enqueue(&env, "a1", 2);
    let p = replay();
    p.push(fail());
    p.push(ok_script());
    let r = start(&env, p.clone(), Duration::from_secs(10));
    wait_for(&env, "failed", || status(&env, &a) == "failed");
    // The pause is written just after the failed status.
    wait_for(&env, "paused", || paused(&env));
    assert_eq!(
        musaic_db::settings::get_raw(&env.open_db().unwrap(), "ai_queue_paused").unwrap(),
        Some("1".into())
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(status(&env, &b), "queued", "paused: not dispatched");
    musaic_ai::jobs::set_queue_paused(&env.open_db().unwrap(), false).unwrap();
    r.signal.notify();
    wait_for(&env, "second completed", || status(&env, &b) == "completed");
    drop(r);
}

// With no model backend
// (the Unconfigured provider) a job that needs a model fails with the
// not-available message and the queue is NOT paused: the job behind it is
// dispatched and fails the same way. The Python app paused on the first failure
// (worker.py:460-483). Fails on the old behaviour (paused after the first).
#[test]
fn no_backend_fails_the_job_without_pausing() {
    let (_t, env) = env(2);
    let a = enqueue(&env, "a0", 1);
    let b = enqueue(&env, "a1", 2);
    let p: Arc<dyn Provider> = Arc::new(musaic_ai::Unconfigured);
    let msg = musaic_ai::model_unavailable(p.as_ref()).expect("no backend");
    // No provider is ready, so the
    // message names no AI (the Python app's template "AI provider '…' isn't
    // ready — it needs …" is kept for a configured provider, below).
    assert_eq!(msg, musaic_ai::provider::NOT_AVAILABLE);
    assert!(!msg.contains("AI") && !msg.contains("Claude"), "{msg}");
    let (worker, _signal) = start_with(&env, p);
    wait_for(&env, "both failed", || {
        status(&env, &a) == "failed" && status(&env, &b) == "failed"
    });
    worker.stop();
    assert!(!paused(&env), "no backend: not paused");
    for id in [&a, &b] {
        let row = ai_jobs::get_job(&env.open_db().unwrap(), id)
            .unwrap()
            .unwrap();
        assert_eq!(row.error.as_deref(), Some(msg.as_str()));
    }
}

// A provider WITH a backend that answers NotReady (the Python app's
// preflight failure) still pauses the queue, as the Python app does.
#[test]
fn a_configured_backend_not_ready_still_pauses() {
    let (_t, env) = env(2);
    let a = enqueue(&env, "a0", 1);
    let b = enqueue(&env, "a1", 2);
    let p = replay();
    p.push(Script {
        events: vec![],
        ending: Ending::Fail(ProviderError::NotReady {
            requirement: "the claude CLI".into(),
            remedy: "Install it.".into(),
        }),
    });
    let r = start(&env, p.clone(), Duration::from_secs(10));
    wait_for(&env, "failed", || status(&env, &a) == "failed");
    // The pause is written just after the failed status.
    wait_for(&env, "paused", || paused(&env));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(status(&env, &b), "queued", "paused: not dispatched");
    drop(r);
}

// worker.py:411-459: the wall-clock cap fails the job with the
// timeout message and pauses the queue.
#[test]
fn the_wall_clock_cap_fails_and_pauses() {
    let (_t, env) = env(1);
    let a = enqueue(&env, "a0", 1);
    let p = replay();
    p.push(hang());
    let r = start(&env, p.clone(), Duration::from_millis(200));
    wait_for(&env, "failed", || status(&env, &a) == "failed");
    let row = ai_jobs::get_job(&env.open_db().unwrap(), &a)
        .unwrap()
        .unwrap();
    assert_eq!(
        row.error.as_deref(),
        Some(musaic_ai::worker::timeout_message().as_str())
    );
    // The pause is written just after the failed status.
    wait_for(&env, "paused", || paused(&env));
    drop(r);
}

// A paused queue is not dispatched from at startup either.
#[test]
fn a_paused_queue_dispatches_nothing() {
    let (_t, env) = env(1);
    let a = enqueue(&env, "a0", 1);
    musaic_ai::jobs::set_queue_paused(&env.open_db().unwrap(), true).unwrap();
    let p = replay();
    p.push(ok_script());
    let r = start(&env, p.clone(), Duration::from_secs(10));
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(status(&env, &a), "queued");
    assert_eq!(p.take_requests().len(), 0);
    drop(r);
}

// No dispatch while a restore holds the
// gate; a running job holds one work guard until it ends.
#[test]
fn jobs_take_the_restore_gate() {
    let (_t, env) = env(1);
    let a = enqueue(&env, "a0", 1);
    env.gate_closed.store(true, Ordering::SeqCst);
    let p = replay();
    p.push(hang());
    let r = start(&env, p.clone(), Duration::from_secs(10));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        status(&env, &a),
        "queued",
        "restore running: not dispatched"
    );
    env.gate_closed.store(false, Ordering::SeqCst);
    wait_for(&env, "running", || status(&env, &a) == "running");
    assert_eq!(
        env.guards.load(Ordering::SeqCst),
        1,
        "guard held while running"
    );
    p.release();
    wait_for(&env, "ended", || status(&env, &a) == "failed");
    wait_for(&env, "guard released", || {
        env.guards.load(Ordering::SeqCst) == 0
    });
    drop(r);
}

// `stop()` ends the loop (the lifespan's cancel).
#[test]
fn stop_ends_the_loop() {
    let (_t, env) = env(1);
    let r = start(&env, replay(), Duration::from_secs(10));
    let t = Instant::now();
    r.worker.stop();
    assert!(t.elapsed() < Duration::from_secs(5));
    let a = enqueue(&env, "a0", 1);
    r.signal.notify();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(status(&env, &a), "queued", "stopped: nothing dispatched");
}

const OLD_RG: &str = "11111111-1111-4111-8111-111111111111";
const AI_RG: &str = "22222222-2222-4222-8222-222222222222";
const USER_RG: &str = "33333333-3333-4333-8333-333333333333";

fn anchor_of(env: &StubEnv, album: &str) -> Option<String> {
    env.open_db()
        .unwrap()
        .query_row(
            "SELECT mb_release_group_mbid FROM albums WHERE id = ?1",
            [album],
            |r| r.get(0),
        )
        .unwrap()
}

/// Runs one metadata job whose provider answers with `AI_RG`, after setting
/// the album's anchor to `OLD_RG`; `meanwhile` runs against the database while
/// the job verifies the answer. Returns once the job has completed.
fn ai_names_a_release_group(
    meanwhile: Option<fn(&rusqlite::Connection)>,
) -> (tempfile::TempDir, Arc<StubEnv>) {
    let (tmp, env) = env(1);
    let c = env.open_db().unwrap();
    c.execute(
        "UPDATE albums SET mb_release_group_mbid = ?1 WHERE id = 'a0'",
        [OLD_RG],
    )
    .unwrap();
    env.mb_status
        .lock()
        .unwrap()
        .insert(AI_RG.into(), serde_json::json!(200));
    if let Some(f) = meanwhile {
        let db = env.db.clone();
        *env.on_verify.lock().unwrap() = Some(Box::new(move || {
            f(&rusqlite::Connection::open(&db).unwrap())
        }));
    }
    let id = enqueue(&env, "a0", 1);
    let p = replay();
    p.push(Script {
        events: vec![ProviderEvent::TurnComplete {
            text: format!(
                r#"{{"suggestions": {{"genre": "Pop"}}, "mb_release_group_mbid": "{AI_RG}"}}"#
            ),
        }],
        ending: Ending::Done,
    });
    let _r = start(&env, p, Duration::from_secs(10));
    wait_for(&env, "completed", || status(&env, &id) == "completed");
    (tmp, env)
}

// The AI's release group is stored through a compare-and-set against the
// anchor the job saw, so an unchanged album takes it...
#[test]
fn an_ai_release_group_is_stored_when_the_anchor_is_unchanged() {
    let (_t, env) = ai_names_a_release_group(None);
    assert_eq!(anchor_of(&env, "a0").as_deref(), Some(AI_RG));
    assert_eq!(*env.anchor_events.lock().unwrap(), ["a0"]);
}

// ...and one the user changed while the job ran keeps the user's choice:
// nothing is overwritten and no anchor event is published.
#[test]
fn a_change_made_while_the_job_ran_is_not_overwritten() {
    let (_t, env) = ai_names_a_release_group(Some(|c| {
        c.execute(
            "UPDATE albums SET mb_release_group_mbid = ?1 WHERE id = 'a0'",
            [USER_RG],
        )
        .unwrap();
    }));
    assert_eq!(anchor_of(&env, "a0").as_deref(), Some(USER_RG));
    assert!(env.anchor_events.lock().unwrap().is_empty());
    assert!(
        env.log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("changed while the job ran")),
        "{:?}",
        env.log.lock().unwrap()
    );
}
