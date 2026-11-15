//! Same-origin previews of a proposed remote cover: deliberate additions,
//! not Python app routes (proxy remote previews through the backend so the
//! desktop CSP can stay `img-src 'self' data: blob:`).
//!
//! - `GET /api/ai/jobs/{job_id}/cover-preview`: the cover a job suggested.
//!   The server reads the job's stored cover suggestion
//!   (`result.cover.cover_url`, written by the AI worker or the model-free
//!   MusicBrainz synthesizer).
//! - `GET /api/albums/{album_id}/cover-preview`: the cover the resolver
//!   proposes for the album, with no job. The URL is derived, never taken:
//!   Tier 0a's anchor (the stored release group, unless blocked with "Wrong
//!   match") must be MBID-shaped, and the URL is the Cover Art Archive front
//!   of that group (`musaic_resolve::resolver::anchored_cover_url`, the
//!   address Tier 1 offers as `cover_url`). The request carries only the
//!   album id, so no part of the fetched address comes from the caller.
//!
//! The client names a job or an album, never a URL, and the server fetches
//! with `musaic_net::preview::fetch_preview` over the app's transport (HTTPS
//! only, public hosts and addresses only, every redirect hop checked, size
//! cap, timeout, allow-listed raster image types with matching signatures).
//! Nothing is cached on disk; the answer carries `Cache-Control: private,
//! no-store` (a suggestion can change on retry or re-anchoring, and a remote
//! picture should not land in the webview's disk cache), `nosniff` and a
//! sandboxing CSP, so the bytes can only ever be an image. In offline mode
//! the transport refuses every request, so both routes answer 502.
//!
//! Read routes: they take no restore gate (reads are not refused while a
//! restore runs); the Host / Origin check and the desktop launch token
//! apply as to every route.
//!
//! Job route: 200 with the image; 404 `Job not found`; 404 [`NO_COVER_URL`]
//! when the job has no cover URL; 502 when the fetch is refused or fails.
//!
//! Album route: 200 with the image; 404 `Album not found` when no album row
//! has the id; 409 (the apply pipeline's Missing refusal text) when the
//! album is Missing, since its cover cannot be changed and a preview only
//! precedes that; 404 [`NO_ALBUM_COVER`] when there is no usable anchor or
//! the Cover Art Archive has no front cover for it (HTTP 404 at the end of
//! the redirects); 502 when the fetch is refused or fails otherwise.

use axum::body::Body;
use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use musaic_db::ai_jobs;
use musaic_net::preview::{Preview, PreviewError, fetch_preview};
use musaic_resolve::db_store::DbStore;
use musaic_resolve::resolver::anchored_cover_url;
use musaic_services::apply::missing_album_detail;

use crate::http::{ApiError, ApiResult, blocking};
use crate::logging::{Level, global};
use crate::state::AppState;

/// The job route, as registered.
pub const COVER_PREVIEW_ROUTE: &str = "/api/ai/jobs/{job_id}/cover-preview";
/// The album route, as registered.
pub const ALBUM_COVER_PREVIEW_ROUTE: &str = "/api/albums/{album_id}/cover-preview";
/// 404 detail when the job stores no cover URL.
pub const NO_COVER_URL: &str = "This job has no suggested cover";
/// 404 detail when the album has no MusicBrainz cover to propose.
pub const NO_ALBUM_COVER: &str = "This album has no MusicBrainz cover to preview";
/// 404 detail for an album id with no row.
pub const ALBUM_NOT_FOUND: &str = "Album not found";
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

/// The 502 for a refused or failed fetch, logged with what it was for.
fn unavailable(subject: &str, e: PreviewError) -> ApiError {
    global(
        Level::Warning,
        "musaic.api.preview",
        &format!("cover preview for {subject} failed: {e}"),
    );
    ApiError::new(502, format!("Cover preview unavailable: {e}"))
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
        fetch_preview(&st.0.transport, &url)
            .map(image_response)
            .map_err(|e| unavailable(&format!("job {job_id}"), e))
    })
    .await?
}

pub async fn album_cover_preview(
    State(st): State<AppState>,
    UrlPath(album_id): UrlPath<String>,
) -> ApiResult {
    blocking(move || {
        let url = {
            let conn = st.conn()?;
            match musaic_db::albums::album_row_missing_since(&conn, &album_id)? {
                None => return Err(ApiError::new(404, ALBUM_NOT_FOUND)),
                Some(Some(since)) if !since.is_empty() => {
                    return Err(ApiError::new(409, missing_album_detail(&since)));
                }
                Some(_) => {}
            }
            let store = DbStore {
                conn: &conn,
                // Only stamped on writes; this route only reads.
                now_iso: String::new(),
            };
            anchored_cover_url(&store, &album_id)
                .map_err(ApiError::internal)?
                .ok_or_else(|| ApiError::new(404, NO_ALBUM_COVER))?
        };
        match fetch_preview(&st.0.transport, &url) {
            Ok(p) => Ok(image_response(p)),
            // The archive has no front cover for the group: what the
            // resolver's HEAD check reads as "Cover not in CAA".
            Err(PreviewError::Status(404)) => Err(ApiError::new(404, NO_ALBUM_COVER)),
            Err(e) => Err(unavailable(&format!("album {album_id}"), e)),
        }
    })
    .await?
}
