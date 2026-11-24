//! Same-origin previews of a proposed remote cover: deliberate additions,
//! not Python app routes (proxy remote previews through the backend so the
//! desktop CSP can stay `img-src 'self' data: blob:`).
//!
//! - `GET /api/ai/jobs/{job_id}/cover-preview`: the cover a research result
//!   suggested. The server reads its stored cover suggestion
//!   (`result.cover.cover_url`, written by an assistant's result, an old job,
//!   or the model-free MusicBrainz synthesizer).
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
use musaic_app::ops::apply::CoverPreview;

use crate::http::{ApiResult, blocking};
use crate::state::AppState;

/// 404 detail when the job stores no cover URL, 404 detail when the album has no
/// MusicBrainz cover to propose, and 404 detail for an album id with no row: the
/// operations' messages.
pub use musaic_app::ops::apply::{ALBUM_NOT_FOUND, NO_ALBUM_COVER, NO_COVER_URL};

/// The job route, as registered.
pub const COVER_PREVIEW_ROUTE: &str = "/api/ai/jobs/{job_id}/cover-preview";
/// The album route, as registered.
pub const ALBUM_COVER_PREVIEW_ROUTE: &str = "/api/albums/{album_id}/cover-preview";
/// `Cache-Control` of a preview.
pub const PREVIEW_CACHE_CONTROL: &str = "private, no-store";
/// `Content-Security-Policy` of a preview: nothing may load or run, even if
/// the response is opened as a document.
pub const PREVIEW_CSP: &str = "default-src 'none'; sandbox";

fn image_response(p: CoverPreview) -> Response {
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
        let preview = st.ops().apply().job_cover_preview(&job_id)?;
        Ok(image_response(preview))
    })
    .await?
}

pub async fn album_cover_preview(
    State(st): State<AppState>,
    UrlPath(album_id): UrlPath<String>,
) -> ApiResult {
    blocking(move || {
        let preview = st.ops().apply().album_cover_preview(&album_id)?;
        Ok(image_response(preview))
    })
    .await?
}
