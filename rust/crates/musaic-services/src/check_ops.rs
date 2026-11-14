//! The checks framework's operations behind `musaic/api/health.py`
//! (refused during a restore): running one check with its log line
//! (checks/runner.py:211-218), ignore / unignore of a finding that must
//! belong to the named check, and the filename-mismatch plan preview with
//! its fallback when the album is gone. They live here rather than in
//! musaic-api so a non-HTTP face runs checks the same way. The host parses the request, takes the restore gate's
//! work guard, and maps errors (each converts into the host's error type
//! where it happens; a refusal is an [`HttpError`]).

use std::path::Path;

use musaic_checks::runner::{self, RunError};
use musaic_checks::{CheckEnv, ScopeKind};
use musaic_db::findings::{FindingRow, RunRow, get_finding};
use musaic_scan::py_resolve;
use rusqlite::Connection;
use serde_json::{Value, json};

use crate::apply::HttpError;
use crate::gated_reruns::CHECKS_TARGET;
use crate::host_log::HostLog;

fn not_found() -> HttpError {
    HttpError {
        status: 404,
        detail: "Finding not found".into(),
    }
}

/// Run `check_id` over the library at `lib` (resolved as the Python app's
/// `Path(lib).resolve()`) on a connection `open` makes after that, and log
/// its outcome (checks/runner.py:211-218; `scope` is the request's scope as
/// given). The caller holds the work guard (a restore waits for it).
#[allow(clippy::too_many_arguments)]
pub fn run_check_logged<E: From<RunError>>(
    open: impl FnOnce() -> Result<Connection, E>,
    env: &CheckEnv,
    check_id: &str,
    lib: &str,
    kind: ScopeKind,
    album_ids: Option<&[String]>,
    scope: &str,
    log: &dyn HostLog,
) -> Result<RunRow, E> {
    let lib = py_resolve(Path::new(lib)).to_string_lossy().into_owned();
    let conn = open()?;
    match musaic_checks::run_check(&conn, env, check_id, &lib, kind, album_ids) {
        Ok(run) => {
            // checks/runner.py:211-218.
            let t = CHECKS_TARGET;
            if run.status == "completed" {
                log.info(
                    t,
                    &format!(
                        "check {} completed in {}ms ({} findings, scope={})",
                        run.check_id,
                        run.duration_ms.unwrap_or(0),
                        run.issue_count,
                        scope
                    ),
                );
            } else {
                log.error(
                    t,
                    &format!(
                        "check {} {}: {}",
                        run.check_id,
                        run.status,
                        run.error.as_deref().unwrap_or("")
                    ),
                );
            }
            Ok(run)
        }
        Err(e) => Err(E::from(e)),
    }
}

/// Ignore finding `id` (404 unless it exists and belongs to `check_id`).
pub fn ignore_owned<E: From<musaic_db::Error> + From<HttpError>>(
    conn: &Connection,
    env: &CheckEnv,
    check_id: &str,
    id: i64,
    reason: Option<&str>,
) -> Result<Option<FindingRow>, E> {
    match get_finding(conn, id)? {
        Some(fr) if fr.check_id == check_id => {}
        _ => return Err(E::from(not_found())),
    }
    Ok(runner::ignore_finding(conn, env, id, reason)?)
}

/// Unignore finding `id` (404 unless it exists and belongs to `check_id`).
pub fn unignore_owned<E: From<musaic_db::Error> + From<HttpError>>(
    conn: &Connection,
    env: &CheckEnv,
    check_id: &str,
    id: i64,
) -> Result<Option<FindingRow>, E> {
    match get_finding(conn, id)? {
        Some(fr) if fr.check_id == check_id => {}
        _ => return Err(E::from(not_found())),
    }
    Ok(runner::unignore_finding(conn, env, id)?)
}

/// The filename-mismatch finding `id`'s rename plan, re-derived
/// against the current database (404 unless it is a filename-mismatch
/// finding, 400 without an album). When the album is gone, the stored
/// detail's `dir_name` and the finding's summary fill an empty plan.
pub fn filename_mismatch_plan_preview<E: From<musaic_db::Error> + From<HttpError>>(
    conn: &Connection,
    id: i64,
) -> Result<Value, E> {
    let finding = match get_finding(conn, id)? {
        Some(f) if f.check_id == "filename-mismatch" => f,
        _ => return Err(E::from(not_found())),
    };
    let Some(album_id) = finding.album_id.clone().filter(|a| !a.is_empty()) else {
        return Err(E::from(HttpError {
            status: 400,
            detail: "Finding has no album".into(),
        }));
    };
    let detail: Value = finding
        .detail_json
        .as_deref()
        .and_then(|d| serde_json::from_str(d).ok())
        .unwrap_or(Value::Null);
    let plan = musaic_fsops::plan::derive_plan(conn, &album_id)?;
    let Some(plan) = plan else {
        return Ok(json!({
            "album_id": album_id,
            "is_empty": true,
            "album_missing": true,
            "dir_name": detail.get("dir_name").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or(""),
            "is_compilation": false,
            "template": "",
            "dir_template": "",
            "headline": if finding.summary.is_empty() { album_id.clone() } else { finding.summary.clone() },
            "renames": [],
            "dir_rename": null,
        }));
    };
    let renames: Vec<Value> = plan
        .renames
        .iter()
        .map(|r| {
            json!({
                "filename": r.filename,
                "expected": r.expected,
                "tracknumber": r.tracknumber,
                "title": r.title,
                "artist": r.artist,
                "collides_with": r.collides_with,
            })
        })
        .collect();
    let mut body = json!({
        "album_id": album_id,
        "is_empty": plan.is_empty(),
        "dir_name": plan.dir_name,
        "is_compilation": plan.is_compilation,
        "template": plan.template,
        "dir_template": plan.dir_template,
        "headline": plan.headline,
        "renames": renames,
        "dir_rename": plan.dir_rename.as_ref().map(|d| json!({"old": d.old, "new": d.new})),
    });
    if plan.album_missing {
        body["album_missing"] = json!(true);
    }
    if let Some(e) = &plan.error {
        body["error"] = json!(e);
    }
    Ok(body)
}
