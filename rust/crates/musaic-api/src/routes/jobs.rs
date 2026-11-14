//! `musaic/api/jobs.py` (the AI job queue's REST + SSE surface) and
//! `GET /api/ai/providers` (musaic/api/settings.py).
//!
//! | route | |
//! |---|---|
//! | `POST /api/ai/jobs` | [`enqueue_job`] |
//! | `GET /api/ai/jobs` | [`list_jobs`] |
//! | `DELETE /api/ai/jobs?status=` | [`bulk_delete_jobs`] |
//! | `GET /api/ai/jobs/stream` (SSE) | [`jobs_stream`] |
//! | `GET /api/ai/jobs/active/{album_id}` | [`active_job_for_album`] |
//! | `GET /api/ai/jobs/{job_id}` | [`get_job`] |
//! | `DELETE /api/ai/jobs/{job_id}` | [`delete_job`] |
//! | `POST /api/ai/jobs/{job_id}/acted-on` | [`mark_job_acted_on`] |
//! | `POST /api/ai/jobs/{job_id}/retry` | [`retry_job`] |
//! | `POST /api/ai/queue/resume` | [`resume_queue`] |
//! | `GET /api/ai/providers` | [`ai_providers`] |
//!
//! Every route that writes takes the restore gate's work guard after
//! request validation (422) and before any check of its own. Bodies are the Python app's, key order included (Starlette's
//! compact rendering of the ordered dict); the stream's frames are
//! `sse_event("jobs", {"jobs": [...], "queue_state": {...}})` byte for byte.
//!
//! `GET /api/ai/providers` lists the provider this build has (by default
//! [`musaic_ai::Unconfigured`]) instead of the installed airframe adapters,
//! and `uninstalled` is empty (this build installs no other provider).

use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State};
use axum::response::Response;
use musaic_ai::Py;
use musaic_app::jobs::{
    bulk_deleted_body, conflict_body, deleted_body, job_body, jobs_body, jobs_stream_payload,
};
use musaic_db::ai_jobs::{self, LIST_LIMIT};
use musaic_services::ai_jobs::{Conflict, JobError, JobRefusal};
use serde_json::json;

use crate::http::{ApiError, ApiResult, blocking, json_ordered, ok_ordered};
use crate::lax::{Fields, err, parse_json_body, unprocessable};
use crate::routes::library::{Q, qp};
use crate::sse;
use crate::state::AppState;

fn ok_py(v: &Py) -> ApiResult {
    ok_ordered(&v.to_json())
}

fn not_found() -> ApiError {
    ApiError::new(404, "Job not found")
}

/// The queue's rules live in [`musaic_services::ai_jobs`]; a refusal is
/// the Python app's `HTTPException` with its status and detail.
impl From<JobRefusal> for ApiError {
    fn from(r: JobRefusal) -> Self {
        ApiError::new(r.status(), r.detail())
    }
}

impl From<JobError> for ApiError {
    fn from(e: JobError) -> Self {
        match e {
            JobError::Db(e) => ApiError::from(e),
            JobError::Refused(r) => ApiError::from(r),
            JobError::Internal(m) => ApiError::internal(m),
        }
    }
}

// ------------------------------------------------------------- enqueue

/// `POST /api/ai/jobs` with `JobEnqueuePayload {album_id: str, job_type:
/// str, hint: str = "", attach_cover: bool = True}`: 400 for an unknown
/// job type, 404 for an album not in the cache, 409 with the existing job
/// when the album has an active one.
pub async fn enqueue_job(State(st): State<AppState>, body: Bytes) -> ApiResult {
    let mut f = Fields::required(parse_json_body(&body)?)?;
    let album_id = f.req_str("album_id");
    let job_type = f.req_str("job_type");
    let hint = f.str_or("hint", "");
    let attach_cover = f.bool_or("attach_cover", true);
    f.finish()?;
    let (album_id, job_type) = (album_id.unwrap_or_default(), job_type.unwrap_or_default());
    let guard = st.begin_work()?;
    blocking(move || {
        let _guard = guard;
        musaic_services::ai_jobs::check_job_type(&job_type)?;
        let mut conn = st.conn()?;
        let out = st.enqueue_job(&mut conn, &album_id, &job_type, &hint, attach_cover)?;
        match out {
            Ok(job) => ok_py(&job_body(&job)),
            Err(c) => Ok(conflict_response(&c)),
        }
    })
    .await?
}

/// The 409 for an album with an active job (`JobConflictError`), its
/// detail in the Python app's key order ([`conflict_body`]).
fn conflict_response(c: &Conflict) -> Response {
    json_ordered(409, &conflict_body(c).to_json())
}

/// What a check fix does where the Python app calls `jobs.enqueue(...)`
/// (checks/*.py): the new job, or the existing active job's (id, status).
/// The caller notifies once the fix has finished (a deliberate change: the
/// Python app does not notify at all).
pub fn enqueue_for_fix(
    st: &AppState,
    conn: &mut rusqlite::Connection,
    req: &musaic_services::check_fixes::AiJobRequest,
) -> Result<Result<String, (String, String)>, ApiError> {
    Ok(st.enqueue_job_for_fix(conn, req)?)
}

// ------------------------------------------------------------- reads

/// `GET /api/ai/jobs?status=a,b`: newest first, at most 200; no `status`
/// (or an empty one) lists every status; values are not trimmed.
pub async fn list_jobs(State(st): State<AppState>, q: Q) -> ApiResult {
    let q = qp(q);
    let statuses: Option<Vec<String>> = q
        .get("status")
        .filter(|s| !s.is_empty())
        .map(|s| s.split(',').map(str::to_string).collect());
    blocking(move || {
        let conn = st.conn()?;
        let rows = ai_jobs::list_jobs(&conn, statuses.as_deref(), LIST_LIMIT)?;
        ok_py(&jobs_body(&rows))
    })
    .await?
}

/// `GET /api/ai/jobs/active/{album_id}`: the newest queued / running /
/// completed job, or `null`.
pub async fn active_job_for_album(
    State(st): State<AppState>,
    UrlPath(album_id): UrlPath<String>,
) -> ApiResult {
    blocking(move || {
        let conn = st.conn()?;
        match ai_jobs::get_active_job_for_album(&conn, &album_id)? {
            Some(j) => ok_py(&job_body(&j)),
            None => ok_py(&Py::Null),
        }
    })
    .await?
}

/// `GET /api/ai/jobs/{job_id}`.
pub async fn get_job(State(st): State<AppState>, UrlPath(job_id): UrlPath<String>) -> ApiResult {
    blocking(move || {
        let conn = st.conn()?;
        match ai_jobs::get_job(&conn, &job_id)? {
            Some(j) => ok_py(&job_body(&j)),
            None => Err(not_found()),
        }
    })
    .await?
}

// ------------------------------------------------------------- stream

fn stream_frame(st: &AppState) -> Result<String, ApiError> {
    let conn = st.conn()?;
    Ok(sse::frame("jobs", &jobs_stream_payload(&conn)?))
}

/// `GET /api/ai/jobs/stream`: a `jobs` frame with the 200 newest jobs and
/// the queue state now, then one after every change (a `: ping` after
/// `ping_interval` without one). Each connection has its own receiver.
pub async fn jobs_stream(State(st): State<AppState>) -> ApiResult {
    let (tx, resp) = sse::channel(&st, 2);
    let ping = st.0.ping_interval;
    // Subscribed before the first snapshot: a change in between is sent
    // (as an extra frame), never lost.
    let mut changes = st.subscribe_jobs();
    let s2 = st.clone();
    let first = blocking(move || stream_frame(&s2)).await??;
    tokio::spawn(async move {
        if !tx.send(first).await {
            return;
        }
        loop {
            let frame = tokio::select! {
                r = changes.changed() => {
                    if r.is_err() {
                        return;
                    }
                    changes.borrow_and_update();
                    let s2 = st.clone();
                    match blocking(move || stream_frame(&s2)).await {
                        Ok(Ok(f)) => f,
                        _ => continue,
                    }
                }
                _ = tokio::time::sleep(ping) => sse::PING.to_string(),
                _ = tx.closed() => return,
            };
            if !tx.send(frame).await {
                return;
            }
        }
    });
    Ok(resp)
}

// ------------------------------------------------------------- writes

/// `POST /api/ai/jobs/{job_id}/acted-on` with `{action: str}`: 404, 409
/// unless completed, 400 for an action other than applied / discarded;
/// metadata jobs stamp the album's `metadata_ai_acted_on_at`.
pub async fn mark_job_acted_on(
    State(st): State<AppState>,
    UrlPath(job_id): UrlPath<String>,
    body: Bytes,
) -> ApiResult {
    let mut f = Fields::required(parse_json_body(&body)?)?;
    let action = f.req_str("action");
    f.finish()?;
    let action = action.unwrap_or_default();
    let guard = st.begin_work()?;
    blocking(move || {
        let _guard = guard;
        let conn = st.conn()?;
        let job = musaic_services::ai_jobs::mark_acted_on(
            &conn,
            &job_id,
            &action,
            &|| st.now_iso(),
            &st.0.log,
            &|| st.ai_jobs_changed(),
        )?;
        ok_py(&job_body(&job))
    })
    .await?
}

/// `POST /api/ai/jobs/{job_id}/retry`: a failed job back to queued (its
/// stored failure excerpt kept, as in the Python app); 404, or 409 unless failed.
pub async fn retry_job(State(st): State<AppState>, UrlPath(job_id): UrlPath<String>) -> ApiResult {
    let guard = st.begin_work()?;
    blocking(move || {
        let _guard = guard;
        let mut conn = st.conn()?;
        // 409 while the album has another active job,
        // in the enqueue conflict's shape. The failure
        // excerpt is cleared with the error.
        match musaic_services::ai_jobs::retry(&mut conn, &job_id, &|| st.now_iso(), &|| {
            st.ai_jobs_changed()
        })? {
            Ok(updated) => ok_py(&job_body(&updated)),
            Err(c) => Ok(conflict_response(&c)),
        }
    })
    .await?
}

/// `POST /api/ai/queue/resume`: clears `ai_queue_paused` (any body is
/// ignored, as FastAPI does for a route without one).
pub async fn resume_queue(State(st): State<AppState>) -> ApiResult {
    let guard = st.begin_work()?;
    blocking(move || {
        let _guard = guard;
        let conn = st.conn()?;
        ok_py(&st.resume_queue(&conn)?)
    })
    .await?
}

/// `DELETE /api/ai/jobs/{job_id}`: 404, 409 while running.
/// `DELETE /api/ai/jobs/stream`: the Python app routes it to `DELETE
/// /api/ai/jobs/{job_id}` with job id `stream` (first full match in
/// registration order; axum prefers the static `stream` route).
pub async fn delete_job_shadowed(st: State<AppState>) -> ApiResult {
    delete_job(st, UrlPath("stream".to_string())).await
}

/// `POST /api/ai/jobs/active/{x}`: the Python app routes `acted-on` / `retry`
/// to the job routes with job id `active`; anything else is a 405 (the
/// GET route matched by path), with the dispatcher's `Allow`.
pub async fn post_shadowed_by_active(
    st: State<AppState>,
    UrlPath(rest): UrlPath<String>,
    body: Bytes,
) -> ApiResult {
    let id = UrlPath("active".to_string());
    match rest.as_str() {
        "acted-on" => mark_job_acted_on(st, id, body).await,
        "retry" => retry_job(st, id).await,
        _ => Err(ApiError::method_not_allowed()),
    }
}

pub async fn delete_job(State(st): State<AppState>, UrlPath(job_id): UrlPath<String>) -> ApiResult {
    let guard = st.begin_work()?;
    blocking(move || {
        let _guard = guard;
        let conn = st.conn()?;
        musaic_services::ai_jobs::delete(&conn, &job_id, &|| st.ai_jobs_changed())?;
        ok_py(&deleted_body(job_id))
    })
    .await?
}

/// `DELETE /api/ai/jobs?status=a,b` (`status` required): trimmed, blank
/// entries dropped; 400 when none is left or one is unknown, 409 when
/// `running` is named.
pub async fn bulk_delete_jobs(State(st): State<AppState>, q: Q) -> ApiResult {
    let q = qp(q);
    let Some(status) = q.get("status").map(str::to_string) else {
        return Err(unprocessable(vec![err(
            "missing",
            vec![json!("query"), json!("status")],
            "Field required",
            serde_json::Value::Null,
        )]));
    };
    let guard = st.begin_work()?;
    blocking(move || {
        let _guard = guard;
        let statuses = musaic_services::ai_jobs::bulk_delete_statuses(&status)?;
        let conn = st.conn()?;
        let n = musaic_services::ai_jobs::bulk_delete(&conn, &statuses, &|| st.ai_jobs_changed())?;
        ok_py(&bulk_deleted_body(n, statuses))
    })
    .await?
}

// ------------------------------------------------------------- providers

/// `GET /api/ai/providers` in this build: the selection as stored
/// (`ai_provider_id` / `ai_model_id` with the Python app's defaults, whether an
/// `ai_api_key` is set), the one provider this build has, nothing
/// uninstalled.
pub async fn ai_providers(State(st): State<AppState>) -> ApiResult {
    blocking(move || {
        let conn = st.conn()?;
        ok_py(&st.ai_providers(&conn)?)
    })
    .await?
}
