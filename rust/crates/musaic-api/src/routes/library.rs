//! `musaic/api/library.py`: config, albums, scan, album detail / delete,
//! cover, tracks, MusicBrainz anchor clear, resolution.
//!
//! Every route here that writes the database takes the
//! restore gate's [`crate::gate::WorkGuard`] after request validation (422) and before
//! anything else, and holds it on the blocking task until its work
//! ends: `POST /api/config` (drops the library's snapshot), album delete,
//! anchor clear, `GET /api/albums?refresh=true` (drops the snapshot), scans,
//! and `GET /api/albums/{id}/resolution`.
//!
//! The resolution is a read in the Python app's API but can store fingerprints,
//! AcoustID matches and an MBID anchor (Tiers 0b / 0c). Choice: it holds a
//! guard for its whole run, so a restore asked for meanwhile is refused with
//! 409 rather than waiting or cancelling it. A resolution is bounded by one
//! album (Tier 0a, the common case, is a cache hit; a cold one is a handful
//! of rate-limited MusicBrainz / AcoustID requests plus one fingerprint per
//! track, seconds to tens of seconds), unlike the fingerprint pool, which a
//! restore stops instead because a pass can run for hours. Cancelling a
//! resolution would need a cancellation point inside the resolver, and a
//! restore that waited would hold the request open with no feedback; the
//! 409 tells the user to retry, which succeeds once the lookup ends.

use std::path::Path;

use axum::body::{Body, Bytes};
use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use musaic_services::Json;
use musaic_services::covers::{self, FullCover};
use rusqlite::Connection;

use crate::files;
use crate::http::{ApiError, ApiResult, blocking, ok_ordered, ok_value};
use crate::lax::{Fields, Params, QueryParams, parse_json_body};
use crate::sse;
use crate::state::AppState;

pub use crate::route_table::Q;

pub fn qp(q: Q) -> QueryParams {
    QueryParams(q.0, q.1)
}

use musaic_services::library_ops::{self, NO_LIBRARY};

/// `str(Path(s))` ([`musaic_services::library_ops::py_path_str`]).
pub use musaic_services::library_ops::py_path_str;

/// `cache.drop_snapshot(library_dir)`
/// ([`musaic_services::library_ops::drop_snapshot`]).
pub fn drop_snapshot(conn: &mut Connection, library_dir: &str) -> Result<(), ApiError> {
    Ok(library_ops::drop_snapshot(conn, library_dir)?)
}

/// `services.albums.sort_albums` ([`musaic_services::library_ops::sort_albums`]).
pub use musaic_services::library_ops::sort_albums;

/// `services.albums.resolve_album_dict` with the Python app's semantics
/// ([`musaic_services::library_ops::resolve_album_dict`]) over this app's
/// configured library.
pub fn resolve_album_dict(
    state: &AppState,
    conn: &Connection,
    album_id: &str,
) -> Result<(String, String, Json), ApiError> {
    Ok(library_ops::resolve_album_dict(
        conn,
        state.library_dir().as_deref(),
        album_id,
        state.0.track_order,
    )?)
}

// ------------------------------------------------------------- config

pub async fn get_config(State(st): State<AppState>) -> ApiResult {
    let cfg = st.0.config.load();
    let exists = cfg
        .library_dir
        .as_deref()
        .is_some_and(|d| !d.is_empty() && Path::new(d).is_dir());
    ok_ordered(&Json::obj([
        ("library_dir", Json::from(cfg.library_dir)),
        ("exists", Json::Bool(exists)),
    ]))
}

pub async fn post_config(State(st): State<AppState>, body: Bytes) -> ApiResult {
    let mut f = Fields::required(parse_json_body(&body)?)?;
    let dir = f.req_str("library_dir");
    f.finish()?;
    let dir = dir.unwrap_or_default();
    let guard = st.begin_work()?;
    blocking(move || {
        let _guard = guard;
        let library_dir = library_ops::set_library_dir::<ApiError>(
            &st.0.config,
            st.0.home.as_deref(),
            &dir,
            || st.conn(),
            ApiError::internal,
        )?;
        ok_ordered(&Json::obj([
            ("library_dir", Json::from(library_dir)),
            ("exists", Json::Bool(true)),
        ]))
    })
    .await?
}

fn configured_and_present(st: &AppState) -> Result<String, ApiError> {
    Ok(library_ops::configured_library(st.library_dir())?)
}

// ------------------------------------------------------------- albums

pub async fn list_albums(State(st): State<AppState>, q: Q) -> ApiResult {
    let q = qp(q);
    let mut p = Params::default();
    let refresh = p.query_bool(&q, "refresh", false);
    p.finish()?;
    // A refresh drops the snapshot (a write); a plain list reads.
    let guard = if refresh {
        Some(st.begin_work()?)
    } else {
        None
    };
    let lib = configured_and_present(&st)?;
    blocking(move || {
        let _guard = guard;
        let mut conn = st.conn()?;
        let out = library_ops::list_albums::<ApiError>(&mut conn, &lib, refresh)?;
        ok_ordered(&out)
    })
    .await?
}

pub async fn get_album(
    State(st): State<AppState>,
    UrlPath(album_id): UrlPath<String>,
) -> ApiResult {
    let lib = st
        .library_dir()
        .filter(|l| !l.is_empty())
        .ok_or_else(|| ApiError::new(409, NO_LIBRARY))?;
    blocking(move || {
        let conn = st.conn()?;
        let album = library_ops::get_album::<ApiError>(&conn, &lib, &album_id, st.0.track_order)?;
        ok_ordered(&album)
    })
    .await?
}

pub async fn delete_album(
    State(st): State<AppState>,
    UrlPath(album_id): UrlPath<String>,
) -> ApiResult {
    let guard = st.begin_work()?;
    blocking(move || {
        let _guard = guard;
        let conn = st.conn()?;
        library_ops::delete_album::<ApiError>(
            conn,
            &st.0.bus,
            st.library_dir().as_deref(),
            &album_id,
        )?;
        ok_ordered(&Json::obj([
            ("ok", Json::Bool(true)),
            ("deleted_album_id", Json::from(album_id)),
        ]))
    })
    .await?
}

// ------------------------------------------------------------- scan

pub async fn scan_status(State(st): State<AppState>) -> ApiResult {
    ok_ordered(&st.0.scan.state().to_json())
}

pub async fn scan_stream(State(st): State<AppState>, q: Q) -> ApiResult {
    let q = qp(q);
    let mut p = Params::default();
    let force = p.query_bool(&q, "force", false);
    p.finish()?;
    // The busy 409 before the library checks. While a scan runs
    // the guard taken here is dropped at once (no restore can be running).
    let guard = st.begin_work()?;
    let lib = configured_and_present(&st)?;
    // Every subscriber replays the current scan's buffer from 0.
    // A guard not needed (a scan runs) is dropped when this handler returns.
    let (sub, _unused_guard) = st.scan_and_subscribe(&lib, force, guard);
    let (tx, resp) = sse::channel(&st, 16);
    tokio::task::spawn_blocking(move || {
        musaic_services::scan_run::forward_events(
            sub,
            |ev| tx.send_blocking(ev.sse_frame()),
            || tx.is_closed(),
        );
    });
    Ok(resp)
}

pub async fn rescan(State(st): State<AppState>) -> ApiResult {
    // The guard covers the snapshot reset as well as the scan;
    // Taken before the library check.
    let guard = st.begin_work()?;
    let lib = st
        .library_dir()
        .filter(|l| !l.is_empty())
        .ok_or_else(|| ApiError::new(409, NO_LIBRARY))?;
    blocking(move || {
        let mut conn = st.conn()?;
        drop_snapshot(&mut conn, &lib)?;
        drop(conn);
        let state = st.start_scan(&lib, true, guard);
        ok_ordered(&Json::obj([
            ("status", Json::from("scan started")),
            ("library_dir", Json::from(lib)),
            ("scan", state.to_json()),
        ]))
    })
    .await?
}

// ------------------------------------------------------------- tracks

/// `services.albums.read_tracks(album_dir)`
/// ([`musaic_services::library_ops::read_tracks`]).
pub use musaic_services::library_ops::read_tracks;

pub async fn get_tracks(
    State(st): State<AppState>,
    UrlPath(album_id): UrlPath<String>,
) -> ApiResult {
    blocking(move || {
        let conn = st.conn()?;
        let (_, path, _) = resolve_album_dict(&st, &conn, &album_id)?;
        ok_ordered(&Json::obj([
            ("album_id", Json::from(album_id)),
            ("tracks", read_tracks(&path)),
        ]))
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
    let size = q.get("size").unwrap_or("thumb").to_string();
    let inm = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    blocking(move || {
        let conn = st.conn()?;
        let (id, path, album) = resolve_album_dict(&st, &conn, &album_id)?;
        drop(conn);
        if size == "full" {
            return match covers::full_cover(&path, &album) {
                Some(FullCover::Disk(disk)) => {
                    let mime = files::guess_mime(&disk);
                    let mut r = files::file_response(&disk, &mime)?;
                    cover_headers(&mut r);
                    Ok(r)
                }
                Some(FullCover::Embedded(data, mime)) => {
                    let mut r = Response::new(Body::from(data));
                    r.headers_mut().insert(
                        header::CONTENT_TYPE,
                        HeaderValue::from_str(&mime)
                            .unwrap_or(HeaderValue::from_static("image/jpeg")),
                    );
                    cover_headers(&mut r);
                    Ok(r)
                }
                None => Err(ApiError::new(404, covers::NO_COVER)),
            };
        }
        let Some(thumb) = covers::thumb_path(&st.thumbs_dir(), &id, &path, &album) else {
            return Err(ApiError::new(404, covers::NO_COVER));
        };
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
    })
    .await?
}

// ------------------------------------------------------------- anchor

pub async fn clear_mb_anchor(
    State(st): State<AppState>,
    UrlPath(album_id): UrlPath<String>,
) -> ApiResult {
    let guard = st.begin_work()?;
    blocking(move || {
        let _guard = guard;
        let conn = st.conn()?;
        let rejected = library_ops::clear_mb_anchor::<ApiError>(
            conn,
            || st.library_dir(),
            st.0.track_order,
            &st.0.resolver,
            &st.0.bus,
            || st.now_iso(),
            &album_id,
        )?;
        ok_ordered(&Json::obj([
            ("ok", Json::Bool(true)),
            ("blocked_mbid", Json::from(rejected)),
        ]))
    })
    .await?
}

// ------------------------------------------------------------- resolution

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
    // Treated as a writing route (see the module docs).
    let guard = st.begin_work()?;
    blocking(move || {
        let _guard = guard;
        let conn = st.conn()?;
        // Tier 0c is reachable here: with an AcoustID key the parts
        // fingerprint the album (stored, TrackFingerprinted) and vote.
        let r = library_ops::album_resolution::<ApiError>(
            &conn,
            &|| st.library_dir(),
            st.0.track_order,
            &|| st.parts(),
            &album_id,
            &hint,
            force,
            ApiError::internal,
        )?;
        // The trace note names AI only when a provider is ready
        // (the memoized resolution keeps the Python app's text).
        let mut r = r;
        musaic_resolve::resolution::reword_notes(&mut r, st.ai_wording());
        ok_value(r)
    })
    .await?
}
