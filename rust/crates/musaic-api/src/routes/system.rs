//! `musaic/api/system.py` (readiness probe), the App-level
//! `/api/library-events/stream` relay, the fingerprint
//! pool status and stream, the SPA, the unmatched-route answers
//! and the 501 stubs.

use std::path::{Component, Path, PathBuf};

use axum::extract::{OriginalUri, Path as UrlPath, State};
use axum::http::Method;

use crate::files;
use crate::http::{ApiError, ApiResult, ok_ordered};
use crate::render::system as render;
use crate::sse;
use crate::state::AppState;

/// `GET /api/health`: touches nothing.
pub async fn health(State(st): State<AppState>) -> ApiResult {
    ok_ordered(&render::health(st.ops().system().health()))
}

/// `GET /api/library-events/stream`: `ready` first, then every bus event as
/// a named frame, `: ping` after `ping_interval` of silence. The relay
/// connection is registered before the response is returned and dropped
/// (unregistered) as soon as the client goes away.
pub async fn library_events_stream(State(st): State<AppState>) -> ApiResult {
    let mut events = st.ops().system().library_events();
    let (tx, resp) = sse::channel(&st, 8);
    let ping = st.ping_interval();
    tokio::spawn(async move {
        if !tx.send(render::library_ready()).await {
            return;
        }
        loop {
            tokio::select! {
                m = events.recv() => {
                    let Some(m) = m else { return };
                    if !tx.send(render::library_event_frame(&m)).await { return; }
                }
                _ = tokio::time::sleep(ping) => {
                    if !tx.send(sse::PING).await { return; }
                }
                _ = tx.closed() => return,
            }
        }
    });
    Ok(resp)
}

/// `GET /api/fingerprint-pool/status`: `fingerprint_pool.get_state()`.
pub async fn fingerprint_pool_status(State(st): State<AppState>) -> ApiResult {
    ok_ordered(&render::pool_state(&st.ops().system().pool_status()))
}

/// `GET /api/fingerprint-pool/stream` (musaic/api/library.py:161-193): the
/// state on connect, then the state again after each pool change, or the
/// keepalive frame after `ping_interval` (30 s) without one.
///
/// One wake-up per connection. The
/// Python app's connections share one `asyncio.Event`: a set wakes the waiters
/// of that moment, the first to run clears it, so a connection busy sending
/// misses that change, and a connection opened while it is still set sends
/// the state twice. Here each connection subscribes to the pool's change
/// counter (`ops::system::System::pool_feed`) before it reads the first state:
/// every change after that wakes this connection once (changes that arrive
/// while it sends are coalesced into one frame with the newest state), and
/// changes before it connected are already reflected in its first frame. The
/// pool's own event is no longer cleared by a stream.
pub async fn fingerprint_pool_stream(State(st): State<AppState>) -> ApiResult {
    let (tx, resp) = sse::channel(&st, 2);
    let ping = st.ping_interval();
    // Subscribed before the state is read: a change in between is sent
    // (as an extra frame), never lost.
    let mut feed = st.ops().system().pool_feed();
    tokio::spawn(async move {
        loop {
            let frame = tokio::select! {
                s = feed.changed() => match s {
                    Some(s) => render::pool_frame(&s),
                    None => return,
                },
                _ = tokio::time::sleep(ping) => render::POOL_KEEPALIVE.to_string(),
                _ = tx.closed() => return,
            };
            if !tx.send(frame).await {
                return;
            }
        }
    });
    Ok(resp)
}

// ------------------------------------------------------------- SPA

fn index(dir: &Path) -> ApiResult {
    files::file_response(&dir.join("index.html"), "text/html; charset=utf-8")
}

/// A path under `dir`, or `None` when it would leave it. Deliberate change
/// from the Python app: `main.spa_fallback` joined
/// the decoded path onto the frontend dir without a check, so
/// `GET /..%2Fsecret` served any file the process could read.
fn contained(dir: &Path, rel: &str) -> Option<PathBuf> {
    let rel_path = Path::new(rel);
    if rel_path
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        return None;
    }
    let candidate = dir.join(rel_path);
    let real = candidate.canonicalize().ok()?;
    let root = dir.canonicalize().ok()?;
    real.starts_with(&root).then_some(real)
}

/// `GET /`.
pub async fn spa_root(State(st): State<AppState>) -> ApiResult {
    match st.frontend_dir() {
        Some(d) => index(d),
        None => Err(ApiError::not_found()),
    }
}

/// `GET /{full_path:path}`: 404 for anything under `api/`, a file
/// of the built SPA when it is one, index.html otherwise.
/// `/assets/*` is the Python app's StaticFiles mount: a missing asset is a 404.
pub async fn spa_fallback(State(st): State<AppState>, UrlPath(full): UrlPath<String>) -> ApiResult {
    if full.starts_with("api/") {
        return Err(ApiError::not_found());
    }
    let Some(dir) = st.frontend_dir().map(Path::to_path_buf) else {
        return Err(ApiError::not_found());
    };
    if let Some(rest) = full.strip_prefix("assets/") {
        return match contained(&dir.join("assets"), rest).filter(|p| p.is_file()) {
            Some(p) => files::file_response(&p, &files::guess_mime(&p)),
            None => Err(ApiError::not_found()),
        };
    }
    match contained(&dir, &full).filter(|p| p.is_file()) {
        Some(p) => files::file_response(&p, &files::guess_mime(&p)),
        None => index(&dir),
    }
}

/// Unmatched path: GET under /api is 404, any other method 405 (the
/// Python app's GET-only catch-all route matches every path). Also serves as
/// the method-not-allowed answer for every route.
pub async fn unmatched(method: Method, OriginalUri(uri): OriginalUri) -> ApiError {
    let _ = uri;
    if method == Method::GET || method == Method::HEAD {
        ApiError::not_found()
    } else {
        ApiError::method_not_allowed()
    }
}

pub async fn method_not_allowed() -> ApiError {
    ApiError::method_not_allowed()
}

/// The answer of every route in [`crate::STUBS`].
pub async fn not_implemented(method: Method, OriginalUri(uri): OriginalUri) -> ApiError {
    ApiError::new(
        501,
        format!(
            "Not implemented in the Rust build yet: {} {}",
            method,
            uri.path()
        ),
    )
}
