//! API shapes of the queue (`jobs.job_to_dict`, `worker.queue_state_dict`)
//! and the change signal the routes, the worker and the jobs stream share
//! (`services.sse.jobs_changed`).

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use musaic_db::ai_jobs::{self, JobRow};
use rusqlite::Connection;

use crate::pyjson::{Py, loads};

/// `jobs.job_to_dict(job)`: the row in the Python app's key order, `result`
/// parsed from `result_json` (`null` when empty or unparseable).
pub fn job_to_py(job: &JobRow) -> Py {
    let result = match job.result_json.as_deref() {
        Some(s) if !s.is_empty() => loads(s).unwrap_or(Py::Null),
        _ => Py::Null,
    };
    Py::dict([
        ("id", Py::str(job.id.clone())),
        ("album_id", Py::str(job.album_id.clone())),
        ("album_path", Py::str(job.album_path.clone())),
        ("album_title", Py::str(job.album_title.clone())),
        ("job_type", Py::str(job.job_type.clone())),
        ("hint", Py::str(job.hint.clone())),
        ("status", Py::str(job.status.clone())),
        ("error", Py::opt_str(job.error.clone())),
        ("acted_on_action", Py::opt_str(job.acted_on_action.clone())),
        ("created_at", Py::str(job.created_at.clone())),
        ("started_at", Py::opt_str(job.started_at.clone())),
        ("completed_at", Py::opt_str(job.completed_at.clone())),
        ("acted_on_at", Py::opt_str(job.acted_on_at.clone())),
        ("attach_cover", Py::Bool(job.attach_cover)),
        ("result", result),
    ])
}

/// `settings.ai_queue_paused()`.
pub fn queue_paused(conn: &Connection) -> musaic_db::Result<bool> {
    musaic_db::settings::get_bool(conn, "ai_queue_paused", false)
}

/// `settings.set_ai_queue_paused(b)`: stored as `"1"` / `"0"`.
pub fn set_queue_paused(conn: &Connection, paused: bool) -> musaic_db::Result<()> {
    musaic_db::settings::set_raw(conn, "ai_queue_paused", if paused { "1" } else { "0" })
}

/// `settings.ai_max_parallel()`: clamped to 1..=8.
pub fn max_parallel(conn: &Connection) -> musaic_db::Result<i64> {
    Ok(musaic_db::settings::get_int(conn, "ai_max_parallel", 1)?.clamp(1, 8))
}

/// `worker.queue_state_dict()`.
pub fn queue_state(conn: &Connection) -> musaic_db::Result<Py> {
    Ok(Py::dict([
        ("paused", Py::Bool(queue_paused(conn)?)),
        ("max_parallel", Py::Int(max_parallel(conn)?)),
        ("running", Py::Int(ai_jobs::count_running(conn)?)),
        ("queued", Py::Int(ai_jobs::count_queued(conn)?)),
    ]))
}

/// The jobs stream's payload: `{"jobs": [...200 newest], "queue_state": ...}`.
pub fn stream_payload(conn: &Connection) -> musaic_db::Result<Py> {
    let jobs = ai_jobs::list_jobs(conn, None, ai_jobs::LIST_LIMIT)?;
    Ok(Py::dict([
        ("jobs", Py::List(jobs.iter().map(job_to_py).collect())),
        ("queue_state", queue_state(conn)?),
    ]))
}

/// `jobs_changed` as a counter: every [`Signal::notify`] bumps it, wakes the
/// worker's waits and runs the listeners (the HTTP layer bumps one watch
/// channel per stream connection from there). Unlike the Python app's single
/// `asyncio.Event`, which the worker and every stream connection `clear()`,
/// no waiter can consume another's wake-up.
#[derive(Default)]
pub struct Signal {
    count: Mutex<u64>,
    cv: Condvar,
    listeners: Mutex<Vec<Arc<dyn Fn() + Send + Sync>>>,
}

impl Signal {
    pub fn new() -> Arc<Signal> {
        Arc::new(Signal::default())
    }

    /// `notify_jobs_changed()`.
    pub fn notify(&self) {
        {
            let mut n = self.count.lock().unwrap_or_else(|e| e.into_inner());
            *n = n.wrapping_add(1);
        }
        self.cv.notify_all();
        let ls = self
            .listeners
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        for l in ls {
            l();
        }
    }

    pub fn add_listener(&self, l: Arc<dyn Fn() + Send + Sync>) {
        self.listeners
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(l);
    }

    /// The number of notifications so far.
    pub fn value(&self) -> u64 {
        *self.count.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Wait until a notification after `seen`, or `timeout`.
    pub fn wait_since(&self, seen: u64, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        let mut n = self.count.lock().unwrap_or_else(|e| e.into_inner());
        while *n == seen {
            let now = Instant::now();
            if now >= deadline {
                return;
            }
            n = self
                .cv
                .wait_timeout(n, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}
