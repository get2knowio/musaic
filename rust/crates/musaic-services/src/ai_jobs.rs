//! The AI job queue's rules over the database (`musaic/api/jobs.py`): what
//! enqueue, acted-on, retry, delete and bulk delete check and write, and the
//! provider selection's stored defaults. This lives here rather than in a
//! face so every face applies the same rules. Rendering jobs
//! (`musaic_ai::job_to_py`), the queue state and the provider's own
//! description stay with the host: musaic-ai depends on this crate.
//!
//! Every refusal is an [`OpError`] whose message is the Python app's detail
//! text; a face maps the variant to its own answer. An album that already has
//! an active job is a [`OpError::Conflict`] carrying the existing job
//! ([`Details::ExistingJob`]).

use musaic_db::ai_jobs::{self, EnqueueRefused, JOB_TYPES, JobRow, NewJob, STATUSES};
use rusqlite::Connection;

use crate::albums::AlbumRow;
use crate::error::{Details, OpError, Subject};
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

fn rejected(message: String) -> OpError {
    OpError::Rejected {
        message,
        details: None,
    }
}

fn conflict(message: String) -> OpError {
    OpError::Conflict {
        message,
        details: None,
    }
}

fn job_not_found(job_id: &str) -> OpError {
    OpError::NotFound {
        what: Subject::Job,
        id: job_id.to_string(),
        message: "Job not found".to_string(),
    }
}

/// `JobConflictError`: the album already has an active job (its id and
/// status), carried so a face can show it.
fn active_job(existing_job_id: String, status: String) -> OpError {
    OpError::Conflict {
        message: format!("Active job {existing_job_id} ({status}) already exists for this album"),
        details: Some(Details::ExistingJob {
            job_id: existing_job_id,
            status,
        }),
    }
}

// ------------------------------------------------------------- enqueue

/// `POST /api/ai/jobs`' first check, before any database access: the job
/// type must be one of [`JOB_TYPES`] (`Rejected`).
pub fn check_job_type(job_type: &str) -> Result<(), OpError> {
    if !JOB_TYPES.contains(&job_type) {
        return Err(rejected(format!("Unknown job_type {}", repr(job_type))));
    }
    Ok(())
}

/// The album a job is enqueued for: cached in the configured library, or
/// `NotFound`.
pub fn album_for_job(
    conn: &Connection,
    library_dir: Option<&str>,
    album_id: &str,
) -> Result<AlbumRow, OpError> {
    match crate::sources::cached_album(conn, library_dir, album_id)? {
        Some(row) => Ok(row),
        None => Err(OpError::NotFound {
            what: Subject::Album,
            id: album_id.to_string(),
            message: format!("Album {} not in cache; rescan first", repr(album_id)),
        }),
    }
}

/// `jobs.enqueue(...)` as `POST /api/ai/jobs` runs it: the new job (`notify`
/// called once its row is written); an unknown job type is `Rejected`, an
/// album with an active job `Conflict` with that job.
pub fn enqueue(
    conn: &mut Connection,
    job: &NewJob<'_>,
    ids: &dyn Fn() -> String,
    now_iso: &str,
    notify: &dyn Fn(),
) -> Result<JobRow, OpError> {
    match ai_jobs::enqueue(conn, job, ids, now_iso)? {
        Ok(job) => {
            notify();
            Ok(job)
        }
        Err(EnqueueRefused::Conflict {
            existing_job_id,
            status,
        }) => Err(active_job(existing_job_id, status)),
        Err(EnqueueRefused::UnknownJobType(t)) => {
            Err(rejected(format!("Unknown job_type {}", repr(&t))))
        }
    }
}

/// What a check fix does where the Python app called `jobs.enqueue(...)`
/// (checks/*.py): the new job's id, or the existing active job's (id,
/// status). The caller notifies once the fix has finished (a deliberate
/// change: the Python app did not notify at all). Differs from [`enqueue`]:
/// the album fields come from the fix, the cover is always attached, nothing
/// is notified here, an active job is an answer and not an error, and an
/// unknown job type is `Internal`.
pub fn enqueue_for_fix(
    conn: &mut Connection,
    req: &crate::check_fixes::AiJobRequest,
    ids: &dyn Fn() -> String,
    now_iso: &str,
) -> Result<Result<String, (String, String)>, OpError> {
    let job = NewJob {
        album_id: &req.album_id,
        album_path: &req.album_path,
        album_title: &req.album_title,
        job_type: req.job_type,
        hint: &req.hint,
        attach_cover: true,
    };
    match ai_jobs::enqueue(conn, &job, ids, now_iso)? {
        Ok(job) => Ok(Ok(job.id)),
        Err(EnqueueRefused::Conflict {
            existing_job_id,
            status,
        }) => Ok(Err((existing_job_id, status))),
        // The type comes from the fix, not from a client: a bad one is a bug.
        Err(EnqueueRefused::UnknownJobType(t)) => {
            Err(OpError::internal(format!("unknown job type {t}")))
        }
    }
}

// ------------------------------------------------------------- writes

/// `POST /api/ai/jobs/{job_id}/acted-on`: 404, 409 unless completed, 400
/// for an action other than applied / discarded; metadata jobs stamp the
/// album's `metadata_ai_acted_on_at` (a failure is logged, not answered).
/// Returns the job as it is afterwards. `now_iso` is asked once, so the job's
/// `acted_on_at` and the album's stamp are the same instant.
pub fn mark_acted_on(
    conn: &Connection,
    job_id: &str,
    action: &str,
    now_iso: &dyn Fn() -> String,
    log: &dyn HostLog,
    notify: &dyn Fn(),
) -> Result<JobRow, OpError> {
    let job = ai_jobs::get_job(conn, job_id)?.ok_or_else(|| job_not_found(job_id))?;
    if job.status != "completed" {
        return Err(conflict(format!(
            "Can only mark a completed job as acted-on (current status: {})",
            job.status
        )));
    }
    if action != "applied" && action != "discarded" {
        return Err(rejected(
            "action must be 'applied' or 'discarded'".to_string(),
        ));
    }
    let now = now_iso();
    ai_jobs::mark_acted_on(conn, job_id, action, &now)?;
    if (job.job_type == "metadata" || job.job_type == "metadata+cover")
        && let Err(e) = ai_jobs::mark_metadata_ai_acted_on(conn, &job.album_id, &now)
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
    let job = ai_jobs::get_job(conn, job_id)?.ok_or_else(|| job_not_found(job_id))?;
    Ok(job)
}

/// `POST /api/ai/jobs/{job_id}/retry`: a failed job back to queued; 404,
/// or 409 unless failed. The album's other active job is a `Conflict` with
/// that job, and the failure excerpt is cleared with the error (both
/// deliberate changes from the Python app).
pub fn retry(
    conn: &mut Connection,
    job_id: &str,
    now_iso: &dyn Fn() -> String,
    notify: &dyn Fn(),
) -> Result<JobRow, OpError> {
    let job = ai_jobs::get_job(conn, job_id)?.ok_or_else(|| job_not_found(job_id))?;
    if job.status != "failed" {
        return Err(conflict(format!(
            "Can only retry a failed job (current status: {})",
            job.status
        )));
    }
    match ai_jobs::retry_job(conn, job_id, &now_iso())? {
        ai_jobs::RetryOutcome::Retried(updated) => {
            notify();
            Ok(*updated)
        }
        ai_jobs::RetryOutcome::OtherActive {
            existing_job_id,
            status,
        } => Err(active_job(existing_job_id, status)),
        ai_jobs::RetryOutcome::NotFailed => Err(conflict(
            "Job state changed; refresh and try again".to_string(),
        )),
    }
}

/// `DELETE /api/ai/jobs/{job_id}`: 404, 409 while running.
pub fn delete(conn: &Connection, job_id: &str, notify: &dyn Fn()) -> Result<(), OpError> {
    let job = ai_jobs::get_job(conn, job_id)?.ok_or_else(|| job_not_found(job_id))?;
    if job.status == "running" {
        return Err(conflict(
            "Can't delete a job while it's running. Wait for it to finish.".to_string(),
        ));
    }
    if !ai_jobs::delete_job(conn, job_id)? {
        return Err(job_not_found(job_id));
    }
    notify();
    Ok(())
}

/// `DELETE /api/ai/jobs?status=a,b`'s statuses: each trimmed, blank entries
/// dropped; `Rejected` when none is left or one is unknown, `Conflict` when
/// `running` is named. No database access.
pub fn bulk_delete_statuses(status: &[String]) -> Result<Vec<String>, OpError> {
    let statuses: Vec<String> = status
        .iter()
        .map(|s| musaic_core::pycompat::strip(s).to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if statuses.is_empty() {
        return Err(rejected("status query param is required".to_string()));
    }
    let invalid: Vec<String> = statuses
        .iter()
        .filter(|s| !STATUSES.contains(&s.as_str()))
        .cloned()
        .collect();
    if !invalid.is_empty() {
        return Err(rejected(format!("Unknown status: {}", list_repr(&invalid))));
    }
    if statuses.iter().any(|s| s == "running") {
        return Err(conflict(
            "Bulk delete cannot include 'running' jobs. Wait for them to finish.".to_string(),
        ));
    }
    Ok(statuses)
}

/// Delete every job in `statuses` (from [`bulk_delete_statuses`]);
/// `notify` only when something was deleted. Returns the count.
pub fn bulk_delete(
    conn: &Connection,
    statuses: &[String],
    notify: &dyn Fn(),
) -> Result<usize, OpError> {
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
    secrets.is_set("ai_api_key").unwrap_or(false)
}
