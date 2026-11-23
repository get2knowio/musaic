//! Desktop research and historical jobs. The jobs feed projects metadata and
//! cover requests into one card and wakes after committed changes. Each child
//! has its own claim, result, review decision and revision.
//!
//! Writers take the work guard before validating input. An unresolved request
//! for the same album, kind and input reuses its existing work. Historical
//! provider queues retain their original conflict and retry behavior.

use musaic_db::ai_jobs::{self, NewJob};
use musaic_services::ai_jobs as queue;
use rusqlite::Connection;

pub use crate::desktop_research::{Preview as ResearchPreview, ReviewChild as ResearchItem};
pub use musaic_ai::provider::ProviderInfo;
pub use musaic_db::ai_jobs::JobRow as Job;

use super::error::OpError;
use super::events::Feed;
use crate::App;

/// The queue's state: `paused`, the parallelism limit, and how many jobs run and
/// wait now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueState {
    pub paused: bool,
    pub max_parallel: i64,
    pub running: i64,
    pub queued: i64,
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

/// The provider and model the settings select, with whether an API key is set
/// (never its value).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedProvider {
    pub provider_id: String,
    pub model_id: String,
    pub api_key_set: bool,
}

/// The selection and the providers this build has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderList {
    pub selected: SelectedProvider,
    pub providers: Vec<ProviderInfo>,
}

/// The `jobs` operations.
pub struct Jobs<'a> {
    pub(super) app: &'a App,
}

fn queue_state(conn: &Connection) -> Result<QueueState, OpError> {
    let (running, queued) = musaic_db::research_jobs::queue_counts(conn)?;
    Ok(QueueState {
        paused: musaic_ai::jobs::queue_paused(conn)?,
        max_parallel: musaic_ai::jobs::max_parallel(conn)?,
        running,
        queued,
    })
}

fn require_historical_job(conn: &Connection, id: &str) -> Result<(), OpError> {
    if !musaic_db::research_jobs::group(conn, id)?.is_empty() {
        return Err(OpError::Conflict {
            message: "Use the research item's review actions.".into(),
            details: None,
        });
    }
    Ok(())
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
    /// empty slice, lists every status; the values are matched as given).
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
        ai_jobs::get_job(&conn, job_id)?.ok_or_else(|| OpError::NotFound {
            details: None,
            what: super::error::Subject::Job,
            id: job_id.to_string(),
            message: "Job not found".to_string(),
        })
    }

    /// The album's newest queued, running or completed job, if any.
    pub fn active_for_album(&self, album_id: &str) -> Result<Option<Job>, OpError> {
        let conn = self.conn()?;
        if let Some(group) = musaic_db::research_jobs::active_group(&conn, album_id)? {
            return Ok(crate::desktop_research::group_job(&conn, &group)?);
        }
        Ok(ai_jobs::get_active_job_for_album(&conn, album_id)?)
    }

    pub fn research_items(&self, group: &str) -> Result<Vec<ResearchItem>, OpError> {
        crate::desktop_research::children(self.app, &self.conn()?, group)
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
        let ids = self.app.0.ai.ids.clone();
        let job = NewJob {
            album_id: &row.album.id,
            album_path: &path,
            album_title: &title,
            job_type,
            hint,
            attach_cover,
        };
        if !self.app.0.legacy_jobs {
            return crate::desktop_research::enqueue(self.app, &mut conn, &job);
        }
        queue::enqueue(&mut conn, &job, &*ids, &self.app.now_iso(), &|| {
            self.app.ai_jobs_changed()
        })
    }

    /// Record what the user did with a completed job's answer (`applied` or
    /// `discarded`); a metadata job also stamps the album. The guard (`Busy`),
    /// the job (`NotFound`), its status (`Conflict` unless completed), then the
    /// action (`Rejected`). Returns the job as it is afterwards.
    pub fn mark_acted_on(&self, job_id: &str, action: &str) -> Result<Job, OpError> {
        let _guard = self.app.gate().begin_work()?;
        let conn = self.conn()?;
        require_historical_job(&conn, job_id)?;
        queue::mark_acted_on(
            &conn,
            job_id,
            action,
            &|| self.app.now_iso(),
            self.app.log(),
            &|| self.app.ai_jobs_changed(),
        )
    }

    /// Put a failed job back in the queue. `Busy`; `NotFound`; `Conflict` unless
    /// the job failed, or while its album has another active job (that job is the
    /// `Details::ExistingJob`). The failure excerpt is cleared with the error.
    pub fn retry(&self, job_id: &str) -> Result<Job, OpError> {
        let _guard = self.app.gate().begin_work()?;
        let mut conn = self.conn()?;
        require_historical_job(&conn, job_id)?;
        queue::retry(&mut conn, job_id, &|| self.app.now_iso(), &|| {
            self.app.ai_jobs_changed()
        })
    }

    /// Delete one job. `Busy`; `NotFound`; `Conflict` while it runs.
    pub fn delete(&self, job_id: &str) -> Result<DeletedJob, OpError> {
        let _guard = self.app.gate().begin_work()?;
        let conn = self.conn()?;
        require_historical_job(&conn, job_id)?;
        queue::delete(&conn, job_id, &|| self.app.ai_jobs_changed())?;
        Ok(DeletedJob {
            id: job_id.to_string(),
        })
    }

    /// Delete every job in `statuses` (each trimmed, blank ones dropped). `Busy`;
    /// then `Rejected` when none is left or one is not a status, `Conflict` when
    /// `running` is named. Wakes the feeds only if it deleted something.
    pub fn delete_many(&self, statuses: &[String]) -> Result<DeletedJobs, OpError> {
        let _guard = self.app.gate().begin_work()?;
        let statuses = queue::bulk_delete_statuses(statuses)?;
        let conn = self.conn()?;
        let count = queue::bulk_delete(&conn, &statuses, &|| self.app.ai_jobs_changed())?;
        Ok(DeletedJobs { count, statuses })
    }

    /// Clear the queue's pause. `Busy` under a restore. Wakes the dispatcher and
    /// every feed.
    pub fn resume_queue(&self) -> Result<QueueState, OpError> {
        let _guard = self.app.gate().begin_work()?;
        let conn = self.conn()?;
        musaic_ai::jobs::set_queue_paused(&conn, false)?;
        self.app.ai_jobs_changed();
        queue_state(&conn)
    }

    /// The selection as stored (with defaults), whether an API key is set, and
    /// the providers this build has. Nothing uninstalled is listed.
    pub fn providers(&self) -> Result<ProviderList, OpError> {
        let conn = self.conn()?;
        let api_key_set = queue::api_key_set(self.app.0.secrets.as_ref());
        let (provider_id, model_id) = queue::selected_provider(&conn)?;
        Ok(ProviderList {
            selected: SelectedProvider {
                provider_id,
                model_id,
                api_key_set,
            },
            providers: self.app.ai_provider_infos(),
        })
    }

    /// The jobs list now, then a value after every change to the queue (the
    /// worker's too), several changes between two reads arriving as one. The
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
