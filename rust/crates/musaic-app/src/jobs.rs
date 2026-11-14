//! The AI job queue's host operations over this app (`musaic/api/jobs.py`
//! without HTTP): enqueue with the album's fields, the check fixes'
//! enqueue, resuming the queue, the provider listing, the jobs stream's
//! subscription and payload, and the bodies the Python app answers with, as
//! data ([`Py`], key order kept). The queue's rules live in
//! [`musaic_services::ai_jobs`]; a host maps their refusals to its own
//! answers and frames these bodies.

use musaic_ai::{Py, job_to_py};
use musaic_db::ai_jobs::{JobRow, NewJob};
use musaic_services::ai_jobs::{Conflict, JobError};
use rusqlite::Connection;
use tokio::sync::watch;

use crate::App;

/// A job as the Python app renders it (`_job_to_dict`).
pub fn job_body(job: &JobRow) -> Py {
    job_to_py(job)
}

/// `GET /api/ai/jobs`: `{"jobs": [...]}`.
pub fn jobs_body(rows: &[JobRow]) -> Py {
    Py::dict([("jobs", Py::List(rows.iter().map(job_to_py).collect()))])
}

/// The detail of the 409 for an album with an active job
/// (`JobConflictError`), wrapped as the body `{"detail": {...}}`, in the
/// Python app's key order.
pub fn conflict_body(c: &Conflict) -> Py {
    let (existing_job_id, status) = (c.existing_job_id.as_str(), c.status.as_str());
    let detail = Py::dict([
        ("error", Py::str("active_job_exists")),
        (
            "message",
            Py::str(format!(
                "Active job {existing_job_id} ({status}) already exists for this album"
            )),
        ),
        ("existing_job_id", Py::str(existing_job_id)),
        ("existing_status", Py::str(status)),
    ]);
    Py::dict([("detail", detail)])
}

/// `DELETE /api/ai/jobs/{job_id}`: `{"deleted": true, "id": ...}`.
pub fn deleted_body(job_id: String) -> Py {
    Py::dict([("deleted", Py::Bool(true)), ("id", Py::str(job_id))])
}

/// `DELETE /api/ai/jobs?status=`: `{"deleted_count": n, "statuses": [...]}`.
pub fn bulk_deleted_body(n: usize, statuses: Vec<String>) -> Py {
    Py::dict([
        ("deleted_count", Py::Int(n as i64)),
        (
            "statuses",
            Py::List(statuses.into_iter().map(Py::Str).collect()),
        ),
    ])
}

impl App {
    /// `jobs.enqueue(...)` as `POST /api/ai/jobs` runs it, after the job
    /// type check: the album must be cached in the configured library
    /// (else [`musaic_services::ai_jobs::JobRefusal::AlbumNotInCache`]);
    /// its title (else its directory name) and path go on the job. The new
    /// job (every jobs stream and the dispatcher woken), or the active
    /// job's [`Conflict`].
    pub fn enqueue_job(
        &self,
        conn: &mut Connection,
        album_id: &str,
        job_type: &str,
        hint: &str,
        attach_cover: bool,
    ) -> Result<Result<JobRow, Conflict>, JobError> {
        let lib = self.library_dir();
        let row = musaic_services::ai_jobs::album_for_job(conn, lib.as_deref(), album_id)?;
        let album = Py::from(&row.json);
        let text = |k: &str| match album.get(k) {
            Some(v) if v.truthy() => v.py_str(),
            _ => String::new(),
        };
        let title = {
            let t = text("title");
            if t.is_empty() { text("dir_name") } else { t }
        };
        let path = album.get("path").map(Py::py_str).unwrap_or_default();
        let ids = self.0.ai.ids.clone();
        musaic_services::ai_jobs::enqueue(
            conn,
            &NewJob {
                album_id: &row.album.id,
                album_path: &path,
                album_title: &title,
                job_type,
                hint,
                attach_cover,
            },
            &*ids,
            &self.now_iso(),
            &|| self.ai_jobs_changed(),
        )
    }

    /// What a check fix does where the Python app calls `jobs.enqueue(...)`
    /// (checks/*.py): the new job, or the existing active job's (id, status).
    /// The caller notifies once the fix has finished (a deliberate change: the
    /// Python app does not notify at all).
    pub fn enqueue_job_for_fix(
        &self,
        conn: &mut Connection,
        req: &musaic_services::check_fixes::AiJobRequest,
    ) -> Result<Result<String, (String, String)>, JobError> {
        let ids = self.0.ai.ids.clone();
        musaic_services::ai_jobs::enqueue_for_fix(conn, req, &*ids, &self.now_iso())
    }

    /// `POST /api/ai/queue/resume`: clears `ai_queue_paused`, wakes the
    /// dispatcher and every jobs stream, and returns `{"queue_state": ...}`.
    pub fn resume_queue(&self, conn: &Connection) -> musaic_db::Result<Py> {
        musaic_ai::jobs::set_queue_paused(conn, false)?;
        self.ai_jobs_changed();
        Ok(Py::dict([("queue_state", musaic_ai::queue_state(conn)?)]))
    }

    /// `GET /api/ai/providers` in this build: the selection as stored
    /// (`ai_provider_id` / `ai_model_id` with the Python app's defaults, whether
    /// an `ai_api_key` is set), the one provider this build has, nothing
    /// uninstalled.
    pub fn ai_providers(&self, conn: &Connection) -> musaic_db::Result<Py> {
        let key_set = musaic_services::ai_jobs::api_key_set(self.0.secrets.as_ref());
        let info = self.0.ai.provider.info();
        let (provider_id, model_id) = musaic_services::ai_jobs::selected_provider(conn)?;
        Ok(Py::dict([
            (
                "selected",
                Py::dict([
                    ("provider_id", Py::str(provider_id)),
                    ("model_id", Py::str(model_id)),
                    ("api_key_set", Py::Bool(key_set)),
                ]),
            ),
            (
                "providers",
                Py::List(vec![Py::dict([
                    ("id", Py::str(info.id)),
                    ("accepts_api_key", Py::Bool(info.accepts_api_key)),
                    ("auth_hint", Py::opt_str(info.auth_hint)),
                    ("ready", Py::Bool(info.ready)),
                    ("requirement", Py::opt_str(info.requirement)),
                    ("remedy", Py::opt_str(info.remedy)),
                ])]),
            ),
            ("uninstalled", Py::List(Vec::new())),
        ]))
    }

    /// The jobs stream's subscription: a receiver of its own (no other
    /// connection or the dispatcher can consume its wake-ups) with every
    /// change so far already seen. Subscribe before reading the first
    /// payload, so a change in between is sent (as an extra frame), never
    /// lost.
    pub fn subscribe_jobs(&self) -> watch::Receiver<u64> {
        let mut changes = self.0.ai.changes.subscribe();
        changes.borrow_and_update();
        changes
    }
}

/// One jobs-stream payload (the 200 newest jobs and the queue state), as
/// the Python app's `json.dumps` text.
pub fn jobs_stream_payload(conn: &Connection) -> musaic_db::Result<String> {
    Ok(musaic_ai::stream_payload(conn)?.dumps())
}
