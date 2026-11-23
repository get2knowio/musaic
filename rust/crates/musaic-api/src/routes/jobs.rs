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
//! Every handler answers the Python app's 422 for its body, query or path first,
//! then calls one operation of `ops().jobs()` and renders the answer
//! (`crate::render::jobs`: the Python app's bodies, key order included; the
//! stream's frames are `sse_event("jobs", {"jobs": [...], "queue_state": {...}})`
//! byte for byte). The writers' guard (taken before any check of their own) and
//! their wake-up of the jobs feeds are the operations' (`docs/operations.md`).
//!
//! `GET /api/ai/providers` lists the provider this build has (by default
//! [`musaic_ai::Unconfigured`]) instead of the installed airframe adapters,
//! and `uninstalled` is empty (this build installs no other provider).

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
    blocking(move || {
        let job = st.ops().jobs().mark_acted_on(&job_id, &action)?;
        ok_ordered(&render::job(&job))
    })
    .await?
}

/// `POST /api/ai/jobs/{job_id}/retry`: a failed job back to queued (its
/// stored failure excerpt cleared with the error); 404, or 409 unless failed, or
/// while the album has another active job (the enqueue conflict's shape).
pub async fn retry_job(State(st): State<AppState>, UrlPath(job_id): UrlPath<String>) -> ApiResult {
    blocking(move || match st.ops().jobs().retry(&job_id) {
        Ok(job) => ok_ordered(&render::job(&job)),
        Err(e) => render::job_error(e),
    })
    .await?
}

/// `POST /api/ai/queue/resume`: clears `ai_queue_paused` (any body is
/// ignored, as FastAPI does for a route without one).
pub async fn resume_queue(State(st): State<AppState>) -> ApiResult {
    blocking(move || {
        let state = st.ops().jobs().resume_queue()?;
        ok_ordered(&render::resumed(&state))
    })
    .await?
}

/// `DELETE /api/ai/jobs/stream`: the Python app routes it to `DELETE
/// /api/ai/jobs/{job_id}` with job id `stream` (first full match in
/// registration order; axum prefers the static `stream` route). A routing
/// alias of this face, no operation of its own.
pub async fn delete_job_shadowed(st: State<AppState>) -> ApiResult {
    delete_job(st, UrlPath("stream".to_string())).await
}

/// `POST /api/ai/jobs/active/{x}`: the Python app routes `acted-on` / `retry`
/// to the job routes with job id `active`; anything else is a 405 (the
/// GET route matched by path), with the dispatcher's `Allow`. A routing alias of
/// this face, no operation of its own.
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

// ------------------------------------------------------------- providers

/// `GET /api/ai/providers` in this build: the selection as stored
/// (`ai_provider_id` / `ai_model_id` with the Python app's defaults, whether an
/// `ai_api_key` is set), the one provider this build has, nothing
/// uninstalled.
pub async fn ai_providers(State(st): State<AppState>) -> ApiResult {
    blocking(move || {
        let list = st.ops().jobs().providers()?;
        ok_ordered(&render::providers(&list))
    })
    .await?
}

pub async fn research_work(State(st): State<AppState>) -> ApiResult {
    blocking(move || crate::http::ok_value(json!(st.ops().jobs().delegated_work()?))).await?
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
