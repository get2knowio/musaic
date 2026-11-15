//! `musaic/api/analyze.py`: `POST /api/albums/{album_id}/analyze/stream`,
//! over `musaic_services::analyze`.
//!
//! Order, as every writing route: the body's 422 (`what` must be
//! `"bpm"` or `"replaygain"`, the Python app's `Literal`), then the restore gate
//! (409 while a restore runs), then the album lookup
//! (`resolve_album`: 409 without a library, 404 for an unknown album, and
//! whatever else the service refuses, e.g. a Missing album), then
//! the SSE stream.
//!
//! The [`crate::WorkGuard`] moves into the blocking task that runs the
//! analysis, so it is held for the whole run, also after the client has gone
//! away. Like an apply, an analysis runs to completion: the
//! sink never asks the service to stop; once the client is gone it only stops
//! forwarding frames. An [`AnalyzeOutcome::Aborted`] run (the album directory
//! cannot be listed: the Python app raised inside the stream after the 200) ends
//! the body with no final frame; the reason is logged.

use std::ops::ControlFlow;

use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State};
use musaic_services::analyze::{
    AnalysisKind, AnalyzeOutcome, AnalyzeRequest, analyze_album, resolve_album,
};

use crate::http::{ApiResult, blocking};
use crate::lax::{Fields, parse_json_body};
use crate::routes::tags::svc;
use crate::sse;
use crate::state::AppState;

pub async fn analyze_stream(
    State(st): State<AppState>,
    UrlPath(album_id): UrlPath<String>,
    body: Bytes,
) -> ApiResult {
    let mut f = Fields::required(parse_json_body(&body)?)?;
    let what = f.req_literal("what", &["bpm", "replaygain"]);
    f.finish()?;
    let what = what
        .as_deref()
        .and_then(AnalysisKind::parse)
        .expect("validated by req_literal");
    let guard = st.begin_work_as(crate::gate::WorkKind::Analyze)?;
    let st2 = st.clone();
    let album = blocking(move || {
        let conn = st2.conn()?;
        let lib = st2.library_dir();
        resolve_album(&conn, lib.as_deref(), &album_id, st2.0.track_order).map_err(svc)
    })
    .await??;
    let req = AnalyzeRequest {
        album_id: album.id,
        album_path: album.path,
        library_dir: Some(st.library_dir().unwrap_or_default()),
        what,
    };
    let (tx, resp) = sse::channel(&st, 1);
    let ctx = musaic_events::context::current();
    tokio::task::spawn_blocking(move || {
        let _guard = guard;
        musaic_events::context::run_in(ctx, || {
            let mut conn = match st.conn() {
                Ok(c) => c,
                Err(_) => return,
            };
            let outcome = st.with_analyze_env(&mut conn, |env| {
                let mut forwarding = true;
                analyze_album(env, &req, &mut |item| {
                    if forwarding && !tx.send_blocking(item.sse_frame()) {
                        forwarding = false;
                    }
                    ControlFlow::Continue(())
                })
            });
            if let AnalyzeOutcome::Aborted(reason) = outcome {
                st.0.log.warn(
                    "musaic.api.analyze",
                    &format!("analyze {} aborted: {reason}", req.album_id),
                );
            }
        })
    });
    Ok(resp)
}
