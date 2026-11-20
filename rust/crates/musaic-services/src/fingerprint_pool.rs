//! The eager fingerprint pool (`musaic/services/fingerprint_pool.py`).
//!
//! After a scan the host calls [`FingerprintPool::stop`] then
//! [`FingerprintPool::warm_library`]; the pool walks the library's albums
//! that have fewer fingerprint rows than tracks, one at a time, with a
//! 0.5 s pause between albums, on its own thread.
//!
//! Reference: `testdata/golden/fingerprint/pool.json`
//! (recorded from the Python app's own module). Kept from it:
//!
//! * the state record, its key order, and its values at every change
//!   ([`PoolState`]; the body of `GET /api/fingerprint-pool/status` in that
//!   order is `musaic-api`'s `render::system::pool_state`);
//! * statuses `idle | running | done | cancelled | unavailable`; a call
//!   while running returns the running state and starts nothing;
//!   "unavailable" stamps `started_at` then `completed_at`, fills
//!   `skip_reason` and starts nothing;
//! * the candidate query (no ORDER BY: SQLite's order); a failing album
//!   counts in `errors` and the walk goes on;
//! * one change notification after every state mutation, at the same
//!   points as the Python app's `_notify_changed()`, through [`ChangedEvent`],
//!   which reproduces the `asyncio.Event` the Python app's SSE handler waits on
//!   (it stays set until a waiter clears it, so a stream opened after a
//!   change sends the state twice: once on connect, once at once);
//! * the stream's frame, `sse_event("state", get_state())`, is rendered by the
//!   HTTP face (`musaic-api` `render::system`); its keepalive is
//!   [`KEEPALIVE_FRAME`] after [`KEEPALIVE_AFTER`].
//!
//! Decided changes:
//!
//! * "Unavailable" means the fingerprinter cannot run at
//!   all (the host's `availability` check), not "no AcoustID key".
//! * Albums holding fingerprints from another implementation (the Python
//!   app's fpcalc) are candidates too, so they are recomputed
//!   ([`musaic_db::fingerprints_ext::pool_candidates`]).
//! * Missing albums are not candidates.
//! * When the job can look up ([`AlbumJob::lookups_enabled`],
//!   read once at the start of the pass: an AcoustID key is set), albums
//!   with a row whose lookup is due at the pass's `started_at` are
//!   candidates too; the job looks them up without fingerprinting again.
//!   Retry storm guard: the candidate list is fixed at the start of a pass
//!   and each album is visited once, with the usual pause after it, so a
//!   failing album is asked at most once per pass; across passes a failed
//!   row waits by the backoff in [`crate::fingerprint::lookup_retry_delay_s`].
//!
//! Cancellation: [`FingerprintPool::stop`] returns once the pool thread has
//! exited. The Python app cancelled its task at the next `await` (the album being
//! fingerprinted keeps going in an abandoned thread, and is not counted);
//! here the album job is asked to stop between files (rows already stored
//! stay), it is not counted either, and the status becomes "cancelled".

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rusqlite::Connection;

use crate::clock::Clock;
use crate::fingerprint::CancelToken;

/// The pause between albums (`await asyncio.sleep(0.5)`).
pub const INTER_ALBUM_SLEEP: Duration = Duration::from_millis(500);
/// The stream's keepalive comment and how long it waits for a change first
/// (`asyncio.wait_for(ev.wait(), timeout=30.0)`, `": ping\n\n"`).
pub const KEEPALIVE_FRAME: &str = ": ping\n\n";
pub const KEEPALIVE_AFTER: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolStatus {
    Idle,
    Running,
    Done,
    Cancelled,
    Unavailable,
}

impl PoolStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            PoolStatus::Idle => "idle",
            PoolStatus::Running => "running",
            PoolStatus::Done => "done",
            PoolStatus::Cancelled => "cancelled",
            PoolStatus::Unavailable => "unavailable",
        }
    }
}

/// `fingerprint_pool._state`.
#[derive(Debug, Clone, PartialEq)]
pub struct PoolState {
    pub status: PoolStatus,
    pub library_dir: Option<String>,
    pub total: usize,
    pub completed: usize,
    pub errors: usize,
    pub current_album_id: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub skip_reason: Option<String>,
}

impl Default for PoolState {
    fn default() -> Self {
        Self {
            status: PoolStatus::Idle,
            library_dir: None,
            total: 0,
            completed: 0,
            errors: 0,
            current_album_id: None,
            started_at: None,
            completed_at: None,
            skip_reason: None,
        }
    }
}

/// The Python app's module-level `asyncio.Event`: `set` wakes every current
/// waiter and leaves the flag set; `wait` returns at once while it is set;
/// waiters clear it themselves. Listeners run on every `set` (to bridge to
/// an async runtime without parking a thread).
#[derive(Default)]
pub struct ChangedEvent {
    inner: Mutex<(bool, u64)>,
    cond: Condvar,
    listeners: Mutex<Vec<Arc<dyn Fn() + Send + Sync>>>,
}

impl ChangedEvent {
    fn lock(&self) -> MutexGuard<'_, (bool, u64)> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn set(&self) {
        {
            let mut g = self.lock();
            g.0 = true;
            g.1 = g.1.wrapping_add(1);
        }
        self.cond.notify_all();
        let listeners = self
            .listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        for l in listeners {
            l();
        }
    }

    pub fn clear(&self) {
        self.lock().0 = false;
    }

    pub fn is_set(&self) -> bool {
        self.lock().0
    }

    /// `await asyncio.wait_for(ev.wait(), timeout)`: `true` when the event
    /// is (or becomes) set, `false` on timeout.
    pub fn wait_timeout(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut g = self.lock();
        if g.0 {
            return true;
        }
        let start = g.1;
        while g.1 == start {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            g = self
                .cond
                .wait_timeout(g, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        true
    }

    /// Run `f` after every `set` (e.g. `tokio::sync::Notify::notify_waiters`).
    pub fn add_listener(&self, f: Arc<dyn Fn() + Send + Sync>) {
        self.listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(f);
    }
}

/// Why an album job did not complete.
#[derive(Debug, Clone, PartialEq)]
pub enum JobError {
    /// Counted in `errors` (the Python app: `FingerprintError` or any exception).
    Failed(String),
    /// The job saw the pool's cancel token; not counted.
    Cancelled,
}

/// The work for one album: `(conn, album_id, album_path, cancel)`.
pub type AlbumJobFn =
    dyn Fn(&Connection, &str, &str, &CancelToken) -> Result<(), JobError> + Send + Sync;

/// Fingerprint one album. The host builds it with [`fingerprint_job`] over
/// [`crate::fingerprint::fingerprint_album`]; tests stub it.
#[derive(Clone)]
pub struct AlbumJob {
    run: Arc<AlbumJobFn>,
    lookups: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl AlbumJob {
    /// A job that never looks anything up on AcoustID (lookups due do not
    /// make an album a candidate).
    pub fn new(
        run: impl Fn(&Connection, &str, &str, &CancelToken) -> Result<(), JobError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            run: Arc::new(run),
            lookups: Arc::new(|| false),
        }
    }

    /// The same job, which can look up on AcoustID whenever `enabled`
    /// returns true (failed lookups are retried later, by backoff).
    pub fn with_lookups(mut self, enabled: Arc<dyn Fn() -> bool + Send + Sync>) -> Self {
        self.lookups = enabled;
        self
    }

    pub fn run(
        &self,
        conn: &Connection,
        album_id: &str,
        path: &str,
        cancel: &CancelToken,
    ) -> Result<(), JobError> {
        (self.run)(conn, album_id, path, cancel)
    }

    /// Whether this job would look up on AcoustID now (a key is set).
    pub fn lookups_enabled(&self) -> bool {
        (self.lookups)()
    }
}

/// The pause between albums; returns `false` when cancelled while waiting.
pub trait PoolSleeper: Send + Sync {
    fn sleep(&self, d: Duration, cancel: &CancelToken) -> bool;
}

/// Real time, cut short by cancellation (polled every 10 ms).
#[derive(Debug, Default, Clone, Copy)]
pub struct RealSleeper;

impl PoolSleeper for RealSleeper {
    fn sleep(&self, d: Duration, cancel: &CancelToken) -> bool {
        let end = Instant::now() + d;
        loop {
            if cancel.is_cancelled() {
                return false;
            }
            let now = Instant::now();
            if now >= end {
                return true;
            }
            std::thread::sleep((end - now).min(Duration::from_millis(10)));
        }
    }
}

/// What a pool run needs.
#[derive(Clone)]
pub struct PoolDeps {
    pub open_db: Arc<dyn Fn() -> Result<Connection, String> + Send + Sync>,
    /// `Err(reason)` when the fingerprinter cannot run (status
    /// "unavailable", `skip_reason` = reason).
    pub availability: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    pub job: AlbumJob,
    /// `_utc_iso_now()`.
    pub clock: Clock,
    pub sleeper: Arc<dyn PoolSleeper>,
    /// `track_fingerprints.fingerprint_impl` of the running fingerprinter.
    pub fingerprint_impl: String,
}

struct Shared {
    state: Mutex<PoolState>,
    changed: ChangedEvent,
    task: Mutex<Option<(JoinHandle<()>, CancelToken)>>,
    pause: Mutex<Pause>,
}

/// [`FingerprintPool::pause`]'s bookkeeping.
#[derive(Default)]
struct Pause {
    /// Pauses held.
    holds: usize,
    /// The last pass started: what a pause that cut it short restarts.
    last: Option<(String, PoolDeps)>,
    /// A pass asked for (or cut short) while paused, started when the last
    /// pause ends.
    wanted: Option<(String, PoolDeps)>,
}

/// [`FingerprintPool::pause`]'s guard: no pass runs while it lives.
#[must_use = "the pool stays paused only while the guard is kept"]
pub struct PoolPause {
    pool: FingerprintPool,
}

impl Drop for PoolPause {
    fn drop(&mut self) {
        let wanted = {
            let mut p = self.pool.shared.lock_pause();
            p.holds = p.holds.saturating_sub(1);
            if p.holds == 0 { p.wanted.take() } else { None }
        };
        if let Some((lib, deps)) = wanted {
            self.pool.warm_library(&lib, deps);
        }
    }
}

/// Process-wide pool (`fingerprint_pool` module state). Clones share it.
#[derive(Clone)]
pub struct FingerprintPool {
    shared: Arc<Shared>,
}

impl Default for FingerprintPool {
    fn default() -> Self {
        Self::new()
    }
}

impl FingerprintPool {
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(PoolState::default()),
                changed: ChangedEvent::default(),
                task: Mutex::new(None),
                pause: Mutex::new(Pause::default()),
            }),
        }
    }

    /// Stop the pass in flight (as [`Self::stop`]) and start none until
    /// the guard drops; a pass asked for meanwhile, or the one cut short,
    /// starts then. For work that moves files between albums (a multi-disc
    /// conversion): a pass that wrote a fingerprint row under an album id
    /// the work just folded away would fail its foreign key.
    pub fn pause(&self) -> PoolPause {
        let was_running = {
            let mut p = self.shared.lock_pause();
            p.holds += 1;
            self.is_running()
        };
        self.stop();
        if was_running {
            let mut p = self.shared.lock_pause();
            if p.wanted.is_none() {
                p.wanted = p.last.clone();
            }
        }
        PoolPause { pool: self.clone() }
    }

    /// Whether a [`PoolPause`] is held.
    pub fn is_paused(&self) -> bool {
        self.shared.lock_pause().holds > 0
    }

    /// `get_state()`.
    pub fn state(&self) -> PoolState {
        self.shared.lock_state().clone()
    }

    pub fn is_running(&self) -> bool {
        self.shared.lock_state().status == PoolStatus::Running
    }

    /// `changed_event()`: what the stream handler waits on.
    pub fn changed(&self) -> &ChangedEvent {
        &self.shared.changed
    }

    /// Test seam: replace the state record (the Python app's tests write
    /// `_state` directly).
    pub fn set_state(&self, state: PoolState) {
        *self.shared.lock_state() = state;
    }

    /// `warm_library(library_dir)`: start a background pass unless one is
    /// running. Returns the state as it stands when the call returns (for a
    /// new pass: "running", total 0).
    pub fn warm_library(&self, library_dir: &str, deps: PoolDeps) -> PoolState {
        {
            let mut p = self.shared.lock_pause();
            if p.holds > 0 {
                p.wanted = Some((library_dir.to_owned(), deps));
                return self.state();
            }
            p.last = Some((library_dir.to_owned(), deps.clone()));
        }
        if self.is_running() {
            return self.state();
        }
        if let Err(reason) = (deps.availability)() {
            self.shared.update(|s| {
                s.status = PoolStatus::Unavailable;
                s.library_dir = Some(library_dir.to_owned());
                s.skip_reason = Some(reason);
                s.started_at = Some((deps.clock)());
                s.completed_at = Some((deps.clock)());
                s.total = 0;
                s.completed = 0;
                s.errors = 0;
                s.current_album_id = None;
            });
            return self.state();
        }
        self.shared.update(|s| {
            s.status = PoolStatus::Running;
            s.library_dir = Some(library_dir.to_owned());
            s.total = 0;
            s.completed = 0;
            s.errors = 0;
            s.current_album_id = None;
            s.started_at = Some((deps.clock)());
            s.completed_at = None;
            s.skip_reason = None;
        });
        let snapshot = self.state();
        let cancel = CancelToken::new();
        let shared = Arc::clone(&self.shared);
        let lib = library_dir.to_owned();
        let token = cancel.clone();
        let handle = std::thread::Builder::new()
            .name("fingerprint-pool".into())
            .spawn(move || run_pool(&shared, &lib, &deps, &token))
            .expect("spawn fingerprint pool thread");
        *self
            .shared
            .task
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some((handle, cancel));
        snapshot
    }

    /// `stop()`: cancel the pass in flight and return once its thread has
    /// exited. Nothing happens (no change, no notification) when none runs.
    pub fn stop(&self) {
        let task = self
            .shared
            .task
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some((handle, cancel)) = task {
            cancel.cancel();
            let _ = handle.join();
        }
    }

    /// [`Self::stop`] with a bound: cancel the pass in flight and wait up to
    /// `timeout` for its thread to exit. Returns `true` when no pass runs any
    /// more (none was running, or its thread exited in time); `false` when
    /// the thread is still inside one file after `timeout`. It is then left
    /// to finish on its own (it has been told to stop and ends at the next
    /// file boundary), and a later call does not wait for it again.
    pub fn stop_within(&self, timeout: Duration) -> bool {
        let task = self
            .shared
            .task
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let Some((handle, cancel)) = task else {
            return true;
        };
        cancel.cancel();
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

    /// Block until the current pass (if any) ends on its own. For tests and
    /// for a host that wants to wait without cancelling.
    pub fn join(&self) {
        let task = self
            .shared
            .task
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some((handle, _)) = task {
            let _ = handle.join();
        }
    }
}

impl Shared {
    fn lock_state(&self) -> MutexGuard<'_, PoolState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_pause(&self) -> MutexGuard<'_, Pause> {
        self.pause.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Mutate under the lock, then `_notify_changed()`.
    fn update(&self, f: impl FnOnce(&mut PoolState)) {
        f(&mut self.lock_state());
        self.changed.set();
    }
}

/// `_run_pool(library_dir)`.
fn run_pool(shared: &Shared, library_dir: &str, deps: &PoolDeps, cancel: &CancelToken) {
    let mut cancelled = false;
    // Lookups due at the pass's own start time (no extra clock read).
    let due_at = deps
        .job
        .lookups_enabled()
        .then(|| shared.lock_state().started_at.clone())
        .flatten();
    let candidates = (deps.open_db)().and_then(|conn| {
        let c = musaic_db::fingerprints_ext::pool_candidates(
            &conn,
            library_dir,
            &deps.fingerprint_impl,
            due_at.as_deref(),
        )
        .map_err(|e| e.to_string())?;
        Ok((conn, c))
    });
    // The Python app's candidate query raising ends the pass through its
    // `finally` (status "done"); nothing else is recorded.
    if let Ok((conn, candidates)) = candidates {
        shared.update(|s| s.total = candidates.len());
        for (album_id, path) in candidates {
            if cancel.is_cancelled() {
                cancelled = true;
                break;
            }
            shared.update(|s| s.current_album_id = Some(album_id.clone()));
            // `except Exception` in the Python app: a panicking job is an error
            // like any other, and the pass goes on.
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                deps.job.run(&conn, &album_id, &path, cancel)
            }))
            .unwrap_or_else(|_| Err(JobError::Failed("album job panicked".into())));
            match outcome {
                Ok(()) => shared.update(|s| s.completed += 1),
                Err(JobError::Failed(_)) => shared.update(|s| s.errors += 1),
                Err(JobError::Cancelled) => {
                    cancelled = true;
                    break;
                }
            }
            if !deps.sleeper.sleep(INTER_ALBUM_SLEEP, cancel) {
                cancelled = true;
                break;
            }
        }
    }
    if cancelled {
        shared.lock_state().status = PoolStatus::Cancelled;
    }
    shared.update(|s| {
        s.current_album_id = None;
        s.completed_at = Some((deps.clock)());
        if s.status == PoolStatus::Running {
            s.status = PoolStatus::Done;
        }
    });
}

/// The [`AlbumJob`] the app runs: [`crate::fingerprint::fingerprint_album`]
/// with a fresh AcoustID client per album (the key is read per album, as
/// the Python app read the setting per call). It can look up whenever a
/// non-empty key is set.
pub fn fingerprint_job<T>(
    transport: T,
    acoustid_key: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    fingerprinter: Arc<dyn crate::fingerprint::Fingerprinter>,
    bus: musaic_events::Bus,
    clock: Clock,
) -> AlbumJob
where
    T: musaic_net::Transport + Clone + Send + Sync + 'static,
{
    let key = Arc::clone(&acoustid_key);
    AlbumJob::new(move |conn, album_id, path, cancel| {
        let client = acoustid_key()
            .filter(|k| !k.is_empty())
            .map(|k| musaic_net::AcoustIdClient::new(transport.clone(), k));
        let now = clock();
        let env = crate::fingerprint::FingerprintEnv {
            conn,
            fingerprinter: fingerprinter.as_ref(),
            acoustid: client
                .as_ref()
                .map(|c| c as &dyn crate::fingerprint::AcoustIdLookup),
            bus: &bus,
            now_iso: &now,
            cancel: Some(cancel),
        };
        match crate::fingerprint::fingerprint_album(&env, album_id, path, false) {
            Ok(_) => Ok(()),
            Err(crate::fingerprint::FingerprintError::Cancelled { .. }) => Err(JobError::Cancelled),
            Err(e) => Err(JobError::Failed(e.to_string())),
        }
    })
    .with_lookups(Arc::new(move || key().is_some_and(|k| !k.is_empty())))
}
