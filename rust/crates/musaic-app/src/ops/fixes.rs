//! The `fixes` group: act on a check's findings. Fix one, fix every open
//! finding of a check in one run, apply the plan of a filename-mismatch finding,
//! delete a duplicate file, write one artist-sort value across a cluster, and
//! convert (or abandon the conversion of) a multi-disc set.
//!
//! Every operation writes, so each takes the restore gate's work guard first,
//! before it looks at its input (a restore running answers `Busy` even for an
//! unknown finding), and holds it until its work ends.
//!
//! [`Fixes::fix`] sends its own follow-up: a fix that fell through to the AI
//! queue and enqueued a job wakes the dispatcher and every jobs feed once the
//! job row is written. With no model backend such a fix answers a not-available
//! message and enqueues nothing.
//!
//! [`Fixes::quick_fix_all`] and [`Fixes::convert_multi_disc`] are runs with a
//! sink, the shape of [`analyze`](super::analyze): every refusal is returned
//! before the sink is called. A sink that answers [`Listening::Gone`] (or panics)
//! is not called again. Quick-fix-all then stops at the next finding, as it
//! always has when its client went away, and still publishes
//! `BulkBatchCompleted`; a conversion runs to its end.

use std::cell::RefCell;
use std::ops::ControlFlow;
use std::panic::{AssertUnwindSafe, catch_unwind};

use musaic_services::fix_flow;
use musaic_services::gate::WorkKind;
use musaic_services::multi_disc_convert;
use musaic_services::multi_disc_gather;

pub use musaic_services::check_fixes::{
    DuplicateDeleted, FixResult, PlanEntry, QuickFixEvent, QuickFixOutcome, QuickFixSummary,
    RenameOutcome, SortResult,
};
pub use musaic_services::multi_disc_convert::ConvertProgress;

use super::error::OpError;
pub use super::events::Listening;
use super::record::{AbandonRecord, ConversionRecord};
use crate::App;

/// How a quick-fix-all run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuickFixEnd {
    /// Every finding was dealt with; the same summary was the last event.
    Finished(QuickFixSummary),
    /// No summary followed: the sink went away, or the loop itself failed (its
    /// `Error` event says how).
    Stopped,
}

/// What a running multi-disc conversion reports. The conversion's answer is the
/// operation's return value, not an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConvertEvent {
    /// The work guard is held: a restore is not running. Always the first
    /// event. A refusal that follows is the operation's return value, for a
    /// face that has already begun its answer.
    Started,
    /// A long step is under way (`rename_files`, `write_tags`, `move_files`).
    /// Droppable: a face that cannot keep up may skip one.
    Progress(ConvertProgress),
}

/// The `fixes` operations.
pub struct Fixes<'a> {
    pub(super) app: &'a App,
}

impl Fixes<'_> {
    /// Fix one finding of `check_id`, with the user's `hint` ("" for none).
    ///
    /// Refusals, with nothing changed: a restore running (`Busy`, before the
    /// finding is looked up), a finding that does not exist or belongs to
    /// another check (`NotFound`), a check that is not registered or has no fix
    /// path (`Rejected`). A finding of a Missing album is not attempted: the
    /// result says so (`ok` false). The checks feed is told after every attempt.
    pub fn fix(&self, check_id: &str, finding_id: i64, hint: &str) -> Result<FixResult, OpError> {
        let guard = self.app.gate().begin_work()?;
        let conn = musaic_db::open_connection(self.app.db_path())?;
        let ids = self.app.0.ai.ids.clone();
        let run = self.app.with_fix_env(conn, |env| {
            fix_flow::fix_finding(env, check_id, finding_id, hint, &*ids, &|| {
                self.app.now_iso()
            })
        });
        drop(guard);
        let (result, enqueued) = run?;
        if enqueued {
            self.app.ai_jobs_changed();
        }
        Ok(result)
    }

    /// Run the quick fix over every open finding of `check_id`, reporting each
    /// step to `sink`: `Start`, one `Progress` per finding, `Done`.
    ///
    /// The work guard (`QuickFixAll`) is taken first and held until after the
    /// last event; then an unknown check is `NotFound`, all before the sink is
    /// called. The loop runs under `in_bulk_run` and publishes one
    /// `BulkBatchCompleted` at the end, also when the sink went away or a fix
    /// panicked. A fix that panics is an `Error` progress and the loop goes on.
    pub fn quick_fix_all(
        &self,
        check_id: &str,
        sink: &mut dyn FnMut(QuickFixEvent) -> Listening,
    ) -> Result<QuickFixEnd, OpError> {
        let _guard = self.app.gate().begin(WorkKind::QuickFixAll)?;
        let conn = musaic_db::open_connection(self.app.db_path())?;
        let pause = self.app.0.quick_fix_pause.clone();
        let mut listening = true;
        let mut summary = None;
        let run = self.app.with_fix_env(conn, |env| {
            musaic_services::check_fixes::quick_fix_all(
                env,
                check_id,
                &mut || pause(),
                &mut |event| {
                    if let QuickFixEvent::Done(s) = &event {
                        summary = Some(*s);
                    }
                    if listening {
                        // A sink that panics is treated as gone.
                        listening = matches!(
                            catch_unwind(AssertUnwindSafe(|| sink(event))),
                            Ok(Listening::Yes)
                        );
                    }
                    if listening {
                        ControlFlow::Continue(())
                    } else {
                        ControlFlow::Break(())
                    }
                },
            )
        });
        run?;
        Ok(summary.map_or(QuickFixEnd::Stopped, QuickFixEnd::Finished))
    }

    /// Carry out the rename plan of a filename-mismatch finding: `plan` (file
    /// renames) and/or `dir_rename` (the folder).
    ///
    /// Refusals, nothing changed: `Busy`; a finding that does not exist
    /// (`NotFound`); one with no album, or with neither a plan nor a folder
    /// rename (`Rejected`); a Missing album, or a rename that did nothing
    /// (`Conflict`, the latter with the rename code's `Refusal` as details).
    /// Publishes the `AlbumRenamed` the rename produced.
    pub fn apply_filename_rename(
        &self,
        finding_id: i64,
        plan: &[PlanEntry],
        dir_rename: Option<&PlanEntry>,
    ) -> Result<RenameOutcome, OpError> {
        let _guard = self.app.gate().begin_work()?;
        let conn = musaic_db::open_connection(self.app.db_path())?;
        self.app.with_fix_env(conn, |env| {
            musaic_services::check_fixes::apply_filename_rename(env, finding_id, plan, dir_rename)
        })
    }

    /// Move one file of a duplicate-tracks finding to the trash and rescan the
    /// album. `Busy`; `NotFound` for the finding or its album; `Rejected` for a
    /// name that is not one of the finding's duplicates or that lies outside
    /// the album folder; `Conflict` when the album folder cannot be reached
    /// (nothing is deleted).
    pub fn delete_duplicate_file(
        &self,
        finding_id: i64,
        filename: &str,
    ) -> Result<DuplicateDeleted, OpError> {
        let _guard = self.app.gate().begin_work()?;
        let conn = musaic_db::open_connection(self.app.db_path())?;
        self.app.with_fix_env(conn, |env| {
            musaic_services::check_fixes::delete_duplicate_file(env, finding_id, filename)
        })
    }

    /// Write one artist-sort value to every album of an
    /// inconsistent-artist-sort cluster. `Busy`; `NotFound` for the finding;
    /// `Rejected` for an empty or over-long value or a finding with no cluster.
    /// Publishes one `BulkBatchCompleted`.
    pub fn apply_cluster_sort(
        &self,
        finding_id: i64,
        sort_value: &str,
    ) -> Result<SortResult, OpError> {
        let _guard = self.app.gate().begin_work()?;
        let conn = musaic_db::open_connection(self.app.db_path())?;
        self.app.with_fix_env(conn, |env| {
            musaic_services::check_fixes::apply_cluster_sort(env, finding_id, sort_value)
        })
    }

    /// Convert the multi-disc set of a multi-disc-layout finding to the layout
    /// Musaic aims for, if the plan re-derived now still has `digest` (the
    /// preview's) and nothing stops it. A set kept in one folder per disc is
    /// gathered into one.
    ///
    /// `sink`, when given, hears `Started` once the guard is held, then
    /// `Progress` while a long step runs. The answer is the return value: `Ok`
    /// when the conversion ran (also when a step failed: `ok` is then false,
    /// with a reason per file), else the refusal, nothing changed: `Busy`
    /// before the sink is called (the guard is `Apply`), then, after `Started`,
    /// `Rejected` (no album), `NotFound`, `Conflict` (no library, the album is
    /// Missing, a scan is running, the set cannot be converted, a blocker, the
    /// plan changed; the refusal's reason code, sentence, blockers and fresh
    /// digest are its details) or `Failed` (a record that could not be
    /// written). The scan service, fingerprint pool and check sweep stay out of
    /// the way for the whole conversion.
    pub fn convert_multi_disc(
        &self,
        finding_id: i64,
        digest: &str,
        sink: Option<&mut dyn FnMut(ConvertEvent) -> Listening>,
    ) -> Result<ConversionRecord, OpError> {
        let _guard = self.app.gate().begin(WorkKind::Apply)?;
        let sink = sink.map(|s| RefCell::new((s, true)));
        let emit = |event: ConvertEvent| {
            let Some(cell) = &sink else { return };
            let mut cell = cell.borrow_mut();
            let (sink, listening) = &mut *cell;
            if *listening {
                // A sink that panics is treated as gone.
                *listening = matches!(
                    catch_unwind(AssertUnwindSafe(|| sink(event))),
                    Ok(Listening::Yes)
                );
            }
        };
        emit(ConvertEvent::Started);
        let mut conn = musaic_db::open_connection(self.app.db_path())?;
        let report = |p: ConvertProgress| emit(ConvertEvent::Progress(p));
        let r = self
            .app
            .with_convert_env_reporting(&mut conn, &report, |env| {
                multi_disc_convert::convert(env, finding_id, digest)
            });
        r.map(ConversionRecord::new)
            .map_err(|e| e.into_op_error(finding_id))
    }

    /// Stop treating the album's unfinished conversion as one: its record is
    /// closed as abandoned and nothing on disk moves. Same refusals as the
    /// conversion's, plus `Conflict` when there is no unfinished conversion.
    pub fn abandon_multi_disc(&self, finding_id: i64) -> Result<AbandonRecord, OpError> {
        let _guard = self.app.gate().begin(WorkKind::Apply)?;
        let mut conn = musaic_db::open_connection(self.app.db_path())?;
        let r = self
            .app
            .with_convert_env(&mut conn, |env| multi_disc_gather::abandon(env, finding_id));
        r.map(AbandonRecord::new)
            .map_err(|e| e.into_op_error(finding_id))
    }
}
