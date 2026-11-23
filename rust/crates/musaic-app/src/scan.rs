//! Scans over this app: starting a scan with the
//! app's dependencies, thumbnail pool and log, and the scan stream's
//! "start unless one runs, then replay from the start" step. The host
//! forwards the subscription's events its own way.

use std::sync::Arc;

use std::time::Duration;

use musaic_services::ScanState;
use musaic_services::gate::WorkGuard;
use musaic_services::scan::{CancelScan, ScanRun, ScanSubscription};

use crate::App;

impl App {
    /// Start a scan on its own thread, holding `guard` for as long as the scan
    /// job runs ([`musaic_services::scan_run::start_scan_with`]), with this
    /// app's scan dependencies and log.
    pub(crate) fn start_scan(&self, lib: &str, force: bool, guard: WorkGuard) -> ScanState {
        self.start_observed_scan(lib, force, guard).0
    }

    pub(crate) fn start_observed_scan(
        &self,
        lib: &str,
        force: bool,
        guard: WorkGuard,
    ) -> (ScanState, ScanRun) {
        // The thumbnail pool is drained by `on_worker_done`, before the
        // terminal status and `LibraryScanCompleted` on every path; the guard
        // is dropped when the job returns, right after the terminal status.
        let (deps, _thumbs) = self.scan_deps_with_thumbs();
        musaic_services::scan_run::start_scan_observed(
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
    pub(crate) fn scan_and_subscribe(
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

    /// Cancel the running scan (`POST /api/scan/cancel`, and the first step
    /// of [`App::shutdown`]): [`musaic_services::ScanService::cancel`],
    /// logged when it was accepted.
    pub(crate) fn cancel_scan(&self) -> CancelScan {
        let out = self.0.scan.cancel();
        self.log_scan_cancel(&out);
        out
    }

    pub(crate) fn log_scan_cancel(&self, out: &CancelScan) {
        if let CancelScan::Requested(s) = out {
            self.log().info(
                musaic_services::scan_run::SCAN_TARGET,
                &format!(
                    "scan cancel requested ({} of {} album(s) read)",
                    s.scanned, s.total
                ),
            );
        }
    }

    /// [`App::cancel_scan`], then wait up to `timeout` for the scan to end.
    /// `true` when no scan runs at the end.
    pub(crate) fn cancel_scan_and_wait(&self, timeout: Duration) -> bool {
        match self.cancel_scan() {
            CancelScan::NotRunning(_) => true,
            _ => self.0.scan.wait_until_stopped(timeout),
        }
    }
}
