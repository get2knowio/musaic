//! Running a library scan for a host: start one on its
//! own thread holding the caller's work guard, log its start and end
//! (`musaic.services.scan`), and forward a subscription's events to a
//! client. It lives here rather than in musaic-api's `routes/library.rs`
//! so a non-HTTP face starts and follows scans the same way. The host
//! builds the [`ScanDeps`] (its thumbnail pool and fingerprint pool) and
//! frames the events for its transport.

use std::sync::Arc;
use std::time::Duration;

use crate::gate::{WorkGuard, WorkKind};
use crate::host_log::HostLog;
use crate::scan::{Poll, ScanDeps, ScanEvent, ScanService, ScanState, ScanSubscription, StartScan};

/// The logger name of scans (`musaic.services.scan`).
pub const SCAN_TARGET: &str = "musaic.services.scan";

/// Start a scan on its own thread, holding `guard` for as long as the scan
/// job runs (a restore is refused while the guard is held; it is released
/// even if the scan thread panics). When a scan is already running its state
/// is returned and the guard is dropped at once.
pub fn start_scan_with(
    scan: &ScanService,
    lib: &str,
    force: bool,
    deps: ScanDeps,
    guard: WorkGuard,
    log: Arc<dyn HostLog>,
) -> ScanState {
    match scan.start_scan(lib, force, deps) {
        StartScan::AlreadyRunning(s) => s,
        StartScan::Started(s, job) => {
            let mut guard = guard;
            guard.relabel(WorkKind::Scan);
            let scan = scan.clone();
            log.info(
                SCAN_TARGET,
                &format!("scan started: {lib}{}", if force { " (force)" } else { "" }),
            );
            std::thread::Builder::new()
                .name("library-scan".into())
                .spawn(move || {
                    let _guard = guard;
                    let t0 = std::time::Instant::now();
                    job.run();
                    log_scan_end(log.as_ref(), &scan, t0.elapsed());
                })
                .expect("spawn scan thread");
            s
        }
    }
}

/// The end of a scan in the log: its status and counts (from the state and
/// the `done` / `aborted` / `error` event).
pub fn log_scan_end(log: &dyn HostLog, scan: &ScanService, took: Duration) {
    let s = scan.state();
    let last = scan.buffer().into_iter().last();
    let ms = took.as_millis();
    let t = SCAN_TARGET;
    match (s.status.as_str(), last) {
        ("done", Some(ev)) if ev.name == "done" => log.info(
            t,
            &format!(
                "scan done in {ms}ms: {} album(s) of {} found ({} cached before), {} marked missing, {} returned",
                s.scanned,
                s.total,
                s.cached_count,
                s.removed_count,
                ev.data.get("returned_count").map_or_else(String::new, |v| v.dumps()),
            ),
        ),
        ("done", Some(ev)) if ev.name == "aborted" => log.warn(
            t,
            &format!(
                "Scan sanity check tripped after {ms}ms: {}",
                ev.data.get("detail").and_then(|v| v.as_str()).unwrap_or("")
            ),
        ),
        (status, _) => log.error(
            t,
            &format!(
                "scan {status} after {ms}ms ({} album(s) read): {}",
                s.scanned,
                s.error.as_deref().unwrap_or("")
            ),
        ),
    }
}

/// Hand a subscription's events to a client until the scan is over or the
/// client is gone (the scan keeps running). Blocking: polls every 200 ms;
/// `send` answers false when the client is gone, `is_closed` is asked
/// between events.
pub fn forward_events(
    mut sub: ScanSubscription,
    mut send: impl FnMut(&ScanEvent) -> bool,
    is_closed: impl Fn() -> bool,
) {
    loop {
        match sub.next_timeout(Duration::from_millis(200)) {
            Poll::Event(ev) => {
                if !send(&ev) {
                    // Client gone; the scan keeps running.
                    return;
                }
            }
            Poll::Pending => {
                if is_closed() {
                    return;
                }
            }
            Poll::Finished => return,
        }
    }
}

/// What [`scan_deps_with_thumbs`] needs from the host.
pub struct ScanHost {
    /// The library database (each scan opens its own connection).
    pub db_path: std::path::PathBuf,
    /// The host's wall clock.
    pub clock: Arc<dyn Fn() -> std::time::SystemTime + Send + Sync>,
    pub bus: musaic_events::Bus,
    pub track_order: musaic_scan::TrackOrder,
    /// The fingerprint pool restarted after the scan, and its deps.
    pub pool: crate::fingerprint_pool::FingerprintPool,
    pub pool_deps: crate::fingerprint_pool::PoolDeps,
    /// `<config>/thumbs`.
    pub thumbs_dir: std::path::PathBuf,
    /// Called after the thumbnail submission for each album (tests).
    pub on_scan_album: Option<crate::scan::AlbumHook>,
    pub log: Arc<dyn HostLog>,
}

/// The scan's dependencies and its thumbnail pool
/// ([`crate::thumb_pool`]): every album the scan reads is submitted
/// (`force` for a fresh one) and `on_worker_done` waits for the pool
/// once the scan worker returns, before `LibraryScanCompleted` and the
/// terminal status, on every path (services/scan.py:402-404).
pub fn scan_deps_with_thumbs(host: ScanHost) -> (ScanDeps, crate::thumb_pool::ThumbPool) {
    let ScanHost {
        db_path: path,
        clock,
        bus,
        track_order,
        pool,
        pool_deps,
        thumbs_dir,
        on_scan_album: test_hook,
        log: scan_log,
    } = host;
    let thumbs = crate::thumb_pool::ThumbPool::new();
    let t_submit = thumbs.clone();
    let t_wait = thumbs.clone();
    let deps = ScanDeps {
        open_db: Arc::new(move || musaic_db::open_connection(&path).map_err(|e| e.to_string())),
        bus: Arc::new(bus),
        clock: Arc::new(move || musaic_db::backup::iso_seconds(clock())),
        track_order,
        // services/scan.py:234-236: stop the pass in flight, start a
        // fresh one (fire and forget) after every scan that did not
        // fail.
        after_scan: Some(Arc::new(move |lib: &str| {
            pool.stop();
            pool.warm_library(lib, pool_deps.clone());
        })),
        // services/scan.py:402-404: the worker's `finally` drains the
        // thumbnail pool, before the terminal status and before
        // `LibraryScanCompleted`, on every path (error included).
        on_worker_done: Some(Arc::new(move || t_wait.wait())),
        log: Some(Arc::new(move |l: &str| {
            scan_log.error(SCAN_TARGET, l);
        })),
        // services/scan.py:304-305.
        on_album: Some(Arc::new(move |album: &crate::Json, fresh: bool| {
            crate::thumb_pool::ensure_thumb_job(&t_submit, thumbs_dir.clone(), album, fresh);
            if let Some(h) = &test_hook {
                h(album, fresh);
            }
        })),
    };
    (deps, thumbs)
}
