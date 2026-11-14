//! Background library scan: port of `musaic/services/scan.py` and the
//! `incremental_scan` /
//! `discover_album_dirs` half of `scanner.py`.
//!
//! # State machine
//!
//! `idle -> running -> done | error`, at most one scan at a time.
//! [`ScanService::start_scan`] while a scan runs returns the running state
//! unchanged ([`StartScan::AlreadyRunning`]); otherwise it resets the state
//! and the event buffer and hands back a [`ScanJob`] the host runs on a
//! blocking thread ([`ScanService::start_scan_thread`] does that with
//! `std::thread`).
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
//! `marked_missing {id, path}`* + `done {count, marked_missing_count,
//! newly_missing_count, returned_count, scanned_at}`, or `error {detail}`.
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
//! `AlbumMarkedMissing` / `AlbumTagsChanged(fields=["missing_since"])` for the
//! save's Missing transitions (in the database's rowid order, the order the
//! Python app's query returns), then `LibraryScanCompleted(album_count=scanned)`
//! after the worker returns, on every path including errors and aborts.
//! `on_worker_done` (the thumbnail pool drain) runs after the worker returns
//! and before that event, on every path, as the Python app's worker `finally`.
//! `after_scan` (the fingerprint pool kick-off) runs after a clean return or
//! an abort, never after an error. Reproduced from the Python app: no
//! cancellation, and an aborted scan still publishes `LibraryScanCompleted`
//! and runs `after_scan`.

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanStatus {
    Idle,
    Running,
    Done,
    Error,
}

impl ScanStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ScanStatus::Idle => "idle",
            ScanStatus::Running => "running",
            ScanStatus::Done => "done",
            ScanStatus::Error => "error",
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
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub error: Option<String>,
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
            started_at: None,
            completed_at: None,
            error: None,
        }
    }

    /// `GET /api/scan/status` body (key order of `to_dict`).
    pub fn to_json(&self) -> Json {
        Json::obj([
            ("status", Json::from(self.status.as_str())),
            ("library_dir", Json::from(self.library_dir.clone())),
            ("force", Json::from(self.force)),
            ("total", Json::from(self.total)),
            ("scanned", Json::from(self.scanned)),
            ("cached_count", Json::from(self.cached_count)),
            ("removed_count", Json::from(self.removed_count)),
            ("started_at", Json::from(self.started_at.clone())),
            ("completed_at", Json::from(self.completed_at.clone())),
            ("error", Json::from(self.error.clone())),
        ])
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
        let mut g = self.lock();
        let seq = g.next_seq;
        g.next_seq += 1;
        g.events.push_back(ScanEvent { seq, name, data });
        while g.events.len() > MAX_BUFFER {
            g.events.pop_front();
        }
        drop(g);
        self.changed.notify_all();
    }

    fn update(&self, f: impl FnOnce(&mut ScanState)) {
        f(&mut self.lock().state);
        self.changed.notify_all();
    }
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
            let terminal = matches!(g.state.status, ScanStatus::Done | ScanStatus::Error);
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
    /// `_run_scan`: the worker, then the bus event, the hook and the
    /// terminal status.
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
        if let Err(detail) = result {
            shared.update(|s| {
                s.status = ScanStatus::Error;
                s.error = Some(detail);
                s.completed_at = Some((deps.clock)());
            });
            deps.bus.publish(&BusEvent::LibraryScanCompleted {
                album_count: scanned,
            });
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

/// `scanner.discover_album_dirs(root)`: `os.walk` top-down in directory
/// listing order, dot-directories pruned, symlinked directories listed but
/// not entered; a directory counts when any non-directory entry's name ends
/// in `.flac` (any case) and does not start with `._`. Resolved paths.
///
/// A name the filesystem lists more than once in one directory (some SMB
/// shares return one folder twice, same name and same inode) is taken once,
/// so its album is found once and the counts are right. The Python app walks
/// it twice.
pub fn discover_album_dirs(root: &Path) -> Vec<String> {
    discover_album_dirs_with(root, &|dir: &Path| {
        std::fs::read_dir(dir)
            .ok()
            .map(|rd| rd.flatten().map(|e| e.file_name()).collect())
    })
}

/// [`discover_album_dirs`] over `list`, which returns the entry names of a
/// directory in listing order (`None`: unreadable). Tests pass a listing
/// that repeats a name.
#[doc(hidden)]
pub fn discover_album_dirs_with(
    root: &Path,
    list: &dyn Fn(&Path) -> Option<Vec<std::ffi::OsString>>,
) -> Vec<String> {
    fn walk(
        dir: &Path,
        list: &dyn Fn(&Path) -> Option<Vec<std::ffi::OsString>>,
        found: &mut Vec<String>,
    ) {
        let Some(names) = list(dir) else {
            return;
        };
        let mut seen = std::collections::HashSet::new();
        let mut dirs = Vec::new();
        let mut has_flac = false;
        for os_name in names {
            if !seen.insert(os_name.clone()) {
                continue;
            }
            let name = os_name.to_string_lossy().into_owned();
            let path = dir.join(&os_name);
            if path.is_dir() {
                dirs.push((name, path));
            } else if name.to_lowercase().ends_with(".flac") && !name.starts_with("._") {
                has_flac = true;
            }
        }
        if has_flac {
            found.push(py_resolve(dir).to_string_lossy().into_owned());
        }
        for (name, path) in dirs {
            if name.starts_with('.') {
                continue;
            }
            let is_link =
                std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink());
            if !is_link {
                walk(&path, list, found);
            }
        }
    }
    let mut found = Vec::new();
    if root.exists() {
        walk(root, list, &mut found);
    }
    found
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

fn rowid_order(conn: &Connection, ids: Vec<String>) -> rusqlite::Result<Vec<String>> {
    let mut keyed: Vec<(i64, String)> = Vec::with_capacity(ids.len());
    for id in ids {
        let rowid: i64 = conn.query_row("SELECT rowid FROM albums WHERE id = ?1", [&id], |r| {
            r.get(0)
        })?;
        keyed.push((rowid, id));
    }
    keyed.sort();
    Ok(keyed.into_iter().map(|(_, id)| id).collect())
}

/// `_scan_worker`. Pushes `error` itself before failing.
fn scan_worker(
    shared: &Shared,
    library_dir: &str,
    force: bool,
    deps: &ScanDeps,
) -> Result<(), String> {
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

fn scan_body(
    shared: &Shared,
    library_dir: &str,
    force: bool,
    deps: &ScanDeps,
) -> Result<(), String> {
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

    let candidates = discover_album_dirs(Path::new(library_dir));
    let candidate_set: HashSet<&str> = candidates.iter().map(String::as_str).collect();
    let total = candidates.len();
    shared.update(|s| {
        s.total = total;
        s.cached_count = cached_count;
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
    let removed: Vec<(String, String)> = cached_rows
        .iter()
        .filter(was_present)
        .filter(|r| !candidate_set.contains(r.album.path.as_str()))
        .map(|r| (r.album.id.clone(), r.album.path.clone()))
        .collect();

    // Refuse to write when a large part of the Present cache vanished
    // (">30% of >= 10 albums", both over Present albums).
    if let Some(abort) =
        musaic_db::albums::scan_sanity_check(present_count, removed.len(), library_dir)
    {
        shared.push(
            "aborted",
            Json::obj([
                ("reason", Json::from("vanish_ratio_exceeded")),
                ("vanish_ratio", Json::Float(abort.vanish_ratio)),
                ("vanish_count", Json::from(abort.vanish_count)),
                ("cached_count", Json::from(abort.cached_count)),
                ("threshold", Json::Float(abort.threshold)),
                ("detail", Json::from(abort.detail)),
            ]),
        );
        return Ok(());
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
    let transitions =
        musaic_db::albums::save_snapshot(&mut conn, &resolved, &scanned_at, &now, &albums_in)
            .map_err(|e| e.to_string())?;
    let newly_missing = rowid_order(&conn, transitions.newly_missing).map_err(|e| e.to_string())?;
    let returned = rowid_order(&conn, transitions.returned).map_err(|e| e.to_string())?;
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
    shared.push(
        "done",
        Json::obj([
            ("count", Json::from(collected.len())),
            ("marked_missing_count", Json::from(removed.len())),
            ("newly_missing_count", Json::from(newly_missing.len())),
            ("returned_count", Json::from(returned.len())),
            ("scanned_at", Json::from(scanned_at)),
        ]),
    );
    Ok(())
}
