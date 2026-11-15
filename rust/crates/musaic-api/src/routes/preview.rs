//! `GET /api/ai/jobs/{job_id}/cover-preview`: a deliberate addition, not an
//! Python app route (proxy remote previews through the
//! backend so the desktop CSP can stay `img-src 'self' data: blob:`).
//!
//! The client names a job, never a URL: the server reads the job's stored
//! cover suggestion (`result.cover.cover_url`, written by the AI worker or
//! the model-free MusicBrainz synthesizer) and fetches that with
//! `musaic_net::preview::fetch_preview` over the app's transport (HTTPS
//! only, public hosts and addresses only, every redirect hop checked, size
//! cap, timeout, allow-listed raster image types with matching signatures).
//! Nothing is cached on disk; the answer carries `Cache-Control: private,
//! no-store` (a job's suggestion can change on retry, and a remote picture
//! should not land in the webview's disk cache), `nosniff` and a
//! sandboxing CSP, so the bytes can only ever be an image.
//!
//! A read route: it takes no restore gate (reads are
//! not refused while a restore runs); the Host / Origin check and the
//! desktop launch token apply as to every route.
//!
//! Answers: 200 with the image; 404 `Job not found`; 404 when the job has
//! no cover URL; 502 when the fetch is refused or fails.

use axum::body::Body;
use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use musaic_db::ai_jobs;
use musaic_net::preview::{Preview, fetch_preview};

use crate::http::{ApiError, ApiResult, blocking};
use crate::logging::{Level, global};
use crate::state::AppState;

/// The route, as registered.
pub const COVER_PREVIEW_ROUTE: &str = "/api/ai/jobs/{job_id}/cover-preview";
/// 404 detail when the job stores no cover URL.
pub const NO_COVER_URL: &str = "This job has no suggested cover";
/// `Cache-Control` of a preview.
pub const PREVIEW_CACHE_CONTROL: &str = "private, no-store";
/// `Content-Security-Policy` of a preview: nothing may load or run, even if
/// the response is opened as a document.
pub const PREVIEW_CSP: &str = "default-src 'none'; sandbox";

/// `result.cover.cover_url` of a stored job result, when it is a non-empty
/// string.
pub fn stored_cover_url(result_json: Option<&str>) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(result_json?).ok()?;
    v.get("cover")?
        .get("cover_url")?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn image_response(p: Preview) -> Response {
    let mut r = Response::new(Body::from(p.bytes));
    *r.status_mut() = StatusCode::OK;
    let h = r.headers_mut();
    if let Ok(ct) = HeaderValue::from_str(&p.mime) {
        h.insert(header::CONTENT_TYPE, ct);
    }
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(PREVIEW_CACHE_CONTROL),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(PREVIEW_CSP),
    );
    r
}

pub async fn job_cover_preview(
    State(st): State<AppState>,
    UrlPath(job_id): UrlPath<String>,
) -> ApiResult {
    blocking(move || {
        let url = {
            let conn = st.conn()?;
            let job = ai_jobs::get_job(&conn, &job_id)?
                .ok_or_else(|| ApiError::new(404, "Job not found"))?;
            stored_cover_url(job.result_json.as_deref())
                .ok_or_else(|| ApiError::new(404, NO_COVER_URL))?
        };
        match fetch_preview(&st.0.transport, &url) {
            Ok(p) => Ok(image_response(p)),
            Err(e) => {
                global(
                    Level::Warning,
                    "musaic.api.preview",
                    &format!("cover preview for job {job_id} failed: {e}"),
                );
                Err(ApiError::new(
                    502,
                    format!("Cover preview unavailable: {e}"),
                ))
            }
        }
    })
    .await?
}
