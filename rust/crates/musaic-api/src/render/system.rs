//! The `system` group as bytes: bodies and SSE frames for health, the
//! fingerprint pool and the library event stream.

use musaic_app::ops::system::{Health, LibraryEvent, PoolState};
use musaic_services::Json;

/// `GET /api/health`.
pub fn health(h: Health) -> Json {
    match h {
        Health::Ok => Json::obj([("status", Json::from("ok"))]),
    }
}

/// `GET /api/fingerprint-pool/status`: `get_state()` in the Python app's key
/// order (`tests/fingerprint_pool_golden.rs` pins it).
pub fn pool_state(s: &PoolState) -> Json {
    Json::obj([
        ("status", Json::from(s.status.as_str())),
        ("library_dir", Json::from(s.library_dir.clone())),
        ("total", Json::from(s.total)),
        ("completed", Json::from(s.completed)),
        ("errors", Json::from(s.errors)),
        ("current_album_id", Json::from(s.current_album_id.clone())),
        ("started_at", Json::from(s.started_at.clone())),
        ("completed_at", Json::from(s.completed_at.clone())),
        ("skip_reason", Json::from(s.skip_reason.clone())),
    ])
}

/// One frame of `GET /api/fingerprint-pool/stream`: `sse_event("state", state)`.
pub fn pool_frame(s: &PoolState) -> String {
    format!("event: state\ndata: {}\n\n", pool_state(s).dumps())
}

/// The pool stream's keepalive comment, sent after a quiet interval.
pub const POOL_KEEPALIVE: &str = musaic_services::fingerprint_pool::KEEPALIVE_FRAME;

/// The first frame of `GET /api/library-events/stream`.
pub fn library_ready() -> String {
    musaic_events::ready_frame()
}

/// One bus event as a named frame.
pub fn library_event_frame(e: &LibraryEvent) -> String {
    musaic_events::sse_frame(e.name(), e.payload_json())
}
