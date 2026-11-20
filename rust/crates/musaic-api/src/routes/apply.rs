//! `musaic/api/apply.py`: the streaming apply and the cover upload, over
//! `ops().apply()`.
//!
//! Each handler parses its request (the body's 422, the multipart reading and the
//! upload cap, which are HTTP's), then runs the operation through
//! [`sse::run_stream`]: on a blocking thread, with a sink that renders each event
//! and sends it down the stream (capacity 1: it never runs more than one frame
//! ahead of the client). The operation owns the
//! restore gate, the album lookup (409 without a library, 404, 409 for a Missing
//! album) and the order of the three: a refusal comes back before the first event
//! and is answered as a status, not a stream. Once the first event is out, the
//! answer is the stream.
//!
//! When the client goes away the sink answers `Gone` and the operation stops
//! calling it; a non-dry-run apply still runs to completion.
//!
//! Removing a tag (not in the Python app): `clear` lists album-level
//! fields and `track_clear` maps a filename to per-track fields to remove
//! from the files. An empty or null suggestion still means "no change", so
//! only these explicit lists remove anything. Refused with 422 before
//! anything runs: an unknown field name, `tracknumber` (the scanner orders
//! tracks by it; set a number instead) and a field that is both suggested
//! and cleared for the same target ([`ApplyChanges::removal_problems`]).

use axum::body::Bytes;
use axum::extract::multipart::MultipartRejection;
use axum::extract::{Multipart, Path as UrlPath, State};
use musaic_app::ops::apply::{
    ApplyChanges, MAX_COVER_UPLOAD_BYTES, RemovalFault, RemovalProblem, RemovalTarget,
    check_cover_type,
};
use serde_json::{Value, json};

use crate::http::{ApiError, ApiResult};
use crate::lax::{Fields, KeyOrder, err, literal_err, parse_json_body, unprocessable};
use crate::render::apply as render;
use crate::sse;
use crate::state::AppState;

fn value_error(loc: Vec<Value>, msg: &str, input: Value) -> Value {
    err("value_error", loc, &format!("Value error, {msg}"), input)
}

/// The 422 entry for one removal that cannot be honoured, in pydantic's shape.
fn removal_error(p: &RemovalProblem) -> Value {
    let loc = match &p.target {
        RemovalTarget::Album { index } => vec![json!("body"), json!("clear"), json!(index)],
        RemovalTarget::Track { filename, index } => vec![
            json!("body"),
            json!("track_clear"),
            json!(filename),
            json!(index),
        ],
    };
    let input = json!(p.field);
    match &p.fault {
        RemovalFault::Unknown { allowed } => {
            let allowed: Vec<&str> = allowed.iter().map(String::as_str).collect();
            literal_err(loc, input, &allowed)
        }
        RemovalFault::SetOnly | RemovalFault::AlsoSuggested => {
            value_error(loc, &p.message(), input)
        }
    }
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
    let changes = ApplyChanges {
        suggestions: f.str_map("suggestions"),
        track_suggestions: f.str_map_map("track_suggestions"),
        clear: f.opt_str_list("clear").unwrap_or_default(),
        track_clear: f.str_list_map("track_clear"),
        cover_url: f.opt_str("cover_url"),
        dry_run: f.bool_or("dry_run", false),
    };
    f.finish()?;
    let problems = changes.removal_problems();
    if !problems.is_empty() {
        return Err(unprocessable(problems.iter().map(removal_error).collect()));
    }
    sse::run_stream(
        &st,
        render::frame,
        move |ops| ops.apply().prepare_changes(&album_id, &changes),
        |run, sink| run.run(sink),
    )
    .await
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
    let limit = st.cover_body_limit();
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
    // missing part) and the transport cap (413), before the operation's
    // checks (the guard, the image type's 400) and the album lookup.
    sse::run_stream(
        &st,
        render::frame,
        move |ops| ops.apply().prepare_upload(&album_id, ct.as_deref(), bytes),
        |run, sink| run.run(sink),
    )
    .await
}

/// The parts read went over the cover cap
/// ([`crate::COVER_BODY_LIMIT`] unless a test set another). The Python app
/// checks the MIME type before the size, so a non-image part still gets
/// its 400; otherwise a 413 in the Python app's shape (`{"detail": ...}`, same
/// wording as its `Cover too large` answer; the byte count is the cap, as
/// the body was not read past it).
fn body_too_large(content_type: Option<Option<&str>>, limit: usize) -> ApiError {
    if let Some(ct) = content_type
        && let Err(e) = check_cover_type(ct)
    {
        return e.into();
    }
    ApiError::new(
        413,
        format!(
            "Cover too large (more than {limit} bytes in the request). Max accepted is {} MB.",
            MAX_COVER_UPLOAD_BYTES / (1024 * 1024)
        ),
    )
}
