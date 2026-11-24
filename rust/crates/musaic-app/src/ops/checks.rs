//! The `checks` group: the health checks, their runs and findings, the three
//! previews a resolve dialog asks for, the duplicate cluster's MusicBrainz
//! title, the background sweep's progress, and the feed of check runs.
//!
//! [`Checks::run`] and ignore / unignore take the work guard first (a restore
//! running makes them `Busy`, with nothing changed); a run holds it until it
//! ends. The reads take none. A run publishes `ChecksRerunFinished` through the
//! bus (a report, never a trigger), so a face watching the library events sees
//! the findings are written. The previews re-derive their plan from the current
//! database and disk; a finding only supplies the references (album id) and the
//! wording of a headline.
//!
//! A run takes the guard before it looks at its scope and the library (a restore
//! running answers `Busy` even for a bad scope, as the route always did), and an
//! unknown check or an `albums` scope without ids is `Rejected`, not `NotFound`.
//! A check run never touches a Missing album: the runner drops them from scope
//! and keeps their findings.
//!
//! [`Checks::run_cancellable`] (the agent bridge's) also takes a cancel flag:
//! once it is set the check stops before its next album, and the run ends
//! `failed` with "check run cancelled", keeping the findings it wrote, resolving
//! nothing and recording no inputs (the background sweep evaluates every album
//! again). It still publishes `ChecksRerunFinished`, listing the check in
//! `failed`. [`Checks::run`] (the desktop's) has no flag.

use std::sync::atomic::AtomicBool;

use musaic_checks::ScopeKind;
use musaic_checks::runner;
pub use musaic_db::findings::RunRow as CheckRun;
use musaic_db::findings::{FindingFilter, list_findings};
use musaic_services::check_ops;
pub use musaic_services::check_ops::{ConversionPreview, PlanPreview};
pub use musaic_services::check_sweep::SweepState;
use musaic_services::error::NoLibraryKind;
use musaic_services::gate::WorkKind;
use musaic_services::recording_canonical::duplicate_finding_canonical;
pub use musaic_services::recording_canonical::{CanonicalRecording, Reason};
use rusqlite::Connection;

use super::error::{OpError, Subject};
use super::events::Feed;
use super::record::{CheckDetailRecord, CheckSummaryRecord, ClusterPreview, FindingRecord};
use crate::App;

/// How many of a check's runs [`Checks::get`] reports.
const RUNS_SHOWN: i64 = 10;

/// What [`Checks::list_findings`] selects. The default is every open finding
/// of every check.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FindingsQuery {
    pub check_id: Option<String>,
    pub album_id: Option<String>,
    pub include_ignored: bool,
    pub include_resolved: bool,
}

/// The list of checks as a [`CheckRunsState`] carries it.
#[derive(Debug, Clone, PartialEq)]
pub enum CheckList {
    /// The checks did not change since the previous value.
    Unchanged,
    /// Every check with its open count and last run, as they are now.
    Listed(Vec<CheckSummaryRecord>),
    /// The list could not be read (the database failed); nothing to show.
    Unreadable,
}

/// One value of the check-runs feed: the checks when they changed (a run
/// started or ended, a finding was ignored or unignored, a fix or a rerun
/// wrote findings) and the sweep's progress when that changed. The first value
/// carries both.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckRunsState {
    pub checks: CheckList,
    pub sweep: Option<SweepState>,
}

/// The `checks` operations.
pub struct Checks<'a> {
    pub(super) app: &'a App,
}

impl Checks<'_> {
    fn conn(&self) -> Result<Connection, OpError> {
        Ok(musaic_db::open_connection(self.app.db_path())?)
    }

    fn summaries(&self, conn: &Connection) -> Result<Vec<CheckSummaryRecord>, OpError> {
        Ok(
            runner::list_checks_with_summary(conn, &self.app.0.checks_env)?
                .into_iter()
                .map(CheckSummaryRecord::new)
                .collect(),
        )
    }

    /// Every registered check with its open-finding count and last run.
    pub fn list(&self) -> Result<Vec<CheckSummaryRecord>, OpError> {
        self.summaries(&self.conn()?)
    }

    /// One check with its last runs and its open findings. `NotFound` for an
    /// unknown check.
    pub fn get(&self, check_id: &str) -> Result<CheckDetailRecord, OpError> {
        let conn = self.conn()?;
        match runner::get_check_with_runs(&conn, &self.app.0.checks_env, check_id, RUNS_SHOWN)? {
            Some(doc) => Ok(CheckDetailRecord::new(doc)),
            None => Err(OpError::NotFound {
                details: None,
                what: Subject::Check,
                id: check_id.to_string(),
                message: runner::RunError::UnknownCheck(check_id.to_string()).to_string(),
            }),
        }
    }

    /// Findings matching `query`, in the order the database keeps them.
    pub fn list_findings(&self, query: &FindingsQuery) -> Result<Vec<FindingRecord>, OpError> {
        let conn = self.conn()?;
        let rows = list_findings(
            &conn,
            &FindingFilter {
                check_id: query.check_id.as_deref(),
                album_id: query.album_id.as_deref(),
                include_ignored: query.include_ignored,
                include_resolved: query.include_resolved,
                limit: None,
            },
        )?;
        Ok(rows
            .iter()
            .map(|f| FindingRecord::new(runner::finding_to_json(f)))
            .collect())
    }

    /// Run `check_id` over the library and record the run. `scope` is `all` or
    /// `albums` (then `album_ids` must name some).
    ///
    /// Order: the work guard (`Busy`), the scope (`Rejected`), the library
    /// (`NoLibrary`), then the run (`Rejected` for an unknown check or an
    /// `albums` scope without ids; `Internal` for a database failure). The guard
    /// is held for the whole run. `incremental` is not offered: it ran every
    /// album, resolved nothing and recorded no inputs, so the background sweep
    /// does that work instead.
    pub fn run(
        &self,
        check_id: &str,
        scope: &str,
        album_ids: Option<&[String]>,
    ) -> Result<CheckRun, OpError> {
        self.run_with(check_id, scope, album_ids, None)
    }

    /// [`run`](Self::run), stopped before the check's next album once
    /// `cancel` is set (the module docs). The same refusals in the same order.
    pub fn run_cancellable(
        &self,
        check_id: &str,
        scope: &str,
        album_ids: Option<&[String]>,
        cancel: &AtomicBool,
    ) -> Result<CheckRun, OpError> {
        self.run_with(check_id, scope, album_ids, Some(cancel))
    }

    fn run_with(
        &self,
        check_id: &str,
        scope: &str,
        album_ids: Option<&[String]>,
        cancel: Option<&AtomicBool>,
    ) -> Result<CheckRun, OpError> {
        let _guard = self.app.gate().begin(WorkKind::CheckRun)?;
        let kind = match ScopeKind::parse(scope) {
            Some(k @ (ScopeKind::All | ScopeKind::Albums)) => k,
            _ => {
                return Err(OpError::Rejected {
                    message: "scope must be 'all' or 'albums'".into(),
                    details: None,
                });
            }
        };
        let lib = self
            .app
            .library_dir()
            .filter(|l| !l.is_empty())
            .ok_or_else(|| OpError::NoLibrary {
                kind: NoLibraryKind::NotConfigured,
                message: "No library configured".into(),
            })?;
        let conn = self.conn()?;
        check_ops::run_check_logged(
            &conn,
            &self.app.0.checks_env,
            check_id,
            &lib,
            kind,
            album_ids,
            scope,
            self.app.log(),
            cancel,
        )
    }

    /// Ignore a finding of `check_id`, with an optional reason. `NotFound`
    /// unless the finding exists and belongs to that check. `None` when the
    /// finding was already ignored.
    pub fn ignore_finding(
        &self,
        check_id: &str,
        finding_id: i64,
        reason: Option<&str>,
    ) -> Result<Option<FindingRecord>, OpError> {
        let _guard = self.app.gate().begin_work()?;
        let conn = self.conn()?;
        let row =
            check_ops::ignore_owned(&conn, &self.app.0.checks_env, check_id, finding_id, reason)?;
        Ok(self.finding(row))
    }

    /// Undo an ignore. The same refusals as [`ignore_finding`](Self::ignore_finding);
    /// `None` when the finding was not ignored.
    pub fn unignore_finding(
        &self,
        check_id: &str,
        finding_id: i64,
    ) -> Result<Option<FindingRecord>, OpError> {
        let _guard = self.app.gate().begin_work()?;
        let conn = self.conn()?;
        let row = check_ops::unignore_owned(&conn, &self.app.0.checks_env, check_id, finding_id)?;
        Ok(self.finding(row))
    }

    fn finding(&self, row: Option<musaic_db::findings::FindingRow>) -> Option<FindingRecord> {
        row.map(|f| FindingRecord::new(runner::finding_to_json(&f)))
    }

    /// The rename plan of a filename-mismatch finding, re-derived now. A read:
    /// no guard. `NotFound` for a finding that is not of that check,
    /// `Rejected` for one without an album.
    pub fn filename_plan_preview(&self, finding_id: i64) -> Result<PlanPreview, OpError> {
        check_ops::filename_mismatch_plan_preview(&self.conn()?, finding_id)
    }

    /// The cluster of a duplicate-tracks finding, re-derived now. A read.
    pub fn cluster_preview(&self, finding_id: i64) -> Result<ClusterPreview, OpError> {
        check_ops::duplicate_cluster_preview(&self.conn()?, finding_id).map(ClusterPreview::new)
    }

    /// What converting a multi-disc set would do, re-derived from disk, with
    /// the unfinished conversion's record when there is one. A read: nothing is
    /// converted. The preview's digest is what a conversion must be sent back.
    pub fn conversion_preview(&self, finding_id: i64) -> Result<ConversionPreview, OpError> {
        let logs = musaic_services::multi_disc_convert::log_dir(self.app.config_dir());
        check_ops::multi_disc_conversion_preview(&self.conn()?, finding_id, Some(&logs))
    }

    /// What MusicBrainz calls the recording a duplicate-tracks finding is about.
    /// A read: no guard, and no refusal for a Missing album (no album file is
    /// touched). The answers are kept for the application's lifetime, the
    /// definitive ones only. A lookup that failed is an answer with a reason,
    /// not an error.
    pub fn duplicate_canonical(&self, finding_id: i64) -> Result<CanonicalRecording, OpError> {
        let conn = self.conn()?;
        let mb = musaic_net::MusicBrainzClient::new(self.app.0.transport.clone());
        duplicate_finding_canonical(&conn, finding_id, &mb, &self.app.0.recording_canonical)
    }

    /// The background check sweep's progress now.
    pub fn sweep_status(&self) -> SweepState {
        self.app.sweeper().state()
    }

    /// The checks now and their sweep, then a value after every change of
    /// either: a run starting or ending, a finding ignored or unignored, the
    /// sweep moving. Both subscriptions are taken before the first value is
    /// read, so a change in between arrives as one more value. A value reads the
    /// database, so a face on an async thread waits with [`Feed::wait`] and reads
    /// on a blocking one.
    pub fn runs_feed(&self) -> Feed<CheckRunsState> {
        let tick = self.app.0.checks_tick.subscribe();
        let sweep = self.app.0.sweep_changes.subscribe();
        let app = self.app.clone();
        Feed::counted_pair(
            tick,
            sweep,
            self.app.shutdown_signal(),
            move |checks_changed, sweep_changed| CheckRunsState {
                checks: if checks_changed {
                    match app.ops().checks().list() {
                        Ok(list) => CheckList::Listed(list),
                        Err(_) => CheckList::Unreadable,
                    }
                } else {
                    CheckList::Unchanged
                },
                sweep: sweep_changed.then(|| app.sweeper().state()),
            },
        )
    }
}
