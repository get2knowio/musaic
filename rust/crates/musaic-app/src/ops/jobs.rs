//! Desktop research and historical jobs. The jobs feed projects metadata and
//! cover requests into one card and wakes after committed changes. Each child
//! has its own claim, result, review decision and revision.
//!
//! Writers take the work guard before validating input. An unresolved request
//! for the same album, kind and input reuses its existing work. An old job's
//! history record is listed, read and deleted, never run again.

use musaic_services::research_request::{self as queue, NewJob};
use rusqlite::Connection;

pub use crate::desktop_research::{
    DelegatedItem, Preview as ResearchPreview, ReviewChild as ResearchItem,
};
pub use musaic_db::research_history::JobRow as Job;

use super::error::OpError;
use super::events::Feed;
use crate::App;

/// The queue's state: `paused` (always `false`) and the parallelism limit
/// (always 1), kept so the stream's frames keep their shape; how many jobs
/// run and wait now, and how many requests and history-only records each
/// Clear would remove: `clearable` for `delete_many(["acted_on"])`,
/// `clearable_failed` for `delete_many(["failed"])`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueState {
    pub paused: bool,
    pub max_parallel: i64,
    pub running: i64,
    pub queued: i64,
    pub clearable: i64,
    pub clearable_failed: i64,
}

/// The newest jobs (at most 200) with the queue's state: one value of the jobs
/// feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobsList {
    pub jobs: Vec<Job>,
    pub queue_state: QueueState,
}

/// A job that was deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletedJob {
    pub id: String,
}

/// What a bulk delete removed: how many rows, for which statuses (trimmed, as
/// they were applied).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletedJobs {
    pub count: usize,
    pub statuses: Vec<String>,
}

/// One bulk run's batch of research requests: its origin, size and counts
/// by state (derived from its items, never stored).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchSummary {
    pub batch_id: String,
    /// The check whose Fix all made it.
    pub check_id: String,
    /// When the run started, ms since the epoch.
    pub started_at: i64,
    /// When the run reached its end, ms; `None` for a run stopped part-way
    /// or still running.
    pub finished_at: Option<i64>,
    /// `finished_at` is `None` and no run in progress holds the batch.
    pub unfinished: bool,
    pub size: i64,
    pub counts: BatchCounts,
}

/// A batch's items by state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BatchCounts {
    pub queued: i64,
    pub claimed: i64,
    pub running: i64,
    pub awaiting_review: i64,
    pub applied: i64,
    pub discarded: i64,
    pub failed: i64,
    pub cancelled: i64,
}

/// One batch with its items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchDetail {
    pub summary: BatchSummary,
    pub items: Vec<DelegatedItem>,
}

/// A batch after its cancel, with how many items the cancel stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchCancelled {
    pub summary: BatchSummary,
    pub cancelled: usize,
}

/// A job card's research after its cancel: the id as asked, and how many
/// requests were cancelled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupCancelled {
    pub id: String,
    pub cancelled: usize,
}

/// The `jobs` operations.
pub struct Jobs<'a> {
    pub(super) app: &'a App,
}

fn queue_state(conn: &Connection) -> Result<QueueState, OpError> {
    let (running, queued) = musaic_db::research_jobs::queue_counts(conn)?;
    Ok(QueueState {
        paused: false,
        max_parallel: 1,
        running,
        queued,
        clearable: musaic_db::research_jobs::clearable(conn, &["acted_on"])?,
        clearable_failed: musaic_db::research_jobs::clearable(conn, &["failed"])?,
    })
}

fn job_not_found(job_id: &str) -> OpError {
    OpError::NotFound {
        details: None,
        what: super::error::Subject::Job,
        id: job_id.to_string(),
        message: "Job not found".to_string(),
    }
}

fn nothing_to_cancel() -> OpError {
    OpError::Conflict {
        message: "Nothing here is waiting or in progress to cancel.".into(),
        details: None,
    }
}

fn batch_not_found(batch_id: &str) -> OpError {
    OpError::NotFound {
        details: None,
        what: super::error::Subject::Job,
        id: batch_id.to_string(),
        message: "Batch not found".to_string(),
    }
}

fn batch_summary(app: &App, b: &musaic_db::delegated_work::BatchRow) -> BatchSummary {
    let held = app
        .0
        .running_batches
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(&b.id);
    BatchSummary {
        batch_id: b.id.clone(),
        check_id: b.check_id.clone(),
        started_at: b.started_at,
        finished_at: b.finished_at,
        unfinished: b.finished_at.is_none() && !held,
        size: b.size(),
        counts: BatchCounts {
            queued: b.count("queued"),
            claimed: b.count("claimed"),
            running: b.count("running"),
            awaiting_review: b.count("awaiting_review"),
            applied: b.count("applied"),
            discarded: b.count("discarded"),
            failed: b.count("failed"),
            cancelled: b.count("cancelled"),
        },
    }
}

fn jobs_list(conn: &Connection) -> Result<JobsList, OpError> {
    let jobs = crate::desktop_research::jobs(conn, None)?;
    Ok(JobsList {
        jobs,
        queue_state: queue_state(conn)?,
    })
}

impl Jobs<'_> {
    fn conn(&self) -> Result<Connection, OpError> {
        Ok(musaic_db::open_connection(self.app.db_path())?)
    }

    /// Jobs, newest first, at most 200. `statuses` limits them (`None`, or an
    /// empty slice, lists every status; the values are matched as given). A
    /// card whose requests all belong to a bulk run's batch is not listed: the
    /// batch is ([`Jobs::research_batches`]).
    pub fn list(&self, statuses: Option<&[String]>) -> Result<Vec<Job>, OpError> {
        let statuses = statuses.filter(|s| !s.is_empty());
        Ok(crate::desktop_research::jobs(&self.conn()?, statuses)?)
    }

    /// One job. `NotFound` for an unknown id.
    pub fn get(&self, job_id: &str) -> Result<Job, OpError> {
        let conn = self.conn()?;
        if let Some(group) = crate::desktop_research::group_job(&conn, job_id)? {
            return Ok(group);
        }
        musaic_db::research_history::get(&conn, job_id)?.ok_or_else(|| job_not_found(job_id))
    }

    /// The album's newest unresolved research request, if any (an old job's
    /// history record is never active). A request in a bulk run's batch
    /// counts like any other, so the album panel never offers a duplicate.
    pub fn active_for_album(&self, album_id: &str) -> Result<Option<Job>, OpError> {
        let conn = self.conn()?;
        if let Some(group) = musaic_db::research_jobs::active_group(&conn, album_id)? {
            return Ok(crate::desktop_research::group_job(&conn, &group)?);
        }
        // An old job no request names is history only: never active.
        Ok(None)
    }

    pub fn research_items(&self, group: &str) -> Result<Vec<ResearchItem>, OpError> {
        crate::desktop_research::children(self.app, &self.conn()?, group)
    }
    /// The delegated work in its six listed states, with holder, heartbeat,
    /// lease and whether anyone is listening for queued work. Reads only.
    pub fn delegated_work(&self) -> Result<Vec<DelegatedItem>, OpError> {
        crate::desktop_research::delegated(self.app)
    }

    pub fn research_preview(&self, id: &str, digest: &str) -> Result<ResearchPreview, OpError> {
        crate::desktop_research::preview(self.app, id, digest)
    }
    pub fn research_decide(
        &self,
        id: &str,
        revision: i64,
        digest: Option<&str>,
        action: &str,
        preview: Option<&str>,
    ) -> Result<(), OpError> {
        crate::desktop_research::decide(self.app, id, revision, digest, action, preview)
    }

    /// The queue's state now.
    pub fn queue_state(&self) -> Result<QueueState, OpError> {
        queue_state(&self.conn()?)
    }

    /// Queue a job of `job_type` (`metadata`, `cover` or `metadata+cover`) for an
    /// album, with the user's `hint` ("" for none).
    ///
    /// Validate the work guard, job type and album before resolving locally.
    /// Group unresolved research by matching input and retain verified local results.
    pub fn enqueue(
        &self,
        album_id: &str,
        job_type: &str,
        hint: &str,
        attach_cover: bool,
    ) -> Result<Job, OpError> {
        let _guard = self.app.gate().begin_work()?;
        queue::check_job_type(job_type)?;
        let mut conn = self.conn()?;
        let lib = self.app.library_dir();
        let row = queue::album_for_job(&conn, lib.as_deref(), album_id)?;
        let album = musaic_ai::Py::from(&row.json);
        let text = |k: &str| match album.get(k) {
            Some(v) if v.truthy() => v.py_str(),
            _ => String::new(),
        };
        let title = {
            let t = text("title");
            if t.is_empty() { text("dir_name") } else { t }
        };
        let path = album
            .get("path")
            .map(musaic_ai::Py::py_str)
            .unwrap_or_default();
        let job = NewJob {
            album_id: &row.album.id,
            album_path: &path,
            album_title: &title,
            job_type,
            hint,
            attach_cover,
        };
        let made = crate::desktop_research::enqueue(
            self.app,
            &mut conn,
            &job,
            None,
            crate::desktop_research::Publish::Now,
        )?;
        crate::desktop_research::group_job(&conn, &made.group_id)?
            .ok_or_else(|| OpError::internal("the new research group is missing"))
    }

    /// Every bulk run's batch that still has a request, of any library epoch,
    /// newest first, with its origin, size and counts by state. Reads only.
    pub fn research_batches(&self) -> Result<Vec<BatchSummary>, OpError> {
        let conn = self.conn()?;
        let rows = musaic_db::delegated_work::batches(&conn)?;
        Ok(rows.iter().map(|b| batch_summary(self.app, b)).collect())
    }

    /// One batch with every item (any epoch and state; Missing albums
    /// listed). `NotFound` when unknown or when no request is left in it.
    /// Reads only.
    pub fn research_batch(&self, batch_id: &str) -> Result<BatchDetail, OpError> {
        let conn = self.conn()?;
        let row = musaic_db::delegated_work::batch(&conn, batch_id)?
            .ok_or_else(|| batch_not_found(batch_id))?;
        let items = crate::desktop_research::delegated_items(
            self.app,
            &conn,
            musaic_db::delegated_work::batch_items(&conn, batch_id)?,
        )?;
        Ok(BatchDetail {
            summary: batch_summary(self.app, &row),
            items,
        })
    }

    /// Cancel every waiting or held item (queued, claimed, running) of a
    /// batch in one transaction, as "Cancel research" does for one item (an
    /// audit, outbox and event row each); items awaiting review or finished
    /// are untouched. The work guard (`Busy`), then `NotFound`. Publishes
    /// `AgentStateChanged(work)` after the commit.
    pub fn cancel_research_batch(&self, batch_id: &str) -> Result<BatchCancelled, OpError> {
        let _guard = self.app.gate().begin_work()?;
        let conn = self.conn()?;
        let cancelled = musaic_db::delegated_work::cancel_batch(
            &conn,
            batch_id,
            crate::agent::delegation::now_ms(self.app),
        )?
        .ok_or_else(|| batch_not_found(batch_id))?;
        crate::agent::delegation::publish_pending(self.app, &conn);
        let row = musaic_db::delegated_work::batch(&conn, batch_id)?
            .ok_or_else(|| batch_not_found(batch_id))?;
        Ok(BatchCancelled {
            summary: batch_summary(self.app, &row),
            cancelled,
        })
    }

    /// Cancel the research behind one job card (FR-036): every waiting or
    /// held item of the group `job_id` names (a group id, an old group id, or
    /// the old job id its requests name), in one transaction, as "Cancel
    /// research" does for one item. Items awaiting review or finished are
    /// untouched. The work guard (`Busy`); `NotFound` for an unknown id;
    /// `Conflict` when nothing is waiting or held (an old job's history record
    /// included). Publishes `AgentStateChanged(work)` after the commit.
    pub fn cancel_research_group(&self, job_id: &str) -> Result<GroupCancelled, OpError> {
        use musaic_db::research_jobs::GroupCancel;
        let _guard = self.app.gate().begin_work()?;
        let conn = self.conn()?;
        match musaic_db::research_jobs::cancel_group(
            &conn,
            job_id,
            crate::agent::delegation::now_ms(self.app),
        )? {
            GroupCancel::Cancelled(cancelled) => {
                crate::agent::delegation::publish_pending(self.app, &conn);
                Ok(GroupCancelled {
                    id: job_id.to_string(),
                    cancelled,
                })
            }
            GroupCancel::NothingToCancel => Err(nothing_to_cancel()),
            GroupCancel::NotFound if musaic_db::research_history::get(&conn, job_id)?.is_some() => {
                Err(nothing_to_cancel())
            }
            GroupCancel::NotFound => Err(job_not_found(job_id)),
        }
    }

    /// Delete one card: a research group whose requests are all finished
    /// (with their attempts, results and replays, and the old job's history
    /// record they named), or an old job's history record no request names.
    /// `Busy`; `Conflict` while a request of the group is unresolved (it is
    /// cancelled, never deleted); `NotFound`. Albums, files and apply backups
    /// are untouched.
    pub fn delete(&self, job_id: &str) -> Result<DeletedJob, OpError> {
        use musaic_db::research_jobs::GroupDelete;
        let _guard = self.app.gate().begin_work()?;
        let conn = self.conn()?;
        match musaic_db::research_jobs::delete_group(
            &conn,
            job_id,
            crate::agent::delegation::now_ms(self.app),
        )? {
            GroupDelete::Unresolved => {
                return Err(OpError::Conflict {
                    message: "This research is still in progress. Cancel it first.".into(),
                    details: None,
                });
            }
            GroupDelete::Removed => {
                crate::agent::delegation::publish_pending(self.app, &conn);
            }
            GroupDelete::NotFound => {
                if !musaic_db::research_history::delete(&conn, job_id)? {
                    return Err(job_not_found(job_id));
                }
            }
        }
        self.app.ai_jobs_changed();
        Ok(DeletedJob {
            id: job_id.to_string(),
        })
    }

    /// Clear by old status (each trimmed, blank ones dropped; research D13).
    /// `Busy`; then `Rejected` when none is left or one is not a status,
    /// `Conflict` when `running`, `queued` or `completed` is named (an
    /// unresolved request is cancelled, never deleted). `acted_on` removes
    /// every applied or discarded request of any epoch, `failed` every failed
    /// or cancelled one, each with its attempts, results and replays and the
    /// history record it alone named, plus the history-only records of that
    /// status. The count is requests plus history-only records removed; it
    /// equals `queue_state().clearable` for `acted_on` and
    /// `queue_state().clearable_failed` for `failed`. Wakes the feeds only
    /// if it deleted something.
    pub fn delete_many(&self, statuses: &[String]) -> Result<DeletedJobs, OpError> {
        let _guard = self.app.gate().begin_work()?;
        let statuses = queue::bulk_delete_statuses(statuses)?;
        if let Some(s) = statuses
            .iter()
            .find(|s| matches!(s.as_str(), "queued" | "completed"))
        {
            return Err(OpError::Conflict {
                message: format!(
                    "Bulk delete cannot include '{s}' jobs. Cancel unfinished research instead."
                ),
                details: None,
            });
        }
        let conn = self.conn()?;
        let count = musaic_db::research_jobs::clear(
            &conn,
            &statuses,
            crate::agent::delegation::now_ms(self.app),
        )?;
        if count > 0 {
            crate::agent::delegation::publish_pending(self.app, &conn);
            self.app.ai_jobs_changed();
        }
        Ok(DeletedJobs { count, statuses })
    }

    /// The jobs list now, then a value after every change to the research
    /// requests, several changes between two reads arriving as one. The
    /// subscription is taken before the first value is read, so a change in
    /// between is delivered as one more value, never lost. A value reads the
    /// database: one that could not be read is the error, which a face may show
    /// for the first value and skip for a later one.
    pub fn feed(&self) -> Feed<Result<JobsList, OpError>> {
        let changes = self.app.subscribe_jobs();
        let path = self.app.db_path().to_path_buf();
        Feed::counted(changes, self.app.shutdown_signal(), move || {
            jobs_list(&musaic_db::open_connection(&path)?)
        })
    }
}
