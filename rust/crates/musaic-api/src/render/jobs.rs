//! The `jobs` group as bodies and frames: each typed result in the keys and
//! order the Python app answered (`_job_to_dict`, the queue state, the provider
//! listing), the jobs stream's frame, and the 409 body that carries the album's
//! existing job.

use musaic_ai::{Py, job_to_py};
use musaic_app::ops::jobs::{DeletedJob, DeletedJobs, Job, JobsList, ProviderList, QueueState};
use musaic_app::ops::{OpError, error::Details};
use musaic_services::Json;

use crate::http::{ApiResult, json_ordered};
use crate::sse;

/// One job.
pub fn job(j: &Job) -> Json {
    job_to_py(j).to_json()
}

/// The newest active job of an album, or `null`.
pub fn job_or_null(j: Option<&Job>) -> Json {
    j.map_or(Json::Null, job)
}

fn jobs_py(jobs: &[Job]) -> Py {
    Py::List(jobs.iter().map(job_to_py).collect())
}

/// `GET /api/ai/jobs`: `{"jobs": [...]}`.
pub fn job_list(jobs: &[Job]) -> Json {
    Py::dict([("jobs", jobs_py(jobs))]).to_json()
}

fn queue_state_py(q: &QueueState) -> Py {
    Py::dict([
        ("paused", Py::Bool(q.paused)),
        ("max_parallel", Py::Int(q.max_parallel)),
        ("running", Py::Int(q.running)),
        ("queued", Py::Int(q.queued)),
    ])
}

/// `POST /api/ai/queue/resume`: `{"queue_state": ...}`.
pub fn resumed(q: &QueueState) -> Json {
    Py::dict([("queue_state", queue_state_py(q))]).to_json()
}

/// `DELETE /api/ai/jobs/{job_id}`: `{"deleted": true, "id": ...}`.
pub fn deleted(d: &DeletedJob) -> Json {
    Py::dict([("deleted", Py::Bool(true)), ("id", Py::str(d.id.clone()))]).to_json()
}

/// `DELETE /api/ai/jobs?status=`: `{"deleted_count": n, "statuses": [...]}`.
pub fn deleted_many(d: &DeletedJobs) -> Json {
    Py::dict([
        ("deleted_count", Py::Int(d.count as i64)),
        (
            "statuses",
            Py::List(d.statuses.iter().cloned().map(Py::Str).collect()),
        ),
    ])
    .to_json()
}

/// `GET /api/ai/providers`: the selection, the providers this build has,
/// nothing uninstalled.
pub fn providers(p: &ProviderList) -> Json {
    Py::dict([
        (
            "selected",
            Py::dict([
                ("provider_id", Py::str(p.selected.provider_id.clone())),
                ("model_id", Py::str(p.selected.model_id.clone())),
                ("api_key_set", Py::Bool(p.selected.api_key_set)),
            ]),
        ),
        (
            "providers",
            Py::List(
                p.providers
                    .iter()
                    .map(|info| {
                        Py::dict([
                            ("id", Py::str(info.id.clone())),
                            ("accepts_api_key", Py::Bool(info.accepts_api_key)),
                            ("auth_hint", Py::opt_str(info.auth_hint.clone())),
                            ("ready", Py::Bool(info.ready)),
                            ("requirement", Py::opt_str(info.requirement.clone())),
                            ("remedy", Py::opt_str(info.remedy.clone())),
                        ])
                    })
                    .collect(),
            ),
        ),
        ("uninstalled", Py::List(Vec::new())),
    ])
    .to_json()
}

/// The stream's `jobs` frame: `{"jobs": [...200 newest], "queue_state": ...}` as
/// the Python app's `json.dumps` text.
pub fn jobs_frame(l: &JobsList) -> String {
    let payload = Py::dict([
        ("jobs", jobs_py(&l.jobs)),
        ("queue_state", queue_state_py(&l.queue_state)),
    ]);
    sse::frame("jobs", &payload.dumps())
}

/// An error of an enqueue or a retry: the 409 that carries the album's existing
/// job (`JobConflictError`'s detail, in the Python app's key order) answers with
/// that body, every other error as any other route's.
pub fn job_error(e: OpError) -> ApiResult {
    match e {
        OpError::Conflict {
            message,
            details: Some(Details::ExistingJob { job_id, status }),
        } => {
            let detail = Py::dict([
                ("error", Py::str("active_job_exists")),
                ("message", Py::str(message)),
                ("existing_job_id", Py::str(job_id)),
                ("existing_status", Py::str(status)),
            ]);
            Ok(json_ordered(409, &Py::dict([("detail", detail)]).to_json()))
        }
        e => Err(e.into()),
    }
}
