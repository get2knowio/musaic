//! Background library scan: port of `musaic/services/scan.py` and the
//! `incremental_scan` /
//! `discover_album_dirs` half of `scanner.py`.
//!
//! # State machine
//!
//! `idle -> running -> done | aborted | cancelled | error`, at most one scan
//! at a time. [`ScanService::start_scan`] while a scan runs returns the
//! running state unchanged ([`StartScan::AlreadyRunning`]); otherwise it
//! resets the state and the event buffer and hands back a [`ScanJob`] the
//! host runs on a blocking thread ([`ScanService::start_scan_thread`] does
//! that with `std::thread`).
//!
//! `done`: the snapshot was saved. `aborted`: the sanity check judged the
//! walk untrustworthy (too many albums vanished at once) and nothing was
//! written. `cancelled`: [`ScanService::cancel`] stopped it and nothing was
//! written. `error`: the scan failed (a failed save is rolled back, so
//! nothing was written either). The Python app had no `aborted` /
//! `cancelled`: an aborted scan ended `done`, and a scan could not be
//! cancelled (deliberate change).
//!
//! # Cancelling
//!
//! [`ScanService::cancel`] sets a flag the worker reads at every directory
//! of the discovery walk and before every album; while the flag is set and
//! the scan has not ended, the status reads `cancelling` (still a running
//! scan: [`ScanService::is_running`] is true and a new scan cannot start).
//! The save is the commit point: once the worker has decided to write the
//! snapshot (one transaction), a cancel is refused as
//! [`CancelScan::TooLate`] and the scan ends `done`. Before that point
//! nothing is written, so a cancelled scan leaves the database exactly as
//! the previous scan left it: no album inserted or updated, nobody marked
//! Missing or returned, `scanned_at` untouched. (A Rescan resets the
//! library's `scanned_at` and the rows' `dir_mtime` before its scan
//! starts; a cancelled Rescan keeps that reset, so the next scan still
//! reads every album afresh. The rows themselves are unchanged.)
//! Thumbnails already written for albums read before the cancel stay; they
//! are a cache keyed by album id.
//!
//! # Buffer
//!
//! Every event of the current scan is kept (capped at [`MAX_BUFFER`], oldest
//! dropped) with a sequence number from 0. A [`ScanSubscription`] created
//! with `from_seq` first gets every buffered event with `seq >= from_seq`,
//! then live ones, and ends once the status is `done` / `error` and it has
//! delivered everything. A client going away just drops its subscription;
//! the scan keeps running.
//!
//! Events: `start {library_dir, total, cached_count, force}`, then per album
//! `album <dict + _status>` and `progress {scanned, total}` (every album below
//! 200, else every `total // 100`), then `aborted {...}` (the sanity check), or
//! `marked_missing {id, path}`* + `adopted {old_id, id, old_path, path}`* +
//! `done {count, marked_missing_count, newly_missing_count, returned_count,
//! scanned_at[, skipped_non_utf8_count]}`, or `error {detail}`. `adopted` (pushed once the save has
//! committed) says that the row of a vanished album moved to a folder found
//! under another spelling ([`crate::scan_adopt`]): the album's `album`
//! event carried the new id, the old id is gone, and it is not counted in
//! `marked_missing_count`.
//! A cancel adds `cancelling {scanned, total}` when it is accepted (at any
//! point, also before `start`) and ends the scan with `cancelled {reason,
//! phase, scanned, total, detail}` (`phase`: `discovery` or `albums`)
//! instead of the rest. Exactly one of `done` / `aborted` / `cancelled` /
//! `error` ends every scan. `aborted` and `cancelled` are pushed together
//! with the terminal status (one lock), so a client that sees them finds
//! the scan over and can start the next one at once.
//!
//! Deliberate change: the sanity check counts only albums that were
//! Present before this scan (`vanish_count` = Present cached albums absent
//! now, `cached_count` in `aborted` = Present cached albums, and the ">= 10"
//! minimum applies to that same Present count), and `marked_missing` /
//! `marked_missing_count` / `removed_count` cover only Present -> Missing
//! transitions. The Python app counted and re-announced already-Missing albums.
//!
//! # Bus events and hooks
//!
//! `AlbumRenamed(old, new)` for each adopted row, published with
//! `in_bulk_run` set so the rename subscribers' per-album check reruns are
//! skipped (the library-wide rerun on `LibraryScanCompleted` covers them;
//! the resolver memo and thumbnail drops and the relay still run), then
//! `AlbumMarkedMissing` / `AlbumTagsChanged(fields=["missing_since"])` for the
//! save's Missing transitions (in the database's rowid order, the order the
//! Python app's query returns), then, after the worker returns:
//!
//! * a saved scan (`done`): `LibraryScanCompleted(album_count=scanned)`, then
//!   `after_scan` (the fingerprint pool kick-off);
//! * an aborted, cancelled or failed scan:
//!   `LibraryScanStopped(reason, album_count=scanned)` (`reason`:
//!   `vanish_ratio_exceeded`, `cancelled` or `error`) and no `after_scan`.
//!
//! Deliberate change: the Python app published `LibraryScanCompleted` on
//! every path and ran the pool kick-off after an abort, so post-scan check
//! reruns and the fingerprint pool started against a library just judged
//! unreachable. `on_worker_done` (the thumbnail pool drain) runs after the
//! worker returns and before any of this, on every path, as the Python
//! app's worker `finally`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use musaic_core::Album;
use musaic_events::{Bus, Event as BusEvent};
use musaic_scan::{TrackOrder, list_flacs, py_resolve};
use rusqlite::Connection;

use crate::albums::{self, album_to_json};
use crate::clock::Clock;
use crate::json::Json;

/// `_MAX_BUFFER`.
pub const MAX_BUFFER: usize = 50_000;

/// `LibraryScanStopped.reason` (and `aborted.reason`) of a sanity-check abort.
pub const ABORT_REASON: &str = "vanish_ratio_exceeded";
/// `LibraryScanStopped.reason` (and `cancelled.reason`) of a cancelled scan.
pub const CANCEL_REASON: &str = "cancelled";
/// `LibraryScanStopped.reason` of a failed scan.
pub const ERROR_REASON: &str = "error";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanStatus {
    Idle,
    Running,
    Done,
    /// The sanity check refused to write (deliberate change: the Python app
    /// reported `done`).
    Aborted,
    /// Cancelled before the save; nothing written.
    Cancelled,
    Error,
}

impl ScanStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ScanStatus::Idle => "idle",
            ScanStatus::Running => "running",
            ScanStatus::Done => "done",
            ScanStatus::Aborted => "aborted",
            ScanStatus::Cancelled => "cancelled",
            ScanStatus::Error => "error",
        }
    }

    /// A scan ran and is over.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            ScanStatus::Done | ScanStatus::Aborted | ScanStatus::Cancelled | ScanStatus::Error
        )
    }
}

/// The result of [`ScanService::cancel`], each with the state after it.
#[derive(Debug, Clone, PartialEq)]
pub enum CancelScan {
    /// The scan will stop at the next directory or album and end
    /// `cancelled`.
    Requested(ScanState),
    /// A cancel was already requested; nothing changed.
    AlreadyRequested(ScanState),
    /// The scan is saving its result (the commit point); it ends `done`.
    TooLate(ScanState),
    /// No scan is running (it may have just ended on its own).
    NotRunning(ScanState),
}

impl CancelScan {
    /// The wire name of the outcome (`POST /api/scan/cancel`).
    pub fn as_str(&self) -> &'static str {
        match self {
            CancelScan::Requested(_) => "requested",
            CancelScan::AlreadyRequested(_) => "already_requested",
            CancelScan::TooLate(_) => "too_late",
            CancelScan::NotRunning(_) => "not_running",
        }
    }

    pub fn state(&self) -> &ScanState {
        match self {
            CancelScan::Requested(s)
            | CancelScan::AlreadyRequested(s)
            | CancelScan::TooLate(s)
            | CancelScan::NotRunning(s) => s,
        }
    }
}

/// `_ScanState.to_dict()`.
#[derive(Debug, Clone, PartialEq)]
pub struct ScanState {
    pub status: ScanStatus,
    pub library_dir: Option<String>,
    pub force: bool,
    pub total: usize,
    pub scanned: usize,
    pub cached_count: usize,
    pub removed_count: usize,
    /// Folders and `.flac` files met during discovery whose names are not
    /// valid UTF-8 and so cannot be stored; they are left out of the scan.
    pub skipped_non_utf8: usize,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub error: Option<String>,
    /// [`ScanService::cancel`] was accepted for this scan.
    pub cancel_requested: bool,
}

impl ScanState {
    fn idle() -> Self {
        ScanState {
            status: ScanStatus::Idle,
            library_dir: None,
            force: false,
            total: 0,
            scanned: 0,
            cached_count: 0,
            removed_count: 0,
            skipped_non_utf8: 0,
            started_at: None,
            completed_at: None,
            error: None,
            cancel_requested: false,
        }
    }

    /// The status as reported: `cancelling` for a running scan whose
    /// cancel was accepted, else [`ScanStatus::as_str`].
    pub fn status_str(&self) -> &'static str {
        if self.status == ScanStatus::Running && self.cancel_requested {
            "cancelling"
        } else {
            self.status.as_str()
        }
    }

    /// `GET /api/scan/status` body (key order of `to_dict`). The cancel is
    /// visible only through `status` (`cancelling`, then `cancelled`), so
    /// the body keeps the Python app's keys.
    ///
    /// `skipped_non_utf8_count` appears only when something was skipped, so the usual body is unchanged.
    pub fn to_json(&self) -> Json {
        let mut pairs = vec![
            ("status", Json::from(self.status_str())),
            ("library_dir", Json::from(self.library_dir.clone())),
            ("force", Json::from(self.force)),
            ("total", Json::from(self.total)),
            ("scanned", Json::from(self.scanned)),
            ("cached_count", Json::from(self.cached_count)),
            ("removed_count", Json::from(self.removed_count)),
            ("started_at", Json::from(self.started_at.clone())),
            ("completed_at", Json::from(self.completed_at.clone())),
            ("error", Json::from(self.error.clone())),
        ];
        if self.skipped_non_utf8 > 0 {
            pairs.push(("skipped_non_utf8_count", Json::from(self.skipped_non_utf8)));
        }
        Json::obj(pairs)
    }
}

/// One buffered event.
#[derive(Debug, Clone, PartialEq)]
pub struct ScanEvent {
    pub seq: u64,
    pub name: &'static str,
    pub data: Json,
}

impl ScanEvent {
    /// `sse_event(name, payload)`.
    pub fn sse_frame(&self) -> String {
        format!("event: {}\ndata: {}\n\n", self.name, self.data.dumps())
    }
}

struct Inner {
    state: ScanState,
    events: VecDeque<ScanEvent>,
    next_seq: u64,
    /// The worker passed the commit point (it is saving the snapshot): a
    /// cancel can no longer stop it.
    saving: bool,
}

impl Inner {
    fn push(&mut self, name: &'static str, data: Json) {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.events.push_back(ScanEvent { seq, name, data });
        while self.events.len() > MAX_BUFFER {
            self.events.pop_front();
        }
    }
}

struct Shared {
    inner: Mutex<Inner>,
    changed: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `_push_event`.
    fn push(&self, name: &'static str, data: Json) {
        self.lock().push(name, data);
        self.changed.notify_all();
    }

    fn update(&self, f: impl FnOnce(&mut ScanState)) {
        f(&mut self.lock().state);
        self.changed.notify_all();
    }

    /// A cancel was accepted (the worker stops at its next check).
    fn cancel_requested(&self) -> bool {
        self.lock().state.cancel_requested
    }

    /// The commit point: `true` (and no cancel can stop the scan from now
    /// on) unless a cancel was accepted first.
    fn enter_save(&self) -> bool {
        let mut g = self.lock();
        if g.state.cancel_requested {
            return false;
        }
        g.saving = true;
        true
    }

    /// The terminal status and the terminal event under one lock, so no
    /// subscriber sees one without the other.
    fn finish(&self, f: impl FnOnce(&mut ScanState), name: &'static str, data: Json) {
        let mut g = self.lock();
        f(&mut g.state);
        g.push(name, data);
        drop(g);
        self.changed.notify_all();
    }
}

/// How the worker ended when it did not fail.
enum Outcome {
    /// The snapshot was saved (`done` is in the buffer).
    Saved,
    /// The sanity check refused to write: the `aborted` payload.
    Aborted(Json),
    /// Cancelled before the save: the `cancelled` payload.
    Cancelled(Json),
}

/// Opens the library database for the worker thread.
pub type DbOpener = Arc<dyn Fn() -> Result<Connection, String> + Send + Sync>;
/// Called with the scan's `library_dir`.
pub type LibraryHook = Arc<dyn Fn(&str) + Send + Sync>;
/// Called after each album's `album` + `progress` events: the album dict and
/// whether it was read fresh (the Python app's thumbnail pool submission).
pub type AlbumHook = Arc<dyn Fn(&Json, bool) + Send + Sync>;
/// Called when the scan worker has returned ([`ScanDeps::on_worker_done`]).
pub type DoneHook = Arc<dyn Fn() + Send + Sync>;
/// A log line sink ([`ScanDeps::log`]).
pub type LogHook = Arc<dyn Fn(&str) + Send + Sync>;

/// What a scan needs besides the library path.
#[derive(Clone)]
pub struct ScanDeps {
    pub open_db: DbOpener,
    pub bus: Arc<Bus>,
    pub clock: Clock,
    pub track_order: TrackOrder,
    /// `fingerprint_pool.stop(); warm_library(library_dir)` in the Python app.
    pub after_scan: Option<LibraryHook>,
    /// `thumbs.ensure_thumb(album, force=fresh)` in the Python app.
    pub on_album: Option<AlbumHook>,
    /// Called once the scan worker has returned and before
    /// `LibraryScanCompleted` is published, on every path (done, aborted,
    /// error, a panicking body): the Python app's `_scan_worker` `finally:
    /// thumb_pool.shutdown(wait=True)` (services/scan.py:402-404), which
    /// runs before `_run_scan` sets the terminal status and publishes.
    pub on_worker_done: Option<DoneHook>,
    /// Where the scan's own problem lines go (a panicking hook); `None`:
    /// stderr.
    pub log: Option<LogHook>,
}

/// The result of [`ScanService::start_scan`].
pub enum StartScan {
    /// A scan was already running; its state, unchanged.
    AlreadyRunning(ScanState),
    /// A new scan; run the job (blocking) to perform it.
    Started(ScanState, ScanJob),
}

/// The background scan body. Run it on a blocking thread.
pub struct ScanJob {
    shared: Arc<Shared>,
    library_dir: String,
    force: bool,
    deps: ScanDeps,
}

/// Process-wide scan service (`services.scan` module state).
#[derive(Clone)]
pub struct ScanService {
    shared: Arc<Shared>,
}

impl Default for ScanService {
    fn default() -> Self {
        Self::new()
    }
}

impl ScanService {
    pub fn new() -> Self {
        ScanService {
            shared: Arc::new(Shared {
                inner: Mutex::new(Inner {
                    state: ScanState::idle(),
                    events: VecDeque::new(),
                    next_seq: 0,
                    saving: false,
                }),
                changed: Condvar::new(),
            }),
        }
    }

    /// `get_state()`.
    pub fn state(&self) -> ScanState {
        self.shared.lock().state.clone()
    }

    pub fn is_running(&self) -> bool {
        self.shared.lock().state.status == ScanStatus::Running
    }

    /// Every buffered event of the current (or last) scan.
    pub fn buffer(&self) -> Vec<ScanEvent> {
        self.shared.lock().events.iter().cloned().collect()
    }

    /// `start_scan(library_dir, force=...)`.
    pub fn start_scan(&self, library_dir: &str, force: bool, deps: ScanDeps) -> StartScan {
        let mut g = self.shared.lock();
        if g.state.status == ScanStatus::Running {
            return StartScan::AlreadyRunning(g.state.clone());
        }
        g.state = ScanState::idle();
        g.state.library_dir = Some(library_dir.to_string());
        g.state.force = force;
        g.events.clear();
        g.next_seq = 0;
        g.saving = false;
        g.state.status = ScanStatus::Running;
        g.state.started_at = Some((deps.clock)());
        let snapshot = g.state.clone();
        drop(g);
        self.shared.changed.notify_all();
        StartScan::Started(
            snapshot,
            ScanJob {
                shared: Arc::clone(&self.shared),
                library_dir: library_dir.to_string(),
                force,
                deps,
            },
        )
    }

    /// [`start_scan`](Self::start_scan) and run the job on a new thread.
    /// Returns the state the Python app's `start_scan` returns.
    pub fn start_scan_thread(&self, library_dir: &str, force: bool, deps: ScanDeps) -> ScanState {
        match self.start_scan(library_dir, force, deps) {
            StartScan::AlreadyRunning(s) => s,
            StartScan::Started(s, job) => {
                std::thread::Builder::new()
                    .name("library-scan".into())
                    .spawn(move || job.run())
                    .expect("spawn scan thread");
                s
            }
        }
    }

    /// Ask the running scan to stop (module docs, "Cancelling"). Safe at any
    /// moment and from any thread; returns at once. An accepted cancel
    /// pushes `cancelling` to the event buffer; the scan then ends
    /// `cancelled` at its next directory or album, after the album being
    /// read.
    pub fn cancel(&self) -> CancelScan {
        let mut g = self.shared.lock();
        if g.state.status != ScanStatus::Running {
            return CancelScan::NotRunning(g.state.clone());
        }
        if g.saving {
            return CancelScan::TooLate(g.state.clone());
        }
        if g.state.cancel_requested {
            return CancelScan::AlreadyRequested(g.state.clone());
        }
        g.state.cancel_requested = true;
        let (scanned, total) = (g.state.scanned, g.state.total);
        g.push(
            "cancelling",
            Json::obj([
                ("scanned", Json::from(scanned)),
                ("total", Json::from(total)),
            ]),
        );
        let state = g.state.clone();
        drop(g);
        self.shared.changed.notify_all();
        CancelScan::Requested(state)
    }

    /// Wait up to `timeout` for the status to leave `running`; `true` when
    /// no scan runs at the end. The terminal status is set after the
    /// worker closed its database connection.
    pub fn wait_until_stopped(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut g = self.shared.lock();
        while g.state.status == ScanStatus::Running {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            g = self
                .shared
                .changed
                .wait_timeout(g, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        true
    }

    /// `subscribe(from_seq)`.
    pub fn subscribe(&self, from_seq: u64) -> ScanSubscription {
        ScanSubscription {
            shared: Arc::clone(&self.shared),
            last_yielded: from_seq as i64 - 1,
        }
    }
}

/// What [`ScanSubscription::next_timeout`] found.
#[derive(Debug, Clone, PartialEq)]
pub enum Poll {
    Event(ScanEvent),
    /// Nothing new within the timeout (the Python app's 30 s heartbeat).
    Pending,
    /// The scan is over and everything was delivered.
    Finished,
}

/// A reader of the event buffer.
pub struct ScanSubscription {
    shared: Arc<Shared>,
    last_yielded: i64,
}

impl ScanSubscription {
    /// The next event, waiting up to `timeout` for one.
    pub fn next_timeout(&mut self, timeout: Duration) -> Poll {
        let deadline = Instant::now() + timeout;
        let mut g = self.shared.lock();
        loop {
            if let Some(ev) = g
                .events
                .iter()
                .find(|e| e.seq as i64 > self.last_yielded)
                .cloned()
            {
                self.last_yielded = ev.seq as i64;
                return Poll::Event(ev);
            }
            let terminal = g.state.status.is_terminal();
            if terminal && self.last_yielded >= g.next_seq as i64 - 1 {
                return Poll::Finished;
            }
            let now = Instant::now();
            if now >= deadline {
                return Poll::Pending;
            }
            g = self
                .shared
                .changed
                .wait_timeout(g, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

impl Iterator for ScanSubscription {
    type Item = ScanEvent;

    /// Blocks until the next event; `None` once the scan finished and
    /// everything was delivered.
    fn next(&mut self) -> Option<ScanEvent> {
        loop {
            match self.next_timeout(Duration::from_secs(30)) {
                Poll::Event(e) => return Some(e),
                Poll::Pending => continue,
                Poll::Finished => return None,
            }
        }
    }
}

impl ScanJob {
    /// `_run_scan`: the worker, then the terminal status, the bus event and
    /// (after a saved scan) the hook.
    pub fn run(self) {
        let ScanJob {
            shared,
            library_dir,
            force,
            deps,
        } = self;
        // A panic anywhere below must not leave the status "running"
        // forever: the guard turns an unwinding job into the error state.
        let guard = Unwinding {
            shared: Arc::clone(&shared),
            clock: deps.clock.clone(),
        };
        let result = scan_worker(&shared, &library_dir, force, &deps);
        // services/scan.py:402-404: the worker's `finally` drains the
        // thumbnail pool before `_run_scan` sees the result, so on every
        // path the thumbnails exist before the terminal status and before
        // `LibraryScanCompleted`.
        if let Some(hook) = &deps.on_worker_done {
            let drained = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| hook()));
            if let Err(p) = drained {
                log_line(
                    &deps,
                    &format!(
                        "scan worker-done hook panicked (continuing): {}",
                        panic_text(p.as_ref())
                    ),
                );
            }
        }
        let scanned = shared.lock().state.scanned as i64;
        // Deliberate change: only a saved scan announces completion and
        // restarts the fingerprint pool. An aborted, cancelled or failed
        // scan wrote nothing, so post-scan reruns and the pool have
        // nothing new to look at (and after an abort the library was just
        // judged unreachable); subscribers learn the scan ended, and why,
        // from `LibraryScanStopped`.
        let (status, reason, name, payload) = match result {
            Ok(Outcome::Saved) => (ScanStatus::Done, "", "", Json::Null),
            Ok(Outcome::Aborted(payload)) => {
                (ScanStatus::Aborted, ABORT_REASON, "aborted", payload)
            }
            Ok(Outcome::Cancelled(payload)) => {
                (ScanStatus::Cancelled, CANCEL_REASON, "cancelled", payload)
            }
            Err(detail) => {
                shared.update(|s| {
                    s.status = ScanStatus::Error;
                    s.error = Some(detail);
                    s.completed_at = Some((deps.clock)());
                });
                deps.bus.publish(&BusEvent::LibraryScanStopped {
                    reason: ERROR_REASON.to_string(),
                    album_count: scanned,
                });
                return;
            }
        };
        if status != ScanStatus::Done {
            let now = (deps.clock)();
            shared.finish(
                |s| {
                    s.status = status;
                    s.completed_at = Some(now);
                },
                name,
                payload,
            );
            deps.bus.publish(&BusEvent::LibraryScanStopped {
                reason: reason.to_string(),
                album_count: scanned,
            });
            drop(guard);
            return;
        }
        deps.bus.publish(&BusEvent::LibraryScanCompleted {
            album_count: scanned,
        });
        if let Some(hook) = &deps.after_scan {
            // The Python app logs a failing pool kick-off and carries on
            // (`except Exception: _log.exception(...)`).
            let kicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                hook(&library_dir);
            }));
            if let Err(p) = kicked {
                log_line(
                    &deps,
                    &format!(
                        "Failed to kick off eager fingerprinting (continuing): {}",
                        panic_text(p.as_ref())
                    ),
                );
            }
        }
        shared.update(|s| {
            if s.status != ScanStatus::Error {
                s.status = ScanStatus::Done;
                s.completed_at = Some((deps.clock)());
            }
        });
        drop(guard);
    }
}

/// Drop guard of [`ScanJob::run`]: when the job unwinds past every
/// `catch_unwind` (a panicking bus subscriber or clock), the scan ends in
/// `error` instead of reporting `running` forever.
struct Unwinding {
    shared: Arc<Shared>,
    clock: Clock,
}

impl Drop for Unwinding {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            return;
        }
        // The clock itself may be what panicked: never panic again here.
        let clock = self.clock.clone();
        let now = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| clock())).ok();
        self.shared.update(|s| {
            if s.status == ScanStatus::Running {
                s.status = ScanStatus::Error;
                s.error = Some("scan job panicked".to_string());
                s.completed_at = now;
            }
        });
    }
}

fn log_line(deps: &ScanDeps, line: &str) {
    match &deps.log {
        Some(l) => l(line),
        None => eprintln!("[musaic] {line}"),
    }
}

/// The text of a panic payload (`str(e)` for the Python app's exception).
fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic".to_string())
}

/// `scanner.discover_album_dirs(root)`: `os.walk` top-down, dot-directories
/// pruned, symlinked directories listed but not entered; a directory counts
/// when any non-directory entry's name ends in `.flac` (any case) and does
/// not start with `._`. Resolved paths.
///
/// Deliberate change: each directory's entries are taken in name order (code
/// point order, as `list_flacs` does), not in the order the filesystem lists
/// them. The Python app used `os.walk`'s listing order, so the scan stream,
/// the order albums are stored in and everything ordered by insertion
/// depended on the filesystem (APFS, ext4 and SMB shares all list
/// differently).
///
/// A name the filesystem lists more than once in one directory (some SMB
/// shares return one folder twice, same name and same inode) is taken once,
/// so its album is found once and the counts are right. The Python app walks
/// it twice.
pub fn discover_album_dirs(root: &Path) -> Vec<String> {
    discover_album_dirs_with(root, &list_dir)
}

/// [`discover_album_dirs`] over `list`, which returns the entry names of a
/// directory in any order (`None`: unreadable). Tests pass a listing that
/// repeats a name.
#[doc(hidden)]
pub fn discover_album_dirs_with(
    root: &Path,
    list: &dyn Fn(&Path) -> Option<Vec<std::ffi::OsString>>,
) -> Vec<String> {
    discover_album_dirs_until(root, list, &|| false).unwrap_or_default()
}

/// [`discover_album_dirs_with`] that asks `stop` before listing each
/// directory and gives up (`None`) as soon as it answers true: a cancel
/// during the walk of a large or slow library takes effect at the next
/// directory.
#[doc(hidden)]
pub fn discover_album_dirs_until(
    root: &Path,
    list: &dyn Fn(&Path) -> Option<Vec<std::ffi::OsString>>,
    stop: &dyn Fn() -> bool,
) -> Option<Vec<String>> {
    discover_album_dirs_reporting(root, list, stop).map(|d| d.dirs)
}

/// What a discovery walk found.
#[doc(hidden)]
pub struct Discovery {
    pub dirs: Vec<String>,
    /// Folders (not entered, so nothing beneath them is found) and `.flac`
    /// files whose names are not valid UTF-8. The API and the `path` column
    /// are UTF-8 text, so they are left out, each with one log line.
    pub skipped_non_utf8: usize,
}

/// [`discover_album_dirs_until`] that also counts, and logs, the entries
/// whose names cannot be stored. A lossy spelling of such a path cannot be
/// opened again, so keeping it would only make the album vanish quietly.
#[doc(hidden)]
pub fn discover_album_dirs_reporting(
    root: &Path,
    list: &dyn Fn(&Path) -> Option<Vec<std::ffi::OsString>>,
    stop: &dyn Fn() -> bool,
) -> Option<Discovery> {
    struct Stopped;
    fn skip(path: &Path, what: &str, skipped: &mut usize) {
        *skipped += 1;
        musaic_log::warning(
            "musaic.scanner",
            &format!(
                "Skipped {what} whose name is not valid UTF-8: {}",
                path.to_string_lossy()
            ),
        );
    }
    fn walk(
        dir: &Path,
        list: &dyn Fn(&Path) -> Option<Vec<std::ffi::OsString>>,
        stop: &dyn Fn() -> bool,
        found: &mut Vec<String>,
        skipped: &mut usize,
    ) -> Result<(), Stopped> {
        if stop() {
            return Err(Stopped);
        }
        let Some(mut names) = list(dir) else {
            return Ok(());
        };
        names.sort();
        let mut seen = std::collections::HashSet::new();
        let mut dirs = Vec::new();
        let mut has_flac = false;
        for os_name in names {
            if !seen.insert(os_name.clone()) {
                continue;
            }
            let name = os_name.to_string_lossy().into_owned();
            let utf8 = os_name.to_str().is_some();
            let path = dir.join(&os_name);
            if path.is_dir() {
                dirs.push((name, utf8, path));
            } else if name.to_lowercase().ends_with(".flac") && !name.starts_with("._") {
                if utf8 {
                    has_flac = true;
                } else {
                    skip(&path, "file", skipped);
                }
            }
        }
        if has_flac {
            found.push(py_resolve(dir).to_string_lossy().into_owned());
        }
        for (name, utf8, path) in dirs {
            if name.starts_with('.') {
                continue;
            }
            let is_link =
                std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink());
            if is_link {
                continue;
            }
            if utf8 {
                walk(&path, list, stop, found, skipped)?;
            } else {
                skip(&path, "folder", skipped);
            }
        }
        Ok(())
    }
    let mut found = Vec::new();
    let mut skipped = 0;
    if root.exists() {
        walk(root, list, stop, &mut found, &mut skipped).ok()?;
    }
    Some(Discovery {
        dirs: found,
        skipped_non_utf8: skipped,
    })
}

fn list_dir(dir: &Path) -> Option<Vec<std::ffi::OsString>> {
    std::fs::read_dir(dir)
        .ok()
        .map(|rd| rd.flatten().map(|e| e.file_name()).collect())
}

fn mtime(p: &Path) -> Option<f64> {
    let m = std::fs::metadata(p).ok()?.modified().ok()?;
    let d = m.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(d.as_secs() as f64 + 1e-9 * d.subsec_nanos() as f64)
}

/// `scanner.compute_effective_mtime(album_dir)`: max of the directory's and
/// its FLACs' mtimes.
pub fn effective_mtime(album_dir: &Path) -> f64 {
    list_flacs(album_dir)
        .iter()
        .filter_map(|f| mtime(f))
        .fold(mtime(album_dir).unwrap_or(0.0), f64::max)
}

/// `scanner._cached_row_is_complete`.
fn cached_row_is_complete(a: &Album) -> bool {
    !a.track_artists.is_empty() || !a.is_compilation
}

/// `ids` in rowid order. Runs after the save committed, so a failed lookup
/// only costs the order (logged), never the scan.
fn rowid_order(conn: &Connection, ids: Vec<String>, deps: &ScanDeps) -> Vec<String> {
    let mut keyed: Vec<(i64, String)> = Vec::with_capacity(ids.len());
    for id in &ids {
        match conn.query_row("SELECT rowid FROM albums WHERE id = ?1", [id], |r| {
            r.get::<_, i64>(0)
        }) {
            Ok(rowid) => keyed.push((rowid, id.clone())),
            Err(e) => {
                log_line(
                    deps,
                    &format!("scan: rowid lookup failed after the save (order kept): {e}"),
                );
                return ids;
            }
        }
    }
    keyed.sort();
    keyed.into_iter().map(|(_, id)| id).collect()
}

/// `_scan_worker`. Pushes `error` itself before failing.
fn scan_worker(
    shared: &Shared,
    library_dir: &str,
    force: bool,
    deps: &ScanDeps,
) -> Result<Outcome, String> {
    // `except Exception as e: emit("error", ...); raise`: a panic in the
    // scan body (or in a hook it calls) is an error like any other.
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        scan_body(shared, library_dir, force, deps)
    }))
    .unwrap_or_else(|p| Err(format!("scan worker panicked: {}", panic_text(p.as_ref()))));
    if let Err(detail) = &res {
        shared.push(
            "error",
            Json::obj([("detail", Json::from(detail.as_str()))]),
        );
    }
    res
}

/// The `cancelled` payload.
fn cancelled_payload(shared: &Shared, phase: &str) -> Outcome {
    let (scanned, total) = {
        let g = shared.lock();
        (g.state.scanned, g.state.total)
    };
    Outcome::Cancelled(Json::obj([
        ("reason", Json::from(CANCEL_REASON)),
        ("phase", Json::from(phase)),
        ("scanned", Json::from(scanned)),
        ("total", Json::from(total)),
        (
            "detail",
            Json::from(format!(
                "Scan cancelled {}. Nothing was saved: the library keeps the result of its \
                 previous scan.",
                if phase == "discovery" {
                    "while looking for album folders".to_string()
                } else {
                    format!("after {scanned} of {total} album(s)")
                }
            )),
        ),
    ]))
}

fn scan_body(
    shared: &Shared,
    library_dir: &str,
    force: bool,
    deps: &ScanDeps,
) -> Result<Outcome, String> {
    if shared.cancel_requested() {
        return Ok(cancelled_payload(shared, "discovery"));
    }
    let mut conn = (deps.open_db)()?;
    let resolved = py_resolve(Path::new(library_dir))
        .to_string_lossy()
        .into_owned();
    let cached_rows = if force {
        Vec::new()
    } else {
        albums::load_snapshot(&conn, &resolved).map_err(|e| e.to_string())?
    };
    // dict(albums_by_path): keyed by path, first-seen order.
    let mut cached_by_path: HashMap<String, usize> = HashMap::new();
    for (i, row) in cached_rows.iter().enumerate() {
        cached_by_path.insert(row.album.path.clone(), i);
    }
    let cached_count = cached_by_path.len();

    let stop = || shared.cancel_requested();
    let Some(discovery) = discover_album_dirs_reporting(Path::new(library_dir), &list_dir, &stop)
    else {
        return Ok(cancelled_payload(shared, "discovery"));
    };
    let skipped_non_utf8 = discovery.skipped_non_utf8;
    let candidates = discovery.dirs;
    let candidate_set: HashSet<&str> = candidates.iter().map(String::as_str).collect();
    let total = candidates.len();
    shared.update(|s| {
        s.total = total;
        s.cached_count = cached_count;
        s.skipped_non_utf8 = skipped_non_utf8;
    });
    shared.push(
        "start",
        Json::obj([
            ("library_dir", Json::from(library_dir)),
            ("total", Json::from(total)),
            ("cached_count", Json::from(cached_count)),
            ("force", Json::from(force)),
        ]),
    );

    // collected: path -> (album for the save), in first-seen order.
    let mut collected: Vec<(String, Album)> = Vec::new();
    let mut scanned = 0usize;
    for path in &candidates {
        // The album boundary: an album being read is finished first.
        if shared.cancel_requested() {
            return Ok(cancelled_payload(shared, "albums"));
        }
        let cached = cached_by_path.get(path).map(|&i| &cached_rows[i]);
        let current = effective_mtime(Path::new(path));
        let (status, mut dict, album) = match cached {
            Some(row)
                if (row.album.dir_mtime - current).abs() < 0.001
                    && cached_row_is_complete(&row.album) =>
            {
                ("cached", row.json.clone(), row.album.clone())
            }
            _ => match musaic_scan::scan_album(Path::new(path), deps.track_order) {
                Some(a) => ("fresh", album_to_json(&a), a),
                None => continue,
            },
        };
        dict.set("_status", Json::from(status));
        let key = album.path.clone();
        match collected.iter_mut().find(|(p, _)| *p == key) {
            Some(slot) => slot.1 = album,
            None => collected.push((key, album)),
        }
        scanned += 1;
        shared.update(|s| s.scanned = scanned);
        shared.push("album", dict.clone());
        let step = if total < 200 { 1 } else { (total / 100).max(1) };
        if scanned == total || scanned.is_multiple_of(step) {
            shared.push(
                "progress",
                Json::obj([
                    ("scanned", Json::from(scanned)),
                    ("total", Json::from(total)),
                ]),
            );
        }
        if let Some(hook) = &deps.on_album {
            hook(&dict, status == "fresh");
        }
    }
    // Deliberate change: only albums that were Present before this scan
    // count. Denominator: the
    // cached rows of this library that were Present (missing_since NULL or
    // empty, the runner's test); numerator: those of them absent now. The
    // Python app used every cached row for both, so once 30% of the cache was
    // Missing every later scan aborted, and it re-announced already-Missing
    // albums on every scan. `start.cached_count` and the state's
    // `cached_count` keep the Python app's meaning (every cached row).
    let was_present = |r: &&albums::AlbumRow| r.missing_since.as_deref().is_none_or(str::is_empty);
    let present_count = cached_rows.iter().filter(was_present).count();
    let vanished: Vec<(String, String)> = cached_rows
        .iter()
        .filter(was_present)
        .filter(|r| !candidate_set.contains(r.album.path.as_str()))
        .map(|r| (r.album.id.clone(), r.album.path.clone()))
        .collect();
    // A vanished row that is the same folder as one found under another
    // spelling moves to it at the save (module `scan_adopt`): it is
    // neither Missing nor new, so it does not count toward the sanity
    // check either.
    let adoptions = {
        let read: HashMap<&str, &Album> = collected.iter().map(|(p, a)| (p.as_str(), a)).collect();
        let found: Vec<crate::scan_adopt::Found<'_>> = candidates
            .iter()
            .map(|p| crate::scan_adopt::Found {
                path: p,
                album: read.get(p.as_str()).copied(),
                has_row: cached_by_path.contains_key(p),
            })
            .collect();
        crate::scan_adopt::plan(&conn, &vanished, &found)
    };
    let adopted_ids: HashSet<&str> = adoptions.iter().map(|a| a.old_id.as_str()).collect();
    let removed: Vec<(String, String)> = vanished
        .iter()
        .filter(|(id, _)| !adopted_ids.contains(id.as_str()))
        .cloned()
        .collect();

    // Refuse to write when a large part of the Present cache vanished
    // (">30% of >= 10 albums", both over Present albums).
    // A cancel that came in during the last album wins over the verdict.
    if shared.cancel_requested() {
        return Ok(cancelled_payload(shared, "albums"));
    }
    // Pushed by `ScanJob::run` together with the terminal status.
    if let Some(abort) =
        musaic_db::albums::scan_sanity_check(present_count, removed.len(), library_dir)
    {
        return Ok(Outcome::Aborted(Json::obj([
            ("reason", Json::from(ABORT_REASON)),
            ("vanish_ratio", Json::Float(abort.vanish_ratio)),
            ("vanish_count", Json::from(abort.vanish_count)),
            ("cached_count", Json::from(abort.cached_count)),
            ("threshold", Json::Float(abort.threshold)),
            ("detail", Json::from(abort.detail)),
        ])));
    }

    // The commit point: from here on a cancel is refused (`TooLate`) and
    // the scan saves. Nothing before this line wrote to the database.
    if !shared.enter_save() {
        return Ok(cancelled_payload(shared, "albums"));
    }

    // Announced only for Present -> Missing transitions.
    for (id, path) in &removed {
        shared.update(|s| s.removed_count += 1);
        shared.push(
            "marked_missing",
            Json::obj([
                ("id", Json::from(id.as_str())),
                ("path", Json::from(path.as_str())),
            ]),
        );
    }

    let scanned_at = (deps.clock)();
    let now = (deps.clock)();
    let albums_in: Vec<Album> = collected.iter().map(|(_, a)| a.clone()).collect();
    let transitions = musaic_db::albums::save_snapshot_adopting(
        &mut conn,
        &resolved,
        &scanned_at,
        &now,
        &albums_in,
        &adoptions,
    )
    .map_err(|e| e.to_string())?;
    // The snapshot is committed: nothing after this point fails the scan,
    // so a saved scan always ends `done` and announces completion.
    let newly_missing = rowid_order(&conn, transitions.newly_missing, deps);
    let returned = rowid_order(&conn, transitions.returned, deps);
    for ad in &adoptions {
        musaic_log::warning(
            crate::scan_run::SCAN_TARGET,
            &format!(
                "scan: {} is now listed as {}; the album keeps its data (id {} -> {})",
                ad.old_path, ad.new_path, ad.old_id, ad.new_id
            ),
        );
        shared.push(
            "adopted",
            Json::obj([
                ("old_id", Json::from(ad.old_id.as_str())),
                ("id", Json::from(ad.new_id.as_str())),
                ("old_path", Json::from(ad.old_path.as_str())),
                ("path", Json::from(ad.new_path.as_str())),
            ]),
        );
    }
    // The rename subscribers would rerun the checks for each adopted album
    // on its own; the scan's `LibraryScanCompleted` reruns them for the
    // whole library right after, and the check sweep checks the new ids
    // (they have no recorded inputs). So the per-album reruns are
    // suppressed as in a bulk loop; the cache drops and the relay still
    // run.
    musaic_events::context::with_bulk_run(|| {
        for (old, new) in &transitions.adopted {
            deps.bus.publish(&BusEvent::AlbumRenamed {
                old_album_id: old.clone(),
                new_album_id: new.clone(),
            });
        }
    });
    // One event per transition, after the commit.
    for id in &newly_missing {
        deps.bus.publish(&BusEvent::AlbumMarkedMissing {
            album_id: id.clone(),
            missing_since: now.clone(),
        });
    }
    for id in &returned {
        deps.bus.publish(&BusEvent::AlbumTagsChanged {
            album_id: id.clone(),
            fields: vec!["missing_since".into()],
        });
    }
    // Deliberate change: only albums whose row this save inserted import
    // their sidecar. The Python app imported for every scanned
    // album without tag rows, so an album whose rows were deliberately
    // removed (a vocabulary strip while it was Missing) took the stale
    // sidecar's tags back when it returned.
    let inserted: std::collections::HashSet<&str> =
        transitions.inserted.iter().map(String::as_str).collect();
    crate::sidecar_import::import_track_tag_sidecars(
        &mut conn,
        albums_in
            .iter()
            .filter(|a| inserted.contains(a.id.as_str()))
            .map(|a| (a.id.as_str(), a.path.as_str())),
        &now,
    );
    let mut done = vec![
        ("count", Json::from(collected.len())),
        ("marked_missing_count", Json::from(removed.len())),
        ("newly_missing_count", Json::from(newly_missing.len())),
        ("returned_count", Json::from(returned.len())),
        ("scanned_at", Json::from(scanned_at)),
    ];
    // Only when something was skipped, so the usual payload is unchanged.
    if skipped_non_utf8 > 0 {
        done.push(("skipped_non_utf8_count", Json::from(skipped_non_utf8)));
    }
    shared.push("done", Json::obj(done));
    Ok(Outcome::Saved)
}
