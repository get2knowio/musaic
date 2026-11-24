//! The check fix actions of `musaic/api/health.py`, over `ops().fixes()`:
//!
//! | route | |
//! |---|---|
//! | `POST /api/checks/{check_id}/findings/{finding_id}/fix` | [`fix_finding`] |
//! | `POST /api/checks/{check_id}/quick-fix-all` (SSE) | [`quick_fix_all`] |
//! | `POST .../filename-mismatch/findings/{id}/apply` | [`apply_filename_rename`] |
//! | `POST .../duplicate-tracks/findings/{id}/delete-file` | [`delete_duplicate_file`] |
//! | `POST .../inconsistent-artist-sort/findings/{id}/apply-sort` | [`apply_cluster_sort`] |
//! | `POST .../multi-disc-layout/findings/{id}/convert` | [`convert_multi_disc`] (not a route of the Python app) |
//!
//! Each handler parses the request (422) and calls one operation of
//! `ops().fixes()`, which owns the restore gate (every fix writes, so a restore
//! running answers 409 before the finding or check is looked up, and the guard
//! is held until the work ends: for quick-fix-all, the whole stream, also after
//! the client has gone), the research request a fix falls through to
//! (`{ok: true, message, job_id}`, the request's card; an album that already
//! has an unresolved request of that kind answers with that one) and the jobs
//! feed's wake-up after it. The bodies and frames are `render::fixes`'.

use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State};
use musaic_app::ops::Listening;
use musaic_app::ops::OpError;
use musaic_app::ops::fixes::{ConvertEvent, PlanEntry};
use serde_json::{Map, Value, json};
use tokio::sync::oneshot;

use crate::http::{ApiError, ApiResult, blocking, json_value, ok_ordered, ok_value};
use crate::lax::{Fields, Params, err, parse_json_body, unprocessable};
use crate::render::fixes as render;
use crate::routes::library::{Q, qp};
use crate::sse;
use crate::state::AppState;

fn finding_id(raw: &str) -> Result<i64, ApiError> {
    let mut p = Params::default();
    let id = p.path_int(raw, "finding_id");
    p.finish()?;
    Ok(id)
}

// ------------------------------------------------------------- fix

/// `POST /api/checks/{check_id}/findings/{finding_id}/fix` with the
/// optional `{"hint": str = ""}` body.
pub async fn fix_finding(
    State(st): State<AppState>,
    UrlPath((check_id, raw_id)): UrlPath<(String, String)>,
    body: Bytes,
) -> ApiResult {
    let id = finding_id(&raw_id)?;
    let hint = match Fields::optional(parse_json_body(&body)?)? {
        Some(mut f) => {
            let h = f.str_or("hint", "");
            f.finish()?;
            h
        }
        None => String::new(),
    };
    let out = blocking(move || st.ops().fixes().fix(&check_id, id, &hint)).await??;
    ok_ordered(&render::fix_body(&out))
}

// ------------------------------------------------------------- quick-fix-all

/// `POST /api/checks/{check_id}/quick-fix-all` (SSE): 409 during a restore,
/// 404 for an unknown check, both before anything streams; else
/// `start`, one `progress` per open finding, `done`. Deliberately no
/// pause between findings (the Python app slept 1 s); the MusicBrainz limiter
/// paces the requests. The builder's `quick_fix_pause` hook (default:
/// nothing) remains only as a test seam. A client that goes away stops the
/// forwarding at the next frame; the finding in hand completes and
/// `BulkBatchCompleted` is still published (the Python app's `finally`).
///
/// The operation refuses before its first event; that is answered as a
/// status. A database failure before the first event (the connection cannot
/// be opened, the findings cannot be listed) is the stream's `error` item, as
/// the Python app's `except Exception` frame was.
///
/// `?research=1` (or `true`) sends what the resolver cannot settle for
/// research, one batch per run; the `done` frame then carries the research
/// counts. Without it the frames are the resolver-only run's.
pub async fn quick_fix_all(
    State(st): State<AppState>,
    UrlPath(check_id): UrlPath<String>,
    q: Q,
) -> ApiResult {
    let research = matches!(qp(q).get("research"), Some("1" | "true"));
    let (tx, resp) = sse::channel(&st, 1);
    // Answers once: `Ok` when the first event is out (or the stream is the
    // answer to a failure), `Err` when the operation refused before any.
    let (ready_tx, ready_rx) = oneshot::channel::<Result<(), ApiError>>();
    let ctx = musaic_events::context::current();
    tokio::task::spawn_blocking(move || {
        musaic_events::context::run_in(ctx, || {
            let mut ready = Some(ready_tx);
            let result = st
                .ops()
                .fixes()
                .quick_fix_all(&check_id, research, &mut |event| {
                    if let Some(r) = ready.take() {
                        let _ = r.send(Ok(()));
                    }
                    if tx.send_blocking(render::quick_fix_frame(&event)) {
                        Listening::Yes
                    } else {
                        Listening::Gone
                    }
                });
            if let Some(r) = ready.take() {
                match result {
                    Ok(_) => {
                        let _ = r.send(Ok(()));
                    }
                    Err(e @ OpError::Internal { .. }) => {
                        let _ = r.send(Ok(()));
                        let _ = tx.send_blocking(render::quick_fix_failed_frame(e));
                    }
                    Err(e) => {
                        let _ = r.send(Err(ApiError::from(e)));
                    }
                }
            }
        })
    });
    match ready_rx.await {
        Ok(Ok(())) => Ok(resp),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(ApiError::internal("the bulk fix ended without an answer")),
    }
}

// ------------------------------------------------------------- filename-mismatch

/// One `{old: str, new: str}` model at `loc`.
fn entry(v: &Value, loc: Vec<Value>, errors: &mut Vec<Value>) -> Option<PlanEntry> {
    let Value::Object(m) = v else {
        errors.push(err(
            "model_attributes_type",
            loc,
            "Input should be a valid dictionary or object to extract fields from",
            v.clone(),
        ));
        return None;
    };
    let mut field = |k: &str| -> Option<String> {
        let mut l = loc.clone();
        l.push(json!(k));
        match m.get(k) {
            None => {
                errors.push(err("missing", l, "Field required", v.clone()));
                None
            }
            Some(Value::String(s)) => Some(s.clone()),
            Some(other) => {
                errors.push(err(
                    "string_type",
                    l,
                    "Input should be a valid string",
                    other.clone(),
                ));
                None
            }
        }
    };
    let old = field("old");
    let new = field("new");
    Some(PlanEntry {
        old: old?,
        new: new?,
    })
}

/// `FilenameRenamePayload`: `plan: list[FilenameRenameEntry]`,
/// `dir_rename: DirRenameEntry | None = None`.
fn rename_payload(body: &Bytes) -> Result<(Vec<PlanEntry>, Option<PlanEntry>), ApiError> {
    let f = Fields::required(parse_json_body(body)?)?;
    f.finish()?;
    let v = parse_json_body(body)?.unwrap_or(Value::Null);
    let m: &Map<String, Value> = v.as_object().expect("checked by Fields::required");
    let mut errors = Vec::new();
    let mut plan = Vec::new();
    match m.get("plan") {
        None => errors.push(err(
            "missing",
            vec![json!("body"), json!("plan")],
            "Field required",
            v.clone(),
        )),
        Some(Value::Array(items)) => {
            for (i, x) in items.iter().enumerate() {
                if let Some(e) = entry(x, vec![json!("body"), json!("plan"), json!(i)], &mut errors)
                {
                    plan.push(e);
                }
            }
        }
        Some(other) => errors.push(err(
            "list_type",
            vec![json!("body"), json!("plan")],
            "Input should be a valid list",
            other.clone(),
        )),
    }
    let dir_rename = match m.get("dir_rename") {
        None | Some(Value::Null) => None,
        Some(x) => entry(x, vec![json!("body"), json!("dir_rename")], &mut errors),
    };
    if !errors.is_empty() {
        return Err(unprocessable(errors));
    }
    Ok((plan, dir_rename))
}

pub async fn apply_filename_rename(
    State(st): State<AppState>,
    UrlPath(raw_id): UrlPath<String>,
    body: Bytes,
) -> ApiResult {
    let id = finding_id(&raw_id)?;
    let (plan, dir_rename) = rename_payload(&body)?;
    let r = blocking(move || {
        st.ops()
            .fixes()
            .apply_filename_rename(id, &plan, dir_rename.as_ref())
    })
    .await?;
    match r {
        Ok(o) => ok_ordered(&render::rename_body(&o)),
        Err(e) => render::rename_refusal(e),
    }
}

// ------------------------------------------------------------- duplicate-tracks

pub async fn delete_duplicate_file(
    State(st): State<AppState>,
    UrlPath(raw_id): UrlPath<String>,
    body: Bytes,
) -> ApiResult {
    let id = finding_id(&raw_id)?;
    let mut f = Fields::required(parse_json_body(&body)?)?;
    let filename = f.req_str("filename");
    f.finish()?;
    let filename = filename.unwrap_or_default();
    let r = blocking(move || st.ops().fixes().delete_duplicate_file(id, &filename)).await??;
    ok_ordered(&render::delete_body(&r))
}

// ------------------------------------------------------------- inconsistent-artist-sort

pub async fn apply_cluster_sort(
    State(st): State<AppState>,
    UrlPath(raw_id): UrlPath<String>,
    body: Bytes,
) -> ApiResult {
    let id = finding_id(&raw_id)?;
    let mut f = Fields::required(parse_json_body(&body)?)?;
    let sort_value = f.req_str("sort_value");
    f.finish()?;
    let sort_value = sort_value.unwrap_or_default();
    let r = blocking(move || st.ops().fixes().apply_cluster_sort(id, &sort_value)).await??;
    ok_ordered(&render::sort_body(&r))
}

// ------------------------------------------------------------- multi-disc-layout

/// `POST /api/checks/multi-disc-layout/findings/{finding_id}/convert` with
/// `{"digest": str}` (the preview's): convert a set kept in one folder or
/// in one folder per disc. The plan is re-derived from disk; a refusal
/// changed nothing and answers its status with `detail`, `reason_code`,
/// `user_message` (and `blockers` or the fresh `digest`). A conversion that
/// ran answers 200, also when a step failed (`ok: false`, a reason per file).
///
/// With `Accept: text/event-stream` the answer is a stream instead, as for
/// the apply and the bulk fix (a set of many discs on a network share takes
/// minutes): `progress {step, done, total}` while it runs, then exactly one
/// `result` (the JSON body above) or `refused` (`status` plus that body). A
/// client that goes away stops only the forwarding; the conversion runs to
/// its end. A restore running is a 409 status either way, before anything
/// streams.
pub async fn convert_multi_disc(
    State(st): State<AppState>,
    UrlPath(raw_id): UrlPath<String>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> ApiResult {
    let id = finding_id(&raw_id)?;
    let mut f = Fields::required(parse_json_body(&body)?)?;
    let digest = f.req_str("digest");
    f.finish()?;
    let digest = digest.unwrap_or_default();
    let streams = headers
        .get(axum::http::header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("text/event-stream"));
    if streams {
        let (tx, resp) = sse::channel(&st, 64);
        // Answers once: `Ok` when the conversion has begun (the guard is held),
        // `Err` when a restore refused it.
        let (ready_tx, ready_rx) = oneshot::channel::<Result<(), ApiError>>();
        let ctx = musaic_events::context::current();
        tokio::task::spawn_blocking(move || {
            musaic_events::context::run_in(ctx, || {
                let mut ready = Some(ready_tx);
                let result = st.ops().fixes().convert_multi_disc(
                    id,
                    &digest,
                    Some(&mut |event| match event {
                        ConvertEvent::Started => {
                            if let Some(r) = ready.take() {
                                let _ = r.send(Ok(()));
                            }
                            Listening::Yes
                        }
                        // Droppable: a slow client never holds the conversion up.
                        ConvertEvent::Progress(p) => {
                            let _ = tx.try_send_frame(render::convert_progress_frame(&p));
                            if tx.is_closed() {
                                Listening::Gone
                            } else {
                                Listening::Yes
                            }
                        }
                    }),
                );
                if let Some(r) = ready.take() {
                    // Refused before it began: a restore is running.
                    if let Err(e) = result {
                        let _ = r.send(Err(ApiError::from(e)));
                        return;
                    }
                    let _ = r.send(Ok(()));
                }
                let frame = match result {
                    Ok(rec) => sse::frame("result", &rec.document().to_string()),
                    // The database could not be opened.
                    Err(OpError::Internal { .. }) => sse::frame(
                        "refused",
                        &json!({"status": 500, "detail": "database unavailable", "user_message": "The library database could not be opened, so nothing was converted."}).to_string(),
                    ),
                    Err(e) => {
                        let (status, mut b) = render::refusal(&e);
                        b["status"] = json!(status);
                        sse::frame("refused", &b.to_string())
                    }
                };
                let _ = tx.send_blocking(frame);
            })
        });
        return match ready_rx.await {
            Ok(Ok(())) => Ok(resp),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(ApiError::internal("the conversion ended without an answer")),
        };
    }
    blocking(
        move || match st.ops().fixes().convert_multi_disc(id, &digest, None) {
            Ok(rec) => ok_value(rec.document().clone()),
            Err(e) => refused_response(e),
        },
    )
    .await?
}

/// A conversion or abandon refusal as its JSON answer: the status and the body
/// with its reason. A restore running, or a database that cannot be opened, is
/// the usual `HTTPException` / plain-text 500.
fn refused_response(e: OpError) -> ApiResult {
    match e {
        OpError::Busy { .. } | OpError::Internal { .. } => Err(e.into()),
        e => {
            let (status, body) = render::refusal(&e);
            Ok(json_value(status, &body))
        }
    }
}

/// `POST /api/checks/multi-disc-layout/findings/{finding_id}/abandon-conversion`:
/// stop treating the album's unfinished conversion as one. Nothing on disk
/// moves; the record is closed as abandoned and says where every file is.
pub async fn abandon_multi_disc_conversion(
    State(st): State<AppState>,
    UrlPath(raw_id): UrlPath<String>,
) -> ApiResult {
    let id = finding_id(&raw_id)?;
    blocking(move || match st.ops().fixes().abandon_multi_disc(id) {
        Ok(rec) => ok_value(rec.document().clone()),
        Err(e) => refused_response(e),
    })
    .await?
}
