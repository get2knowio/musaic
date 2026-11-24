//! The `jobs` group as bodies and frames: each typed result in the keys and
//! order the Python app answered (`_job_to_dict`, the queue state), the jobs
//! stream's frame, and the 409 body that carries the album's
//! existing job.

use musaic_ai::{Py, job_to_py};
use musaic_app::ops::jobs::{
    BatchCancelled, BatchDetail, BatchSummary, DeletedJob, DeletedJobs, GroupCancelled, Job,
    JobsList, QueueState,
};
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

/// The stream's `jobs` frame: `{"jobs": [...200 newest], "queue_state": ...}` as
/// the Python app's `json.dumps` text. The frame's `queue_state` adds
/// `clearable` and `clearable_failed` after the Python keys: what Clear
/// applied or discarded, and Clear failed, would remove, counted in requests
/// (a card can hold two) plus history-only records.
pub fn jobs_frame(l: &JobsList) -> String {
    let mut state = queue_state_py(&l.queue_state);
    if let Py::Dict(entries) = &mut state {
        entries.push(("clearable".into(), Py::Int(l.queue_state.clearable)));
        entries.push((
            "clearable_failed".into(),
            Py::Int(l.queue_state.clearable_failed),
        ));
    }
    let payload = Py::dict([("jobs", jobs_py(&l.jobs)), ("queue_state", state)]);
    sse::frame("jobs", &payload.dumps())
}

/// An error of an enqueue: the 409 that carries the album's existing
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

fn stamp(ms: i64) -> String {
    musaic_db::backup::iso_seconds(
        std::time::UNIX_EPOCH + std::time::Duration::from_millis(ms.max(0) as u64),
    )
}

fn batch_map(b: &BatchSummary) -> serde_json::Map<String, serde_json::Value> {
    use serde_json::json;
    let c = &b.counts;
    let mut m = serde_json::Map::new();
    m.insert("batchId".into(), json!(b.batch_id));
    m.insert("checkId".into(), json!(b.check_id));
    m.insert("startedAt".into(), json!(stamp(b.started_at)));
    m.insert("finishedAt".into(), json!(b.finished_at.map(stamp)));
    m.insert("unfinished".into(), json!(b.unfinished));
    m.insert("size".into(), json!(b.size));
    m.insert(
        "counts".into(),
        json!({
            "queued": c.queued, "claimed": c.claimed, "running": c.running,
            "awaiting_review": c.awaiting_review, "applied": c.applied,
            "discarded": c.discarded, "failed": c.failed, "cancelled": c.cancelled,
        }),
    );
    m
}

/// `GET /api/research/batches`: an array of `BatchSummary`.
pub fn batches(list: &[BatchSummary]) -> serde_json::Value {
    serde_json::Value::Array(
        list.iter()
            .map(|b| serde_json::Value::Object(batch_map(b)))
            .collect(),
    )
}

/// `GET /api/research/batches/{batch_id}`: `BatchSummary` plus `items`.
pub fn batch_detail(d: &BatchDetail) -> serde_json::Value {
    let mut m = batch_map(&d.summary);
    m.insert("items".into(), serde_json::json!(d.items));
    serde_json::Value::Object(m)
}

/// The batch after its cancel: `BatchSummary` plus `cancelled`.
pub fn batch_cancelled(d: &BatchCancelled) -> serde_json::Value {
    let mut m = batch_map(&d.summary);
    m.insert("cancelled".into(), serde_json::json!(d.cancelled));
    serde_json::Value::Object(m)
}

/// `POST /api/ai/jobs/{job_id}/cancel`: `{"id": ..., "cancelled": n}`.
pub fn group_cancelled(d: &GroupCancelled) -> serde_json::Value {
    serde_json::json!({"id": d.id, "cancelled": d.cancelled})
}
