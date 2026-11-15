//! Execution + persistence of check runs (port of `musaic/checks/runner.py`).
//!
//! The run is synchronous: the check runs on the caller's thread and every
//! finding it emits is upserted before the next one is produced (the Python app
//! drains a worker thread through a bounded queue; the observable effect on
//! the database is the same). Because the check body runs on the caller's
//! thread it sees the caller's guard flags (`in_post_scan_rerun`,
//! `in_bulk_run`); the Python app's executor hop loses them (see the crate docs).

use std::collections::HashSet;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Instant;

use musaic_core::AiWording;
use musaic_db::findings::{self, FindingRow, RunRow};
use musaic_fsops::py_repr;
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Value, json};

use crate::base::{
    CheckCtx, CheckEnv, CheckError, CheckMeta, CheckScope, Finding, ScopeAlbum, ScopeKind,
};
use crate::pyjson::PyJson;

/// `run_check` refused before starting (the Python app's `ValueError`, a 400).
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("Unknown check_id: {}", py_repr(.0))]
    UnknownCheck(String),
    #[error("scope_kind='albums' requires non-empty album_ids")]
    AlbumsScopeNeedsIds,
    #[error(transparent)]
    Db(#[from] musaic_db::Error),
}

impl From<rusqlite::Error> for RunError {
    fn from(e: rusqlite::Error) -> Self {
        RunError::Db(e.into())
    }
}

/// `cache._parse_track_numbers_json`: malformed / non-list -> empty; numbers
/// truncate to int (booleans count as 0/1), anything else is `None`.
pub fn parse_track_numbers_json(raw: Option<&str>) -> Vec<Option<i64>> {
    let Some(raw) = raw.filter(|r| !r.is_empty()) else {
        return Vec::new();
    };
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };
    items
        .iter()
        .map(|x| match x {
            Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f.trunc() as i64)),
            Value::Bool(b) => Some(i64::from(*b)),
            _ => None,
        })
        .collect()
}

/// `cache.load_snapshot(library_path)` reduced to what scopes carry, minus
/// Missing albums. Empty when the library has no `libraries` row.
pub fn load_scope_albums(
    conn: &Connection,
    library_path: &str,
) -> Result<Vec<ScopeAlbum>, RunError> {
    let known: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM libraries WHERE path = ?1",
            [library_path],
            |r| r.get(0),
        )
        .optional()?;
    if known.is_none() {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT id, path, dir_name, track_count, track_numbers_json, missing_since \
         FROM albums WHERE library_path = ?1",
    )?;
    let rows = stmt.query_map([library_path], |r| {
        Ok((
            ScopeAlbum {
                id: r.get(0)?,
                path: r.get(1)?,
                dir_name: r.get(2)?,
                track_count: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                track_numbers: parse_track_numbers_json(r.get::<_, Option<String>>(4)?.as_deref()),
            },
            r.get::<_, Option<String>>(5)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (album, missing) = row?;
        // `not a.get("missing_since")`: NULL and "" both count as Present.
        if missing.as_deref().is_none_or(str::is_empty) {
            out.push(album);
        }
    }
    Ok(out)
}

fn panic_message(p: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "check panicked".to_string()
    }
}

/// Execute one check and return its final `check_runs` row.
///
/// `library_path` is used as given (the Python app's callers pass the resolved
/// library directory). Missing albums are left out of `scope.albums`. When
/// the scope can resolve, findings of this check not seen in this run are
/// resolved, limited to the scope's albums for an album-scoped run; findings
/// of Missing albums are never resolved (a library-wide run leaves
/// them untouched until the album returns; the Python app resolved them), and
/// neither are findings of albums of another library (the Python app's
/// library-wide run resolved them). Findings with no album, or whose album
/// row is gone, are resolved by any library-wide run, as in the Python app. A check
/// that errors (or panics) marks the run `failed` with the message; findings
/// it emitted before failing stay, and nothing is auto-resolved.
pub fn run_check(
    conn: &Connection,
    env: &CheckEnv,
    check_id: &str,
    library_path: &str,
    scope_kind: ScopeKind,
    album_ids: Option<&[String]>,
) -> Result<RunRow, RunError> {
    let check = env
        .registry
        .get(check_id)
        .cloned()
        .ok_or_else(|| RunError::UnknownCheck(check_id.to_string()))?;

    let all_albums = load_scope_albums(conn, library_path)?;
    let albums = if scope_kind == ScopeKind::Albums {
        let ids = album_ids
            .filter(|ids| !ids.is_empty())
            .ok_or(RunError::AlbumsScopeNeedsIds)?;
        let wanted: HashSet<&str> = ids.iter().map(String::as_str).collect();
        all_albums
            .into_iter()
            .filter(|a| wanted.contains(a.id.as_str()))
            .collect()
    } else {
        all_albums
    };
    let scope = CheckScope {
        library_path: library_path.to_string(),
        kind: scope_kind,
        albums,
    };

    let started_at = env.clock.now_iso();
    let started = Instant::now();
    let scope_str = match (scope_kind, album_ids) {
        (ScopeKind::Albums, Some(ids)) => {
            PyJson::List(ids.iter().map(|s| PyJson::Str(s.clone())).collect()).dumps()
        }
        _ => scope_kind.as_str().to_string(),
    };
    let run_id = findings::insert_run(conn, check_id, &started_at, library_path, &scope_str)?;
    (env.notify)();

    let outcome: Result<i64, CheckError> = (|| {
        let mut seen: HashSet<i64> = HashSet::new();
        let mut issue_count = 0i64;
        let ctx = CheckCtx { conn, env };
        let mut emit = |f: Finding| -> Result<(), CheckError> {
            let detail = f.detail.as_ref().map(PyJson::dumps);
            let now = env.clock.now_iso();
            let id = findings::upsert_finding(
                conn,
                check_id,
                f.album_id.as_deref(),
                f.severity.as_str(),
                &f.summary,
                detail.as_deref(),
                &now,
            )?;
            seen.insert(id);
            issue_count += 1;
            Ok(())
        };
        catch_unwind(AssertUnwindSafe(|| check.run(&ctx, &scope, &mut emit)))
            .unwrap_or_else(|p| Err(CheckError::Failed(panic_message(p))))?;
        if scope.can_resolve() {
            let now = env.clock.now_iso();
            let mut seen: Vec<i64> = seen.into_iter().collect();
            seen.sort_unstable();
            let ids = scope.resolve_scope_album_ids();
            // Only findings of this library's albums (plus, as in
            // the Python app, findings with no album or a vanished album row).
            findings::resolve_unseen_in_library(
                conn,
                check_id,
                &seen,
                ids.as_deref(),
                library_path,
                &now,
            )?;
        }
        Ok(issue_count)
    })();

    let completed_at = env.clock.now_iso();
    let duration_ms = started.elapsed().as_millis() as i64;
    match outcome {
        Ok(issue_count) => {
            findings::complete_run(conn, run_id, &completed_at, issue_count, duration_ms)?
        }
        Err(e) => findings::fail_run(conn, run_id, &completed_at, &e.to_string(), duration_ms)?,
    }
    (env.notify)();
    Ok(findings::get_run(conn, run_id)?.expect("check_runs row vanished after insert"))
}

/// `ignore_finding`: `None` when there is no such finding.
pub fn ignore_finding(
    conn: &Connection,
    env: &CheckEnv,
    finding_id: i64,
    reason: Option<&str>,
) -> Result<Option<FindingRow>, musaic_db::Error> {
    let now = env.clock.now_iso();
    if !findings::ignore_finding(conn, finding_id, reason, &now)? {
        return Ok(None);
    }
    (env.notify)();
    findings::get_finding(conn, finding_id)
}

/// `unignore_finding`: `None` when there is no such finding.
pub fn unignore_finding(
    conn: &Connection,
    env: &CheckEnv,
    finding_id: i64,
) -> Result<Option<FindingRow>, musaic_db::Error> {
    if !findings::unignore_finding(conn, finding_id)? {
        return Ok(None);
    }
    (env.notify)();
    findings::get_finding(conn, finding_id)
}

/// `_finding_row_to_dict`: the API shape of a finding (detail parsed; an
/// unparseable detail becomes null). The summary in `wording`
/// ([`crate::wording::finding_summary`]; the stored one is unchanged).
pub fn finding_to_json(f: &FindingRow, wording: AiWording) -> Value {
    let detail = f
        .detail_json
        .as_deref()
        .filter(|d| !d.is_empty())
        .and_then(|d| serde_json::from_str::<Value>(d).ok())
        .unwrap_or(Value::Null);
    json!({
        "id": f.id,
        "check_id": f.check_id,
        "album_id": f.album_id,
        "severity": f.severity,
        "summary": crate::wording::finding_summary(&f.check_id, &f.summary, wording),
        "detail": detail,
        "first_seen_at": f.first_seen_at,
        "last_seen_at": f.last_seen_at,
        "resolved_at": f.resolved_at,
        "ignored_at": f.ignored_at,
        "ignored_reason": f.ignored_reason,
    })
}

/// `_run_row_to_dict`.
pub fn run_to_json(r: &RunRow) -> Value {
    json!({
        "id": r.id,
        "check_id": r.check_id,
        "started_at": r.started_at,
        "completed_at": r.completed_at,
        "status": r.status,
        "library_path": r.library_path,
        "scope": r.scope,
        "issue_count": r.issue_count,
        "error": r.error,
        "duration_ms": r.duration_ms,
    })
}

/// The `meta` object of the API listing. The description in
/// `wording` ([`crate::wording::description`]).
pub fn meta_to_json(m: &CheckMeta, wording: AiWording) -> Value {
    json!({
        "id": m.id,
        "label": m.label,
        "description": crate::wording::description(m, wording),
        "category": m.category,
        "cost": m.cost.as_str(),
        "runs_during_scan": m.runs_during_scan,
        "fix_kind": m.fix_kind.as_str(),
        "bulk_action_label": m.bulk_action_label,
    })
}

/// `list_checks_with_summary()` (GET /api/checks): every registered check with
/// its open-finding count and last run, in `wording`.
pub fn list_checks_with_summary(
    conn: &Connection,
    env: &CheckEnv,
    wording: AiWording,
) -> Result<Vec<Value>, musaic_db::Error> {
    let last = findings::last_runs(conn)?;
    let counts = findings::open_counts(conn)?;
    Ok(env
        .registry
        .all_meta()
        .into_iter()
        .map(|m| {
            json!({
                "meta": meta_to_json(m, wording),
                "open_count": counts.iter().find(|(c, _)| c == m.id).map_or(0, |(_, n)| *n),
                "last_run": last.iter().find(|r| r.check_id == m.id).map(run_to_json),
            })
        })
        .collect())
}

/// `get_check_with_runs(check_id)`: last `run_limit` runs + open findings
/// (newest `last_seen_at` first), in `wording`. `None` for an
/// unknown check.
pub fn get_check_with_runs(
    conn: &Connection,
    env: &CheckEnv,
    check_id: &str,
    run_limit: i64,
    wording: AiWording,
) -> Result<Option<Value>, musaic_db::Error> {
    let Some(check) = env.registry.get(check_id) else {
        return Ok(None);
    };
    let runs = findings::runs_for_check(conn, check_id, run_limit)?;
    let open = findings::list_findings(
        conn,
        &findings::FindingFilter {
            check_id: Some(check_id),
            limit: Some(i64::MAX),
            ..Default::default()
        },
    )?;
    Ok(Some(json!({
        "meta": meta_to_json(check.meta(), wording),
        "runs": runs.iter().map(run_to_json).collect::<Vec<_>>(),
        "findings": open.iter().map(|f| finding_to_json(f, wording)).collect::<Vec<_>>(),
    })))
}
