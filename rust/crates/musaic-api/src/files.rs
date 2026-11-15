//! File serving helpers: Starlette's `FileResponse` (content type from the
//! file name, an ETag). The largest embedded FLAC picture
//! (`scanner.get_embedded_cover`) and cover thumbnails (`thumbs.py`:
//! `<config>/thumbs/<album id>.jpg`, long edge 384 px, JPEG quality 82,
//! the same cache format as the Python app) live in [`musaic_services::covers`] and are re-exported here.

use std::path::Path;
use std::time::UNIX_EPOCH;

use axum::body::Body;
use axum::http::{HeaderValue, header};
use axum::response::Response;

use crate::http::ApiError;

/// `mimetypes.guess_type` for the suffixes a built SPA and the cover files
/// use; Starlette falls back to `text/plain` and adds `charset=utf-8` to
/// `text/*`.
pub fn guess_mime(p: &Path) -> String {
    let ext = p
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let base = match ext.as_str() {
        "html" | "htm" => "text/html",
        "js" | "mjs" => "text/javascript",
        "css" => "text/css",
        "json" | "map" => "application/json",
        "txt" => "text/plain",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "wasm" => "application/wasm",
        "webmanifest" => "application/manifest+json",
        _ => "text/plain",
    };
    if base.starts_with("text/") {
        format!("{base}; charset=utf-8")
    } else {
        base.to_string()
    }
}

/// `FileResponse(path, media_type=...)`: the bytes, the type, an ETag from
/// mtime and size.
pub fn file_response(path: &Path, mime: &str) -> Result<Response, ApiError> {
    let bytes = std::fs::read(path).map_err(ApiError::internal)?;
    let meta = std::fs::metadata(path).map_err(ApiError::internal)?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    let mut r = Response::new(Body::from(bytes));
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(mime).unwrap_or(HeaderValue::from_static("text/plain")),
    );
    if let Ok(v) = HeaderValue::from_str(&format!("\"{mtime:x}-{:x}\"", meta.len())) {
        h.insert(header::ETAG, v);
    }
    Ok(r)
}

/// Album covers and thumbnails live in [`musaic_services::covers`]
/// (`scanner.get_embedded_cover`, `thumbs.ensure_thumb`, `thumbs.thumb_etag`);
/// re-exported under their old paths.
pub use musaic_services::covers::{embedded_cover, ensure_thumb, thumb_etag};
