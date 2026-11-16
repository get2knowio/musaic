//! The background check sweep: every health check runs on its own,
//! rerunning only what is stale (issue #187).
//!
//! **What is stale.** Every completed check run records a digest of what it
//! ran against in `check_inputs` (musaic-checks' `inputs` module, schema
//! v19). A pass plans from those rows ([`musaic_checks::inputs::plan_stale`]):
//! a check is rerun for the albums whose inputs changed (or for the whole
//! library, for a check that reads all of it), and once its result is older
//! than its refresh window when it depends on time or on MusicBrainz. Nothing
//! changed, nothing runs; a restart resumes where the last pass stopped,
//! since every finished batch is already recorded. Missing albums are never
//! planned (rule 6): their findings and rows stay as they are.
//!
//! **When.** The sweep is a bus subscriber (`_check_sweep_wake` in
//! musaic-events' catalog, rule 1): every event wakes it, and
//! [`CheckSweeper::start`] wakes it once at startup. Wakes are coalesced: a
//! pass starts once [`SweepDeps::settle`] has passed without another one, so
//! a burst of events (a scan, the fingerprint pool) is one pass. A
//! `SettingsChanged` naming a setting a check depends on also forgets that
//! check's rows (every album stale), as the AcoustID key cannot be part of
//! a digest.
//!
//! **Yielding.** Before each batch the sweep waits while
//! [`SweepDeps::busy`] (a scan, a restore, any work holding the work gate:
//! applies, bulk runs, user-started check runs). Its network requests go
//! through the background path ([`crate::production::Network::background`]),
//! which takes a MusicBrainz slot only when no foreground request wants it.
//! Checks that use the network run last, in small batches.
//!
//! **What it writes.** Only what a check run writes: findings, `check_runs`
//! rows and `check_inputs` rows, plus what the resolver stores while a check
//! resolves (anchors and fingerprints, as on any run of that check). It
//! never writes tags or files: no fix runs here. Its runs are made with
//! `in_post_scan_rerun` set, so events they publish (an anchor stored) do not
//! start the bus-driven reruns; they wake the sweep, which reruns what they
//! made stale.
//!
//! **Stopping.** The sweep takes no work guard: it never makes a restore wait
//! with a 409 or a quit ask for confirmation. A restore calls
//! [`CheckSweeper::suspend`] instead, which stops a request in flight, waits
//! for the batch in progress (at most one batch of local work) and for the
//! sweep's database connection to close, and keeps the sweep out until the
//! restore ends. [`CheckSweeper::stop_within`] (shutdown) stops it for good.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use musaic_checks::inputs::{StaleWork, plan_stale};
use musaic_checks::{CheckEnv, Granularity, ScopeKind};
use musaic_events::Event;
use rusqlite::Connection;

use crate::fingerprint_pool::ChangedEvent;
use crate::host_log::HostLog;
use crate::json::Json;

/// The logger name of the sweep's lines.
pub const SWEEP_TARGET: &str = "musaic.checks.sweep";

/// How long the sweep waits after the last wake before it plans a pass.
pub const SETTLE: Duration = Duration::from_secs(2);
/// How often a waiting sweep looks again at the app's busy state.
pub const POLL: Duration = Duration::from_millis(250);
/// Albums per run of a check that uses the network: a small batch keeps a
/// stop prompt and the progress honest.
pub const NETWORK_BATCH: usize = 5;
/// Albums per run of a local check.
pub const LOCAL_BATCH: usize = 50;

/// Which checks a setting changes the results of (by setting name).
pub fn checks_for_setting(key: &str) -> &'static [&'static str] {
    match key {
        "filename_template_single_artist"
        | "filename_template_compilation"
        | "dir_template_single_artist"
        | "dir_template_compilation" => &["filename-mismatch"],
        // The resolver asks AcoustID only with a key; a new key can find
        // anchors the old one could not.
        "acoustid_api_key" => &["tags-out-of-sync"],
        _ => &[],
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepStatus {
    /// Nothing stale (or nothing to check).
    Idle,
    /// Running checks.
    Running,
    /// Has stale work but waits for the app (a scan, a restore, other work).
    Waiting,
}

impl SweepStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            SweepStatus::Idle => "idle",
            SweepStatus::Running => "running",
            SweepStatus::Waiting => "waiting",
        }
    }
}

/// The sweep's progress (`GET /api/checks/sweep`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepState {
    pub status: SweepStatus,
    /// Units of work in this pass (an album of one check, or one
    /// library-wide run) and how many are done.
    pub total: usize,
    pub completed: usize,
    /// The check running now.
    pub current_check: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

impl Default for SweepState {
    fn default() -> Self {
        SweepState {
            status: SweepStatus::Idle,
            total: 0,
            completed: 0,
            current_check: None,
            started_at: None,
            completed_at: None,
        }
    }
}

impl SweepState {
    pub fn to_json(&self) -> Json {
        Json::obj([
            ("status", Json::from(self.status.as_str())),
            ("total", Json::from(self.total)),
            ("completed", Json::from(self.completed)),
            ("current_check", Json::from(self.current_check.clone())),
            ("started_at", Json::from(self.started_at.clone())),
            ("completed_at", Json::from(self.completed_at.clone())),
        ])
    }
}

/// What the sweep needs from its host.
#[derive(Clone)]
pub struct SweepDeps {
    /// A new connection to the library database.
    pub open_db: Arc<dyn Fn() -> Result<Connection, String> + Send + Sync>,
    /// The configured library directory, if any.
    pub library_dir: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    /// The checks environment the sweep's runs use: its resolver must go
    /// through the background network path and stop with
    /// [`CheckSweeper::stop_flag`] (`production::StoppableResolution`).
    pub env: CheckEnv,
    /// True while the sweep must not start a batch.
    pub busy: Arc<dyn Fn() -> bool + Send + Sync>,
    /// The clock (staleness windows, `started_at`).
    pub now: Arc<dyn Fn() -> SystemTime + Send + Sync>,
    pub log: Arc<dyn HostLog>,
    /// See [`SETTLE`] (tests shorten it).
    pub settle: Duration,
}

#[derive(Default)]
struct Ctl {
    wake: bool,
    last_wake: Option<Instant>,
    shutdown: bool,
    suspended: usize,
    /// A batch is running.
    in_batch: bool,
    /// The sweep holds a database connection.
    conn_open: bool,
    /// Checks to mark stale everywhere at the next pass.
    forget: BTreeSet<String>,
    /// A pass is in progress (planned, not yet finished).
    in_pass: bool,
}

struct Inner {
    state: Mutex<SweepState>,
    changed: ChangedEvent,
    ctl: Mutex<Ctl>,
    cv: Condvar,
    /// Set to stop network requests at once (shutdown, suspension).
    stop: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Inner {
    fn ctl(&self) -> MutexGuard<'_, Ctl> {
        self.ctl.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn update(&self, f: impl FnOnce(&mut SweepState)) {
        {
            let mut s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let before = s.clone();
            f(&mut s);
            if *s == before {
                return;
            }
        }
        self.changed.set();
    }
}

/// The background check sweep. Clones share it.
#[derive(Clone)]
pub struct CheckSweeper {
    inner: Arc<Inner>,
}

impl Default for CheckSweeper {
    fn default() -> Self {
        Self::new()
    }
}

/// Why a pass ended early.
enum Halt {
    Shutdown,
    Suspended,
}

/// Keeps the sweep out while held ([`CheckSweeper::suspend`]).
#[must_use = "the sweep resumes when this is dropped"]
pub struct SweepSuspension {
    inner: Arc<Inner>,
}

impl Drop for SweepSuspension {
    fn drop(&mut self) {
        let mut c = self.inner.ctl();
        c.suspended = c.suspended.saturating_sub(1);
        if c.suspended == 0 && !c.shutdown {
            self.inner.stop.store(false, Ordering::SeqCst);
            // The database may be another one now: look again.
            c.wake = true;
            c.last_wake = Some(Instant::now());
        }
        drop(c);
        self.inner.cv.notify_all();
    }
}

impl CheckSweeper {
    pub fn new() -> Self {
        CheckSweeper {
            inner: Arc::new(Inner {
                state: Mutex::new(SweepState::default()),
                changed: ChangedEvent::default(),
                ctl: Mutex::new(Ctl::default()),
                cv: Condvar::new(),
                stop: Arc::new(AtomicBool::new(false)),
                thread: Mutex::new(None),
            }),
        }
    }

    /// The flag the sweep's network transport and resolver stop on.
    pub fn stop_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.inner.stop)
    }

    pub fn state(&self) -> SweepState {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Set after every change of [`Self::state`].
    pub fn changed(&self) -> &ChangedEvent {
        &self.inner.changed
    }

    /// Start the sweep thread (once; later calls do nothing) and wake it
    /// for a first pass.
    pub fn start(&self, deps: SweepDeps) {
        let mut t = self
            .inner
            .thread
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if t.is_some() || self.inner.ctl().shutdown {
            return;
        }
        let inner = Arc::clone(&self.inner);
        *t = Some(
            std::thread::Builder::new()
                .name("check-sweep".into())
                .spawn(move || run(&inner, &deps))
                .expect("spawn check sweep thread"),
        );
        drop(t);
        self.wake();
    }

    /// Ask for a pass (after [`SweepDeps::settle`] without another wake).
    pub fn wake(&self) {
        let mut c = self.inner.ctl();
        c.wake = true;
        c.last_wake = Some(Instant::now());
        drop(c);
        self.inner.cv.notify_all();
    }

    /// The bus subscriber's entry: note what `ev` makes stale that the
    /// stored digests cannot show (a setting such as the AcoustID key), and
    /// wake the sweep. Never blocks on a running pass.
    pub fn on_event(&self, ev: &Event) {
        if let Event::SettingsChanged { keys } = ev {
            let mut c = self.inner.ctl();
            for k in keys {
                c.forget
                    .extend(checks_for_setting(k).iter().map(|s| s.to_string()));
            }
        }
        self.wake();
    }

    /// Stop the request in flight, wait for the batch in progress to end and
    /// for the sweep's database connection to close, and keep the sweep out
    /// until the returned guard drops (then it plans again). For a restore,
    /// which replaces the database file.
    pub fn suspend(&self) -> SweepSuspension {
        let mut c = self.inner.ctl();
        c.suspended += 1;
        self.inner.stop.store(true, Ordering::SeqCst);
        self.inner.cv.notify_all();
        while c.in_batch || c.conn_open {
            c = self
                .inner
                .cv
                .wait(c)
                .unwrap_or_else(PoisonError::into_inner);
        }
        SweepSuspension {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Whether a pass is planned or running (tests).
    pub fn is_busy(&self) -> bool {
        let c = self.inner.ctl();
        c.in_pass || c.wake
    }

    /// Stop the sweep for good and wait up to `timeout` for its thread.
    /// `true` when it has exited (or never started).
    pub fn stop_within(&self, timeout: Duration) -> bool {
        {
            let mut c = self.inner.ctl();
            c.shutdown = true;
            self.inner.stop.store(true, Ordering::SeqCst);
        }
        self.inner.cv.notify_all();
        let handle = self
            .inner
            .thread
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let Some(handle) = handle else {
            return true;
        };
        let deadline = Instant::now() + timeout;
        while !handle.is_finished() {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = handle.join();
        true
    }
}

/// The sweep thread.
fn run(inner: &Inner, deps: &SweepDeps) {
    loop {
        // Wait for a wake, then for it to settle.
        {
            let mut c = inner.ctl();
            loop {
                if c.shutdown {
                    return;
                }
                if c.wake {
                    let since = c.last_wake.map_or(deps.settle, |t| t.elapsed());
                    if since >= deps.settle {
                        break;
                    }
                    c = inner
                        .cv
                        .wait_timeout(c, deps.settle - since)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0;
                } else {
                    c = inner.cv.wait(c).unwrap_or_else(PoisonError::into_inner);
                }
            }
            c.wake = false;
            c.in_pass = true;
        }
        let outcome = pass(inner, deps);
        // A suspended pass plans again once the restore is over (its
        // guard wakes the sweep).
        if let Err(Halt::Shutdown) = outcome {
            inner.ctl().in_pass = false;
            return;
        }
        inner.update(|s| {
            if s.status != SweepStatus::Idle {
                s.status = SweepStatus::Idle;
                s.current_check = None;
                s.completed_at = Some(musaic_db::backup::iso_seconds((deps.now)()));
            }
        });
        inner.ctl().in_pass = false;
    }
}

/// Wait until the sweep may start a batch: not busy, not suspended. With
/// `show`, the state says "waiting" meanwhile.
fn wait_ready(inner: &Inner, deps: &SweepDeps, show: bool) -> Result<(), Halt> {
    let mut shown = false;
    loop {
        {
            let c = inner.ctl();
            if c.shutdown {
                return Err(Halt::Shutdown);
            }
            if c.suspended > 0 {
                return Err(Halt::Suspended);
            }
        }
        if !(deps.busy)() {
            if shown {
                inner.update(|s| s.status = SweepStatus::Running);
            }
            return Ok(());
        }
        if show && !shown {
            inner.update(|s| s.status = SweepStatus::Waiting);
            shown = true;
        }
        let c = inner.ctl();
        if c.shutdown || c.suspended > 0 {
            continue;
        }
        drop(
            inner
                .cv
                .wait_timeout(c, POLL)
                .unwrap_or_else(PoisonError::into_inner),
        );
    }
}

/// Marks the connection open while it lives (a suspension waits for it).
struct ConnSlot<'a> {
    inner: &'a Inner,
    conn: Connection,
}

impl Drop for ConnSlot<'_> {
    fn drop(&mut self) {
        let mut c = self.inner.ctl();
        c.conn_open = false;
        drop(c);
        self.inner.cv.notify_all();
    }
}

/// Open the database unless the sweep is being suspended or stopped.
fn open<'a>(inner: &'a Inner, deps: &SweepDeps) -> Result<Option<ConnSlot<'a>>, Halt> {
    {
        let mut c = inner.ctl();
        if c.shutdown {
            return Err(Halt::Shutdown);
        }
        if c.suspended > 0 {
            return Err(Halt::Suspended);
        }
        c.conn_open = true;
    }
    match (deps.open_db)() {
        Ok(conn) => Ok(Some(ConnSlot { inner, conn })),
        Err(e) => {
            inner.ctl().conn_open = false;
            inner.cv.notify_all();
            deps.log.error(
                SWEEP_TARGET,
                &format!("check sweep: cannot open the database: {e}"),
            );
            Ok(None)
        }
    }
}

/// One pass: plan what is stale, then run it, batch by batch.
fn pass(inner: &Inner, deps: &SweepDeps) -> Result<(), Halt> {
    let Some(lib) = (deps.library_dir)().filter(|l| !l.is_empty()) else {
        return Ok(());
    };
    let lib = musaic_scan::py_resolve(Path::new(&lib))
        .to_string_lossy()
        .into_owned();
    wait_ready(inner, deps, false)?;
    let Some(slot) = open(inner, deps)? else {
        return Ok(());
    };
    let conn = &slot.conn;
    let forget: Vec<String> = std::mem::take(&mut inner.ctl().forget)
        .into_iter()
        .collect();
    for check_id in &forget {
        if let Err(e) = musaic_db::check_inputs::forget_check(conn, check_id) {
            deps.log.error(
                SWEEP_TARGET,
                &format!("check sweep: cannot mark {check_id} stale: {e}"),
            );
            // Keep it for the next pass.
            inner.ctl().forget.insert(check_id.clone());
        }
    }
    if let Err(e) = musaic_db::check_inputs::prune_orphans(conn) {
        deps.log.warn(
            SWEEP_TARGET,
            &format!("check sweep: cannot prune old input rows: {e}"),
        );
    }
    let mut plan: Vec<StaleWork> = match plan_stale(conn, &deps.env, &lib, (deps.now)()) {
        Ok(p) => p,
        Err(e) => {
            deps.log
                .error(SWEEP_TARGET, &format!("check sweep: cannot plan: {e}"));
            return Ok(());
        }
    };
    if plan.is_empty() {
        return Ok(());
    }
    // Local checks first (quick results); network checks last, in the
    // background path's gaps. Registry order otherwise.
    plan.sort_by_key(|w| w.uses_network);
    let total: usize = plan.iter().map(StaleWork::units).sum();
    let t0 = Instant::now();
    inner.update(|s| {
        *s = SweepState {
            status: SweepStatus::Running,
            total,
            completed: 0,
            current_check: None,
            started_at: Some(musaic_db::backup::iso_seconds((deps.now)())),
            completed_at: None,
        }
    });
    let mut failed = 0usize;
    for work in &plan {
        inner.update(|s| s.current_check = Some(work.check_id.to_string()));
        let batches: Vec<Option<&[String]>> = match work.granularity {
            Granularity::Library => vec![None],
            Granularity::Album => work
                .albums
                .chunks(if work.uses_network {
                    NETWORK_BATCH
                } else {
                    LOCAL_BATCH
                })
                .map(Some)
                .collect(),
        };
        for batch in batches {
            wait_ready(inner, deps, true)?;
            {
                let mut c = inner.ctl();
                if c.shutdown {
                    return Err(Halt::Shutdown);
                }
                if c.suspended > 0 {
                    return Err(Halt::Suspended);
                }
                c.in_batch = true;
            }
            let (kind, ids) = match batch {
                None => (ScopeKind::All, None),
                Some(ids) => (ScopeKind::Albums, Some(ids)),
            };
            // Events the run publishes (an anchor stored) must not start
            // the bus-driven reruns: they wake this sweep instead.
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                musaic_events::context::with_post_scan_rerun(|| {
                    musaic_checks::run_check(conn, &deps.env, work.check_id, &lib, kind, ids)
                })
            }));
            {
                let mut c = inner.ctl();
                c.in_batch = false;
            }
            inner.cv.notify_all();
            let units = batch.map_or(1, <[String]>::len);
            match r {
                Ok(Ok(run)) if run.status == "completed" => {}
                Ok(Ok(run)) => {
                    failed += 1;
                    if !inner.stop.load(Ordering::SeqCst) {
                        deps.log.warn(
                            SWEEP_TARGET,
                            &format!(
                                "check sweep: {} failed: {}",
                                work.check_id,
                                run.error.unwrap_or_default()
                            ),
                        );
                    }
                }
                Ok(Err(e)) => {
                    failed += 1;
                    deps.log.error(
                        SWEEP_TARGET,
                        &format!("check sweep: {} could not run: {e}", work.check_id),
                    );
                }
                Err(_) => {
                    failed += 1;
                    deps.log.error(
                        SWEEP_TARGET,
                        &format!("check sweep: {} panicked", work.check_id),
                    );
                }
            }
            inner.update(|s| s.completed = (s.completed + units).min(s.total));
        }
    }
    deps.log.info(
        SWEEP_TARGET,
        &format!(
            "check sweep: {total} item(s) checked in {}ms{}",
            t0.elapsed().as_millis(),
            if failed > 0 {
                format!(", {failed} run(s) failed")
            } else {
                String::new()
            }
        ),
    );
    drop(slot);
    Ok(())
}
