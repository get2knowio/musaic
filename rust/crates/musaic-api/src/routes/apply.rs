//! `musaic/api/apply.py`: the streaming apply and the cover upload.
//!
//! The pipeline runs on a blocking thread and hands each item to the SSE
//! channel (capacity 1: it never runs more than one frame ahead of the
//! client). When the client goes away the sink answers
//! `Break` and musaic-services runs a non-dry-run apply to completion anyway;
//! the HTTP layer only stops forwarding. A Missing album is refused
//! by musaic-services' `resolve_album` with 409, surfaced as is.

use std::ops::ControlFlow;

use axum::body::Bytes;
use axum::extract::multipart::MultipartRejection;
use axum::extract::{Multipart, Path as UrlPath, State};
use musaic_services::{ApplyRequest, apply_album_changes};
use serde_json::{Value, json};

use crate::gate::WorkGuard;
use crate::http::{ApiError, ApiResult, blocking};
use crate::lax::{Fields, KeyOrder, err, parse_json_body, unprocessable};
use crate::sse;
use crate::state::AppState;

fn sugg(pairs: Vec<(String, String)>) -> Vec<(String, Option<String>)> {
    pairs.into_iter().map(|(k, v)| (k, Some(v))).collect()
}

/// `guard` is held until the pipeline returns, which is after the
/// client has gone away when it does, or released by unwinding.
fn stream_apply(
    st: AppState,
    req: ApplyRequest,
    guard: Option<WorkGuard>,
) -> axum::response::Response {
    let (tx, resp) = sse::channel(&st, 1);
    let ctx = musaic_events::context::current();
    tokio::task::spawn_blocking(move || {
        let _guard = guard;
        musaic_events::context::run_in(ctx, || {
            let mut conn = match st.conn() {
                Ok(c) => c,
                Err(_) => {
                    let _ = tx.send_blocking(sse::frame(
                        "error",
                        &json!({"detail": "database unavailable"}).to_string(),
                    ));
                    return;
                }
            };
            st.with_apply_env(&mut conn, |env| {
                let mut forwarding = true;
                apply_album_changes(env, &req, &mut |item| {
                    if forwarding && !tx.send_blocking(item.sse_frame()) {
                        forwarding = false;
                    }
                    if forwarding {
                        ControlFlow::Continue(())
                    } else {
                        ControlFlow::Break(())
                    }
                });
            });
        })
    });
    resp
}

async fn resolve(st: &AppState, album_id: String) -> Result<(String, String), ApiError> {
    let st = st.clone();
    blocking(move || {
        let conn = st.conn()?;
        let lib = st.library_dir();
        let a = musaic_services::resolve_album(&conn, lib.as_deref(), &album_id, st.0.track_order)?;
        Ok::<_, ApiError>((a.id, a.path))
    })
    .await?
}

pub async fn apply_stream(
    State(st): State<AppState>,
    UrlPath(album_id): UrlPath<String>,
    body: Bytes,
) -> ApiResult {
    // The suggestions keep the
    // body's key order, as the Python app's dicts do: Vorbis comments are
    // appended and `AlbumTagsChanged.fields` listed in that order.
    let mut f = Fields::required(parse_json_body(&body)?)?.with_order(KeyOrder::of_body(&body));
    let suggestions = f.str_map("suggestions");
    let track = f.str_map_map("track_suggestions");
    let cover_url = f.opt_str("cover_url");
    let dry_run = f.bool_or("dry_run", false);
    f.finish()?;
    // An apply is refused while a restore runs, and
    // so is a dry run — it reads the database the restore swaps —
    // so it takes the gate like a real apply, which also keeps a restore
    // from starting under it (409 for the restore instead).
    let guard = Some(st.begin_work_as(crate::gate::WorkKind::Apply)?);
    let (id, path) = resolve(&st, album_id).await?;
    let req = ApplyRequest {
        album_id: id,
        album_path: path,
        library_dir: Some(st.library_dir().unwrap_or_default()),
        suggestions: sugg(suggestions),
        track_suggestions: track.into_iter().map(|(k, v)| (k, sugg(v))).collect(),
        cover_url,
        cover_bytes: None,
        cover_mime: None,
        dry_run,
    };
    Ok(stream_apply(st, req, guard))
}

pub async fn upload_cover(
    State(st): State<AppState>,
    UrlPath(album_id): UrlPath<String>,
    mp: Result<Multipart, MultipartRejection>,
) -> ApiResult {
    let missing = || {
        unprocessable(vec![err(
            "missing",
            vec![json!("body"), json!("cover")],
            "Field required",
            Value::Null,
        )])
    };
    // The body is counted as it is read (every part, not only
    // the cover) and reading stops past the cap. Not a `DefaultBodyLimit`:
    // that refuses a too-long Content-Length before the part headers are
    // read, and the Python app checks the part's MIME type (400) before its
    // size (413).
    let limit = st.0.cover_body_limit;
    let mut mp = mp.map_err(|_| missing())?;
    let mut read: usize = 0;
    let mut got: Option<(Option<String>, Vec<u8>)> = None;
    while let Some(mut field) = mp.next_field().await.map_err(|_| missing())? {
        let is_cover = field.name() == Some("cover") && field.file_name().is_some();
        let ct = field.content_type().map(str::to_string);
        let mut bytes = Vec::new();
        while let Some(c) = field.chunk().await.map_err(ApiError::internal)? {
            read += c.len();
            if read > limit {
                return Err(body_too_large(is_cover.then_some(ct.as_deref()), limit));
            }
            if is_cover {
                bytes.extend_from_slice(&c);
            }
        }
        if is_cover {
            got = Some((ct, bytes));
            break;
        }
    }
    let (ct, bytes) = got.ok_or_else(missing)?;
    // After FastAPI's own validation (the 422 for a
    // missing part) and the transport cap (413), before the handler's
    // checks (the image type's 400) and the album lookup.
    let guard = st.begin_work()?;
    let mime = musaic_services::validate_cover_upload(ct.as_deref(), &bytes)?;
    let (id, path) = resolve(&st, album_id).await?;
    let req = ApplyRequest {
        album_id: id,
        album_path: path,
        library_dir: Some(st.library_dir().unwrap_or_default()),
        cover_bytes: Some(bytes),
        cover_mime: Some(mime),
        ..Default::default()
    };
    Ok(stream_apply(st, req, Some(guard)))
}

/// The parts read went over the cover cap
/// ([`crate::COVER_BODY_LIMIT`] unless a test set another). The Python app
/// checks the MIME type before the size, so a non-image part still gets
/// its 400; otherwise a 413 in the Python app's shape (`{"detail": ...}`, same
/// wording as its `Cover too large` answer; the byte count is the cap, as
/// the body was not read past it).
fn body_too_large(content_type: Option<Option<&str>>, limit: usize) -> ApiError {
    if let Some(ct) = content_type
        && let Err(e) = musaic_services::validate_cover_upload(ct, b"\0")
        && e.status == 400
    {
        return e.into();
    }
    ApiError::new(
        413,
        format!(
            "Cover too large (more than {limit} bytes in the request). Max accepted is {} MB.",
            musaic_services::apply::MAX_COVER_UPLOAD_BYTES / (1024 * 1024)
        ),
    )
}
