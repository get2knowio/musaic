//! Starlette's routing, where it differs from axum's.
//!
//! * Starlette matches routes against the percent-decoded path
//!   (`scope["path"]`), so an encoded slash splits a segment:
//!   `POST /api/db/snapshots/..%2Fescape.db/restore` matches no route and
//!   falls to the GET-only catch-all (405), as does a per-track tag route
//!   whose filename carries `%2F`. axum matches the raw path, where `%2F`
//!   stays inside one segment. [`dispatch`] therefore routes on the decoded
//!   path, re-encoding only what a URI path cannot carry (so `%25` stays a
//!   literal `%` for the extractors). The SPA's path containment and the
//!   tag routes' filename handling still apply to whatever reaches them.
//! * FastAPI's `APIRoute` does not add HEAD to a GET route, so HEAD on any
//!   API or SPA path is a 405; only the `/assets` static mount answers it.
//! * A GET that only a route of another method matches is answered by the
//!   GET catch-all, which matches fully: 404 under `/api/`, where
//!   axum would answer 405.
//! * A 405's `Allow` header is the methods of the *first* Python app route
//!   whose path matches (Starlette takes the first partial match), joined
//!   with ", " — e.g. `GET` for `DELETE /api/settings` (GET registered
//!   before PATCH) and for any unknown path (the catch-all) — or the
//!   StaticFiles mount's fixed `GET, HEAD` under `/assets/`. axum lists
//!   every method of the path, with HEAD.
//!
//! [`PYTHON_ROUTES`] is the Python app's `app.routes` in registration order
//! (testdata/golden/api/route_order.json;
//! tests/starlette_routing.rs compares them). Not reproduced: the Python app's
//! `/openapi.json`, `/docs`, `/docs/oauth2-redirect`, `/redoc` (their
//! `Allow` is a Python set joined in hash order).

use axum::Router;
use axum::extract::Request;
use axum::http::{HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use tower::ServiceExt;

/// `(path, Allow)` of every Python app route that can answer a 405, in
/// `app.routes` order.
pub const PYTHON_ROUTES: &[(&str, &str)] = &[
    ("/api/health", "GET"),
    ("/api/config", "GET"),
    ("/api/config", "POST"),
    ("/api/albums", "GET"),
    ("/api/scan/status", "GET"),
    ("/api/library-events/stream", "GET"),
    ("/api/fingerprint-pool/status", "GET"),
    ("/api/fingerprint-pool/stream", "GET"),
    ("/api/scan/stream", "GET"),
    ("/api/scan/rescan", "POST"),
    ("/api/albums/{album_id}", "GET"),
    ("/api/albums/{album_id}", "DELETE"),
    ("/api/albums/{album_id}/cover", "GET"),
    ("/api/albums/{album_id}/tracks", "GET"),
    ("/api/albums/{album_id}/mb-anchor/clear", "POST"),
    ("/api/albums/{album_id}/resolution", "GET"),
    ("/api/albums/{album_id}/tags", "GET"),
    ("/api/albums/{album_id}/tags", "PUT"),
    ("/api/albums/{album_id}/tags", "POST"),
    ("/api/albums/{album_id}/tags/{tag}", "DELETE"),
    ("/api/albums/{album_id}/tracks/{filename}/tags", "POST"),
    (
        "/api/albums/{album_id}/tracks/{filename}/tags/{tag}",
        "DELETE",
    ),
    ("/api/tags", "GET"),
    ("/api/tags/{tag}/tracks", "GET"),
    ("/api/vocabulary", "GET"),
    ("/api/vocabulary", "POST"),
    ("/api/vocabulary/{tag}", "PATCH"),
    ("/api/vocabulary/{tag}/rename", "POST"),
    ("/api/vocabulary/{tag}/merge", "POST"),
    ("/api/vocabulary/{tag}", "DELETE"),
    ("/api/vocabulary/orphans/{tag}/promote", "POST"),
    ("/api/ai/jobs", "POST"),
    ("/api/ai/jobs", "GET"),
    ("/api/ai/jobs/stream", "GET"),
    ("/api/ai/jobs/active/{album_id}", "GET"),
    ("/api/ai/jobs/{job_id}", "GET"),
    ("/api/ai/jobs/{job_id}/acted-on", "POST"),
    ("/api/ai/jobs/{job_id}/retry", "POST"),
    ("/api/ai/queue/resume", "POST"),
    ("/api/ai/jobs/{job_id}", "DELETE"),
    ("/api/ai/jobs", "DELETE"),
    ("/api/settings", "GET"),
    ("/api/settings", "PATCH"),
    ("/api/ai/providers", "GET"),
    ("/api/db/info", "GET"),
    ("/api/db/backup", "GET"),
    ("/api/db/restore", "POST"),
    ("/api/db/snapshots", "GET"),
    ("/api/db/snapshots", "POST"),
    ("/api/db/snapshots/{name}", "DELETE"),
    ("/api/db/snapshots/{name}/restore", "POST"),
    ("/api/checks/stream", "GET"),
    ("/api/checks/findings", "GET"),
    ("/api/checks", "GET"),
    ("/api/checks/{check_id}", "GET"),
    ("/api/checks/{check_id}/run", "POST"),
    (
        "/api/checks/{check_id}/findings/{finding_id}/ignore",
        "POST",
    ),
    (
        "/api/checks/{check_id}/findings/{finding_id}/unignore",
        "POST",
    ),
    ("/api/checks/{check_id}/findings/{finding_id}/fix", "POST"),
    (
        "/api/checks/duplicate-tracks/findings/{finding_id}/canonical",
        "GET",
    ),
    (
        "/api/checks/inconsistent-artist-sort/findings/{finding_id}/apply-sort",
        "POST",
    ),
    (
        "/api/checks/filename-mismatch/findings/{finding_id}/plan-preview",
        "GET",
    ),
    (
        "/api/checks/filename-mismatch/findings/{finding_id}/apply",
        "POST",
    ),
    (
        "/api/checks/duplicate-tracks/findings/{finding_id}/cluster-preview",
        "GET",
    ),
    (
        "/api/checks/duplicate-tracks/findings/{finding_id}/delete-file",
        "POST",
    ),
    ("/api/checks/{check_id}/quick-fix-all", "POST"),
    ("/api/albums/{album_id}/apply/stream", "POST"),
    ("/api/albums/{album_id}/cover/upload", "POST"),
    ("/api/albums/{album_id}/analyze/stream", "POST"),
    ("/", "GET"),
    ("/{full_path:path}", "GET"),
];

/// The StaticFiles mount's own 405 header (starlette/staticfiles.py).
const ASSETS_ALLOW: &str = "GET, HEAD";

/// Starlette's path match: `{name}` is one non-empty segment, a trailing
/// `{name:path}` anything (including nothing).
pub fn path_matches(pattern: &str, path: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix("{full_path:path}") {
        return path.starts_with(prefix);
    }
    let (p, q): (Vec<&str>, Vec<&str>) = (pattern.split('/').collect(), path.split('/').collect());
    p.len() == q.len()
        && p.iter().zip(&q).all(|(a, b)| {
            if a.starts_with('{') && a.ends_with('}') {
                !b.is_empty()
            } else {
                a == b
            }
        })
}

fn under_assets(path: &str, frontend: bool) -> bool {
    frontend && path.starts_with("/assets/")
}

/// The `Allow` header of the Python app's 405 for `path` (decoded).
pub fn python_allow(path: &str, frontend: bool) -> Option<&'static str> {
    if under_assets(path, frontend) {
        return Some(ASSETS_ALLOW);
    }
    PYTHON_ROUTES
        .iter()
        .find(|(p, _)| path_matches(p, path))
        .map(|(_, a)| *a)
}

fn percent_decode(raw: &str) -> Vec<u8> {
    let b = raw.as_bytes();
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let (Some(h), Some(l)) = (
                b.get(i + 1).and_then(|c| hex(*c)),
                b.get(i + 2).and_then(|c| hex(*c)),
            )
        {
            out.push(h << 4 | l);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// Bytes a URI path may carry as they are (RFC 3986 pchar and `/`).
fn keep(c: u8) -> bool {
    c.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:@/".contains(&c)
}

fn reencode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len());
    for &c in bytes {
        if keep(c) {
            s.push(c as char);
        } else {
            s.push_str(&format!("%{c:02X}"));
        }
    }
    s
}

/// The decoded path, as Starlette routes it (lossy UTF-8, as uvicorn's
/// `unquote`).
pub fn decoded_path(raw: &str) -> String {
    String::from_utf8_lossy(&percent_decode(raw)).into_owned()
}

/// Route `req` through `inner` the way Starlette would (module docs).
pub async fn dispatch(inner: Router, frontend: bool, mut req: Request) -> Response {
    let raw = req.uri().path().to_string();
    let bytes = percent_decode(&raw);
    let decoded = String::from_utf8_lossy(&bytes).into_owned();
    let routed = reencode(&bytes);
    if routed != raw {
        let pq = match req.uri().query() {
            Some(q) => format!("{routed}?{q}"),
            None => routed,
        };
        let mut parts = req.uri().clone().into_parts();
        if let Ok(p) = pq.parse() {
            parts.path_and_query = Some(p);
            if let Ok(u) = Uri::from_parts(parts) {
                *req.uri_mut() = u;
            }
        }
    }
    if req.method() == Method::HEAD && !under_assets(&decoded, frontend) {
        // No Python app route accepts HEAD here: let the router refuse it
        // (behind the Host / Origin check) with a method nothing serves.
        if let Ok(m) = Method::from_bytes(b"X-HEAD") {
            *req.method_mut() = m;
        }
    }
    let is_get = req.method() == Method::GET;
    let mut resp = match inner.oneshot(req).await {
        Ok(r) => r,
        Err(never) => match never {},
    };
    if resp.status() == StatusCode::METHOD_NOT_ALLOWED && is_get {
        // The GET-only catch-all fully matches every path, ahead of any
        // partial match: a GET on a path served only for other methods
        // is the catch-all's answer, a 404 under /api/.
        return crate::http::ApiError::not_found().into_response();
    }
    if resp.status() == StatusCode::METHOD_NOT_ALLOWED {
        match python_allow(&decoded, frontend) {
            Some(a) => {
                resp.headers_mut()
                    .insert(header::ALLOW, HeaderValue::from_static(a));
            }
            None => {
                resp.headers_mut().remove(header::ALLOW);
            }
        }
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_reencode_keeps_extractor_values() {
        assert_eq!(decoded_path("/a/..%2Fx.db/b"), "/a/../x.db/b");
        assert_eq!(
            reencode(&percent_decode("/a%2Fb/c%20d/%25/%zz")),
            "/a/b/c%20d/%25/%25zz"
        );
        assert_eq!(reencode(&percent_decode("/api/%68ealth")), "/api/health");
    }
}
