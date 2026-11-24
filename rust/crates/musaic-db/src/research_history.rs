//! The history of research an older build ran itself (schema v24): one row per
//! job of the old `ai_jobs` table, copied verbatim by the v23 -> v24 step.
//!
//! A request in `delegated_work` names its record through `legacy_job_id`; a
//! combined metadata-and-cover job has two requests in one group, both naming
//! the same record. A record no request names is a history-only record (a
//! job type no request kind matches): it is shown as stored, never listed to
//! an agent (agents read only `delegated_work`), never retried or reviewed,
//! and blocks nothing. Records are never updated except `album_id`, when the
//! album's id changes ([`repoint`], `renames::move_album_rows`, the merge),
//! and are deleted by Clear and delete.
//!
//! `result_json` is kept as stored, readable or not: a caller that cannot
//! parse it reports an unsupported old result.

use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::Result;

/// The job types an older build queued (`jobs.JOB_TYPES`). A record of any
/// other type is history-only.
pub const JOB_TYPES: [&str; 3] = ["metadata", "cover", "metadata+cover"];
/// How many jobs a list returns at most (`list_jobs`' default limit).
pub const LIST_LIMIT: i64 = 200;

/// One old job, as its `research_history` row holds it (the same fields the
/// old `ai_jobs` row had; the desktop's jobs list renders it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRow {
    pub id: String,
    pub album_id: String,
    pub album_path: String,
    /// `row["album_title"] or ""`.
    pub album_title: String,
    pub job_type: String,
    /// `row["hint"] or ""`.
    pub hint: String,
    pub status: String,
    pub result_json: Option<String>,
    pub error: Option<String>,
    pub acted_on_action: Option<String>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub acted_on_at: Option<String>,
    pub attach_cover: bool,
}

impl JobRow {
    /// Test seam: an old job with these fields and every other one empty
    /// (no path, title, hint, result, error or later times; `attach_cover`
    /// on), for [`import_legacy_job`].
    #[doc(hidden)]
    pub fn seed(id: &str, album_id: &str, job_type: &str, status: &str, created_at: &str) -> Self {
        JobRow {
            id: id.into(),
            album_id: album_id.into(),
            album_path: String::new(),
            album_title: String::new(),
            job_type: job_type.into(),
            hint: String::new(),
            status: status.into(),
            result_json: None,
            error: None,
            acted_on_action: None,
            created_at: created_at.into(),
            started_at: None,
            completed_at: None,
            acted_on_at: None,
            attach_cover: true,
        }
    }

    /// Reads the columns by name, so it serves an old `ai_jobs` row and a
    /// `research_history` row alike.
    pub(crate) fn from_row(r: &Row<'_>) -> rusqlite::Result<JobRow> {
        Ok(JobRow {
            id: r.get("id")?,
            album_id: r.get("album_id")?,
            album_path: r.get("album_path")?,
            album_title: r
                .get::<_, Option<String>>("album_title")?
                .unwrap_or_default(),
            job_type: r.get("job_type")?,
            hint: r.get::<_, Option<String>>("hint")?.unwrap_or_default(),
            status: r.get("status")?,
            result_json: r.get("result_json")?,
            error: r.get("error")?,
            acted_on_action: r.get("acted_on_action")?,
            created_at: r.get("created_at")?,
            started_at: r.get("started_at")?,
            completed_at: r.get("completed_at")?,
            acted_on_at: r.get("acted_on_at")?,
            // `bool(row["attach_cover"])`; the column defaults to 1.
            attach_cover: r.get::<_, Option<i64>>("attach_cover")?.unwrap_or(1) != 0,
        })
    }
}

/// A record no request names.
pub const HISTORY_ONLY: &str =
    "NOT EXISTS (SELECT 1 FROM delegated_work w WHERE w.legacy_job_id = research_history.id)";

/// One record.
pub fn get(conn: &Connection, id: &str) -> Result<Option<JobRow>> {
    Ok(conn
        .query_row(
            "SELECT * FROM research_history WHERE id = ?1",
            [id],
            JobRow::from_row,
        )
        .optional()?)
}

/// Records newest first (`created_at DESC`, then id), at most `limit`.
/// `statuses` filters on the old status as stored (`None`: every status; an
/// empty list: nothing).
pub fn list(conn: &Connection, statuses: Option<&[String]>, limit: i64) -> Result<Vec<JobRow>> {
    list_where(conn, statuses, limit, "1")
}

/// [`list`], history-only records only.
pub fn list_history_only(
    conn: &Connection,
    statuses: Option<&[String]>,
    limit: i64,
) -> Result<Vec<JobRow>> {
    list_where(conn, statuses, limit, HISTORY_ONLY)
}

fn list_where(
    conn: &Connection,
    statuses: Option<&[String]>,
    limit: i64,
    predicate: &str,
) -> Result<Vec<JobRow>> {
    let filter = statuses.map(|s| serde_json::to_string(s).expect("strings serialize"));
    let mut stmt = conn.prepare(&format!(
        "SELECT * FROM research_history WHERE {predicate}
           AND (?1 IS NULL OR status IN (SELECT value FROM json_each(?1)))
         ORDER BY created_at DESC, id LIMIT ?2"
    ))?;
    let rows = stmt
        .query_map(params![filter, limit], JobRow::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Whether a record was removed. Its requests, if any, are the caller's.
pub fn delete(conn: &Connection, id: &str) -> Result<bool> {
    Ok(conn.execute("DELETE FROM research_history WHERE id = ?1", [id])? > 0)
}

/// Removes the history-only records whose old status is one of `statuses`;
/// how many (0 for an empty list). Records some request names are left: they
/// go when their requests do.
pub fn delete_for_statuses(conn: &Connection, statuses: &[String]) -> Result<usize> {
    if statuses.is_empty() {
        return Ok(0);
    }
    let filter = serde_json::to_string(statuses).expect("strings serialize");
    Ok(conn.execute(
        &format!(
            "DELETE FROM research_history WHERE {HISTORY_ONLY}
               AND status IN (SELECT value FROM json_each(?1))"
        ),
        [filter],
    )?)
}

/// Moves every record of `old_album_id` to `new_album_id`; how many.
/// `album_path` is a display copy and keeps the old text.
pub fn repoint(conn: &Connection, old_album_id: &str, new_album_id: &str) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE research_history SET album_id = ?1 WHERE album_id = ?2",
        [new_album_id, old_album_id],
    )?)
}

/// Test seam: what the v23 -> v24 step does for one old job, on the caller's
/// transaction. Inserts its record (kept if one with that id exists) and, when
/// no request names it yet, the requests its type maps to in the current
/// epoch (`delegated_work::link_job`). Harnesses whose recorded inputs hold
/// old jobs seed them through this, so they go through the fold's own code.
#[doc(hidden)]
pub fn import_legacy_job(tx: &Connection, job: &JobRow) -> Result<()> {
    tx.execute(
        "INSERT OR IGNORE INTO research_history
             (id, album_id, album_path, album_title, job_type, hint, status, acted_on_action,
              error, result_json, created_at, started_at, completed_at, acted_on_at,
              attach_cover, migrated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,
                 CAST(unixepoch('subsec') * 1000 AS INTEGER))",
        params![
            job.id,
            job.album_id,
            job.album_path,
            job.album_title,
            job.job_type,
            job.hint,
            job.status,
            job.acted_on_action,
            job.error,
            job.result_json,
            job.created_at,
            job.started_at,
            job.completed_at,
            job.acted_on_at,
            i64::from(job.attach_cover),
        ],
    )?;
    let linked: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM delegated_work WHERE legacy_job_id = ?1)",
        [&job.id],
        |r| r.get(0),
    )?;
    if !linked {
        let epoch = crate::delegated_work::epoch(tx)?;
        crate::delegated_work::link_job(tx, &epoch, job)?;
    }
    Ok(())
}
