//! The checks framework's operations behind `musaic/api/health.py`
//! (refused during a restore): running one check with its log line
//! (checks/runner.py:211-218), ignore / unignore of a finding that must
//! belong to the named check, and the three previews (filename-mismatch
//! plan, duplicate-tracks cluster, multi-disc conversion), each re-derived
//! from the current database and disk. They live here rather than in
//! musaic-api so a non-HTTP face runs checks the same way. The caller takes
//! the restore gate's work guard; a refusal is an [`OpError`], and the
//! previews return typed results (their wording is rendered by the face).

use std::path::Path;

use musaic_checks::runner::{self, RunError};
use musaic_checks::{CheckEnv, CheckError, ScopeKind};
use musaic_db::findings::{FindingRow, RunRow, get_finding};
use musaic_fsops::multi_disc;
use musaic_fsops::plan::Plan;
use musaic_scan::py_resolve;
use rusqlite::Connection;
use serde_json::Value;

use crate::error::{OpError, Subject};
use crate::gated_reruns::CHECKS_TARGET;
use crate::host_log::HostLog;

fn not_found(id: i64) -> OpError {
    OpError::NotFound {
        details: None,
        what: Subject::Finding,
        id: id.to_string(),
        message: "Finding not found".into(),
    }
}

fn no_album() -> OpError {
    OpError::Rejected {
        message: "Finding has no album".into(),
        details: None,
    }
}

/// A run refused: a database error is an internal failure, anything else
/// (unknown check, albums scope without ids) is refused with its message.
pub fn run_error(e: RunError) -> OpError {
    match e {
        RunError::Db(e) => OpError::internal(e),
        e => OpError::Rejected {
            message: e.to_string(),
            details: None,
        },
    }
}

/// Run `check_id` over the library at `lib` (resolved as the Python app's
/// `Path(lib).resolve()`) and log its outcome (checks/runner.py:211-218;
/// `scope` is the request's scope as given). The caller holds the work guard
/// (a restore waits for it). `cancel`, once set, stops the run between albums
/// ([`musaic_checks::run_check_cancellable`]): it ends `failed`, and is
/// reported in `ChecksRerunFinished` like any run that started.
#[allow(clippy::too_many_arguments)]
pub fn run_check_logged(
    conn: &Connection,
    env: &CheckEnv,
    check_id: &str,
    lib: &str,
    kind: ScopeKind,
    album_ids: Option<&[String]>,
    scope: &str,
    log: &dyn HostLog,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<RunRow, OpError> {
    let lib = py_resolve(Path::new(lib)).to_string_lossy().into_owned();
    match musaic_checks::run_check_cancellable(conn, env, check_id, &lib, kind, album_ids, cancel) {
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
            } else if cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::SeqCst))
                && run.error.as_deref() == Some(&CheckError::Cancelled.to_string())
            {
                // Asked for, not a fault.
                log.info(
                    t,
                    &format!(
                        "check {} cancelled after {}ms ({} findings so far, scope={})",
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
            // A user-started run is a batch of one.
            let mut report = musaic_checks::RerunReport::new();
            let scoped = album_ids.filter(|_| kind == ScopeKind::Albums);
            report.record(&run, scoped);
            report.publish(env);
            Ok(run)
        }
        Err(e) => Err(run_error(e)),
    }
}

/// Ignore finding `id` (`NotFound` unless it exists and belongs to `check_id`).
pub fn ignore_owned(
    conn: &Connection,
    env: &CheckEnv,
    check_id: &str,
    id: i64,
    reason: Option<&str>,
) -> Result<Option<FindingRow>, OpError> {
    match get_finding(conn, id)? {
        Some(fr) if fr.check_id == check_id => {}
        _ => return Err(not_found(id)),
    }
    Ok(runner::ignore_finding(conn, env, id, reason)?)
}

/// Unignore finding `id` (`NotFound` unless it exists and belongs to `check_id`).
pub fn unignore_owned(
    conn: &Connection,
    env: &CheckEnv,
    check_id: &str,
    id: i64,
) -> Result<Option<FindingRow>, OpError> {
    match get_finding(conn, id)? {
        Some(fr) if fr.check_id == check_id => {}
        _ => return Err(not_found(id)),
    }
    Ok(runner::unignore_finding(conn, env, id)?)
}

/// The text of a stored detail's string field, `None` when it is missing or
/// empty.
fn detail_text(finding: &FindingRow, key: &str) -> Option<String> {
    let detail: Value = finding
        .detail_json
        .as_deref()
        .and_then(|d| serde_json::from_str(d).ok())
        .unwrap_or(Value::Null);
    detail
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The filename-mismatch finding's rename plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanPreview {
    /// Re-derived against the current database.
    Derived { album_id: String, plan: Plan },
    /// The album is gone: the stored detail's `dir_name` and the finding's
    /// summary fill an empty plan.
    AlbumGone {
        album_id: String,
        dir_name: String,
        headline: String,
    },
}

/// The filename-mismatch finding `id`'s rename plan, re-derived
/// against the current database (`NotFound` unless it is a filename-mismatch
/// finding, `Rejected` without an album).
pub fn filename_mismatch_plan_preview(conn: &Connection, id: i64) -> Result<PlanPreview, OpError> {
    let finding = match get_finding(conn, id)? {
        Some(f) if f.check_id == "filename-mismatch" => f,
        _ => return Err(not_found(id)),
    };
    let Some(album_id) = finding.album_id.clone().filter(|a| !a.is_empty()) else {
        return Err(no_album());
    };
    Ok(match musaic_fsops::plan::derive_plan(conn, &album_id)? {
        Some(plan) => PlanPreview::Derived { album_id, plan },
        None => PlanPreview::AlbumGone {
            dir_name: detail_text(&finding, "dir_name").unwrap_or_default(),
            headline: if finding.summary.is_empty() {
                album_id.clone()
            } else {
                finding.summary.clone()
            },
            album_id,
        },
    })
}

/// The duplicate-tracks finding `id`'s cluster, re-derived from the current
/// database. The checks crate builds the document (its tests pin it); a
/// refusal is mapped to the matching [`OpError`].
pub fn duplicate_cluster_preview(conn: &Connection, id: i64) -> Result<Value, OpError> {
    let f = get_finding(conn, id)?;
    musaic_checks::checks::duplicate_tracks::cluster_preview(conn, f.as_ref()).map_err(|r| match r
        .status
    {
        404 => not_found(id),
        400 => OpError::Rejected {
            message: r.detail,
            details: None,
        },
        _ => OpError::internal(r.detail),
    })
}

/// The multi-disc-layout finding's conversion preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversionPreview {
    /// Re-derived from the current database and disk.
    Derived {
        finding_id: i64,
        headline: String,
        /// Where the conversion keeps its record: the confirmation names it.
        records_dir: Option<String>,
        preview: Box<multi_disc::ConversionPreview>,
    },
    /// The album is no longer in the library.
    NotInLibrary {
        finding_id: i64,
        album_id: String,
        headline: String,
    },
}

/// The multi-disc-layout finding `id`'s conversion preview, re-derived
/// from the current database and disk (`musaic_fsops::multi_disc`; the
/// stored detail fills only the headline). `NotFound` unless it is a finding of
/// that check, `Rejected` without an album. Read-only: nothing is converted.
/// `logs` is the folder of conversion records: an interrupted conversion
/// of the album is re-derived with its record's help. The digest is what the
/// conversion must be sent back ([`crate::multi_disc_convert`]).
pub fn multi_disc_conversion_preview(
    conn: &Connection,
    id: i64,
    logs: Option<&Path>,
) -> Result<ConversionPreview, OpError> {
    let finding = match get_finding(conn, id)? {
        Some(f) if f.check_id == musaic_checks::checks::multi_disc_layout::META.id => f,
        _ => return Err(not_found(id)),
    };
    let Some(album_id) = finding.album_id.clone().filter(|a| !a.is_empty()) else {
        return Err(no_album());
    };
    let headline = detail_text(&finding, "headline").unwrap_or_else(|| finding.summary.clone());
    Ok(
        match multi_disc::derive_conversion(conn, &album_id, logs)? {
            None => ConversionPreview::NotInLibrary {
                finding_id: id,
                album_id,
                headline,
            },
            Some(preview) => ConversionPreview::Derived {
                finding_id: id,
                headline,
                records_dir: logs.map(|l| l.to_string_lossy().into_owned()),
                preview: Box::new(preview),
            },
        },
    )
}
