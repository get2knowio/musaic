//! The `analyze` group: measure an album's tempo or ReplayGain and write the
//! result into its files.
//!
//! [`Analyze::run`] is the shape of every long run: it refuses before it says
//! anything (`Busy`, `NoLibrary`, `NotFound`, `Conflict`: nothing was read or
//! written and the sink was never called), then hands the sink one typed event
//! per step on the calling thread, `Done` last, and returns the outcome. The work
//! guard is taken first and held until after `Done`. A sink that answers
//! [`Listening::Gone`] is not called again; the run still analyses and writes
//! every file, rescans the album and publishes `AlbumTagsChanged`.
//!
//! [`Analyze::run_cancellable`] also takes a cancel flag (the agent bridge's):
//! once it is set the run stops before its next file, never inside one, writes
//! no album gain, still rescans the album and publishes `AlbumTagsChanged` when
//! it wrote any file, hands the sink `Done` with the partial counts and ends
//! [`AnalyzeOutcome::Cancelled`]. [`Analyze::run`] (the desktop's) has no flag
//! and always goes to the end.

use std::ops::ControlFlow;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::AtomicBool;

pub use musaic_services::analyze::AnalysisKind;
use musaic_services::analyze::{self, AnalyzeOutcome as Ended, AnalyzeRequest};
use musaic_services::gate::WorkKind;

use super::error::OpError;
pub use super::events::{AnalyzeEvent, AnalyzeSummary, FileResult, Listening};
use crate::App;

/// How a run that began ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnalyzeOutcome {
    /// Every file was dealt with; the same summary was the last event.
    Finished(AnalyzeSummary),
    /// The album's folder vanished after the checks, so no further event
    /// followed. `reason` is for a log.
    Aborted { reason: String },
    /// The cancel flag stopped the run between files; the summary (also the
    /// last event) counts the files dealt with before it stopped.
    Cancelled(AnalyzeSummary),
}

/// The `analyze` operations.
pub struct Analyze<'a> {
    pub(super) app: &'a App,
}

/// The log target the HTTP route used for the abort note, kept so the server
/// log reads as before.
const LOG_TARGET: &str = "musaic.api.analyze";

impl Analyze<'_> {
    /// Analyse `what` for every FLAC of the album, reporting each step to
    /// `sink`: [`prepare`](Self::prepare), then [`AnalyzeRun::run`].
    pub fn run(
        &self,
        album_id: &str,
        what: AnalysisKind,
        sink: &mut dyn FnMut(AnalyzeEvent) -> Listening,
    ) -> Result<AnalyzeOutcome, OpError> {
        self.prepare(album_id, what)?.run(sink)
    }

    /// [`run`](Self::run), stopped before the next file once `cancel` is set
    /// (the module docs).
    pub fn run_cancellable(
        &self,
        album_id: &str,
        what: AnalysisKind,
        cancel: &AtomicBool,
        sink: &mut dyn FnMut(AnalyzeEvent) -> Listening,
    ) -> Result<AnalyzeOutcome, OpError> {
        self.prepare(album_id, what)?.run_with(Some(cancel), sink)
    }

    /// The checks of an analysis, with nothing read or written: the returned
    /// run holds the work guard and the album found. A face that may lose its
    /// caller before the work starts drops it instead.
    ///
    /// Refusals: the work guard (`Busy` during a restore), no library folder
    /// (`NoLibrary`), an unknown album (`NotFound`), a Missing album
    /// (`Conflict`). An album whose folder cannot be listed is refused by the
    /// run (`Conflict`, before its first event). The event sequence is
    /// [`AnalyzeEvent`]'s.
    pub fn prepare(&self, album_id: &str, what: AnalysisKind) -> Result<AnalyzeRun, OpError> {
        let guard = self.app.gate().begin(WorkKind::Analyze)?;
        let conn = musaic_db::open_connection(self.app.db_path())?;
        let library = self.app.library_dir();
        let album =
            analyze::resolve_album(&conn, library.as_deref(), album_id, self.app.0.track_order)?;
        Ok(AnalyzeRun {
            app: self.app.clone(),
            guard,
            req: AnalyzeRequest {
                album_id: album.id,
                album_path: album.path,
                library_dir: Some(library.unwrap_or_default()),
                what,
            },
        })
    }
}

/// An analysis whose checks passed; nothing happens until [`run`](Self::run).
/// Dropping it releases the guard.
#[must_use = "nothing is analysed until the run is run"]
pub struct AnalyzeRun {
    app: App,
    guard: musaic_services::gate::WorkGuard,
    req: AnalyzeRequest,
}

impl AnalyzeRun {
    /// Analyse every file, reporting each step to `sink`, `Done` last. The
    /// guard is held until after it. A sink that answers [`Listening::Gone`]
    /// is not called again; the run still goes to its end.
    pub fn run(
        self,
        sink: &mut dyn FnMut(AnalyzeEvent) -> Listening,
    ) -> Result<AnalyzeOutcome, OpError> {
        self.run_with(None, sink)
    }

    fn run_with(
        self,
        cancel: Option<&AtomicBool>,
        sink: &mut dyn FnMut(AnalyzeEvent) -> Listening,
    ) -> Result<AnalyzeOutcome, OpError> {
        let AnalyzeRun { app, guard, req } = self;
        let _guard = guard;
        let what = req.what;
        let mut conn = musaic_db::open_connection(app.db_path())?;
        let mut listening = true;
        let mut summary = None;
        let ended = app.with_analyze_env(&mut conn, |env| {
            analyze::analyze_album_cancellable(env, &req, cancel, &mut |event| {
                if let AnalyzeEvent::Done(s) = &event {
                    summary = Some(s.clone());
                }
                if listening {
                    // A sink that panics is treated as gone, as the service does.
                    listening = matches!(
                        catch_unwind(AssertUnwindSafe(|| sink(event))),
                        Ok(Listening::Yes)
                    );
                }
                // A sink that stops listening never stops the run; only `cancel` does.
                ControlFlow::Continue(())
            })
        });
        match ended {
            Ended::Refused(e) => Err(e),
            Ended::Aborted(reason) => {
                app.log().warn(
                    LOG_TARGET,
                    &format!("analyze {} aborted: {reason}", req.album_id),
                );
                Ok(AnalyzeOutcome::Aborted { reason })
            }
            Ended::Finished | Ended::Stopped => Ok(AnalyzeOutcome::Finished(summary.unwrap_or(
                AnalyzeSummary {
                    what,
                    analyzed_count: 0,
                    errors: 0,
                },
            ))),
            Ended::Cancelled => Ok(AnalyzeOutcome::Cancelled(summary.unwrap_or(
                AnalyzeSummary {
                    what,
                    analyzed_count: 0,
                    errors: 0,
                },
            ))),
        }
    }
}
