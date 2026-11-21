//! `musaic/api/library.py`: config, albums, scan, album detail / delete,
//! cover, tracks, MusicBrainz anchor clear, resolution.
//!
//! Every handler parses the request (a 422 before anything runs), calls one
//! operation of `ops().library()` / `ops().scan()` and renders the answer
//! (`crate::render::library`). The guard, the order of the preconditions and
//! the events are the operations' (`docs/operations.md`); what stays here is
//! HTTP: the query parsing, the cover's ETag and cache headers, and the scan
//! stream's framing.

use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use musaic_app::ops::library::{CoverImage, CoverSize};
use musaic_app::ops::scan::{ScanFollow, ScanPoll, ScanSubscription};

use crate::files;
use crate::http::{ApiResult, blocking, ok_ordered, ok_value};
use crate::lax::{Fields, Params, QueryParams, parse_json_body};
use crate::render::library as render;
use crate::sse;
use crate::state::AppState;

pub use crate::route_table::Q;

pub fn qp(q: Q) -> QueryParams {
    QueryParams(q.0, q.1)
}

// ------------------------------------------------------------- config

pub async fn get_config(State(st): State<AppState>) -> ApiResult {
    ok_ordered(&render::config(&st.ops().library().config()))
}

pub async fn post_config(State(st): State<AppState>, body: Bytes) -> ApiResult {
    let mut f = Fields::required(parse_json_body(&body)?)?;
    let dir = f.req_str("library_dir");
    f.finish()?;
    let dir = dir.unwrap_or_default();
    blocking(move || {
        let choice = st.ops().library().set_library_dir(&dir)?;
        ok_ordered(&render::library_choice(choice))
    })
    .await?
}

// ------------------------------------------------------------- albums

pub async fn list_albums(State(st): State<AppState>, q: Q) -> ApiResult {
    let q = qp(q);
    let mut p = Params::default();
    let refresh = p.query_bool(&q, "refresh", false);
    p.finish()?;
    blocking(move || {
        let out = st.ops().library().list_albums(refresh)?;
        ok_ordered(&render::album_list(out))
    })
    .await?
}

pub async fn get_album(
    State(st): State<AppState>,
    UrlPath(album_id): UrlPath<String>,
) -> ApiResult {
    blocking(move || {
        let album = st.ops().library().album(&album_id)?;
        ok_ordered(&render::album(&album))
    })
    .await?
}

pub async fn delete_album(
    State(st): State<AppState>,
    UrlPath(album_id): UrlPath<String>,
) -> ApiResult {
    blocking(move || {
        let deleted = st.ops().library().remove_album(&album_id)?;
        ok_ordered(&render::deleted_album(deleted))
    })
    .await?
}

// ------------------------------------------------------------- scan

pub async fn scan_status(State(st): State<AppState>) -> ApiResult {
    ok_ordered(&render::scan_state(&st.ops().scan().status()))
}

/// `POST /api/scan/cancel` (not a route of the Python app): ask the running
/// scan to stop. Always 200 with the outcome, because a cancel can race with
/// the scan ending on its own: `requested` / `already_requested` (it ends
/// `cancelled`, nothing saved), `too_late` (it is saving and ends `done`),
/// `not_running`. Takes no work guard: it writes nothing, and must work while
/// a restore holds the gate.
pub async fn scan_cancel(State(st): State<AppState>) -> ApiResult {
    ok_ordered(&render::cancel(&st.ops().scan().cancel()))
}

pub async fn scan_stream(State(st): State<AppState>, q: Q) -> ApiResult {
    let q = qp(q);
    let mut p = Params::default();
    let force = p.query_bool(&q, "force", false);
    p.finish()?;
    // Every subscriber replays the current scan's buffer from 0.
    let sub = st.ops().scan().subscribe(force)?;
    Ok(stream_scan(&st, sub))
}

/// `GET /api/scan/follow` (not a route of the Python app): the scan stream
/// for a reader that only watches. It never starts a scan: a running scan is
/// streamed as on `/api/scan/stream`, the last one (already ended) is
/// replayed to its terminal event, and with no scan since startup the
/// stream is one `idle` event carrying the scan state, then closes.
pub async fn scan_follow(State(st): State<AppState>) -> ApiResult {
    match st.ops().scan().follow() {
        ScanFollow::Following(sub) => Ok(stream_scan(&st, sub)),
        ScanFollow::Idle(state) => {
            // One frame into an empty channel of one; dropping the sender
            // then ends the stream.
            let (tx, resp) = sse::channel(&st, 1);
            let _ = tx.try_send_frame(render::scan_idle_frame(&state));
            Ok(resp)
        }
    }
}

/// Frame a scan subscription onto an SSE response.
fn stream_scan(st: &AppState, mut sub: ScanSubscription) -> Response {
    let (tx, resp) = sse::channel(st, 16);
    // Until the scan is over or the client is gone (the scan keeps running):
    // poll every 200 ms, asking `is_closed` between events.
    tokio::task::spawn_blocking(move || {
        loop {
            match sub.next_timeout(Duration::from_millis(200)) {
                ScanPoll::Event(ev) => {
                    if !tx.send_blocking(render::scan_frame(&ev)) {
                        return;
                    }
                }
                ScanPoll::Pending => {
                    if tx.is_closed() {
                        return;
                    }
                }
                ScanPoll::Finished => return,
            }
        }
    });
    resp
}

pub async fn rescan(State(st): State<AppState>) -> ApiResult {
    blocking(move || {
        let started = st.ops().scan().rescan()?;
        ok_ordered(&render::rescan(started))
    })
    .await?
}

// ------------------------------------------------------------- tracks

pub async fn get_tracks(
    State(st): State<AppState>,
    UrlPath(album_id): UrlPath<String>,
) -> ApiResult {
    blocking(move || {
        let tracks = st.ops().library().tracks(&album_id)?;
        ok_ordered(&render::tracks(tracks))
    })
    .await?
}

// ------------------------------------------------------------- cover

fn cover_headers(r: &mut Response) {
    r.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=86400"),
    );
}

pub async fn get_cover(
    State(st): State<AppState>,
    UrlPath(album_id): UrlPath<String>,
    q: Q,
    headers: HeaderMap,
) -> ApiResult {
    let q = qp(q);
    let size = if q.get("size").unwrap_or("thumb") == "full" {
        CoverSize::Full
    } else {
        CoverSize::Thumb
    };
    let inm = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    blocking(move || {
        let cover = st.ops().library().cover(&album_id, size)?;
        match cover {
            CoverImage::File(disk) => {
                let mime = files::guess_mime(&disk);
                let mut r = files::file_response(&disk, &mime)?;
                cover_headers(&mut r);
                Ok(r)
            }
            CoverImage::Embedded { data, mime } => {
                let mut r = Response::new(Body::from(data));
                r.headers_mut().insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_str(&mime).unwrap_or(HeaderValue::from_static("image/jpeg")),
                );
                cover_headers(&mut r);
                Ok(r)
            }
            CoverImage::Thumbnail(thumb) => {
                let etag = files::thumb_etag(&thumb, &album_id);
                if let (Some(e), Some(i)) = (&etag, &inm)
                    && e == i
                {
                    let mut r = Response::new(Body::empty());
                    *r.status_mut() = StatusCode::NOT_MODIFIED;
                    if let Ok(v) = HeaderValue::from_str(e) {
                        r.headers_mut().insert(header::ETAG, v);
                    }
                    return Ok(r);
                }
                let mut r = files::file_response(&thumb, "image/jpeg")?;
                cover_headers(&mut r);
                if let Some(e) = etag.and_then(|e| HeaderValue::from_str(&e).ok()) {
                    r.headers_mut().insert(header::ETAG, e);
                }
                Ok(r)
            }
        }
    })
    .await?
}

// ------------------------------------------------------------- anchor

pub async fn clear_mb_anchor(
    State(st): State<AppState>,
    UrlPath(album_id): UrlPath<String>,
) -> ApiResult {
    blocking(move || {
        let cleared = st.ops().library().clear_mb_anchor(&album_id)?;
        ok_ordered(&render::cleared_anchor(cleared))
    })
    .await?
}

// ------------------------------------------------------------- resolution

/// A resolution is a read in the Python app's API but can store fingerprints,
/// AcoustID matches and an MBID anchor (Tiers 0b / 0c), so the operation holds
/// a guard for its whole run (a restore asked for meanwhile gets 409).
pub async fn get_resolution(
    State(st): State<AppState>,
    UrlPath(album_id): UrlPath<String>,
    q: Q,
) -> ApiResult {
    let q = qp(q);
    let mut p = Params::default();
    let force = p.query_bool(&q, "force", false);
    p.finish()?;
    let hint = q.get("hint").unwrap_or("").to_string();
    blocking(move || {
        let r = st.ops().library().resolution(&album_id, force, &hint)?;
        ok_value(render::resolution(&r))
    })
    .await?
}
