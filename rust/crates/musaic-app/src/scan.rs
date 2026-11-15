//! Scans over this app: starting a scan with the
//! app's dependencies, thumbnail pool and log, and the scan stream's
//! "start unless one runs, then replay from the start" step. The host
//! forwards the subscription's events its own way.

use std::sync::Arc;

use musaic_services::ScanState;
use musaic_services::gate::WorkGuard;
use musaic_services::scan::ScanSubscription;

use crate::App;

impl App {
    /// Start a scan on its own thread, holding `guard` for as long as the scan
    /// job runs ([`musaic_services::scan_run::start_scan_with`]), with this
    /// app's scan dependencies and log.
    pub fn start_scan(&self, lib: &str, force: bool, guard: WorkGuard) -> ScanState {
        // The thumbnail pool is drained by `on_worker_done`, before the
        // terminal status and `LibraryScanCompleted` on every path; the guard
        // is dropped when the job returns, right after the terminal status.
        let (deps, _thumbs) = self.scan_deps_with_thumbs();
        musaic_services::scan_run::start_scan_with(
            &self.0.scan,
            lib,
            force,
            deps,
            guard,
            Arc::new(self.log().clone()),
        )
    }

    /// `GET /api/scan/stream`'s step: start a scan unless one runs, then
    /// subscribe from sequence 0 (every subscriber replays the
    /// current scan's buffer). While a scan runs `guard` is not needed and
    /// is handed back; the caller drops it when it is done.
    pub fn scan_and_subscribe(
        &self,
        lib: &str,
        force: bool,
        guard: WorkGuard,
    ) -> (ScanSubscription, Option<WorkGuard>) {
        let unused = if !self.0.scan.is_running() {
            self.start_scan(lib, force, guard);
            None
        } else {
            Some(guard)
        };
        (self.0.scan.subscribe(0), unused)
    }
}
