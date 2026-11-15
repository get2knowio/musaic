//! The AI job queue's rules over the database (`musaic/api/jobs.py`): what
//! enqueue,
//! acted-on, retry, delete and bulk delete check and write, and the
//! provider selection's stored defaults. This lives here rather than in
//! musaic-api's `routes/jobs.rs` so a non-HTTP face applies the same
//! rules. Rendering jobs (`musaic_ai::job_to_py`), the queue state and the
//! provider's own description stay with the host: musaic-ai depends on this
//! crate.
//!
//! Each refusal carries the Python app's detail text ([`JobRefusal::detail`]);
//! the HTTP host maps it to its status code. An active-job conflict is not
//! a refusal: it is answered with the existing job ([`Conflict`]).

use musaic_db::ai_jobs::{self, EnqueueRefused, JOB_TYPES, JobRow, NewJob, STATUSES};
use rusqlite::Connection;

use crate::albums::AlbumRow;
use crate::host_log::HostLog;

fn repr(s: &str) -> String {
    musaic_fsops::py_repr(s)
}

/// `str(list_of_str)`.
fn list_repr(items: &[String]) -> String {
    format!(
        "[{}]",
        items.iter().map(|s| repr(s)).collect::<Vec<_>>().join(", ")
    )
}

/// Why a job operation was refused (the Python app's `HTTPException`s).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobRefusal {
    /// 400 `Unknown job_type '...'`.
    UnknownJobType(String),
    /// 404 `Album '...' not in cache; rescan first`.
    AlbumNotInCache(String),
    /// 404 `Job not found`.
    NotFound,
    /// 409: acted-on for a job that is not completed (its status).
    ActedOnNotCompleted(String),
    /// 400: an action other than applied / discarded.
    BadAction,
    /// 409: retry of a job that is not failed (its status).
    RetryNotFailed(String),
    /// 409: the job changed between the check and the retry.
    RetryStateChanged,
    /// 409: delete of a running job.
    DeleteRunning,
    /// 400: bulk delete with no status left after trimming.
    BulkNoStatus,
    /// 400: bulk delete naming unknown statuses.
    BulkUnknownStatus(Vec<String>),
    /// 409: bulk delete naming `running`.
    BulkRunning,
}

impl JobRefusal {
    /// The Python app's status code.
    pub fn status(&self) -> u16 {
        match self {
            JobRefusal::UnknownJobType(_)
            | JobRefusal::BadAction
            | JobRefusal::BulkNoStatus
            | JobRefusal::BulkUnknownStatus(_) => 400,
            JobRefusal::AlbumNotInCache(_) | JobRefusal::NotFound => 404,
            JobRefusal::ActedOnNotCompleted(_)
            | JobRefusal::RetryNotFailed(_)
            | JobRefusal::RetryStateChanged
            | JobRefusal::DeleteRunning
            | JobRefusal::BulkRunning => 409,
        }
    }

    /// The Python app's `detail` text.
    pub fn detail(&self) -> String {
        match self {
            JobRefusal::UnknownJobType(t) => format!("Unknown job_type {}", repr(t)),
            JobRefusal::AlbumNotInCache(id) => {
                format!("Album {} not in cache; rescan first", repr(id))
            }
            JobRefusal::NotFound => "Job not found".to_string(),
            JobRefusal::ActedOnNotCompleted(status) => format!(
                "Can only mark a completed job as acted-on (current status: {})",
                status
            ),
            JobRefusal::BadAction => "action must be 'applied' or 'discarded'".to_string(),
            JobRefusal::RetryNotFailed(status) => {
                format!("Can only retry a failed job (current status: {})", status)
            }
            JobRefusal::RetryStateChanged => "Job state changed; refresh and try again".to_string(),
            JobRefusal::DeleteRunning => {
                "Can't delete a job while it's running. Wait for it to finish.".to_string()
            }
            JobRefusal::BulkNoStatus => "status query param is required".to_string(),
            JobRefusal::BulkUnknownStatus(invalid) => {
                format!("Unknown status: {}", list_repr(invalid))
            }
            JobRefusal::BulkRunning => {
                "Bulk delete cannot include 'running' jobs. Wait for them to finish.".to_string()
            }
        }
    }
}

/// A job operation's failure: a database error, a refusal, or (check-fix
/// enqueue only) an unexpected job type, which the host treats as an
/// internal error.
#[derive(Debug)]
pub enum JobError {
    Db(musaic_db::Error),
    Refused(JobRefusal),
    Internal(String),
}

impl From<musaic_db::Error> for JobError {
    fn from(e: musaic_db::Error) -> Self {
        JobError::Db(e)
    }
}

impl From<JobRefusal> for JobError {
    fn from(r: JobRefusal) -> Self {
        JobError::Refused(r)
    }
}

/// `JobConflictError`: the album already has an active job (its id and
/// status).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub existing_job_id: String,
    pub status: String,
}

// ------------------------------------------------------------- enqueue

/// `POST /api/ai/jobs`' first check, before any database access: the job
/// type must be one of [`JOB_TYPES`].
pub fn check_job_type(job_type: &str) -> Result<(), JobRefusal> {
    if !JOB_TYPES.contains(&job_type) {
        return Err(JobRefusal::UnknownJobType(job_type.to_string()));
    }
    Ok(())
}

/// The album a job is enqueued for: cached in the configured library, or
/// [`JobRefusal::AlbumNotInCache`].
pub fn album_for_job(
    conn: &Connection,
    library_dir: Option<&str>,
    album_id: &str,
) -> Result<AlbumRow, JobRefusal> {
    match crate::sources::cached_album(conn, library_dir, album_id) {
        Some(row) => Ok(row),
        None => Err(JobRefusal::AlbumNotInCache(album_id.to_string())),
    }
}

/// `jobs.enqueue(...)` as `POST /api/ai/jobs` runs it: the new job
/// (`notify` called once its row is written), or the active job's
/// [`Conflict`]; an unknown job type is [`JobRefusal::UnknownJobType`].
pub fn enqueue(
    conn: &mut Connection,
    job: &NewJob<'_>,
    ids: &dyn Fn() -> String,
    now_iso: &str,
    notify: &dyn Fn(),
) -> Result<Result<JobRow, Conflict>, JobError> {
    let out = ai_jobs::enqueue(conn, job, ids, now_iso)?;
    match out {
        Ok(job) => {
            notify();
            Ok(Ok(job))
        }
        Err(EnqueueRefused::Conflict {
            existing_job_id,
            status,
        }) => Ok(Err(Conflict {
            existing_job_id,
            status,
        })),
        Err(EnqueueRefused::UnknownJobType(t)) => Err(JobRefusal::UnknownJobType(t).into()),
    }
}

/// What a check fix does where the Python app called `jobs.enqueue(...)`
/// (checks/*.py): the new job, or the existing active job's (id, status).
/// The caller notifies once the fix has finished (a deliberate change: the
/// Python app did not notify at all). Differs from [`enqueue`]: the album
/// fields come from the fix, the cover is always attached, nothing is
/// notified here, and an unknown job type is [`JobError::Internal`].
pub fn enqueue_for_fix(
    conn: &mut Connection,
    req: &crate::check_fixes::AiJobRequest,
    ids: &dyn Fn() -> String,
    now_iso: &str,
) -> Result<Result<String, (String, String)>, JobError> {
    let out = ai_jobs::enqueue(
        conn,
        &NewJob {
            album_id: &req.album_id,
            album_path: &req.album_path,
            album_title: &req.album_title,
            job_type: req.job_type,
            hint: &req.hint,
            attach_cover: true,
        },
        ids,
        now_iso,
    )?;
    Ok(match out {
        Ok(job) => Ok(job.id),
        Err(EnqueueRefused::Conflict {
            existing_job_id,
            status,
        }) => Err((existing_job_id, status)),
        Err(EnqueueRefused::UnknownJobType(t)) => {
            return Err(JobError::Internal(format!("unknown job type {t}")));
        }
    })
}

// ------------------------------------------------------------- writes

/// `POST /api/ai/jobs/{job_id}/acted-on`: 404, 409 unless completed, 400
/// for an action other than applied / discarded; metadata jobs stamp the
/// album's `metadata_ai_acted_on_at` (a failure is logged, not answered).
/// Returns the job as it is afterwards. `now_iso` is asked once per write.
pub fn mark_acted_on(
    conn: &Connection,
    job_id: &str,
    action: &str,
    now_iso: &dyn Fn() -> String,
    log: &dyn HostLog,
    notify: &dyn Fn(),
) -> Result<JobRow, JobError> {
    let job = ai_jobs::get_job(conn, job_id)?.ok_or(JobRefusal::NotFound)?;
    if job.status != "completed" {
        return Err(JobRefusal::ActedOnNotCompleted(job.status).into());
    }
    if action != "applied" && action != "discarded" {
        return Err(JobRefusal::BadAction.into());
    }
    ai_jobs::mark_acted_on(conn, job_id, action, &now_iso())?;
    if (job.job_type == "metadata" || job.job_type == "metadata+cover")
        && let Err(e) = ai_jobs::mark_metadata_ai_acted_on(conn, &job.album_id, &now_iso())
    {
        log.error(
            "musaic.api.jobs",
            &format!(
                "Couldn't stamp metadata_ai_acted_on_at for album {}: {e}",
                job.album_id
            ),
        );
    }
    notify();
    let job = ai_jobs::get_job(conn, job_id)?.ok_or(JobRefusal::NotFound)?;
    Ok(job)
}

/// `POST /api/ai/jobs/{job_id}/retry`: a failed job back to queued; 404,
/// or 409 unless failed. The album's other active job is answered as a
/// [`Conflict`], and the failure excerpt is cleared with the error (both
/// deliberate changes from the Python app).
pub fn retry(
    conn: &mut Connection,
    job_id: &str,
    now_iso: &dyn Fn() -> String,
    notify: &dyn Fn(),
) -> Result<Result<JobRow, Conflict>, JobError> {
    let job = ai_jobs::get_job(conn, job_id)?.ok_or(JobRefusal::NotFound)?;
    if job.status != "failed" {
        return Err(JobRefusal::RetryNotFailed(job.status).into());
    }
    match ai_jobs::retry_job(conn, job_id, &now_iso())? {
        ai_jobs::RetryOutcome::Retried(updated) => {
            notify();
            Ok(Ok(*updated))
        }
        ai_jobs::RetryOutcome::OtherActive {
            existing_job_id,
            status,
        } => Ok(Err(Conflict {
            existing_job_id,
            status,
        })),
        ai_jobs::RetryOutcome::NotFailed => Err(JobRefusal::RetryStateChanged.into()),
    }
}

/// `DELETE /api/ai/jobs/{job_id}`: 404, 409 while running.
pub fn delete(conn: &Connection, job_id: &str, notify: &dyn Fn()) -> Result<(), JobError> {
    let job = ai_jobs::get_job(conn, job_id)?.ok_or(JobRefusal::NotFound)?;
    if job.status == "running" {
        return Err(JobRefusal::DeleteRunning.into());
    }
    if !ai_jobs::delete_job(conn, job_id)? {
        return Err(JobRefusal::NotFound.into());
    }
    notify();
    Ok(())
}

/// `DELETE /api/ai/jobs?status=a,b`'s statuses: trimmed, blank entries
/// dropped; refused when none is left or one is unknown, and when `running`
/// is named. No database access.
pub fn bulk_delete_statuses(status: &str) -> Result<Vec<String>, JobRefusal> {
    let statuses: Vec<String> = status
        .split(',')
        .map(|s| musaic_core::pycompat::strip(s).to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if statuses.is_empty() {
        return Err(JobRefusal::BulkNoStatus);
    }
    let invalid: Vec<String> = statuses
        .iter()
        .filter(|s| !STATUSES.contains(&s.as_str()))
        .cloned()
        .collect();
    if !invalid.is_empty() {
        return Err(JobRefusal::BulkUnknownStatus(invalid));
    }
    if statuses.iter().any(|s| s == "running") {
        return Err(JobRefusal::BulkRunning);
    }
    Ok(statuses)
}

/// Delete every job in `statuses` (from [`bulk_delete_statuses`]);
/// `notify` only when something was deleted. Returns the count.
pub fn bulk_delete(
    conn: &Connection,
    statuses: &[String],
    notify: &dyn Fn(),
) -> Result<usize, JobError> {
    let n = ai_jobs::delete_jobs_by_status(conn, statuses)?;
    if n > 0 {
        notify();
    }
    Ok(n)
}

// ------------------------------------------------------------- providers

/// The stored provider selection with the Python app's defaults
/// (`ai_provider_id` / `ai_model_id`, stripped; `claude` /
/// `claude-sonnet-4-6` when blank), read in that order.
pub fn selected_provider(conn: &Connection) -> musaic_db::Result<(String, String)> {
    let stripped = |k: &str, default: &str| -> musaic_db::Result<String> {
        let v = musaic_db::settings::get_raw(conn, k)?.unwrap_or_default();
        let v = musaic_core::pycompat::strip(&v).to_string();
        Ok(if v.is_empty() { default.into() } else { v })
    };
    let provider_id = stripped("ai_provider_id", "claude")?;
    let model_id = stripped("ai_model_id", "claude-sonnet-4-6")?;
    Ok((provider_id, model_id))
}

/// Whether an `ai_api_key` is set (a store error counts as not set).
pub fn api_key_set(secrets: &dyn musaic_secrets::SecretStore) -> bool {
    musaic_secrets::secret(secrets, "ai_api_key")
        .map(|k| !k.is_empty())
        .unwrap_or(false)
}
