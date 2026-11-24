//! `musaic/api/jobs.py` (the job queue's REST + SSE surface, now the
//! desktop's view of research requests).
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
//! | `POST /api/ai/jobs/{job_id}/cancel` | [`cancel_research_group`] (not a route of the Python app) |
//! | `GET /api/research/batches` | [`research_batches`] (not a route of the Python app) |
//! | `GET /api/research/batches/{batch_id}` | [`research_batch`] (not a route of the Python app) |
//! | `POST /api/research/batches/{batch_id}/cancel` | [`cancel_research_batch`] (not a route of the Python app) |
//!
//! Every handler answers the Python app's 422 for its body, query or path first,
//! then calls one operation of `ops().jobs()` and renders the answer
//! (`crate::render::jobs`: the Python app's bodies, key order included; the
//! stream's frames are `sse_event("jobs", {"jobs": [...], "queue_state": {...}})`
//! byte for byte). The writers' guard (taken before any check of their own) and
//! their wake-up of the jobs feeds are the operations' (`docs/operations.md`).
//!
//! The Python app's provider list, queue resume, job retry and acted-on routes
//! are removed (Musaic has no built-in model); research is retried and reviewed
//! through its items (`POST /api/research/{id}/decision`).

use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State};
use serde_json::json;

use crate::http::{ApiError, ApiResult, blocking, ok_ordered};
use crate::lax::{Fields, err, parse_json_body, unprocessable};
use crate::render::jobs as render;
use crate::routes::library::{Q, qp};
use crate::sse;
use crate::state::AppState;

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
    blocking(move || {
        match st
            .ops()
            .jobs()
            .enqueue(&album_id, &job_type, &hint, attach_cover)
        {
            Ok(job) => ok_ordered(&render::job(&job)),
            Err(e) => render::job_error(e),
        }
    })
    .await?
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
        let jobs = st.ops().jobs().list(statuses.as_deref())?;
        ok_ordered(&render::job_list(&jobs))
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
        let job = st.ops().jobs().active_for_album(&album_id)?;
        ok_ordered(&render::job_or_null(job.as_ref()))
    })
    .await?
}

/// `GET /api/ai/jobs/{job_id}`.
pub async fn get_job(State(st): State<AppState>, UrlPath(job_id): UrlPath<String>) -> ApiResult {
    blocking(move || {
        let job = st.ops().jobs().get(&job_id)?;
        ok_ordered(&render::job(&job))
    })
    .await?
}

// ------------------------------------------------------------- stream

/// `GET /api/ai/jobs/stream`: a `jobs` frame with the 200 newest jobs and
/// the queue state now, then one after every change (a `: ping` after
/// `ping_interval` without one). Each connection has its own feed, subscribed
/// before the first value is read: a change in between is sent (as an extra
/// frame), never lost. The first value is read before the response begins, so a
/// database failure there is an error response; a later value that cannot be
/// read is skipped.
pub async fn jobs_stream(State(st): State<AppState>) -> ApiResult {
    let mut feed = st.ops().jobs().feed();
    let (tx, resp) = sse::channel(&st, 2);
    let ping = st.ping_interval();
    let (back, first) = blocking(move || {
        let first = feed.next(Duration::ZERO);
        (feed, first)
    })
    .await?;
    feed = back;
    let first = match first {
        Some(Ok(list)) => render::jobs_frame(&list),
        Some(Err(e)) => return Err(e.into()),
        // The application is shutting down: the stream ends.
        None => return Ok(resp),
    };
    tokio::spawn(async move {
        if !tx.send(first).await {
            return;
        }
        loop {
            let ready = tokio::select! {
                r = feed.wait() => Some(r),
                _ = tokio::time::sleep(ping) => None,
                _ = tx.closed() => return,
            };
            let frame = match ready {
                Some(false) => return,
                None => sse::PING.to_string(),
                Some(true) => {
                    let Ok((back, next)) = blocking(move || {
                        let next = feed.next(Duration::ZERO);
                        (feed, next)
                    })
                    .await
                    else {
                        return;
                    };
                    feed = back;
                    match next {
                        Some(Ok(list)) => render::jobs_frame(&list),
                        Some(Err(_)) => continue,
                        None => return,
                    }
                }
            };
            if !tx.send(frame).await {
                return;
            }
        }
    });
    Ok(resp)
}

// ------------------------------------------------------------- writes

/// `DELETE /api/ai/jobs/stream`: the Python app routes it to `DELETE
/// /api/ai/jobs/{job_id}` with job id `stream` (first full match in
/// registration order; axum prefers the static `stream` route). A routing
/// alias of this face, no operation of its own.
pub async fn delete_job_shadowed(st: State<AppState>) -> ApiResult {
    delete_job(st, UrlPath("stream".to_string())).await
}

/// `POST /api/ai/jobs/active/{x}`: a 405 (the GET route matched by path),
/// with the dispatcher's `Allow`. The Python app routed `acted-on` / `retry`
/// to the job routes with job id `active`; those routes are removed. A routing alias of this face, no operation of its own.
pub async fn post_shadowed_by_active(UrlPath(_rest): UrlPath<String>) -> ApiResult {
    Err(ApiError::method_not_allowed())
}

/// `DELETE /api/ai/jobs/{job_id}`: 404, 409 while running.
pub async fn delete_job(State(st): State<AppState>, UrlPath(job_id): UrlPath<String>) -> ApiResult {
    blocking(move || {
        let deleted = st.ops().jobs().delete(&job_id)?;
        ok_ordered(&render::deleted(&deleted))
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
    let statuses: Vec<String> = status.split(',').map(str::to_string).collect();
    blocking(move || {
        let deleted = st.ops().jobs().delete_many(&statuses)?;
        ok_ordered(&render::deleted_many(&deleted))
    })
    .await?
}

pub async fn research_work(State(st): State<AppState>) -> ApiResult {
    blocking(move || crate::http::ok_value(json!(st.ops().jobs().delegated_work()?))).await?
}

/// `GET /api/research/batches`: every batch with a request left, newest first.
pub async fn research_batches(State(st): State<AppState>) -> ApiResult {
    blocking(move || {
        let batches = st.ops().jobs().research_batches()?;
        crate::http::ok_value(render::batches(&batches))
    })
    .await?
}

/// `GET /api/research/batches/{batch_id}`: one batch with its items; 404.
pub async fn research_batch(
    State(st): State<AppState>,
    UrlPath(batch_id): UrlPath<String>,
) -> ApiResult {
    blocking(move || {
        let batch = st.ops().jobs().research_batch(&batch_id)?;
        crate::http::ok_value(render::batch_detail(&batch))
    })
    .await?
}

/// `POST /api/research/batches/{batch_id}/cancel`: 409 during a restore, 404;
/// the batch after the cancel with `cancelled`.
pub async fn cancel_research_batch(
    State(st): State<AppState>,
    UrlPath(batch_id): UrlPath<String>,
) -> ApiResult {
    blocking(move || {
        let done = st.ops().jobs().cancel_research_batch(&batch_id)?;
        crate::http::ok_value(render::batch_cancelled(&done))
    })
    .await?
}

/// `POST /api/ai/jobs/{job_id}/cancel` (any body is ignored): 409 during a
/// restore or when nothing is waiting or held, 404;
/// `{"id": ..., "cancelled": n}`.
pub async fn cancel_research_group(
    State(st): State<AppState>,
    UrlPath(job_id): UrlPath<String>,
) -> ApiResult {
    blocking(move || {
        let done = st.ops().jobs().cancel_research_group(&job_id)?;
        crate::http::ok_value(render::group_cancelled(&done))
    })
    .await?
}

pub async fn research_items(
    State(st): State<AppState>,
    UrlPath(group): UrlPath<String>,
) -> ApiResult {
    blocking(move || crate::http::ok_value(json!(st.ops().jobs().research_items(&group)?))).await?
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReviewRequest {
    action: String,
    revision: i64,
    digest: Option<String>,
    preview_digest: Option<String>,
}

pub async fn research_decide(
    State(st): State<AppState>,
    UrlPath(id): UrlPath<String>,
    body: Bytes,
) -> ApiResult {
    let r: ReviewRequest =
        serde_json::from_slice(&body).map_err(|_| ApiError::new(400, "Invalid review decision"))?;
    blocking(move || {
        st.ops().jobs().research_decide(
            &id,
            r.revision,
            r.digest.as_deref(),
            &r.action,
            r.preview_digest.as_deref(),
        )?;
        crate::http::ok_value(json!({"ok":true}))
    })
    .await?
}

pub async fn research_preview(
    State(st): State<AppState>,
    UrlPath((id, digest)): UrlPath<(String, String)>,
) -> ApiResult {
    blocking(move || {
        use base64::Engine as _;
        let preview = st.ops().jobs().research_preview(&id, &digest)?;
        let mut value = json!(preview);
        if let Some(thumb) = preview.cover_thumbnail {
            let encoded = base64::engine::general_purpose::STANDARD.encode(thumb);
            value["coverThumbnail"] = json!(format!("data:image/jpeg;base64,{encoded}"));
        }
        crate::http::ok_value(value)
    })
    .await?
}
