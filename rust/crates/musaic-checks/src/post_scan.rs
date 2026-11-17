//! The post-scan check sweep (port of `musaic/services/post_scan.py`) and
//! the check-runner side of musaic-events' [`SideEffects`].

use std::path::Path;
use std::sync::Arc;

use musaic_db::findings::RunRow;
use musaic_events::{SideEffects, SubscriberError, run_post_scan_guarded};
use musaic_scan::py_resolve;
use rusqlite::Connection;

use crate::base::{CheckEnv, ScopeKind};
use crate::rerun::RerunReport;
use crate::runner::{RunError, run_check};

/// One check's result in a sweep. A failing check is reported here and the
/// sweep goes on (the Python app logs and continues).
#[derive(Debug)]
pub struct SweepEntry {
    pub check_id: &'static str,
    pub result: Result<RunRow, RunError>,
}

/// The unguarded body of `run_post_scan_checks`: every `runs_during_scan`
/// check in registry order, scoped to `album_ids` (`Some` and non-empty ->
/// `albums`, else `all`), on `library_dir` resolved like `Path.resolve()`.
/// The caller is responsible for the recursion guard: use
/// [`run_post_scan_checks`] unless the guard is already applied (the
/// musaic-events catalog applies it around [`SideEffects::run_post_scan_checks`]).
pub fn post_scan_sweep(
    conn: &Connection,
    env: &CheckEnv,
    library_dir: &str,
    album_ids: Option<&[String]>,
) -> Vec<SweepEntry> {
    let library_path = py_resolve(Path::new(library_dir))
        .to_string_lossy()
        .into_owned();
    let ids = album_ids.filter(|ids| !ids.is_empty());
    let kind = if ids.is_some() {
        ScopeKind::Albums
    } else {
        ScopeKind::All
    };
    env.registry
        .iter()
        .filter(|c| c.meta().runs_during_scan)
        .map(|c| SweepEntry {
            check_id: c.meta().id,
            result: run_check(conn, env, c.meta().id, &library_path, kind, ids),
        })
        .collect()
}

/// `run_post_scan_checks(library_dir, album_ids=...)`: the sweep with the
/// recursion guard. Returns `None` (and runs nothing) when called inside a
/// sweep already in flight on this dispatch chain; otherwise runs the sweep
/// with `in_post_scan_rerun` set, resetting it afterwards even on panic.
pub fn run_post_scan_checks(
    conn: &Connection,
    env: &CheckEnv,
    library_dir: &str,
    album_ids: Option<&[String]>,
) -> Option<Vec<SweepEntry>> {
    run_post_scan_guarded(|| post_scan_sweep(conn, env, library_dir, album_ids))
}

type OpenConn = dyn Fn() -> Result<Connection, SubscriberError> + Send + Sync;
type AlbumHook = dyn Fn(&str) -> Result<(), SubscriberError> + Send + Sync;

/// musaic-events' [`SideEffects`] with the check-runner targets implemented
/// here and the others (library dir, resolver memo, thumbs) injected.
///
/// Deliberate change: [`SideEffects::run_post_scan_checks`] is the
/// unguarded sweep. The catalog wraps it in `run_post_scan_guarded`, which
/// sets `in_post_scan_rerun`, so a bus-driven rerun really runs the checks,
/// while publishes from inside the sweep (the checks run on this thread and
/// see the flag) are still suppressed.
pub struct ChecksSideEffects {
    pub env: CheckEnv,
    /// Opens a connection to the library database for a rerun (subscribers
    /// may run on a background thread).
    pub open: Arc<OpenConn>,
    pub library_dir: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    pub resolver_invalidate: Arc<AlbumHook>,
    pub delete_thumb: Arc<AlbumHook>,
}

fn sweep_error(entries: &[SweepEntry]) -> Option<SubscriberError> {
    // The Python app logs per-check failures and never raises out of the sweep;
    // only a refusal to start (unknown check, DB error) is surfaced here.
    entries
        .iter()
        .find_map(|e| e.result.as_ref().err())
        .map(|e| format!("post-scan check failed to start: {e}").into())
}

impl SideEffects for ChecksSideEffects {
    fn library_dir(&self) -> Option<String> {
        (self.library_dir)()
    }

    fn resolver_invalidate(&self, album_id: &str) -> Result<(), SubscriberError> {
        (self.resolver_invalidate)(album_id)
    }

    fn delete_thumb(&self, album_id: &str) -> Result<(), SubscriberError> {
        (self.delete_thumb)(album_id)
    }

    fn run_post_scan_checks(
        &self,
        library_dir: &str,
        album_ids: Option<&[String]>,
    ) -> Result<(), SubscriberError> {
        let conn = (self.open)()?;
        let entries = post_scan_sweep(&conn, &self.env, library_dir, album_ids);
        let ids = album_ids.filter(|ids| !ids.is_empty());
        let mut report = RerunReport::new();
        for run in entries.iter().filter_map(|e| e.result.as_ref().ok()) {
            report.record(run, ids);
        }
        report.publish(&self.env);
        sweep_error(&entries).map_or(Ok(()), Err)
    }

    fn run_check_scoped(
        &self,
        check_id: &str,
        library_path: &str,
        album_ids: &[String],
    ) -> Result<(), SubscriberError> {
        let conn = (self.open)()?;
        let run = run_check(
            &conn,
            &self.env,
            check_id,
            library_path,
            ScopeKind::Albums,
            Some(album_ids),
        )?;
        let mut report = RerunReport::new();
        report.record(&run, Some(album_ids));
        report.publish(&self.env);
        Ok(())
    }

    fn notify_findings_changed(&self) -> Result<(), SubscriberError> {
        (self.env.notify)();
        Ok(())
    }
}
