//! `musaic/api/health.py` (the checks framework): list / detail / run,
//! findings list / ignore / unignore, the filename-mismatch plan preview,
//! the duplicate-tracks cluster preview and canonical
//! recording lookup, the
//! `/api/checks/stream` SSE channel, and the background check sweep's
//! progress (`GET /api/checks/sweep`, not a route of the Python app; also a
//! `sweep` event on the stream).

use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State};
use musaic_checks::ScopeKind;
use musaic_checks::runner::{self, RunError};
use musaic_db::findings::{FindingFilter, get_finding, list_findings};
use musaic_services::Json;
use musaic_services::check_ops;
use serde_json::{Value, json};

use crate::http::{ApiError, ApiResult, blocking, ok_value};
use crate::lax::{Fields, Params, parse_json_body};
use crate::routes::library::{Q, qp};
use crate::sse;
use crate::state::AppState;

/// Key order of the Python app's dicts, for bodies the checks crate builds as
/// (unordered) `serde_json::Value`s; SSE frames are compared byte for byte.
const KEY_ORDER: &[&str] = &[
    // summary
    "meta",
    "open_count",
    "last_run",
    // meta
    "id",
    "label",
    "description",
    "category",
    "cost",
    "runs_during_scan",
    "fix_kind",
    "bulk_action_label",
    // run
    "check_id",
    "started_at",
    "completed_at",
    "status",
    "library_path",
    "scope",
    "issue_count",
    "error",
    "duration_ms",
];

/// A `serde_json::Value` as an ordered Python dict.
pub fn ordered(v: &Value) -> Json {
    match v {
        Value::Null => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::Number(n) => n
            .as_i64()
            .map(Json::Int)
            .unwrap_or_else(|| Json::Float(n.as_f64().unwrap_or(0.0))),
        Value::String(s) => Json::Str(s.clone()),
        Value::Array(a) => Json::Arr(a.iter().map(ordered).collect()),
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort_by_key(|k| {
                (
                    KEY_ORDER
                        .iter()
                        .position(|o| o == k)
                        .unwrap_or(KEY_ORDER.len()),
                    (*k).clone(),
                )
            });
            Json::Obj(
                keys.into_iter()
                    .map(|k| (k.clone(), ordered(&m[k])))
                    .collect(),
            )
        }
    }
}

fn summaries(st: &AppState) -> Result<Vec<Value>, ApiError> {
    let conn = st.conn()?;
    Ok(runner::list_checks_with_summary(
        &conn,
        &st.0.checks_env,
        st.ai_wording(),
    )?)
}

pub async fn list_checks(State(st): State<AppState>) -> ApiResult {
    blocking(move || ok_value(json!({"checks": summaries(&st)?}))).await?
}

pub async fn get_check(
    State(st): State<AppState>,
    UrlPath(check_id): UrlPath<String>,
) -> ApiResult {
    blocking(move || {
        let conn = st.conn()?;
        match runner::get_check_with_runs(&conn, &st.0.checks_env, &check_id, 10, st.ai_wording())?
        {
            Some(v) => ok_value(v),
            None => Err(ApiError::new(
                404,
                format!("Unknown check_id: {}", musaic_fsops::py_repr(&check_id)),
            )),
        }
    })
    .await?
}

pub async fn run_check(
    State(st): State<AppState>,
    UrlPath(check_id): UrlPath<String>,
    body: Bytes,
) -> ApiResult {
    let mut f = Fields::required(parse_json_body(&body)?)?;
    let scope = f.str_or("scope", "all");
    let album_ids = f.opt_str_list("album_ids");
    f.finish()?;
    // Refused while a restore runs; holds the gate until the run
    // ends (the guard lives in the blocking task, so a client that goes
    // away does not release it early, and a panic does release it).
    // Before the handler's own 400 / 409.
    let guard = st.begin_work_as(crate::gate::WorkKind::CheckRun)?;
    // `incremental` still parses (stored runs and the engine's own tests use
    // it) but is not offered here: it ran every album, resolved nothing and
    // recorded no inputs, so the background sweep does that work instead.
    let kind = match ScopeKind::parse(&scope) {
        Some(k @ (ScopeKind::All | ScopeKind::Albums)) => k,
        _ => return Err(ApiError::new(400, "scope must be 'all' or 'albums'")),
    };
    let lib = st
        .library_dir()
        .filter(|l| !l.is_empty())
        .ok_or_else(|| ApiError::new(409, "No library configured"))?;
    blocking(move || {
        let _guard = guard;
        let run = check_ops::run_check_logged::<ApiError>(
            || st.conn(),
            &st.0.checks_env,
            &check_id,
            &lib,
            kind,
            album_ids.as_deref(),
            &scope,
            st.log(),
        )?;
        ok_value(runner::run_to_json(&run))
    })
    .await?
}

/// A check run refused: a database error is a 500, anything else (unknown
/// check, albums scope without ids) a 400 with its message.
impl From<RunError> for ApiError {
    fn from(e: RunError) -> Self {
        match e {
            RunError::Db(e) => ApiError::internal(e),
            e => ApiError::new(400, e.to_string()),
        }
    }
}

pub async fn list_all_findings(State(st): State<AppState>, q: Q) -> ApiResult {
    let q = qp(q);
    let mut p = Params::default();
    let include_ignored = p.query_bool(&q, "include_ignored", false);
    let include_resolved = p.query_bool(&q, "include_resolved", false);
    p.finish()?;
    let album_id = q.get("album_id").map(str::to_string);
    let check_id = q.get("check_id").map(str::to_string);
    blocking(move || {
        let conn = st.conn()?;
        let rows = list_findings(
            &conn,
            &FindingFilter {
                check_id: check_id.as_deref(),
                album_id: album_id.as_deref(),
                include_ignored,
                include_resolved,
                limit: None,
            },
        )?;
        let wording = st.ai_wording();
        ok_value(json!({"findings": rows.iter().map(|f| runner::finding_to_json(f, wording)).collect::<Vec<_>>()}))
    })
    .await?
}

fn finding_id(raw: &str) -> Result<i64, ApiError> {
    let mut p = Params::default();
    let id = p.path_int(raw, "finding_id");
    p.finish()?;
    Ok(id)
}

pub async fn ignore_finding(
    State(st): State<AppState>,
    UrlPath((check_id, raw_id)): UrlPath<(String, String)>,
    body: Bytes,
) -> ApiResult {
    let id = finding_id(&raw_id)?;
    let mut f = Fields::required(parse_json_body(&body)?)?;
    let reason = f.opt_str("reason");
    f.finish()?;
    // Writing route: the restore gate, before the handler's own checks.
    let guard = st.begin_work()?;
    blocking(move || {
        let _guard = guard;
        let conn = st.conn()?;
        match check_ops::ignore_owned::<ApiError>(
            &conn,
            &st.0.checks_env,
            &check_id,
            id,
            reason.as_deref(),
        )? {
            Some(fr) => ok_value(runner::finding_to_json(&fr, st.ai_wording())),
            None => ok_value(Value::Null),
        }
    })
    .await?
}

pub async fn unignore_finding(
    State(st): State<AppState>,
    UrlPath((check_id, raw_id)): UrlPath<(String, String)>,
) -> ApiResult {
    let id = finding_id(&raw_id)?;
    // Writing route: the restore gate, before the handler's own checks.
    let guard = st.begin_work()?;
    blocking(move || {
        let _guard = guard;
        let conn = st.conn()?;
        match check_ops::unignore_owned::<ApiError>(&conn, &st.0.checks_env, &check_id, id)? {
            Some(fr) => ok_value(runner::finding_to_json(&fr, st.ai_wording())),
            None => ok_value(Value::Null),
        }
    })
    .await?
}

pub async fn plan_preview(
    State(st): State<AppState>,
    UrlPath(raw_id): UrlPath<String>,
) -> ApiResult {
    let id = finding_id(&raw_id)?;
    blocking(move || {
        let conn = st.conn()?;
        ok_value(check_ops::filename_mismatch_plan_preview::<ApiError>(
            &conn, id,
        )?)
    })
    .await?
}

/// `GET /api/checks/multi-disc-layout/findings/{id}/conversion-preview`
/// (not a route of the Python app): what converting the set would do,
/// re-derived from disk. A read: no restore gate.
pub async fn conversion_preview(
    State(st): State<AppState>,
    UrlPath(raw_id): UrlPath<String>,
) -> ApiResult {
    let id = finding_id(&raw_id)?;
    blocking(move || {
        let conn = st.conn()?;
        let logs = musaic_services::multi_disc_convert::log_dir(st.config_dir());
        ok_value(check_ops::multi_disc_conversion_preview::<ApiError>(
            &conn,
            id,
            Some(&logs),
        )?)
    })
    .await?
}

pub async fn cluster_preview(
    State(st): State<AppState>,
    UrlPath(raw_id): UrlPath<String>,
) -> ApiResult {
    let id = finding_id(&raw_id)?;
    blocking(move || {
        let conn = st.conn()?;
        let f = get_finding(&conn, id)?;
        match musaic_checks::checks::duplicate_tracks::cluster_preview(&conn, f.as_ref()) {
            Ok(v) => ok_value(v),
            Err(r) if r.status == 500 => Err(ApiError::internal(r.detail)),
            Err(r) => Err(ApiError::new(r.status, r.detail)),
        }
    })
    .await?
}

/// `GET /api/checks/duplicate-tracks/findings/{id}/canonical`
/// (health.py:224-265): the operation is
/// `musaic_services::recording_canonical`; MusicBrainz goes through the
/// app's transport, the answers are cached for the server's lifetime. A
/// read: no restore gate, no Missing-album refusal (the Python app has none and
/// no album file is touched).
pub async fn duplicate_canonical(
    State(st): State<AppState>,
    UrlPath(raw_id): UrlPath<String>,
) -> ApiResult {
    let id = finding_id(&raw_id)?;
    blocking(move || {
        let conn = st.conn()?;
        let mb = musaic_net::MusicBrainzClient::new(st.0.transport.clone());
        match musaic_services::recording_canonical::duplicate_finding_canonical(
            &conn,
            id,
            &mb,
            &st.0.recording_canonical,
        ) {
            Ok(v) => crate::http::ok_ordered(&v),
            Err(r) if r.status == 500 => Err(ApiError::internal(r.detail)),
            Err(r) => Err(ApiError::new(r.status, r.detail)),
        }
    })
    .await?
}

/// `GET /api/checks/sweep` (not a route of the Python app): the background
/// check sweep's progress
/// ([`musaic_services::check_sweep::SweepState::to_json`]).
pub async fn sweep_status(State(st): State<AppState>) -> ApiResult {
    crate::http::ok_ordered(&st.0.sweeper.state().to_json())
}

fn sweep_frame(st: &AppState) -> String {
    sse::frame("sweep", &st.0.sweeper.state().to_json().dumps())
}

/// `GET /api/checks/stream`: the summary list on connect, again
/// after every change, `: ping` after `ping_interval` of silence.
///
/// Deliberate addition: after the first summary, a `sweep` event with the
/// background check sweep's progress, and another after every change of it
/// (one stream, so the UI holds no extra connection for it).
pub async fn checks_stream(State(st): State<AppState>) -> ApiResult {
    let mut rx = st.0.checks_tick.subscribe();
    rx.mark_unchanged();
    let mut sweep_rx = st.0.sweep_changes.subscribe();
    sweep_rx.mark_unchanged();
    let (tx, resp) = sse::channel(&st, 4);
    let ping = st.0.ping_interval;
    tokio::spawn(async move {
        let frame = |st: AppState| async move {
            let list = blocking(move || summaries(&st)).await.and_then(|r| r);
            match list {
                Ok(l) => {
                    let data = Json::obj([("checks", ordered(&Value::Array(l)))]).dumps();
                    Some(sse::frame("state", &data))
                }
                Err(_) => None,
            }
        };
        let Some(f) = frame(st.clone()).await else {
            return;
        };
        if !tx.send(f).await {
            return;
        }
        if !tx.send(sweep_frame(&st)).await {
            return;
        }
        loop {
            tokio::select! {
                changed = rx.changed() => {
                    if changed.is_err() { return; }
                    rx.mark_unchanged();
                    let Some(f) = frame(st.clone()).await else { return };
                    if !tx.send(f).await { return; }
                }
                changed = sweep_rx.changed() => {
                    if changed.is_err() { return; }
                    sweep_rx.mark_unchanged();
                    if !tx.send(sweep_frame(&st)).await { return; }
                }
                _ = tokio::time::sleep(ping) => {
                    if !tx.send(sse::PING).await { return; }
                }
                _ = tx.closed() => return,
            }
        }
    });
    Ok(resp)
}
