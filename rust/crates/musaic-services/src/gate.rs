//! One gate between a database restore and the work that writes to the
//! database (a deliberate change from the Python app).
//!
//! * A restore is refused (409) while a scan, a check run or an apply holds
//!   a [`WorkGuard`].
//! * While a restore holds the [`RestoreGuard`], new scans, check runs and
//!   applies are refused (409).
//!
//! The HTTP host (musaic-api's `gate` module) turns a refusal ([`Busy`])
//! into the 409 with [`Busy::detail`]; the gate itself knows nothing about
//! HTTP, so a non-HTTP face over the same services shares it unchanged.
//!
//! Both guards are RAII: they release the gate when dropped, which also
//! happens when the thread holding one unwinds from a panic. A guard is
//! moved into the task that does the work (the scan thread, the apply's
//! blocking task, the check run's blocking task), so a client that goes
//! away releases the gate when the work it started actually ends, not
//! before (an apply runs to completion after a disconnect).
//!
//! Read-only requests never take the gate. The Python app had no counterpart:
//! it swaps the database file under whatever is running.
//!
//! Every route that writes takes the gate (409 while a
//! restore runs), not only scans, check runs and applies: delete album,
//! clear MusicBrainz anchor, ignore / unignore, `POST /api/config`,
//! `PATCH /api/settings`, snapshot create / delete, analyze, and
//! `GET /api/albums/{id}/resolution` (it can store fingerprints and
//! anchors). musaic-api's `tests/restore_gate.rs` walks the route
//! inventory so a new writing route cannot be left out.
//!
//! A bus-driven check rerun refused because a restore runs is not
//! lost. [`WorkGate::begin_rerun`] records it (under the gate's lock, so a
//! restore ending at that moment cannot miss it) and the restore's guard,
//! when it drops, hands everything recorded to the hook set with
//! [`WorkGate::set_after_restore`]: one coalesced library-wide rerun (plus
//! the album-scoped filename-mismatch reruns that were skipped).
//!
//! The busy 409 comes before every check a handler makes
//! itself (unknown id, bad snapshot name, unknown check, no library, ...).
//! Only FastAPI's own request validation (422 for the body, query and path
//! parameter types) comes first, as it ran before the Python app's handler.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, RwLock};

/// 409 detail when a restore is refused because work is running.
pub const RESTORE_REFUSED: &str = "Cannot restore the database while a scan, check run, apply or other change to the library is in progress. Try again when it finishes.";
/// 409 detail when work is refused because a restore is running.
pub const WORK_REFUSED: &str = "A database restore is in progress. Try again when it finishes.";

/// The database file's lock: a restore replaces the file and takes it for
/// writing; every other database user takes it for reading (a poisoned lock
/// is used as is). Held by musaic-app's `App` as `db_gate`.
pub type DbGate = RwLock<()>;

/// What a [`WorkGuard`] is held for (the desktop shell names the work a
/// quit would interrupt). Every writing route is at least
/// [`WorkKind::Other`]; the long-running kinds are labelled where they start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WorkKind {
    Scan,
    Apply,
    Analyze,
    CheckRun,
    QuickFixAll,
    /// A short write (settings, tags, delete album, ...).
    Other,
}

impl WorkKind {
    pub const ALL: [WorkKind; 6] = [
        WorkKind::Scan,
        WorkKind::Apply,
        WorkKind::Analyze,
        WorkKind::CheckRun,
        WorkKind::QuickFixAll,
        WorkKind::Other,
    ];

    fn index(self) -> usize {
        self as usize
    }

    /// A short phrase for a confirmation message.
    pub fn describe(self) -> &'static str {
        match self {
            WorkKind::Scan => "a library scan",
            WorkKind::Apply => "applying changes to an album",
            WorkKind::Analyze => "audio analysis",
            WorkKind::CheckRun => "a health check run",
            WorkKind::QuickFixAll => "a bulk quick fix",
            WorkKind::Other => "a change to the library",
        }
    }
}

#[derive(Debug, Default)]
struct State {
    /// Writing work in progress.
    active: usize,
    /// `active` split by [`WorkKind`] (indexed by `WorkKind::index`).
    kinds: [usize; 6],
    restoring: bool,
    /// Reruns refused while the current restore runs.
    deferred: Option<DeferredRerun>,
}

/// What the check reruns refused during a restore leave to do once it
/// ends: always one library-wide post-scan rerun, plus filename-mismatch
/// over the albums whose rename-triggered rerun was refused.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DeferredRerun {
    pub filename_mismatch: BTreeSet<String>,
}

/// Called (on the thread that drops the restore guard) with the reruns that
/// were refused during the restore.
pub type AfterRestore = Arc<dyn Fn(DeferredRerun) + Send + Sync>;

/// The gate. Shared through an `Arc` so guards can outlive the request.
#[derive(Default)]
pub struct WorkGate {
    state: Mutex<State>,
    after_restore: OnceLock<AfterRestore>,
}

impl std::fmt::Debug for WorkGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkGate")
            .field("state", &*self.lock())
            .finish_non_exhaustive()
    }
}

/// Held by writing work (a scan, a check run, an apply, any writing route)
/// for as long as it runs.
#[derive(Debug)]
#[must_use = "the gate is released when the guard is dropped"]
pub struct WorkGuard {
    gate: Arc<WorkGate>,
    kind: WorkKind,
}

/// Held by a restore for as long as it runs.
#[derive(Debug)]
#[must_use = "the gate is released when the guard is dropped"]
pub struct RestoreGuard(Arc<WorkGate>);

/// Why the gate refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Busy {
    /// Work asked while a restore runs.
    Restoring,
    /// A restore asked while work runs.
    WorkRunning,
}

impl Busy {
    pub fn detail(self) -> &'static str {
        match self {
            Busy::Restoring => WORK_REFUSED,
            Busy::WorkRunning => RESTORE_REFUSED,
        }
    }
}

impl std::fmt::Display for Busy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.detail())
    }
}

impl std::error::Error for Busy {}

impl WorkGate {
    pub fn new() -> Arc<WorkGate> {
        Arc::new(WorkGate::default())
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // A panic while the mutex is held cannot leave the counters half
        // updated (every critical section is a single assignment).
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Start a scan, check run or apply; refused while a restore runs.
    pub fn begin_work(self: &Arc<Self>) -> Result<WorkGuard, Busy> {
        self.begin(WorkKind::Other)
    }

    /// [`begin_work`](Self::begin_work) labelled with what the work is.
    pub fn begin(self: &Arc<Self>, kind: WorkKind) -> Result<WorkGuard, Busy> {
        let mut s = self.lock();
        if s.restoring {
            return Err(Busy::Restoring);
        }
        s.active += 1;
        s.kinds[kind.index()] += 1;
        Ok(WorkGuard {
            gate: self.clone(),
            kind,
        })
    }

    /// [`begin_work`](Self::begin_work) for a bus-driven check rerun: when a
    /// restore refuses it, the rerun is recorded (with the albums of a
    /// skipped filename-mismatch rerun) and handed to the after-restore hook
    /// when the restore ends.
    pub fn begin_rerun(self: &Arc<Self>, filename_mismatch: &[String]) -> Result<WorkGuard, Busy> {
        let mut s = self.lock();
        if s.restoring {
            s.deferred
                .get_or_insert_with(DeferredRerun::default)
                .filename_mismatch
                .extend(filename_mismatch.iter().cloned());
            return Err(Busy::Restoring);
        }
        s.active += 1;
        s.kinds[WorkKind::CheckRun.index()] += 1;
        Ok(WorkGuard {
            gate: self.clone(),
            kind: WorkKind::CheckRun,
        })
    }

    /// The hook a restore's end hands refused reruns to. Set once (later
    /// calls are ignored).
    pub fn set_after_restore(&self, hook: AfterRestore) {
        let _ = self.after_restore.set(hook);
    }

    /// Reruns recorded during the restore in progress, for tests.
    pub fn deferred(&self) -> Option<DeferredRerun> {
        self.lock().deferred.clone()
    }

    /// Start a restore; refused while any work (or another restore) runs.
    pub fn begin_restore(self: &Arc<Self>) -> Result<RestoreGuard, Busy> {
        let mut s = self.lock();
        if s.restoring {
            return Err(Busy::Restoring);
        }
        if s.active > 0 {
            return Err(Busy::WorkRunning);
        }
        s.restoring = true;
        Ok(RestoreGuard(self.clone()))
    }

    /// Whether [`begin_restore`](Self::begin_restore) would succeed now
    /// (an early answer; the guard is what counts).
    pub fn restore_allowed(&self) -> Result<(), Busy> {
        let s = self.lock();
        if s.restoring {
            Err(Busy::Restoring)
        } else if s.active > 0 {
            Err(Busy::WorkRunning)
        } else {
            Ok(())
        }
    }

    /// (work in progress, restoring), for tests and diagnostics.
    pub fn snapshot(&self) -> (usize, bool) {
        let s = self.lock();
        (s.active, s.restoring)
    }

    /// Work in progress by kind (kinds with none left out), and whether a
    /// restore runs (the desktop shell asks before quitting while work runs).
    pub fn running(&self) -> (Vec<(WorkKind, usize)>, bool) {
        let s = self.lock();
        let kinds = WorkKind::ALL
            .iter()
            .filter_map(|k| {
                let n = s.kinds[k.index()];
                (n > 0).then_some((*k, n))
            })
            .collect();
        (kinds, s.restoring)
    }
}

impl WorkGuard {
    pub fn kind(&self) -> WorkKind {
        self.kind
    }

    /// Relabel the work this guard covers (a scan request becomes a
    /// [`WorkKind::Scan`] once the scan has actually started).
    pub fn relabel(&mut self, kind: WorkKind) {
        if kind == self.kind {
            return;
        }
        let mut s = self.gate.lock();
        s.kinds[self.kind.index()] = s.kinds[self.kind.index()].saturating_sub(1);
        s.kinds[kind.index()] += 1;
        self.kind = kind;
    }
}

impl Drop for WorkGuard {
    fn drop(&mut self) {
        let mut s = self.gate.lock();
        s.active = s.active.saturating_sub(1);
        s.kinds[self.kind.index()] = s.kinds[self.kind.index()].saturating_sub(1);
    }
}

impl Drop for RestoreGuard {
    fn drop(&mut self) {
        let deferred = {
            let mut s = self.0.lock();
            s.restoring = false;
            s.deferred.take()
        };
        // Outside the lock: the hook starts work, which takes the gate.
        if let (Some(d), Some(hook)) = (deferred, self.0.after_restore.get()) {
            hook(d);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Guards are counted by kind, relabelled, released on drop;
    // reruns count as check runs; the restore flag is reported.
    #[test]
    fn work_kinds_are_counted() {
        let g = WorkGate::new();
        assert_eq!(g.running(), (vec![], false));
        let a = g.begin(WorkKind::Apply).unwrap();
        let mut o = g.begin_work().unwrap();
        let r = g.begin_rerun(&[]).unwrap();
        assert_eq!(
            g.running().0,
            vec![
                (WorkKind::Apply, 1),
                (WorkKind::CheckRun, 1),
                (WorkKind::Other, 1)
            ]
        );
        o.relabel(WorkKind::Scan);
        assert_eq!(o.kind(), WorkKind::Scan);
        assert_eq!(
            g.running().0,
            vec![
                (WorkKind::Scan, 1),
                (WorkKind::Apply, 1),
                (WorkKind::CheckRun, 1)
            ]
        );
        assert_eq!(g.snapshot(), (3, false));
        drop((a, o, r));
        assert_eq!(g.running(), (vec![], false));
        let rg = g.begin_restore().unwrap();
        assert_eq!(g.running(), (vec![], true));
        assert!(g.begin(WorkKind::QuickFixAll).is_err());
        drop(rg);
        assert_eq!(g.snapshot(), (0, false));
    }
}
