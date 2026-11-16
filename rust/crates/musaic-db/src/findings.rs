//! Health-check persistence: `check_findings` and `check_runs`.
//!
//! Port of the SQL in the Python app's `musaic/checks/runner.py`. Timestamps are
//! passed in by the caller (the runner owns the clock), so every function here
//! is deterministic.

use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};

use crate::Result;

/// One `check_findings` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindingRow {
    pub id: i64,
    pub check_id: String,
    /// `None` = library-level finding.
    pub album_id: Option<String>,
    pub severity: String,
    pub summary: String,
    /// The stored JSON text, as written.
    pub detail_json: Option<String>,
    pub first_seen_at: String,
    pub last_seen_at: String,
    pub resolved_at: Option<String>,
    pub ignored_at: Option<String>,
    pub ignored_reason: Option<String>,
}

/// One `check_runs` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRow {
    pub id: i64,
    pub check_id: String,
    pub started_at: String,
    pub completed_at: Option<String>,
    /// `running` | `completed` | `failed`.
    pub status: String,
    pub library_path: String,
    /// `all` | `incremental` | the JSON list of album ids.
    pub scope: String,
    pub issue_count: i64,
    pub error: Option<String>,
    pub duration_ms: Option<i64>,
}

const FINDING_COLS: &str = "id, check_id, album_id, severity, summary, detail_json, first_seen_at, \
     last_seen_at, resolved_at, ignored_at, ignored_reason";
const RUN_COLS: &str = "id, check_id, started_at, completed_at, status, library_path, scope, \
     issue_count, error, duration_ms";

fn finding_from(r: &Row<'_>) -> rusqlite::Result<FindingRow> {
    Ok(FindingRow {
        id: r.get(0)?,
        check_id: r.get(1)?,
        album_id: r.get(2)?,
        severity: r.get(3)?,
        summary: r.get(4)?,
        detail_json: r.get(5)?,
        first_seen_at: r.get(6)?,
        last_seen_at: r.get(7)?,
        resolved_at: r.get(8)?,
        ignored_at: r.get(9)?,
        ignored_reason: r.get(10)?,
    })
}

fn run_from(r: &Row<'_>) -> rusqlite::Result<RunRow> {
    Ok(RunRow {
        id: r.get(0)?,
        check_id: r.get(1)?,
        started_at: r.get(2)?,
        completed_at: r.get(3)?,
        status: r.get(4)?,
        library_path: r.get(5)?,
        scope: r.get(6)?,
        issue_count: r.get(7)?,
        error: r.get(8)?,
        duration_ms: r.get(9)?,
    })
}

// ---------- check_runs ----------

/// Insert a `running` run row; returns its id.
pub fn insert_run(
    conn: &Connection,
    check_id: &str,
    started_at: &str,
    library_path: &str,
    scope: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO check_runs (check_id, started_at, status, library_path, scope, issue_count) \
         VALUES (?1, ?2, 'running', ?3, ?4, 0)",
        params![check_id, started_at, library_path, scope],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn complete_run(
    conn: &Connection,
    run_id: i64,
    completed_at: &str,
    issue_count: i64,
    duration_ms: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE check_runs SET status='completed', completed_at=?1, issue_count=?2, duration_ms=?3 \
         WHERE id=?4",
        params![completed_at, issue_count, duration_ms, run_id],
    )?;
    Ok(())
}

/// Mark a run failed, with the number of findings it upserted before
/// failing (they stay recorded). The Python app left `issue_count` at 0.
pub fn fail_run(
    conn: &Connection,
    run_id: i64,
    completed_at: &str,
    error: &str,
    issue_count: i64,
    duration_ms: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE check_runs SET status='failed', completed_at=?1, error=?2, issue_count=?3, \
         duration_ms=?4 WHERE id=?5",
        params![completed_at, error, issue_count, duration_ms, run_id],
    )?;
    Ok(())
}

pub fn get_run(conn: &Connection, run_id: i64) -> Result<Option<RunRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {RUN_COLS} FROM check_runs WHERE id=?1"),
            [run_id],
            run_from,
        )
        .optional()?)
}

/// The latest run (highest id) per check id.
pub fn last_runs(conn: &Connection) -> Result<Vec<RunRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM check_runs cr INNER JOIN \
         (SELECT check_id, MAX(id) AS max_id FROM check_runs GROUP BY check_id) m \
         ON cr.id = m.max_id",
        RUN_COLS
            .split(", ")
            .map(|c| format!("cr.{c}"))
            .collect::<Vec<_>>()
            .join(", ")
    ))?;
    let rows = stmt.query_map([], run_from)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The last `limit` runs of one check, newest first.
pub fn runs_for_check(conn: &Connection, check_id: &str, limit: i64) -> Result<Vec<RunRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {RUN_COLS} FROM check_runs WHERE check_id=?1 ORDER BY id DESC LIMIT ?2"
    ))?;
    let rows = stmt.query_map(params![check_id, limit], run_from)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

// ---------- check_findings ----------

/// Insert or refresh a finding; returns its row id.
///
/// Key: `(check_id, album_id, summary)`, with `album_id IS ?` so a NULL album
/// matches NULL. On a match: `last_seen_at`, `severity` and `detail_json` are
/// refreshed and `resolved_at` cleared (the issue is back); `ignored_*` are
/// left alone. In one transaction.
pub fn upsert_finding(
    conn: &Connection,
    check_id: &str,
    album_id: Option<&str>,
    severity: &str,
    summary: &str,
    detail_json: Option<&str>,
    now: &str,
) -> Result<i64> {
    let tx = crate::immediate(conn)?;
    let existing: Option<i64> = tx
        .query_row(
            "SELECT id FROM check_findings WHERE check_id=?1 AND album_id IS ?2 AND summary=?3",
            params![check_id, album_id, summary],
            |r| r.get(0),
        )
        .optional()?;
    let id = match existing {
        Some(id) => {
            tx.execute(
                "UPDATE check_findings SET last_seen_at=?1, severity=?2, detail_json=?3, \
                 resolved_at=NULL WHERE id=?4",
                params![now, severity, detail_json, id],
            )?;
            id
        }
        None => {
            tx.execute(
                "INSERT INTO check_findings (check_id, album_id, severity, summary, detail_json, \
                 first_seen_at, last_seen_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                params![check_id, album_id, severity, summary, detail_json, now],
            )?;
            tx.last_insert_rowid()
        }
    };
    tx.commit()?;
    Ok(id)
}

/// Resolve every open, un-ignored finding of `check_id` whose id is not in
/// `seen_ids`. With `scope_album_ids = Some(ids)` only findings on those
/// albums are touched (an empty list touches nothing). Returns the count.
///
/// Findings of Missing albums (`missing_since` neither NULL nor empty, the
/// runner's own test) are never touched: the
/// run could not look at them, so they stay as they are and are
/// re-evaluated when the album returns. The Python app's library-wide run
/// resolved them. Album-scoped runs never list a Missing album, so they are
/// unaffected. Findings with no album, or whose album row is gone, are
/// resolved as before.
///
/// Not limited to a library; the runner uses [`resolve_unseen_in_library`].
pub fn resolve_unseen(
    conn: &Connection,
    check_id: &str,
    seen_ids: &[i64],
    scope_album_ids: Option<&[String]>,
    now: &str,
) -> Result<usize> {
    let mut sql = String::from(
        "UPDATE check_findings SET resolved_at=? WHERE check_id=? AND resolved_at IS NULL \
         AND ignored_at IS NULL AND (album_id IS NULL OR album_id NOT IN \
         (SELECT id FROM albums WHERE missing_since IS NOT NULL AND missing_since != ''))",
    );
    let mut args: Vec<rusqlite::types::Value> =
        vec![now.to_string().into(), check_id.to_string().into()];
    if !seen_ids.is_empty() {
        sql.push_str(&format!(" AND id NOT IN ({})", marks(seen_ids.len())));
        args.extend(seen_ids.iter().map(|&i| i.into()));
    }
    if let Some(ids) = scope_album_ids {
        if ids.is_empty() {
            return Ok(0);
        }
        sql.push_str(&format!(" AND album_id IN ({})", marks(ids.len())));
        args.extend(ids.iter().map(|s| s.clone().into()));
    }
    Ok(conn.execute(&sql, params_from_iter(args))?)
}

/// [`resolve_unseen`] limited to one library (deliberate change). This is
/// what the runner uses.
///
/// Besides everything [`resolve_unseen`] does, a finding is touched only
/// when its album row belongs to `library_path` (`albums.library_path =
/// library_path`, compared exactly as given: the runner passes the same
/// resolved path its detection queries filter on). The Python app's library-wide
/// run resolved every unseen finding of the check, whatever library its
/// album was in, so a run for library A closed library B's open findings.
///
/// Kept from the Python app: a finding with no album (`album_id IS NULL`) or
/// whose album row no longer exists has no library to compare, and is
/// resolved by a library-wide run of ANY library, as in the Python app. Findings
/// of Missing albums stay untouched.
pub fn resolve_unseen_in_library(
    conn: &Connection,
    check_id: &str,
    seen_ids: &[i64],
    scope_album_ids: Option<&[String]>,
    library_path: &str,
    now: &str,
) -> Result<usize> {
    let mut sql = String::from(
        "UPDATE check_findings SET resolved_at=? WHERE check_id=? AND resolved_at IS NULL \
         AND ignored_at IS NULL AND (album_id IS NULL OR album_id NOT IN \
         (SELECT id FROM albums WHERE missing_since IS NOT NULL AND missing_since != '')) \
         AND (album_id IS NULL OR album_id NOT IN (SELECT id FROM albums) \
         OR album_id IN (SELECT id FROM albums WHERE library_path = ?))",
    );
    let mut args: Vec<rusqlite::types::Value> = vec![
        now.to_string().into(),
        check_id.to_string().into(),
        library_path.to_string().into(),
    ];
    if !seen_ids.is_empty() {
        sql.push_str(&format!(" AND id NOT IN ({})", marks(seen_ids.len())));
        args.extend(seen_ids.iter().map(|&i| i.into()));
    }
    if let Some(ids) = scope_album_ids {
        if ids.is_empty() {
            return Ok(0);
        }
        sql.push_str(&format!(" AND album_id IN ({})", marks(ids.len())));
        args.extend(ids.iter().map(|s| s.clone().into()));
    }
    Ok(conn.execute(&sql, params_from_iter(args))?)
}

fn marks(n: usize) -> String {
    vec!["?"; n].join(",")
}

pub fn get_finding(conn: &Connection, finding_id: i64) -> Result<Option<FindingRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {FINDING_COLS} FROM check_findings WHERE id=?1"),
            [finding_id],
            finding_from,
        )
        .optional()?)
}

/// Filters for [`list_findings`] (`runner.list_findings`).
#[derive(Debug, Clone, Default)]
pub struct FindingFilter<'a> {
    pub check_id: Option<&'a str>,
    pub album_id: Option<&'a str>,
    pub include_ignored: bool,
    pub include_resolved: bool,
    /// `None` = the Python app's default of 10000.
    pub limit: Option<i64>,
}

/// Findings ordered by `last_seen_at DESC, id ASC` (the id tiebreaker is a
/// deliberate delta; the Python app leaves ties to SQLite).
pub fn list_findings(conn: &Connection, f: &FindingFilter<'_>) -> Result<Vec<FindingRow>> {
    let mut wh: Vec<&str> = Vec::new();
    let mut args: Vec<rusqlite::types::Value> = Vec::new();
    if let Some(c) = f.check_id {
        wh.push("check_id = ?");
        args.push(c.to_string().into());
    }
    if let Some(a) = f.album_id {
        wh.push("album_id = ?");
        args.push(a.to_string().into());
    }
    if !f.include_ignored {
        wh.push("ignored_at IS NULL");
    }
    if !f.include_resolved {
        wh.push("resolved_at IS NULL");
    }
    let mut sql = format!("SELECT {FINDING_COLS} FROM check_findings");
    if !wh.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&wh.join(" AND "));
    }
    sql.push_str(" ORDER BY last_seen_at DESC, id ASC LIMIT ?");
    args.push(f.limit.unwrap_or(10000).into());
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(args), finding_from)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Open (unresolved, un-ignored) finding count per check id.
pub fn open_counts(conn: &Connection) -> Result<Vec<(String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT check_id, COUNT(*) AS n FROM check_findings \
         WHERE resolved_at IS NULL AND ignored_at IS NULL GROUP BY check_id",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// `ignore_finding`: set `ignored_at` and the reason (stripped; empty -> NULL).
/// `false` when no such row.
pub fn ignore_finding(
    conn: &Connection,
    finding_id: i64,
    reason: Option<&str>,
    now: &str,
) -> Result<bool> {
    let reason = reason
        .map(musaic_core::pycompat::strip)
        .filter(|r| !r.is_empty());
    Ok(conn.execute(
        "UPDATE check_findings SET ignored_at=?1, ignored_reason=?2 WHERE id=?3",
        params![now, reason, finding_id],
    )? > 0)
}

/// `unignore_finding`. `false` when no such row.
pub fn unignore_finding(conn: &Connection, finding_id: i64) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE check_findings SET ignored_at=NULL, ignored_reason=NULL WHERE id=?1",
        [finding_id],
    )? > 0)
}

/// Every row, in id order (for tests and diagnostics).
pub fn all_findings(conn: &Connection) -> Result<Vec<FindingRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {FINDING_COLS} FROM check_findings ORDER BY id"
    ))?;
    let rows = stmt.query_map([], finding_from)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Every run row, in id order (for tests and diagnostics).
pub fn all_runs(conn: &Connection) -> Result<Vec<RunRow>> {
    let mut stmt = conn.prepare(&format!("SELECT {RUN_COLS} FROM check_runs ORDER BY id"))?;
    let rows = stmt.query_map([], run_from)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The ids of `check_id`'s open (unresolved) findings on these albums.
pub fn open_finding_ids(
    conn: &Connection,
    check_id: &str,
    album_ids: &[String],
) -> Result<Vec<i64>> {
    if album_ids.is_empty() {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT id FROM check_findings WHERE check_id = ? AND resolved_at IS NULL \
         AND album_id IN ({})",
        marks(album_ids.len())
    );
    let mut args: Vec<rusqlite::types::Value> = vec![check_id.to_string().into()];
    args.extend(album_ids.iter().map(|s| s.clone().into()));
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(args), |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}
