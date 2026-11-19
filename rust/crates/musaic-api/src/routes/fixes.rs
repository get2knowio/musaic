//! The check fix actions of `musaic/api/health.py`, over
//! `musaic_services::check_fixes`:
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
//! Every one of them writes, so each takes the restore gate's
//! [`crate::WorkGuard`] after request validation and before any
//! check of its own (an unknown check or finding is still a 409
//! during a restore), and holds it on the blocking task until the work ends
//! (for quick-fix-all: the whole stream, also after the client has gone).
//!
//! Where a fix falls through to AI
//! ([`musaic_services::check_fixes::FixOutcome::NeedsAiJob`]) the job is
//! enqueued on the same blocking task, under the same work guard, as the
//! Python app's `jobs.enqueue(...)` inside `fix()`: `{ok: true, message, job_id}`,
//! or, when the album already has an active job, `{ok: false, message:
//! "Already has an active job (<status>)", job_id: <existing>}`. A fix that
//! wrote something first (tags-out-of-sync) answers the same way (as the
//! Python app does). The Python app's enqueue did not notify the jobs stream or
//! wake the dispatcher; the handler does, once the fix has finished.
//!
//! With no model backend (the provider is
//! `musaic_ai::Unconfigured`) [`run_fix_env`] hands the not-available
//! message to the services, which then answer `{ok: false, message,
//! job_id: null}` where a job would be queued; no job is enqueued.

use std::ops::ControlFlow;

use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State};
use musaic_fsops::rename::PlanEntry;
use musaic_services::check_fixes::{self, FixEnv};
use musaic_services::fix_flow::{self, FixFindingError};
use musaic_services::{HttpError, Json};
use serde_json::{Map, Value, json};

use crate::http::{ApiError, ApiResult, blocking, json_ordered, json_value, ok_ordered, ok_value};
use crate::lax::{Fields, Params, err, parse_json_body, unprocessable};
use crate::routes::tags::svc;
use crate::sse;
use crate::state::AppState;

fn finding_id(raw: &str) -> Result<i64, ApiError> {
    let mut p = Params::default();
    let id = p.path_int(raw, "finding_id");
    p.finish()?;
    Ok(id)
}

/// Run `f` with a [`FixEnv`] on a blocking task, holding `guard`.
async fn with_fix_env<T, F>(st: AppState, guard: crate::WorkGuard, f: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce(&mut FixEnv<'_>) -> Result<T, ApiError> + Send + 'static,
{
    blocking(move || {
        let _guard = guard;
        run_fix_env(&st, f)
    })
    .await?
}

fn run_fix_env<T>(
    st: &AppState,
    f: impl FnOnce(&mut FixEnv<'_>) -> Result<T, ApiError>,
) -> Result<T, ApiError> {
    let conn = st.conn()?;
    st.with_fix_env(conn, f)
}

fn json_ok(r: Result<Json, HttpError>) -> ApiResult {
    match r {
        Ok(j) => ok_ordered(&j),
        Err(e) => Err(svc(e)),
    }
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
    let guard = st.begin_work()?;
    let st2 = st.clone();
    let st3 = st.clone();
    let (out, enqueued) = with_fix_env(st, guard, move |env| {
        let ids = st2.0.ai.ids.clone();
        let (r, enqueued) =
            fix_flow::fix_finding(env, &check_id, id, &hint, &*ids, &|| st2.now_iso()).map_err(
                |e| match e {
                    FixFindingError::Fix(e) => svc(e),
                    FixFindingError::Job(e) => ApiError::from(e),
                },
            )?;
        Ok((r.to_json(), enqueued))
    })
    .await?;
    // A fix that enqueued a job
    // wakes the dispatcher and every jobs stream (`notify_jobs_changed()`),
    // once the job row is written. The Python app does not (the job waited for
    // the dispatcher's next 30 s wake and was invisible to the stream).
    if enqueued {
        st3.ai_jobs_changed();
    }
    ok_ordered(&out)
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
pub async fn quick_fix_all(
    State(st): State<AppState>,
    UrlPath(check_id): UrlPath<String>,
) -> ApiResult {
    let guard = st.begin_work_as(crate::gate::WorkKind::QuickFixAll)?;
    if st.0.checks_env.registry.get(&check_id).is_none() {
        return Err(ApiError::new(
            404,
            format!("Unknown check_id: {}", musaic_fsops::py_repr(&check_id)),
        ));
    }
    let (tx, resp) = sse::channel(&st, 1);
    let ctx = musaic_events::context::current();
    tokio::task::spawn_blocking(move || {
        let _guard = guard;
        musaic_events::context::run_in(ctx, || {
            let pause = st.0.quick_fix_pause.clone();
            let r = run_fix_env(&st, |env| {
                let mut forwarding = true;
                check_fixes::quick_fix_all(env, &check_id, &mut || pause(), &mut |item| {
                    if forwarding && !tx.send_blocking(item.sse_frame()) {
                        forwarding = false;
                    }
                    if forwarding {
                        ControlFlow::Continue(())
                    } else {
                        ControlFlow::Break(())
                    }
                })
                .map_err(svc)
            });
            if let Err(e) = r {
                // `except Exception as e: yield sse_event("error", ...)`.
                let detail = e.detail.as_str().unwrap_or("Internal Server Error");
                let _ = tx.send_blocking(sse::frame(
                    "error",
                    &Json::obj([("detail", Json::from(detail))]).dumps(),
                ));
            }
        })
    });
    Ok(resp)
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
    let guard = st.begin_work()?;
    let r = with_fix_env(st, guard, move |env| {
        Ok(check_fixes::apply_filename_rename(
            env,
            id,
            &plan,
            dir_rename.as_ref(),
        ))
    })
    .await?;
    match r {
        Ok(j) => ok_ordered(&j),
        // `detail` as for every refusal, plus the code and the sentence the
        // rename code gives for it.
        Err(e) if !e.extra.is_empty() => {
            let mut pairs = vec![("detail".to_string(), Json::from(e.error.detail.as_str()))];
            pairs.extend(
                e.extra
                    .iter()
                    .map(|(k, v)| (k.clone(), Json::from(v.as_str()))),
            );
            Ok(json_ordered(e.error.status, &Json::Obj(pairs)))
        }
        Err(e) => Err(svc(e.error)),
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
    let guard = st.begin_work()?;
    let r = with_fix_env(st, guard, move |env| {
        Ok(check_fixes::delete_duplicate_file(env, id, &filename))
    })
    .await?;
    json_ok(r)
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
    let guard = st.begin_work()?;
    let r = with_fix_env(st, guard, move |env| {
        Ok(check_fixes::apply_cluster_sort(env, id, &sort_value))
    })
    .await?;
    json_ok(r)
}

// ------------------------------------------------------------- multi-disc-layout

/// `POST /api/checks/multi-disc-layout/findings/{finding_id}/convert` with
/// `{"digest": str}` (the preview's): convert a set kept in one folder or
/// in one folder per disc ([`musaic_services::multi_disc_convert::convert`]).
/// The plan is re-derived from disk; a refusal changed nothing and answers
/// its status with `detail`, `reason_code`, `user_message` (and `blockers`
/// or the fresh `digest`). A conversion that ran answers 200, also when a
/// step failed (`ok: false`, a reason per file).
///
/// With `Accept: text/event-stream` the answer is a stream instead, as for
/// the apply and the bulk fix (a set of many discs on a network share takes
/// minutes): `progress {step, done, total}` while it runs, then exactly one
/// `result` (the JSON body above) or `refused` (`status` plus that body). A
/// client that goes away stops only the forwarding; the conversion runs to
/// its end.
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
    let guard = st.begin_work_as(crate::gate::WorkKind::Apply)?;
    let streams = headers
        .get(axum::http::header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("text/event-stream"));
    if streams {
        let (tx, resp) = sse::channel(&st, 64);
        let ctx = musaic_events::context::current();
        tokio::task::spawn_blocking(move || {
            let _guard = guard;
            musaic_events::context::run_in(ctx, || {
                let mut conn = match st.conn() {
                    Ok(c) => c,
                    Err(_) => {
                        let _ = tx.send_blocking(sse::frame(
                            "refused",
                            &json!({"status": 500, "detail": "database unavailable", "user_message": "The library database could not be opened, so nothing was converted."}).to_string(),
                        ));
                        return;
                    }
                };
                let progress_tx = tx.clone();
                let progress = move |v: Value| {
                    let _ = progress_tx.try_send_frame(sse::frame("progress", &v.to_string()));
                };
                let r = st.with_convert_env(&mut conn, |env| {
                    env.progress = Some(std::sync::Arc::new(progress));
                    musaic_services::multi_disc_convert::convert(env, id, &digest)
                });
                let frame = match r {
                    Ok(v) => sse::frame("result", &v.to_string()),
                    Err(e) => {
                        let mut b = e.body();
                        b["status"] = json!(e.status);
                        sse::frame("refused", &b.to_string())
                    }
                };
                let _ = tx.send_blocking(frame);
            })
        });
        return Ok(resp);
    }
    blocking(move || {
        let _guard = guard;
        let mut conn = st.conn()?;
        let r = st.with_convert_env(&mut conn, |env| {
            musaic_services::multi_disc_convert::convert(env, id, &digest)
        });
        match r {
            Ok(v) => ok_value(v),
            Err(e) => Ok(json_value(e.status, &e.body())),
        }
    })
    .await?
}

/// `POST /api/checks/multi-disc-layout/findings/{finding_id}/abandon-conversion`:
/// stop treating the album's unfinished conversion as one
/// ([`musaic_services::multi_disc_gather::abandon`]). Nothing on disk
/// moves; the record is closed as abandoned and says where every file is.
pub async fn abandon_multi_disc_conversion(
    State(st): State<AppState>,
    UrlPath(raw_id): UrlPath<String>,
) -> ApiResult {
    let id = finding_id(&raw_id)?;
    let guard = st.begin_work_as(crate::gate::WorkKind::Apply)?;
    blocking(move || {
        let _guard = guard;
        let mut conn = st.conn()?;
        let r = st.with_convert_env(&mut conn, |env| {
            musaic_services::multi_disc_gather::abandon(env, id)
        });
        match r {
            Ok(v) => ok_value(v),
            Err(e) => Ok(json_value(e.status, &e.body())),
        }
    })
    .await?
}
