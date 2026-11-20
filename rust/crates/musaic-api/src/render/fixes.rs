//! The `fixes` group as bodies and frames: each typed result in the keys and
//! order the Python app answered, the quick-fix-all stream's frames, the
//! conversion's progress frames and the refusal body a conversion carries.

use musaic_app::ops::fixes::{
    ConvertProgress, DuplicateDeleted, FixResult, QuickFixEvent, RenameOutcome, SortResult,
};
use musaic_services::Json;
use musaic_services::error::{Details, OpError};
use serde_json::{Map, Value, json};

use crate::http::{ApiError, ApiResult, json_ordered};
use crate::sse;

/// `{ok, message, job_id}`.
pub fn fix_body(r: &FixResult) -> Json {
    Json::obj([
        ("ok", Json::Bool(r.ok)),
        ("message", Json::from(r.message.as_str())),
        ("job_id", r.job_id.clone().map_or(Json::Null, Json::Str)),
    ])
}

/// `execute_renames`' dict, as the route returns it.
pub fn rename_body(o: &RenameOutcome) -> Json {
    let mut pairs: Vec<(String, Json)> = vec![
        ("ok".into(), Json::Bool(o.ok)),
        ("message".into(), Json::from(o.message.as_str())),
        ("renamed".into(), Json::from(o.renamed)),
    ];
    if o.already_renamed {
        pairs.push(("already_renamed".into(), Json::Bool(true)));
    }
    // Only when there is something to say, so the body stays the Python
    // app's otherwise.
    if !o.skipped.is_empty() {
        pairs.push((
            "skipped".into(),
            Json::Arr(
                o.skipped
                    .iter()
                    .map(|e| {
                        Json::obj([
                            ("old", Json::from(e.old.as_str())),
                            ("new", Json::from(e.new.as_str())),
                            ("reason", Json::from(e.reason.as_str())),
                        ])
                    })
                    .collect(),
            ),
        ));
    }
    // Only when the rename needs a note, so a clean result keeps its shape:
    // a stable code and the sentence to show the user (`message` is the
    // technical text).
    if let (Some(r), Some(u)) = (o.reason, &o.user_message) {
        pairs.push(("reason_code".into(), Json::from(r.code())));
        pairs.push(("user_message".into(), Json::from(u.as_str())));
    }
    if let Some(d) = &o.detail {
        pairs.push((
            "errors".into(),
            Json::Arr(d.errors.iter().map(|e| Json::from(e.as_str())).collect()),
        ));
        pairs.push(("dir_renamed".into(), Json::Bool(d.dir_renamed)));
        pairs.push(("new_album_id".into(), Json::from(d.new_album_id.as_str())));
        pairs.push((
            "dir_migration".into(),
            match d.dir_migration {
                Some(m) => Json::obj([
                    ("albums", Json::from(m.albums)),
                    ("track_fingerprints", Json::from(m.track_fingerprints)),
                    ("track_tags", Json::from(m.track_tags)),
                    ("check_findings", Json::from(m.check_findings)),
                    ("ai_jobs", Json::from(m.ai_jobs)),
                    (
                        "album_blocked_mb_release_groups",
                        Json::from(m.album_blocked_mb_release_groups),
                    ),
                ]),
                None => Json::Obj(Vec::new()),
            },
        ));
    }
    Json::Obj(pairs)
}

/// `{ok, message, trashed_to}`.
pub fn delete_body(d: &DuplicateDeleted) -> Json {
    Json::obj([
        ("ok", Json::Bool(true)),
        ("message", Json::from(d.message.as_str())),
        (
            "trashed_to",
            d.trashed_to.clone().map_or(Json::Null, Json::Str),
        ),
    ])
}

/// `{ok, applied, cluster_size, distinct_albums, sort_value, message}`.
pub fn sort_body(r: &SortResult) -> Json {
    Json::obj([
        ("ok", Json::Bool(r.ok)),
        ("applied", Json::from(r.applied)),
        ("cluster_size", Json::from(r.cluster_size)),
        ("distinct_albums", Json::from(r.distinct_albums)),
        ("sort_value", Json::from(r.sort_value.as_str())),
        ("message", Json::from(r.message.as_str())),
    ])
}

/// The SSE event name of `e`.
pub fn quick_fix_event_name(e: &QuickFixEvent) -> &'static str {
    match e {
        QuickFixEvent::Start { .. } => "start",
        QuickFixEvent::Progress { .. } => "progress",
        QuickFixEvent::Done(_) => "done",
        QuickFixEvent::Error { .. } => "error",
    }
}

/// The item's JSON payload.
pub fn quick_fix_payload(e: &QuickFixEvent) -> Json {
    match e {
        QuickFixEvent::Start { total } => Json::obj([("total", Json::from(*total))]),
        QuickFixEvent::Progress {
            finding_id,
            outcome,
            message,
        } => Json::obj([
            ("finding_id", Json::Int(*finding_id)),
            ("outcome", Json::from(outcome.as_str())),
            ("message", Json::from(message.as_str())),
        ]),
        QuickFixEvent::Done(s) => Json::obj([
            ("total", Json::from(s.total)),
            ("fixed", Json::from(s.fixed)),
            ("skipped", Json::from(s.skipped)),
            ("errors", Json::from(s.errors)),
            ("not_applicable", Json::from(s.not_applicable)),
            ("still_open", Json::from(s.still_open)),
        ]),
        QuickFixEvent::Error { detail } => Json::obj([("detail", Json::from(detail.as_str()))]),
    }
}

/// `services.sse.sse_event(event, data)` for one quick-fix-all event.
pub fn quick_fix_frame(e: &QuickFixEvent) -> String {
    format!(
        "event: {}\ndata: {}\n\n",
        quick_fix_event_name(e),
        quick_fix_payload(e).dumps()
    )
}

/// The `error` item of a quick-fix-all run that failed before its first event:
/// what the stream said when the loop failed, `Internal Server Error` for the
/// database being unavailable.
pub fn quick_fix_failed_frame(e: OpError) -> String {
    let detail = ApiError::from(e)
        .detail
        .as_str()
        .unwrap_or("Internal Server Error")
        .to_string();
    quick_fix_frame(&QuickFixEvent::Error { detail })
}

/// The conversion's `progress` item: `{done, step, total}`.
pub fn convert_progress_frame(p: &ConvertProgress) -> String {
    sse::frame(
        "progress",
        &json!({"step": p.step, "done": p.done, "total": p.total}).to_string(),
    )
}

/// The status and body of a conversion (or abandon) refusal: `detail` and the
/// keys its details carry (`reason_code`, `user_message`, and `blockers` or the
/// fresh `digest`), a 404 included. A `NotFound` without details (a plain
/// lookup before the conversion code runs) carries the code its subject names.
pub fn refusal(e: &OpError) -> (u16, Value) {
    let mut m = Map::new();
    m.insert("detail".into(), json!(e.message()));
    match e.details() {
        Some(Details::Refusal(r)) => {
            m.insert("reason_code".into(), json!(r.reason_code));
            m.insert("user_message".into(), json!(r.user_message));
            if !r.blockers.is_empty() {
                m.insert(
                    "blockers".into(),
                    Value::Array(
                        r.blockers
                            .iter()
                            .map(|b| json!({"code": b.code, "message": b.message}))
                            .collect(),
                    ),
                );
            }
            if let Some(d) = &r.digest {
                m.insert("digest".into(), json!(d));
            }
        }
        _ => {
            if let OpError::NotFound { what, message, .. } = e {
                use musaic_services::Subject;
                let code = if *what == Subject::Album {
                    "album_not_found"
                } else {
                    "not_found"
                };
                m.insert("reason_code".into(), json!(code));
                m.insert("user_message".into(), json!(message));
            }
        }
    }
    (super::status(e), Value::Object(m))
}

/// A refusal of the rename apply: `detail` as for every refusal, plus, for one
/// the rename code explains, its code and the sentence it gives, beside it.
pub fn rename_refusal(e: OpError) -> ApiResult {
    match e {
        OpError::Conflict {
            message,
            details: Some(Details::Refusal(r)),
        } => Ok(json_ordered(
            409,
            &Json::Obj(vec![
                ("detail".to_string(), Json::from(message.as_str())),
                (
                    "reason_code".to_string(),
                    Json::from(r.reason_code.as_str()),
                ),
                (
                    "user_message".to_string(),
                    Json::from(r.user_message.as_str()),
                ),
            ]),
        )),
        e => Err(e.into()),
    }
}
