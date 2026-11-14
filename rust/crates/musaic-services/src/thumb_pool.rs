//! The scan's thumbnail pool (services/scan.py:255, 304-305,
//! 406): `ThreadPoolExecutor(max_workers=4, thread_name_prefix="mm-thumb")`
//! created per scan, `submit(thumbs.ensure_thumb, album, force=(status ==
//! "fresh"))` for every album the scan reads (cached or fresh), and
//! `shutdown(wait=True)` at the end of the scan worker, so every thumbnail
//! is written before the scan reports `done`.
//!
//! Semantics kept: at most four jobs at once, started in submission order
//! (FIFO queue, threads created on demand up to four), a fresh album's
//! thumbnail is regenerated even when one exists, a cached album's only
//! when missing; a job failure only loses that thumbnail. Albums that went
//! Missing are never submitted, so their thumbnails stay.
//!
//! The pool takes no [`crate::gate::WorkGuard`]: it lives inside the scan, whose
//! own guard covers it, and [`ThumbPool::wait`] returns once the queue is
//! drained, so nothing holds the restore gate past the scan.
//!
//! Drain point: as in the Python app, the pool is drained once the scan worker
//! returns (`ScanDeps::on_worker_done`), before the terminal status and
//! before `LibraryScanCompleted`, on the error path too (the Python app's
//! worker `finally`). The scan's guard is released when the scan job
//! returns, right after the terminal status, so the gate never outlives
//! the reported end of the scan.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};

use crate::Json;

/// `max_workers=4`.
pub const MAX_WORKERS: usize = 4;

type Job = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    /// Submitted and not yet finished.
    pending: usize,
    workers: usize,
    idle: usize,
}

#[derive(Default)]
struct Shared {
    q: Mutex<Queue>,
    work: Condvar,
    drained: Condvar,
}

/// One scan's pool. Cloning shares it.
#[derive(Clone, Default)]
pub struct ThumbPool(Arc<Shared>);

impl ThumbPool {
    pub fn new() -> Self {
        Self::default()
    }

    /// `executor.submit(job)`.
    pub fn submit(&self, job: impl FnOnce() + Send + 'static) {
        let mut q = self.0.q.lock().unwrap_or_else(|e| e.into_inner());
        q.jobs.push_back(Box::new(job));
        q.pending += 1;
        if q.idle == 0 && q.workers < MAX_WORKERS {
            q.workers += 1;
            let shared = Arc::clone(&self.0);
            let spawned = std::thread::Builder::new()
                .name("mm-thumb".into())
                .spawn(move || worker(&shared));
            if spawned.is_err() {
                q.workers -= 1;
            }
        }
        drop(q);
        self.0.work.notify_one();
    }

    /// `executor.shutdown(wait=True)`: returns once every submitted job has
    /// run. Idempotent; jobs submitted later are still run.
    pub fn wait(&self) {
        let mut q = self.0.q.lock().unwrap_or_else(|e| e.into_inner());
        while q.pending > 0 {
            if q.workers == 0 {
                // Could not start a thread: run the rest here.
                while let Some(job) = q.jobs.pop_front() {
                    drop(q);
                    run(job);
                    q = self.0.q.lock().unwrap_or_else(|e| e.into_inner());
                    q.pending -= 1;
                }
                break;
            }
            q = self.0.drained.wait(q).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// Jobs submitted and not finished (tests).
    pub fn pending(&self) -> usize {
        self.0.q.lock().unwrap_or_else(|e| e.into_inner()).pending
    }
}

fn run(job: Job) {
    // A panicking job only loses its thumbnail (the Python app's future held
    // the exception and nobody reads it).
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
}

fn worker(shared: &Shared) {
    let mut q = shared.q.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if let Some(job) = q.jobs.pop_front() {
            drop(q);
            run(job);
            q = shared.q.lock().unwrap_or_else(|e| e.into_inner());
            q.pending -= 1;
            if q.pending == 0 {
                shared.drained.notify_all();
            }
            continue;
        }
        // Idle: exit once nothing is queued for a while, like the
        // executor's threads after shutdown.
        q.idle += 1;
        let (g, timeout) = shared
            .work
            .wait_timeout(q, std::time::Duration::from_secs(2))
            .unwrap_or_else(|e| e.into_inner());
        q = g;
        q.idle -= 1;
        if timeout.timed_out() && q.jobs.is_empty() {
            q.workers -= 1;
            return;
        }
    }
}

/// The per-album hook: `ensure_thumb(album, force=fresh)` on the pool.
pub fn ensure_thumb_job(pool: &ThumbPool, thumbs_dir: PathBuf, album: &Json, fresh: bool) {
    let id = album
        .get("id")
        .and_then(Json::as_str)
        .unwrap_or("")
        .to_string();
    let path = album
        .get("path")
        .and_then(Json::as_str)
        .unwrap_or("")
        .to_string();
    if id.is_empty() {
        return;
    }
    let album = album.clone();
    pool.submit(move || {
        let _ = crate::covers::ensure_thumb(&thumbs_dir, &id, &path, &album, fresh);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// services/scan.py:255: never more than four jobs at
    /// once, all of them run before `wait` returns, started in order.
    #[test]
    fn scan_thumb_pool_runs_four_at_a_time_in_order_and_drains() {
        let pool = ThumbPool::new();
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let order = Arc::new(Mutex::new(Vec::new()));
        for i in 0..12 {
            let (r, p, o) = (running.clone(), peak.clone(), order.clone());
            pool.submit(move || {
                o.lock().unwrap().push(i);
                let now = r.fetch_add(1, Ordering::SeqCst) + 1;
                p.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(20));
                r.fetch_sub(1, Ordering::SeqCst);
            });
        }
        pool.wait();
        assert_eq!(pool.pending(), 0);
        assert_eq!(peak.load(Ordering::SeqCst), MAX_WORKERS);
        let started = order.lock().unwrap().clone();
        let mut sorted = started.clone();
        sorted.sort();
        assert_eq!(sorted, (0..12).collect::<Vec<_>>());
        // FIFO: job i starts no later than four places after its turn.
        for (pos, i) in started.iter().enumerate() {
            assert!(pos.abs_diff(*i) < MAX_WORKERS, "{started:?}");
        }
        // A panicking job does not stop the pool.
        pool.submit(|| panic!("boom"));
        pool.submit(|| {});
        pool.wait();
        assert_eq!(pool.pending(), 0);
    }
}
