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
            // A user-started run is a batch of one.
            let mut report = musaic_checks::RerunReport::new();
            let scoped = album_ids.filter(|_| kind == ScopeKind::Albums);
            report.record(&run, scoped);
            report.publish(env);
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

/// The multi-disc-layout finding `id`'s conversion preview, re-derived
/// from the current database and disk (`musaic_fsops::multi_disc`; the
/// stored detail fills only the headline). 404 unless it is a finding of
/// that check, 400 without an album. Read-only: nothing is converted.
/// `logs` is the folder of conversion records: an interrupted conversion
/// of the album is re-derived with its record's help. `digest` is what the
/// conversion must be sent back ([`crate::multi_disc_convert`]).
pub fn multi_disc_conversion_preview<E: From<musaic_db::Error> + From<HttpError>>(
    conn: &Connection,
    id: i64,
    logs: Option<&Path>,
) -> Result<Value, E> {
    use musaic_fsops::multi_disc::{self, PreviewState};
    let finding = match get_finding(conn, id)? {
        Some(f) if f.check_id == musaic_checks::checks::multi_disc_layout::META.id => f,
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
    let headline = detail
        .get("headline")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map_or_else(|| finding.summary.clone(), str::to_string);
    let Some(p) = multi_disc::derive_conversion(conn, &album_id, logs)? else {
        return Ok(json!({
            "finding_id": id,
            "album_id": album_id,
            "state": PreviewState::NotASet.as_str(),
            "convertible": false,
            "conversion_available": false,
            "headline": headline,
            "reason": "This album is no longer in the library.",
            "members": [],
            "tracks": [],
            "folders": [],
            "blockers": [],
        }));
    };
    let plan = &p.plan;
    let tracks: Vec<Value> = plan
        .tracks
        .iter()
        .map(|t| {
            json!({
                "album_id": t.album_id,
                "disc": t.disc,
                "filename": t.filename,
                "title": t.title,
                "tracknumber": t.tracknumber,
                "track": t.track,
                "new_track": t.new_track,
                "new_filename": t.new_filename,
                "has_disc_tags": t.has_disc_tags,
                "tags_done": t.tags_done,
                "state": t.state.as_str(),
                "folder": t.folder,
            })
        })
        .collect();
    let folders: Vec<Value> = plan
        .folders
        .iter()
        .map(|f| {
            json!({
                "album_id": f.album_id,
                "dir_name": f.dir_name,
                "disc": f.disc,
                "destination": f.destination,
                "covers": f.covers.iter().map(|c| json!({"name": c.name, "action": c.action.as_str(), "done": c.done})).collect::<Vec<_>>(),
                "sidecar": f.sidecar,
                "sidecar_done": f.sidecar_done,
                "other_files": f.other_files,
                "strays": f.strays.iter().map(|s| json!({
                    "name": s.name,
                    "action": s.action.as_str(),
                    "new_name": s.new_name,
                    "done": s.done,
                })).collect::<Vec<_>>(),
                "junk": f.junk.iter().map(|(n, d)| json!({"name": n, "done": d})).collect::<Vec<_>>(),
                "exists": f.exists,
            })
        })
        .collect();
    let converts = matches!(p.state, PreviewState::OneFolder | PreviewState::Folders);
    let resume = p.resume.as_ref().map(|r| {
        json!({
            "record": r.path.to_string_lossy(),
            "started_at": r.started_at,
            "attempts": r.attempts.len(),
        })
    });
    let gather = p.gather.as_ref().map(|g| {
        json!({
            "folded": g.folded,
            "folder_renamed": g.folder_renamed,
            "merges": g.members.iter().skip(1).filter(|m| m.has_row).count(),
            "orphan_rows": g.orphan_rows.len(),
        })
    });
    Ok(json!({
        "finding_id": id,
        "album_id": p.album_id,
        "state": p.state.as_str(),
        "convertible": p.convertible(),
        "conversion_available": p.conversion_available(),
        "gather": gather,
        // Where the conversion keeps its record, and where what it no
        // longer needs goes: the confirmation names both.
        "records_dir": logs.map(|l| l.to_string_lossy().into_owned()),
        "digest": p.digest(),
        "resume": resume,
        "headline": headline,
        "reason": p.reason,
        "numbering": p.numbering.map(|n| n.as_str()),
        "disc_total": p.disc_total,
        "present_discs": p.present_discs,
        "absent_discs": p.absent_discs,
        "members": p.members.iter().map(|m| json!({
            "album_id": m.album_id,
            "dir_name": m.dir_name,
            "disc": m.disc,
            "missing": m.missing,
        })).collect::<Vec<_>>(),
        "is_compilation": p.is_compilation,
        "template": p.template,
        "dir_template": p.dir_template,
        "tracks": tracks,
        "track_total": converts.then_some(plan.track_total),
        "target_folder": converts.then(|| json!({
            "current": p.target_current,
            "new": plan.target_name,
        })),
        "removes_disc_tags": converts,
        "folders": folders,
        "blockers": plan.blockers.iter().map(|b| json!({"code": b.code, "message": b.message})).collect::<Vec<_>>(),
    }))
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
