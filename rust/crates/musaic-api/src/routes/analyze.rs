//! `musaic/api/analyze.py`: `POST /api/albums/{album_id}/analyze/stream`, over
//! `ops().analyze()`.
//!
//! The handler answers the body's 422 (`what` must be `"bpm"` or
//! `"replaygain"`, the Python app's `Literal`), then runs the operation through
//! [`sse::run_stream`]: on a blocking thread, with a sink that renders each event
//! and sends it down the stream. The operation owns the restore gate (409 while a restore runs), the
//! album lookup (409 without a library, 404 for an unknown album, 409 for a
//! Missing album or an unreachable folder) and the order of the three: a refusal
//! comes back before the first event and is answered as a status, not a stream.
//! Once the first event is out, the answer is the stream.
//!
//! The run goes to completion whatever the client does: a send that fails
//! (the client is gone) answers `Listening::Gone` and the operation stops
//! calling, but still finishes every file. A run that aborts (the album folder
//! vanished mid-run) ends the body with no final frame; the operation logs it.

use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State};
use musaic_app::ops::analyze::AnalysisKind;

use crate::http::ApiResult;
use crate::lax::{Fields, parse_json_body};
use crate::render::analyze as render;
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
    sse::run_stream(
        &st,
        render::frame,
        move |ops| ops.analyze().prepare(&album_id, what),
        |run, sink| run.run(sink),
    )
    .await
}
