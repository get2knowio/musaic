//! The AI job queue table (`ai_jobs`, schema v16) and the two album-row
//! stamps the AI paths write.
//!
//! Same SQL as the Python app's `musaic/jobs.py` and the two helpers of
//! `musaic/cache.py` it relies on (`record_mb_lookup_failure`,
//! `mark_metadata_ai_acted_on`). Timestamps and new ids are passed in by the
//! caller (the Python app writes `now()` as `YYYY-MM-DDTHH:MM:SSZ` and ids as
//! 16 hex characters), and `result_json` is passed already serialised (the
//! the Python app's `json.dumps(...)`, which the caller renders), so these
//! functions stay pure. Nothing here notifies anyone; the caller does.
//!
//! One deliberate tightening: [`enqueue`] checks for an active job and
//! inserts inside one `BEGIN IMMEDIATE` transaction. The Python app runs the
//! check outside its insert transaction, so two concurrent enqueues for one
//! album could both pass; sequentially the two behave the same.

use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};

use crate::Result;

/// `jobs.JOB_TYPES`.
pub const JOB_TYPES: [&str; 3] = ["metadata", "cover", "metadata+cover"];
/// `jobs.STATUSES`.
pub const STATUSES: [&str; 5] = ["queued", "running", "completed", "failed", "acted_on"];
/// `jobs.ACTIVE_STATUSES`: a job in one of these counts against the
/// one-job-per-album rule. Failed jobs do not.
pub const ACTIVE_STATUSES: [&str; 3] = ["queued", "running", "completed"];
/// `list_jobs`' default limit.
pub const LIST_LIMIT: i64 = 200;

/// One `ai_jobs` row (`jobs.Job`).
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
    fn from_row(r: &Row<'_>) -> rusqlite::Result<JobRow> {
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

/// What `enqueue` inserts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewJob<'a> {
    pub album_id: &'a str,
    pub album_path: &'a str,
    pub album_title: &'a str,
    pub job_type: &'a str,
    pub hint: &'a str,
    pub attach_cover: bool,
}

/// Why `enqueue` refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnqueueRefused {
    /// `ValueError`: not one of [`JOB_TYPES`].
    UnknownJobType(String),
    /// `JobConflictError`: an active job already exists for the album.
    Conflict {
        existing_job_id: String,
        status: String,
    },
}

fn placeholders(n: usize, from: usize) -> String {
    (0..n)
        .map(|i| format!("?{}", from + i))
        .collect::<Vec<_>>()
        .join(",")
}

fn active_sql(prefix: &str, suffix: &str) -> String {
    format!(
        "{prefix} WHERE album_id = ?1 AND status IN ({}){suffix}",
        placeholders(ACTIVE_STATUSES.len(), 2)
    )
}

/// `jobs.enqueue`: refuses an unknown job type or an album with an active
/// job (the existing job's id and status are returned); otherwise inserts a
/// `queued` row with the id `new_id()` makes (called only once the checks
/// pass, as the Python app's `_new_id()`) and `created_at = now_iso`.
pub fn enqueue(
    conn: &mut Connection,
    job: &NewJob<'_>,
    new_id: impl FnOnce() -> String,
    now_iso: &str,
) -> Result<std::result::Result<JobRow, EnqueueRefused>> {
    if !JOB_TYPES.contains(&job.job_type) {
        return Ok(Err(EnqueueRefused::UnknownJobType(
            job.job_type.to_string(),
        )));
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing: Option<(String, String)> = {
        let sql = active_sql("SELECT id, status FROM ai_jobs", " LIMIT 1");
        let mut p: Vec<&dyn rusqlite::ToSql> = vec![&job.album_id];
        for s in &ACTIVE_STATUSES {
            p.push(s);
        }
        tx.query_row(&sql, p.as_slice(), |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?
    };
    if let Some((existing_job_id, status)) = existing {
        return Ok(Err(EnqueueRefused::Conflict {
            existing_job_id,
            status,
        }));
    }
    let id = new_id();
    tx.execute(
        "INSERT INTO ai_jobs (id, album_id, album_path, album_title, job_type, \
         hint, status, created_at, attach_cover) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'queued', ?7, ?8)",
        params![
            id,
            job.album_id,
            job.album_path,
            job.album_title,
            job.job_type,
            job.hint,
            now_iso,
            i64::from(job.attach_cover)
        ],
    )?;
    tx.commit()?;
    Ok(Ok(JobRow {
        id,
        album_id: job.album_id.to_string(),
        album_path: job.album_path.to_string(),
        album_title: job.album_title.to_string(),
        job_type: job.job_type.to_string(),
        hint: job.hint.to_string(),
        status: "queued".into(),
        result_json: None,
        error: None,
        acted_on_action: None,
        created_at: now_iso.to_string(),
        started_at: None,
        completed_at: None,
        acted_on_at: None,
        attach_cover: job.attach_cover,
    }))
}

fn rows(conn: &Connection, sql: &str, p: &[&dyn rusqlite::ToSql]) -> Result<Vec<JobRow>> {
    let mut st = conn.prepare(sql)?;
    let out = st
        .query_map(p, JobRow::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(out)
}

/// `jobs.list_jobs(statuses=..., limit=...)`: newest first
/// (`ORDER BY created_at DESC`). `None` lists every status; an empty list
/// lists nothing.
pub fn list_jobs(
    conn: &Connection,
    statuses: Option<&[String]>,
    limit: i64,
) -> Result<Vec<JobRow>> {
    match statuses {
        None => rows(
            conn,
            "SELECT * FROM ai_jobs ORDER BY created_at DESC LIMIT ?1",
            &[&limit],
        ),
        Some([]) => Ok(Vec::new()),
        Some(st) => {
            let sql = format!(
                "SELECT * FROM ai_jobs WHERE status IN ({}) ORDER BY created_at DESC LIMIT ?{}",
                placeholders(st.len(), 1),
                st.len() + 1
            );
            let mut p: Vec<&dyn rusqlite::ToSql> =
                st.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
            p.push(&limit);
            rows(conn, &sql, &p)
        }
    }
}

/// `jobs.get_job`.
pub fn get_job(conn: &Connection, job_id: &str) -> Result<Option<JobRow>> {
    Ok(conn
        .query_row(
            "SELECT * FROM ai_jobs WHERE id = ?1",
            [job_id],
            JobRow::from_row,
        )
        .optional()?)
}

/// `jobs.get_active_job_for_album`: the newest queued / running / completed
/// job of the album.
pub fn get_active_job_for_album(conn: &Connection, album_id: &str) -> Result<Option<JobRow>> {
    let sql = active_sql("SELECT * FROM ai_jobs", " ORDER BY created_at DESC LIMIT 1");
    let mut p: Vec<&dyn rusqlite::ToSql> = vec![&album_id];
    for s in &ACTIVE_STATUSES {
        p.push(s);
    }
    Ok(conn
        .query_row(&sql, p.as_slice(), JobRow::from_row)
        .optional()?)
}

/// `jobs.get_next_queued_excluding(exclude_ids)` (`get_next_queued` when
/// empty): the oldest queued job not in `exclude`.
pub fn next_queued_excluding(conn: &Connection, exclude: &[String]) -> Result<Option<JobRow>> {
    if exclude.is_empty() {
        return Ok(conn
            .query_row(
                "SELECT * FROM ai_jobs WHERE status = 'queued' ORDER BY created_at ASC LIMIT 1",
                [],
                JobRow::from_row,
            )
            .optional()?);
    }
    let sql = format!(
        "SELECT * FROM ai_jobs WHERE status = 'queued' AND id NOT IN ({}) \
         ORDER BY created_at ASC LIMIT 1",
        placeholders(exclude.len(), 1)
    );
    let p: Vec<&dyn rusqlite::ToSql> = exclude.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
    Ok(conn
        .query_row(&sql, p.as_slice(), JobRow::from_row)
        .optional()?)
}

fn count(conn: &Connection, status: &str) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) AS n FROM ai_jobs WHERE status = ?1",
        [status],
        |r| r.get(0),
    )?)
}

/// `jobs.count_running`.
pub fn count_running(conn: &Connection) -> Result<i64> {
    count(conn, "running")
}

/// `jobs.count_queued`.
pub fn count_queued(conn: &Connection) -> Result<i64> {
    count(conn, "queued")
}

/// `jobs.reset_stuck_running` (worker startup): every `running` row back to
/// `queued` with `started_at` cleared. Returns how many.
pub fn reset_stuck_running(conn: &Connection) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE ai_jobs SET status = 'queued', started_at = NULL WHERE status = 'running'",
        [],
    )?)
}

/// `jobs.mark_running`: only a `queued` row moves.
pub fn mark_running(conn: &Connection, job_id: &str, now_iso: &str) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE ai_jobs SET status = 'running', started_at = ?1 WHERE id = ?2 AND status = 'queued'",
        params![now_iso, job_id],
    )? > 0)
}

/// `jobs.mark_completed` with the rendered `json.dumps(result)`.
pub fn mark_completed(
    conn: &Connection,
    job_id: &str,
    result_json: &str,
    now_iso: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE ai_jobs SET status = 'completed', result_json = ?1, completed_at = ?2 WHERE id = ?3",
        params![result_json, now_iso, job_id],
    )?;
    Ok(())
}

/// `jobs.mark_failed`: `result_json` is `json.dumps({"failure_excerpt": ...})`
/// when there is an excerpt, else NULL (the caller renders it).
pub fn mark_failed(
    conn: &Connection,
    job_id: &str,
    error: &str,
    result_json: Option<&str>,
    now_iso: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE ai_jobs SET status = 'failed', error = ?1, result_json = ?2, completed_at = ?3 \
         WHERE id = ?4",
        params![error, result_json, now_iso, job_id],
    )?;
    Ok(())
}

/// `jobs.mark_acted_on`: `action` must be `applied` or `discarded`
/// (`Ok(false)` otherwise, the Python app's `ValueError`). No status check here;
/// the route checks.
pub fn mark_acted_on(conn: &Connection, job_id: &str, action: &str, now_iso: &str) -> Result<bool> {
    if action != "applied" && action != "discarded" {
        return Ok(false);
    }
    conn.execute(
        "UPDATE ai_jobs SET status = 'acted_on', acted_on_action = ?1, acted_on_at = ?2 WHERE id = ?3",
        params![action, now_iso, job_id],
    )?;
    Ok(true)
}

/// `jobs.retry_failed`: a `failed` row back to `queued` (error, started_at
/// and completed_at cleared, `created_at = now` so it sorts last; the
/// stored failure excerpt in `result_json` is kept, as in the Python app). The
/// API retries with [`retry_job`]; this is the Python app's
/// rule, pinned by musaic-db/tests/ai_jobs.rs.
/// `None` when the job does not exist or is not failed.
pub fn retry_failed(conn: &mut Connection, job_id: &str, now_iso: &str) -> Result<Option<JobRow>> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let job = get_job(&tx, job_id)?;
    if job.as_ref().is_none_or(|j| j.status != "failed") {
        return Ok(None);
    }
    tx.execute(
        "UPDATE ai_jobs SET status = 'queued', error = NULL, started_at = NULL, \
         completed_at = NULL, created_at = ?1 WHERE id = ?2",
        params![now_iso, job_id],
    )?;
    tx.commit()?;
    get_job(conn, job_id)
}

/// What [`retry_job`] did.
#[derive(Debug, Clone, PartialEq)]
pub enum RetryOutcome {
    /// The job, back in `queued`.
    Retried(Box<JobRow>),
    /// No such job, or it is not `failed` (the Python app's `None`).
    NotFailed,
    /// Another job of the same album is active (its id, status).
    OtherActive {
        existing_job_id: String,
        status: String,
    },
}

/// The retry the API runs: [`retry_failed`] with two deliberate changes.
/// Refused while the album has
/// another active job (the one-active-job rule enqueue keeps; the Python app
/// let retry break it), and the previous failure excerpt
/// (`result_json`) is cleared with the error. One IMMEDIATE transaction,
/// so a concurrent enqueue cannot slip between the check and the update.
pub fn retry_job(conn: &mut Connection, job_id: &str, now_iso: &str) -> Result<RetryOutcome> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let Some(job) = get_job(&tx, job_id)?.filter(|j| j.status == "failed") else {
        return Ok(RetryOutcome::NotFailed);
    };
    let other: Option<(String, String)> = {
        let sql = active_sql("SELECT id, status FROM ai_jobs", " AND id != ?5 LIMIT 1");
        let mut p: Vec<&dyn rusqlite::ToSql> = vec![&job.album_id];
        for s in &ACTIVE_STATUSES {
            p.push(s);
        }
        p.push(&job_id);
        tx.query_row(&sql, p.as_slice(), |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?
    };
    if let Some((existing_job_id, status)) = other {
        return Ok(RetryOutcome::OtherActive {
            existing_job_id,
            status,
        });
    }
    tx.execute(
        "UPDATE ai_jobs SET status = 'queued', error = NULL, result_json = NULL, \
         started_at = NULL, completed_at = NULL, created_at = ?1 WHERE id = ?2",
        params![now_iso, job_id],
    )?;
    tx.commit()?;
    Ok(match get_job(conn, job_id)? {
        Some(j) => RetryOutcome::Retried(Box::new(j)),
        None => RetryOutcome::NotFailed,
    })
}

/// `jobs.delete_job`: whether a row was removed.
pub fn delete_job(conn: &Connection, job_id: &str) -> Result<bool> {
    Ok(conn.execute("DELETE FROM ai_jobs WHERE id = ?1", [job_id])? > 0)
}

/// `jobs.delete_jobs_by_status`: rows deleted (0 for an empty list).
pub fn delete_jobs_by_status(conn: &Connection, statuses: &[String]) -> Result<usize> {
    if statuses.is_empty() {
        return Ok(0);
    }
    let sql = format!(
        "DELETE FROM ai_jobs WHERE status IN ({})",
        placeholders(statuses.len(), 1)
    );
    let p: Vec<&dyn rusqlite::ToSql> = statuses.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
    Ok(conn.execute(&sql, p.as_slice())?)
}

/// `cache.record_mb_lookup_failure`: stamps `mb_last_checked_at` on an album
/// that has no release-group anchor ("tried, no match").
pub fn record_mb_lookup_failure(conn: &Connection, album_id: &str, now_iso: &str) -> Result<()> {
    conn.execute(
        "UPDATE albums SET mb_last_checked_at = ?1 WHERE id = ?2 AND mb_release_group_mbid IS NULL",
        params![now_iso, album_id],
    )?;
    Ok(())
}

/// `cache.mark_metadata_ai_acted_on`.
pub fn mark_metadata_ai_acted_on(conn: &Connection, album_id: &str, now_iso: &str) -> Result<()> {
    conn.execute(
        "UPDATE albums SET metadata_ai_acted_on_at = ?1 WHERE id = ?2",
        params![now_iso, album_id],
    )?;
    Ok(())
}
