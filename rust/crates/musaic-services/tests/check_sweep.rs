//! The background check sweep (`musaic_services::check_sweep`) with stub
//! checks over a small database: what a pass runs, that an unchanged
//! library runs nothing, that a changed album reruns alone, that Missing
//! albums are never checked, that it waits while the app is busy, that a
//! suspension (a restore) interrupts a network wait without recording
//! anything from it, and that shutdown is prompt.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use musaic_checks::base::{CheckCtx, CheckEnv, CheckMeta, Cost, FixKind, SystemClock};
use musaic_checks::{
    Check, CheckError, CheckScope, Emit, Finding, Granularity, Registry, Resolution,
    ResolutionSource, Severity,
};
use musaic_events::Event;
use musaic_services::check_sweep::{CheckSweeper, SweepDeps, SweepStatus};
use musaic_services::host_log::HostLog;
use musaic_services::production::StoppableResolution;
use rusqlite::Connection;

const fn meta(id: &'static str) -> CheckMeta {
    CheckMeta {
        id,
        label: id,
        description: "",
        category: "metadata",
        cost: Cost::Cheap,
        runs_during_scan: false,
        fix_kind: FixKind::None,
        bulk_action_label: None,
    }
}

static PER_ALBUM: CheckMeta = meta("filename-mismatch");
static WHOLE: CheckMeta = meta("whole-library");
static NET: CheckMeta = meta("tags-out-of-sync");

type Seen = Arc<Mutex<Vec<(&'static str, Vec<String>)>>>;

/// Records the albums of each run; emits one finding per album.
struct Recording {
    meta: &'static CheckMeta,
    granularity: Granularity,
    seen: Seen,
    refresh: Option<Duration>,
    /// While set, computing this check's inputs panics.
    panic: Arc<AtomicBool>,
    /// While set, a run stays inside `run` after recording that it began
    /// (an uninterruptible batch: the sweep only stops between batches).
    hold: Arc<AtomicBool>,
}

impl Check for Recording {
    fn meta(&self) -> &CheckMeta {
        self.meta
    }
    fn granularity(&self) -> Granularity {
        self.granularity
    }
    fn refresh_after(&self) -> Option<Duration> {
        self.refresh
    }
    fn input_extras(&self, _ctx: &CheckCtx<'_>) -> Result<String, CheckError> {
        assert!(!self.panic.load(Ordering::SeqCst), "injected panic");
        Ok(String::new())
    }
    fn run(
        &self,
        _ctx: &CheckCtx<'_>,
        scope: &CheckScope,
        emit: &mut Emit<'_>,
    ) -> Result<(), CheckError> {
        let ids: Vec<String> = scope.albums.iter().map(|a| a.id.clone()).collect();
        self.seen.lock().unwrap().push((self.meta.id, ids.clone()));
        while self.hold.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(2));
        }
        for id in ids {
            emit(Finding {
                summary: format!("{} finding", self.meta.id),
                album_id: Some(id),
                severity: Severity::Info,
                detail: None,
            })?;
        }
        Ok(())
    }
}

/// A network check: asks the resolver for every album, as tags-out-of-sync
/// does, and refuses to record anything once the resolver was stopped.
struct Network {
    seen: Seen,
}

impl Check for Network {
    fn meta(&self) -> &CheckMeta {
        &NET
    }
    fn uses_network(&self) -> bool {
        true
    }
    fn run(
        &self,
        ctx: &CheckCtx<'_>,
        scope: &CheckScope,
        emit: &mut Emit<'_>,
    ) -> Result<(), CheckError> {
        let ids: Vec<String> = scope.albums.iter().map(|a| a.id.clone()).collect();
        self.seen.lock().unwrap().push((NET.id, ids.clone()));
        for id in ids {
            let r = ctx.env.resolver.resolve(ctx.conn, &id);
            if ctx.env.resolver.interrupted() {
                return Err(CheckError::Interrupted);
            }
            if r.is_err() {
                emit(Finding {
                    summary: "no resolution".into(),
                    album_id: Some(id),
                    severity: Severity::Info,
                    detail: None,
                })?;
            }
        }
        Ok(())
    }
}

/// A resolver that blocks (as a background request waiting for a quiet
/// slot) until released or stopped.
struct Gate {
    release: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    waiting: Arc<AtomicBool>,
}

impl ResolutionSource for Gate {
    fn resolve(&self, _conn: &Connection, _album_id: &str) -> Result<Resolution, String> {
        self.waiting.store(true, Ordering::SeqCst);
        while !self.release.load(Ordering::SeqCst) {
            if self.stop.load(Ordering::SeqCst) {
                self.waiting.store(false, Ordering::SeqCst);
                return Err("stopped".into());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        self.waiting.store(false, Ordering::SeqCst);
        Ok(Resolution::default())
    }
}

#[derive(Default)]
struct Lines(Mutex<Vec<String>>);

impl HostLog for Lines {
    fn info(&self, _t: &str, m: &str) {
        self.0.lock().unwrap().push(format!("INFO {m}"));
    }
    fn warn(&self, _t: &str, m: &str) {
        self.0.lock().unwrap().push(format!("WARN {m}"));
    }
    fn error(&self, _t: &str, m: &str) {
        self.0.lock().unwrap().push(format!("ERROR {m}"));
    }
}

struct Rig {
    _tmp: tempfile::TempDir,
    db: PathBuf,
    lib: String,
    sweeper: CheckSweeper,
    seen: Seen,
    busy: Arc<AtomicBool>,
    release: Arc<AtomicBool>,
    waiting: Arc<AtomicBool>,
    libdir: PathBuf,
    panic: Arc<AtomicBool>,
    /// Holds the whole-library check's run (see `Recording::hold`).
    hold: Arc<AtomicBool>,
    log: Arc<Lines>,
    /// Every `ChecksRerunFinished` the sweep published, in order.
    finished: Arc<Mutex<Vec<Event>>>,
}

/// How a rig differs from the default.
struct Opts {
    network: bool,
    refresh_tick: Duration,
    /// The whole-library check's refresh window.
    refresh: Option<Duration>,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            network: true,
            refresh_tick: Duration::from_secs(3600),
            refresh: None,
        }
    }
}

/// How long a test lets the sweep take to stop. The sweep stops between
/// batches, never inside one (only a network wait is interruptible), so a
/// stop can wait for a whole check run: a few dozen database commits, each
/// as slow as the runner's disk makes it (Windows CI has shown stalls of
/// seconds). What is being waited for is the end of that run, not a
/// property of the sweep, so the bound only has to catch a hang: the same
/// 20 s every other wait in this file allows. [`wait_for`] is the matching
/// bound for conditions; the promptness of a stop is asserted separately, on
/// a network wait that writes nothing
/// (`shutdown_is_prompt_during_a_network_wait`).
const STOP_BUDGET: Duration = Duration::from_secs(20);

/// Stop the sweep and say what it was doing if it does not stop.
fn stop(r: &Rig) {
    let stopped = r.sweeper.stop_within(STOP_BUDGET);
    assert!(
        stopped,
        "the sweep did not stop within {STOP_BUDGET:?}: {:?}, log {:?}",
        r.sweeper.state(),
        r.log.0.lock().unwrap()
    );
}

const ALBUMS: [&str; 3] = ["a1", "a2", "a3"];

fn conn(p: &Path) -> Connection {
    musaic_db::open_connection(p).unwrap()
}

fn rig() -> Rig {
    rig_with(Opts::default())
}

/// Three Present albums and one Missing one, in a library that was scanned.
fn rig_with(o: Opts) -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let libdir = tmp.path().join("lib");
    std::fs::create_dir_all(&libdir).unwrap();
    let lib = musaic_scan::py_resolve(&libdir)
        .to_string_lossy()
        .into_owned();
    let db = tmp.path().join("library.db");
    musaic_db::Db::open_or_create(&db).unwrap();
    let c = conn(&db);
    c.execute(
        "INSERT INTO libraries(path, scanned_at) VALUES (?1, '2001-01-01T00:00:00Z')",
        [&lib],
    )
    .unwrap();
    for id in ALBUMS {
        c.execute(
            "INSERT INTO albums(id, library_path, path, dir_name, track_count) \
             VALUES (?1, ?2, ?3, ?1, 2)",
            rusqlite::params![id, lib, format!("{lib}/{id}")],
        )
        .unwrap();
    }
    c.execute(
        "INSERT INTO albums(id, library_path, path, dir_name, track_count, missing_since) \
         VALUES ('gone', ?1, ?2, 'gone', 2, '2001-01-02T00:00:00Z')",
        rusqlite::params![lib, format!("{lib}/gone")],
    )
    .unwrap();
    let seen: Seen = Arc::default();
    let panic: Arc<AtomicBool> = Arc::default();
    let hold: Arc<AtomicBool> = Arc::default();
    let log: Arc<Lines> = Arc::default();
    let finished: Arc<Mutex<Vec<Event>>> = Arc::default();
    let sweeper = CheckSweeper::new();
    let release = Arc::new(AtomicBool::new(true));
    let waiting = Arc::new(AtomicBool::new(false));
    let registry = Registry::new(vec![
        // Registry order puts the network check first; the sweep runs it last.
        Arc::new(Network { seen: seen.clone() }),
        Arc::new(Recording {
            meta: &PER_ALBUM,
            granularity: Granularity::Album,
            seen: seen.clone(),
            refresh: None,
            panic: panic.clone(),
            hold: Arc::default(),
        }),
        Arc::new(Recording {
            meta: &WHOLE,
            granularity: Granularity::Library,
            seen: seen.clone(),
            refresh: o.refresh,
            panic: Arc::default(),
            hold: hold.clone(),
        }),
    ]);
    let env = CheckEnv {
        registry,
        resolver: Arc::new(StoppableResolution {
            inner: Arc::new(Gate {
                release: release.clone(),
                stop: sweeper.stop_flag(),
                waiting: waiting.clone(),
            }),
            stop: sweeper.stop_flag(),
        }),
        clock: Arc::new(SystemClock),
        notify: Arc::new(|| {}),
        publish: {
            let finished = finished.clone();
            Arc::new(move |ev| finished.lock().unwrap().push(ev))
        },
        conversion_logs: None,
    };
    let busy = Arc::new(AtomicBool::new(false));
    let b2 = busy.clone();
    let db2 = db.clone();
    let lib2 = lib.clone();
    sweeper.start(SweepDeps {
        open_db: Arc::new(move || musaic_db::open_connection(&db2).map_err(|e| e.to_string())),
        library_dir: Arc::new(move || Some(lib2.clone())),
        env,
        busy: Arc::new(move || b2.load(Ordering::SeqCst)),
        now: Arc::new(SystemTime::now),
        log: log.clone(),
        settle: Duration::from_millis(10),
        network: o.network,
        refresh_tick: o.refresh_tick,
    });
    Rig {
        _tmp: tmp,
        db,
        lib,
        sweeper,
        seen,
        busy,
        release,
        waiting,
        libdir,
        panic,
        hold,
        log,
        finished,
    }
}

fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Wait for the sweep to finish what it has to do.
fn settle(r: &Rig) {
    // A wake is pending or a pass runs; then a short quiet period.
    let mut quiet_since = Instant::now();
    wait_for("the sweep to go idle", || {
        if r.sweeper.is_busy() {
            quiet_since = Instant::now();
        }
        quiet_since.elapsed() > Duration::from_millis(100)
    });
}

fn runs(r: &Rig) -> Vec<(String, String, String)> {
    let c = conn(&r.db);
    let mut s = c
        .prepare("SELECT check_id, scope, status FROM check_runs ORDER BY id")
        .unwrap();
    s.query_map([], |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn inputs(r: &Rig) -> Vec<(String, String)> {
    let c = conn(&r.db);
    musaic_db::check_inputs::rows_for_library(&c, &r.lib)
        .unwrap()
        .into_iter()
        .map(|x| (x.check_id, x.album_id))
        .collect()
}

fn ids(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

// A first pass runs every check on every Present album (local checks
// first, the network one last), records what each ran against, and a pass
// with nothing changed runs nothing. A Missing album is never checked.
#[test]
fn first_pass_runs_everything_then_nothing_is_stale() {
    let r = rig();
    settle(&r);
    let seen = r.seen.lock().unwrap().clone();
    assert_eq!(
        seen,
        vec![
            ("filename-mismatch", ids(&ALBUMS)),
            ("whole-library", ids(&ALBUMS)),
            ("tags-out-of-sync", ids(&ALBUMS)),
        ]
    );
    let mut want: Vec<(String, String)> = Vec::new();
    for a in ALBUMS {
        want.push(("filename-mismatch".into(), a.into()));
        want.push(("tags-out-of-sync".into(), a.into()));
    }
    want.push(("whole-library".into(), String::new()));
    want.sort();
    assert_eq!(inputs(&r), want);
    assert!(runs(&r).iter().all(|(_, _, s)| s == "completed"));
    assert_eq!(r.sweeper.state().status, SweepStatus::Idle);
    let st = r.sweeper.state();
    assert_eq!((st.total, st.completed), (7, 7));
    // One event for the whole pass, once its findings are written; the
    // whole-library run makes it library-wide.
    assert_eq!(
        *r.finished.lock().unwrap(),
        [Event::ChecksRerunFinished {
            checks: ids(&["filename-mismatch", "whole-library", "tags-out-of-sync"]),
            failed: vec![],
            album_ids: vec![],
        }]
    );

    // Nothing changed: a wake plans nothing.
    let before = runs(&r).len();
    r.sweeper.wake();
    settle(&r);
    assert_eq!(runs(&r).len(), before);
    assert_eq!(
        r.finished.lock().unwrap().len(),
        1,
        "a pass that ran nothing publishes nothing"
    );
    assert!(
        !r.seen
            .lock()
            .unwrap()
            .iter()
            .any(|(_, a)| a.contains(&"gone".to_string()))
    );
    stop(&r);
}

// An album whose row changed is rerun alone by the per-album checks;
// a whole-library check reruns library-wide.
#[test]
fn a_changed_album_reruns_alone() {
    let r = rig();
    settle(&r);
    r.seen.lock().unwrap().clear();
    conn(&r.db)
        .execute("UPDATE albums SET title = 'New' WHERE id = 'a2'", [])
        .unwrap();
    r.sweeper.on_event(&Event::AlbumTagsChanged {
        album_id: "a2".into(),
        fields: vec!["title".into()],
    });
    settle(&r);
    assert_eq!(
        r.seen.lock().unwrap().clone(),
        vec![
            ("filename-mismatch", ids(&["a2"])),
            ("whole-library", ids(&ALBUMS)),
            ("tags-out-of-sync", ids(&["a2"])),
        ]
    );
    assert_eq!(r.finished.lock().unwrap().len(), 2, "one event per pass");
    // A setting a check depends on makes it stale everywhere.
    r.seen.lock().unwrap().clear();
    r.sweeper.on_event(&Event::SettingsChanged {
        keys: vec!["dir_template_single_artist".into()],
    });
    settle(&r);
    assert_eq!(
        r.seen.lock().unwrap().clone(),
        vec![("filename-mismatch", ids(&ALBUMS))]
    );
    assert_eq!(
        r.finished.lock().unwrap().last(),
        Some(&Event::ChecksRerunFinished {
            checks: ids(&["filename-mismatch"]),
            failed: vec![],
            album_ids: ids(&ALBUMS),
        })
    );
    // A setting no check depends on changes nothing.
    r.seen.lock().unwrap().clear();
    r.sweeper.on_event(&Event::SettingsChanged {
        keys: vec!["snapshot_retention_count".into()],
    });
    settle(&r);
    assert!(r.seen.lock().unwrap().is_empty());
    stop(&r);
}

// While the app is busy (a scan, other work, a restore) the sweep waits
// before it even plans, without showing progress (the work it waits for
// shows its own); it goes on once the app is free.
#[test]
fn waits_while_the_app_is_busy() {
    let r = rig();
    settle(&r);
    r.seen.lock().unwrap().clear();
    r.busy.store(true, Ordering::SeqCst);
    let n = runs(&r).len();
    r.sweeper.on_event(&Event::SettingsChanged {
        keys: vec!["filename_template_single_artist".into()],
    });
    wait_for("a pass to start", || r.sweeper.is_busy());
    std::thread::sleep(Duration::from_millis(200));
    assert!(r.seen.lock().unwrap().is_empty(), "ran while busy");
    assert_eq!(runs(&r).len(), n);
    assert_eq!(r.sweeper.state().status, SweepStatus::Idle);
    r.busy.store(false, Ordering::SeqCst);
    settle(&r);
    assert_eq!(
        r.seen.lock().unwrap().clone(),
        vec![("filename-mismatch", ids(&ALBUMS))]
    );
    stop(&r);
}

// A suspension (what a restore does) stops a network wait at once: the
// interrupted run is failed, records no finding and no inputs for its
// albums, no batch starts while suspended, and the sweep finishes the work
// once resumed.
#[test]
fn a_suspension_interrupts_a_network_wait_and_records_nothing_from_it() {
    let r = rig();
    settle(&r);
    r.release.store(false, Ordering::SeqCst);
    conn(&r.db)
        .execute(
            "DELETE FROM check_inputs WHERE check_id = 'tags-out-of-sync'",
            [],
        )
        .unwrap();
    let findings_before: i64 = conn(&r.db)
        .query_row(
            "SELECT COUNT(*) FROM check_findings WHERE check_id = 'tags-out-of-sync'",
            [],
            |x| x.get(0),
        )
        .unwrap();
    r.sweeper.wake();
    wait_for("the network wait", || r.waiting.load(Ordering::SeqCst));
    let t0 = Instant::now();
    let held = r.sweeper.suspend();
    assert!(t0.elapsed() < Duration::from_secs(5), "suspend was slow");
    assert!(!r.waiting.load(Ordering::SeqCst));
    let last = runs(&r).pop().unwrap();
    assert_eq!(
        (last.0.as_str(), last.2.as_str()),
        ("tags-out-of-sync", "failed")
    );
    let c = conn(&r.db);
    let err: String = c
        .query_row(
            "SELECT error FROM check_runs ORDER BY id DESC LIMIT 1",
            [],
            |x| x.get(0),
        )
        .unwrap();
    assert_eq!(err, "check run interrupted");
    assert!(
        !inputs(&r).iter().any(|(c, _)| c == "tags-out-of-sync"),
        "inputs recorded from an interrupted run"
    );
    let findings_after: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM check_findings WHERE check_id = 'tags-out-of-sync' \
             AND resolved_at IS NULL",
            [],
            |x| x.get(0),
        )
        .unwrap();
    assert_eq!(findings_after, findings_before, "findings changed");
    // Suspended: nothing starts.
    let n = runs(&r).len();
    r.sweeper.wake();
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(runs(&r).len(), n, "a batch ran while suspended");
    // Resumed: the work is done.
    r.release.store(true, Ordering::SeqCst);
    drop(held);
    settle(&r);
    let mut got: Vec<String> = inputs(&r)
        .into_iter()
        .filter(|(c, _)| c == "tags-out-of-sync")
        .map(|(_, a)| a)
        .collect();
    got.sort();
    assert_eq!(got, ids(&ALBUMS));
    stop(&r);
}

// Shutdown stops a sweep waiting on the network promptly.
#[test]
fn shutdown_is_prompt_during_a_network_wait() {
    let r = rig();
    settle(&r);
    r.release.store(false, Ordering::SeqCst);
    conn(&r.db).execute("DELETE FROM check_inputs", []).unwrap();
    r.sweeper.wake();
    wait_for("the network wait", || r.waiting.load(Ordering::SeqCst));
    let t0 = Instant::now();
    assert!(r.sweeper.stop_within(Duration::from_secs(5)));
    assert!(t0.elapsed() < Duration::from_secs(2), "{:?}", t0.elapsed());
    // Stopped for good: a wake does nothing.
    let n = runs(&r).len();
    r.sweeper.wake();
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(runs(&r).len(), n);
}

// An unmounted library (its folder gone) is not checked at all: nothing
// runs, nothing is recorded; once it is back, the work is done.
#[test]
fn an_absent_library_is_not_checked() {
    let r = rig();
    settle(&r);
    let away = r.libdir.with_extension("away");
    std::fs::rename(&r.libdir, &away).unwrap();
    conn(&r.db).execute("DELETE FROM check_inputs", []).unwrap();
    let n = runs(&r).len();
    r.sweeper.wake();
    settle(&r);
    assert_eq!(runs(&r).len(), n, "checks ran on an absent library");
    assert!(inputs(&r).is_empty());
    std::fs::rename(&away, &r.libdir).unwrap();
    r.sweeper.wake();
    settle(&r);
    assert_eq!(inputs(&r).len(), 7);
    stop(&r);
}

// Offline, checks that use the network are not planned.
#[test]
fn offline_plans_no_network_check() {
    let r = rig_with(Opts {
        network: false,
        ..Opts::default()
    });
    settle(&r);
    let checks: Vec<&str> = r.seen.lock().unwrap().iter().map(|(c, _)| *c).collect();
    assert_eq!(checks, ["filename-mismatch", "whole-library"]);
    assert!(!inputs(&r).iter().any(|(c, _)| c == "tags-out-of-sync"));
    stop(&r);
}

fn count(r: &Rig, check: &str) -> usize {
    r.seen
        .lock()
        .unwrap()
        .iter()
        .filter(|(c, _)| *c == check)
        .count()
}

// Without any event, a result past its refresh window is redone on the
// periodic look; a look with nothing else stale runs nothing else.
#[test]
fn refresh_windows_fire_without_events() {
    let r = rig_with(Opts {
        refresh_tick: Duration::from_millis(100),
        refresh: Some(Duration::from_secs(1)),
        ..Opts::default()
    });
    settle(&r);
    // The window is one second and `checked_at` is truncated to whole
    // seconds, so a result can be past its window by the time the sweep has
    // settled, on a slow runner or a fast one. How many runs have happened
    // by now is therefore not asserted, only that there was one.
    assert!(count(&r, "whole-library") >= 1);
    // checked_at has a one-second resolution: this takes a few seconds.
    wait_for("the refresh", || count(&r, "whole-library") >= 2);
    // Nothing else had a window, so nothing else is rerun, however late
    // the refresh was.
    assert_eq!(count(&r, "filename-mismatch"), 1);
    assert_eq!(count(&r, "tags-out-of-sync"), 1);
    // The refresh run is likely still going: the stop waits for it.
    stop(&r);
}

// A stop does not interrupt a check run that has begun: it takes effect
// when the batch ends. (This is why `stop` allows a run's duration.)
#[test]
fn a_stop_waits_for_the_running_batch_and_then_completes() {
    let r = rig_with(Opts {
        refresh_tick: Duration::from_millis(100),
        refresh: Some(Duration::from_secs(1)),
        ..Opts::default()
    });
    settle(&r);
    r.hold.store(true, Ordering::SeqCst);
    let before = count(&r, "whole-library");
    wait_for("a refresh run to begin", || {
        count(&r, "whole-library") > before
    });
    let sweeper = r.sweeper.clone();
    let stopping = std::thread::spawn(move || sweeper.stop_within(STOP_BUDGET));
    // The run is held, so the stop cannot have completed; no deadline can
    // make this assertion flaky, only a stop that interrupts a batch.
    std::thread::sleep(Duration::from_millis(200));
    assert!(!stopping.is_finished(), "stopped inside a running batch");
    r.hold.store(false, Ordering::SeqCst);
    assert!(
        stopping.join().unwrap(),
        "did not stop once the batch ended"
    );
}

// A panic while planning is logged, leaves the sweep idle and its thread
// alive: the next wake works.
#[test]
fn a_panic_while_planning_does_not_kill_the_sweep() {
    let r = rig();
    settle(&r);
    r.panic.store(true, Ordering::SeqCst);
    conn(&r.db).execute("DELETE FROM check_inputs", []).unwrap();
    r.seen.lock().unwrap().clear();
    r.sweeper.wake();
    settle(&r);
    assert!(r.seen.lock().unwrap().is_empty());
    assert_eq!(r.sweeper.state().status, SweepStatus::Idle);
    {
        let lines = r.log.0.lock().unwrap();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("pass failed") && l.contains("injected panic")),
            "{lines:?}"
        );
    }
    r.panic.store(false, Ordering::SeqCst);
    r.sweeper.wake();
    settle(&r);
    assert_eq!(inputs(&r).len(), 7);
    stop(&r);
}

// Right before a batch, the sweep asks again what is stale: albums another
// run refreshed while it waited are dropped, and nothing is left when all
// of them were.
#[test]
fn still_stale_drops_what_another_run_refreshed() {
    let r = rig();
    settle(&r);
    stop(&r);
    let seen: Seen = Arc::default();
    let env = CheckEnv {
        registry: Registry::new(vec![Arc::new(Recording {
            meta: &PER_ALBUM,
            granularity: Granularity::Album,
            seen,
            refresh: None,
            panic: Arc::default(),
            hold: Arc::default(),
        })]),
        resolver: Arc::new(musaic_checks::NoResolver),
        clock: Arc::new(SystemClock),
        notify: Arc::new(|| {}),
        publish: Arc::new(|_| {}),
        conversion_logs: None,
    };
    let c = conn(&r.db);
    let now = SystemTime::now();
    let check = |v: &[&str]| {
        musaic_checks::inputs::still_stale(
            &c,
            &env,
            &r.lib,
            "filename-mismatch",
            Some(&ids(v)),
            now,
        )
        .unwrap()
    };
    assert_eq!(check(&["a1", "a2"]), None);
    c.execute("UPDATE albums SET title = 'New' WHERE id = 'a1'", [])
        .unwrap();
    assert_eq!(check(&["a1", "a2"]), Some(ids(&["a1"])));
    // Another run (a user's, a post-scan rerun) checks a1 meanwhile.
    musaic_checks::run_check(
        &c,
        &env,
        "filename-mismatch",
        &r.lib,
        musaic_checks::ScopeKind::Albums,
        Some(&ids(&["a1"])),
    )
    .unwrap();
    assert_eq!(check(&["a1", "a2"]), None);
    // A Missing album is dropped.
    assert_eq!(check(&["gone"]), None);
}
