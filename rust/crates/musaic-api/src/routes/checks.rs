//! `musaic/api/health.py` (the checks framework): list / detail / run,
//! findings list / ignore / unignore, the filename-mismatch plan preview,
//! the duplicate-tracks cluster preview and canonical
//! recording lookup, the
//! `/api/checks/stream` SSE channel, and the background check sweep's
//! progress (`GET /api/checks/sweep`, not a route of the Python app; also a
//! `sweep` event on the stream), over `ops().checks()`.
//!
//! Every handler answers the Python app's 422 for its body, query or path
//! first, then calls one operation and renders the answer
//! (`crate::render::checks`). The restore gate's guard (a run is labelled as a
//! check run), the scope and library refusals, the finding lookups and the order
//! of them are the operations' (`docs/operations.md`).

use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State};
use musaic_app::ops::checks::FindingsQuery;

use crate::http::{ApiResult, blocking, ok_ordered, ok_value};
use crate::lax::{Fields, Params, parse_json_body};
use crate::render::checks as render;
use crate::routes::library::{Q, qp};
use crate::sse;
use crate::state::AppState;

pub async fn list_checks(State(st): State<AppState>) -> ApiResult {
    blocking(move || {
        let checks = st.ops().checks().list()?;
        ok_value(render::check_list(&checks))
    })
    .await?
}

pub async fn get_check(
    State(st): State<AppState>,
    UrlPath(check_id): UrlPath<String>,
) -> ApiResult {
    blocking(move || {
        let check = st.ops().checks().get(&check_id)?;
        ok_value(render::check_detail(&check))
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
    blocking(move || {
        let run = st
            .ops()
            .checks()
            .run(&check_id, &scope, album_ids.as_deref())?;
        ok_value(render::run(&run))
    })
    .await?
}

pub async fn list_all_findings(State(st): State<AppState>, q: Q) -> ApiResult {
    let q = qp(q);
    let mut p = Params::default();
    let include_ignored = p.query_bool(&q, "include_ignored", false);
    let include_resolved = p.query_bool(&q, "include_resolved", false);
    p.finish()?;
    let query = FindingsQuery {
        check_id: q.get("check_id").map(str::to_string),
        album_id: q.get("album_id").map(str::to_string),
        include_ignored,
        include_resolved,
    };
    blocking(move || {
        let findings = st.ops().checks().list_findings(&query)?;
        ok_value(render::finding_list(&findings))
    })
    .await?
}

fn finding_id(raw: &str) -> Result<i64, crate::http::ApiError> {
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
    blocking(move || {
        let finding = st
            .ops()
            .checks()
            .ignore_finding(&check_id, id, reason.as_deref())?;
        ok_value(render::finding_or_null(finding.as_ref()))
    })
    .await?
}

pub async fn unignore_finding(
    State(st): State<AppState>,
    UrlPath((check_id, raw_id)): UrlPath<(String, String)>,
) -> ApiResult {
    let id = finding_id(&raw_id)?;
    blocking(move || {
        let finding = st.ops().checks().unignore_finding(&check_id, id)?;
        ok_value(render::finding_or_null(finding.as_ref()))
    })
    .await?
}

pub async fn plan_preview(
    State(st): State<AppState>,
    UrlPath(raw_id): UrlPath<String>,
) -> ApiResult {
    let id = finding_id(&raw_id)?;
    blocking(move || {
        let plan = st.ops().checks().filename_plan_preview(id)?;
        ok_value(render::plan_preview(&plan))
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
        let preview = st.ops().checks().conversion_preview(id)?;
        ok_value(render::conversion_preview(&preview))
    })
    .await?
}

pub async fn cluster_preview(
    State(st): State<AppState>,
    UrlPath(raw_id): UrlPath<String>,
) -> ApiResult {
    let id = finding_id(&raw_id)?;
    blocking(move || {
        let cluster = st.ops().checks().cluster_preview(id)?;
        ok_value(render::cluster_preview(&cluster))
    })
    .await?
}

/// `GET /api/checks/duplicate-tracks/findings/{id}/canonical`
/// (health.py:224-265): a read, no restore gate, no Missing-album refusal (the
/// Python app has none and no album file is touched).
pub async fn duplicate_canonical(
    State(st): State<AppState>,
    UrlPath(raw_id): UrlPath<String>,
) -> ApiResult {
    let id = finding_id(&raw_id)?;
    blocking(move || {
        let canonical = st.ops().checks().duplicate_canonical(id)?;
        ok_ordered(&render::canonical(&canonical))
    })
    .await?
}

/// `GET /api/checks/sweep` (not a route of the Python app): the background
/// check sweep's progress.
pub async fn sweep_status(State(st): State<AppState>) -> ApiResult {
    ok_ordered(&render::sweep(&st.ops().checks().sweep_status()))
}

/// `GET /api/checks/stream`: the summary list on connect, again
/// after every change, `: ping` after `ping_interval` of silence.
///
/// Deliberate addition: after the first summary, a `sweep` event with the
/// background check sweep's progress, and another after every change of it
/// (one stream, so the UI holds no extra connection for it).
///
/// The feed is subscribed before the response is returned. A value reads the
/// database, so the handler waits for one on this thread and reads it on a
/// blocking one.
pub async fn checks_stream(State(st): State<AppState>) -> ApiResult {
    let mut feed = st.ops().checks().runs_feed();
    let (tx, resp) = sse::channel(&st, 4);
    let ping = st.ping_interval();
    tokio::spawn(async move {
        loop {
            let ready = tokio::select! {
                r = feed.wait() => Some(r),
                _ = tokio::time::sleep(ping) => None,
                _ = tx.closed() => return,
            };
            let frames = match ready {
                Some(false) => return,
                None => vec![sse::PING.to_string()],
                Some(true) => {
                    let Ok((back, next)) = blocking(move || {
                        let next = feed.next(Duration::ZERO);
                        (feed, next)
                    })
                    .await
                    else {
                        return;
                    };
                    feed = back;
                    match next.as_ref().and_then(render::frames) {
                        Some(frames) => frames,
                        None => return,
                    }
                }
            };
            for frame in frames {
                if !tx.send(frame).await {
                    return;
                }
            }
        }
    });
    Ok(resp)
}
