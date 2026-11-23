//! The `scan` group: the library scan's whole lifecycle. One scan runs at a
//! time; any number of subscribers each replay it from its first event.
//!
//! Sequence of events: `Start`; then `Album`, `Progress` and `MarkedMissing`
//! in walk order (and `Adopted` once the save has committed); optionally
//! `Cancelling`; then exactly one of `Done`, `Aborted`, `Cancelled` and
//! `Error`, and nothing after it (`contracts/events.md`).
//!
//! A scan holds the work guard for as long as it runs. `cancel` takes none: it
//! writes nothing and must work while a restore holds the gate.

use std::time::Duration;

pub use musaic_db::albums::ScanAbort;
pub use musaic_services::scan::{CancelPhase, ScanCancelled, ScanDone};
use musaic_services::scan::{ScanEventKind, ScanSubscription as ServiceSubscription};
pub use musaic_services::{CancelScan as CancelOutcome, ScanState, ScanStatus};

use super::error::OpError;
use super::record::AlbumRecord;
use crate::App;

/// Whether a scanned album came from the cache or from its files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlbumSource {
    Cached,
    Fresh,
}

/// One event of a scan.
#[derive(Debug, Clone, PartialEq)]
pub enum ScanEvent {
    Start {
        library_dir: String,
        total: usize,
        cached_count: usize,
        force: bool,
    },
    Album {
        album: AlbumRecord,
        source: AlbumSource,
    },
    Progress {
        scanned: usize,
        total: usize,
    },
    /// An album whose folder was not found (Present to Missing).
    MarkedMissing {
        id: String,
        path: String,
    },
    /// A vanished album's row moved to the same folder found under another
    /// spelling.
    Adopted {
        old_id: String,
        id: String,
        old_path: String,
        path: String,
    },
    /// A cancel was accepted; the scan ends `Cancelled` at its next check.
    Cancelling {
        scanned: usize,
        total: usize,
    },
    Done(ScanDone),
    /// The sanity check judged the walk untrustworthy; nothing was written.
    Aborted(ScanAbort),
    /// Stopped by a cancel; nothing was written.
    Cancelled(ScanCancelled),
    Error {
        detail: String,
    },
}

impl ScanEvent {
    /// The event ends the scan: nothing follows it.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            ScanEvent::Done(_)
                | ScanEvent::Aborted(_)
                | ScanEvent::Cancelled(_)
                | ScanEvent::Error { .. }
        )
    }

    /// The event's name (`start`, `album`, `progress`, `marked_missing`,
    /// `adopted`, `cancelling`, `done`, `aborted`, `cancelled`, `error`).
    pub fn name(&self) -> &'static str {
        match self {
            ScanEvent::Start { .. } => "start",
            ScanEvent::Album { .. } => "album",
            ScanEvent::Progress { .. } => "progress",
            ScanEvent::MarkedMissing { .. } => "marked_missing",
            ScanEvent::Adopted { .. } => "adopted",
            ScanEvent::Cancelling { .. } => "cancelling",
            ScanEvent::Done(_) => "done",
            ScanEvent::Aborted(_) => "aborted",
            ScanEvent::Cancelled(_) => "cancelled",
            ScanEvent::Error { .. } => "error",
        }
    }
}

impl From<ScanEventKind> for ScanEvent {
    fn from(kind: ScanEventKind) -> Self {
        match kind {
            ScanEventKind::Start {
                library_dir,
                total,
                cached_count,
                force,
            } => ScanEvent::Start {
                library_dir,
                total,
                cached_count,
                force,
            },
            ScanEventKind::Album { album, status } => ScanEvent::Album {
                album: AlbumRecord::new(album),
                source: if status == "fresh" {
                    AlbumSource::Fresh
                } else {
                    AlbumSource::Cached
                },
            },
            ScanEventKind::Progress { scanned, total } => ScanEvent::Progress { scanned, total },
            ScanEventKind::MarkedMissing { id, path } => ScanEvent::MarkedMissing { id, path },
            ScanEventKind::Adopted {
                old_id,
                id,
                old_path,
                path,
            } => ScanEvent::Adopted {
                old_id,
                id,
                old_path,
                path,
            },
            ScanEventKind::Cancelling { scanned, total } => {
                ScanEvent::Cancelling { scanned, total }
            }
            ScanEventKind::Done(d) => ScanEvent::Done(d),
            ScanEventKind::Aborted(a) => ScanEvent::Aborted(a),
            ScanEventKind::Cancelled(c) => ScanEvent::Cancelled(c),
            ScanEventKind::Error { detail } => ScanEvent::Error { detail },
        }
    }
}

/// What [`ScanSubscription::next_timeout`] found.
#[derive(Debug, Clone, PartialEq)]
pub enum ScanPoll {
    Event(ScanEvent),
    /// Nothing new within the timeout.
    Pending,
    /// The scan is over and everything was delivered.
    Finished,
}

/// A reader of the current scan's events, from its first. Dropping it
/// unsubscribes; the scan keeps running.
pub struct ScanSubscription {
    inner: ServiceSubscription,
}

impl ScanSubscription {
    /// Blocking: the next event, waiting up to `timeout` for one.
    pub fn next_timeout(&mut self, timeout: Duration) -> ScanPoll {
        match self.inner.next_timeout(timeout) {
            musaic_services::Poll::Event(e) => ScanPoll::Event(e.kind.into()),
            musaic_services::Poll::Pending => ScanPoll::Pending,
            musaic_services::Poll::Finished => ScanPoll::Finished,
        }
    }
}

impl Iterator for ScanSubscription {
    type Item = ScanEvent;

    /// Blocks until the next event; `None` once the scan is over and
    /// everything was delivered.
    fn next(&mut self) -> Option<ScanEvent> {
        self.inner.next().map(|e| e.kind.into())
    }
}

/// What [`Scan::follow`] found.
pub enum ScanFollow {
    /// A scan runs, or the last one has ended: its events from the first
    /// (as far as the buffer keeps them), ending with its terminal event.
    Following(ScanSubscription),
    /// No scan has run since the app started; there is nothing to follow.
    /// The state is the idle one, for the face to say so.
    Idle(ScanState),
}

/// What [`Scan::rescan`] started.
#[derive(Debug, Clone, PartialEq)]
pub struct RescanStarted {
    pub library_dir: String,
    pub scan: ScanState,
}

/// The `scan` operations.
pub struct Scan<'a> {
    pub(super) app: &'a App,
}

impl Scan<'_> {
    /// The configured library folder, which must be there.
    fn library(&self) -> Result<String, OpError> {
        musaic_services::library_ops::configured_library(self.app.library_dir())
    }

    /// The scan's state now.
    pub fn status(&self) -> ScanState {
        self.app.0.scan.state()
    }

    /// Start a scan unless one runs (then its state is returned and nothing
    /// is started). `Busy` while a restore runs, then `NoLibrary`.
    pub fn start(&self, force: bool) -> Result<ScanState, OpError> {
        let guard = self.app.gate().begin_work()?;
        let lib = self.library()?;
        Ok(self.app.start_scan(&lib, force, guard))
    }

    /// Observe the exact scan started or joined by an agent operation.
    pub(crate) fn start_observed(&self) -> Result<musaic_services::scan::ScanRun, OpError> {
        let guard = self.app.gate().begin_work()?;
        let lib = self.library()?;
        Ok(self.app.start_observed_scan(&lib, false, guard).1)
    }

    /// Start a scan unless one runs, and follow the current one from its first
    /// event. Takes the guard before it checks the library (a restore running
    /// is reported first); a guard a running scan makes unnecessary is
    /// released at once.
    pub fn subscribe(&self, force: bool) -> Result<ScanSubscription, OpError> {
        let guard = self.app.gate().begin_work()?;
        let lib = self.library()?;
        let (inner, unused) = self.app.scan_and_subscribe(&lib, force, guard);
        drop(unused);
        Ok(ScanSubscription { inner })
    }

    /// Follow the current scan, or replay the last one, and never start one.
    ///
    /// For a reader that only watches (the Activity panel): [`subscribe`]
    /// starts a scan when none runs, so a watcher that arrives just after a
    /// scan ended would start one nobody asked for. Here a running scan is
    /// joined from its first event; a scan that already ended is replayed
    /// from its buffer, ending with its terminal event, so a late watcher
    /// still learns how it ended; with no scan since startup the answer is
    /// [`ScanFollow::Idle`]. Takes no guard and checks no library: it writes
    /// nothing and starts nothing, so it is never refused (not even while a
    /// restore holds the gate).
    ///
    /// [`subscribe`]: Self::subscribe
    pub fn follow(&self) -> ScanFollow {
        let state = self.app.0.scan.state();
        if state.status == ScanStatus::Idle {
            return ScanFollow::Idle(state);
        }
        // A scan that starts between the read above and here is the one
        // followed: the subscription reads whichever buffer is current.
        ScanFollow::Following(ScanSubscription {
            inner: self.app.0.scan.subscribe(0),
        })
    }

    /// Forget the library's scan time and every album's recorded folder time
    /// (the snapshot) and start a forced scan. The guard covers the reset as
    /// well as the scan; it is taken before the library is checked.
    pub fn rescan(&self) -> Result<RescanStarted, OpError> {
        let guard = self.app.gate().begin_work()?;
        let lib = self
            .app
            .library_dir()
            .filter(|l| !l.is_empty())
            .ok_or_else(musaic_services::library_ops::no_library)?;
        let mut conn = musaic_db::open_connection(self.app.db_path())?;
        musaic_services::library_ops::drop_snapshot(&mut conn, &lib)?;
        drop(conn);
        let scan = self.app.start_scan(&lib, true, guard);
        Ok(RescanStarted {
            library_dir: lib,
            scan,
        })
    }

    /// Ask the running scan to stop. Never refused: it races with the scan
    /// ending on its own, so the answer says what happened
    /// (`Requested`, `AlreadyRequested`, `TooLate`, `NotRunning`). No guard.
    pub fn cancel(&self) -> CancelOutcome {
        self.app.cancel_scan()
    }
}
