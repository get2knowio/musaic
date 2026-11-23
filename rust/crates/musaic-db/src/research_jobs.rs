//! Desktop projections of delegated work onto the historical jobs list.
use crate::{
    Result,
    delegated_work::{WorkRow, work},
};
use rusqlite::{Connection, OptionalExtension, params};

const STATUS: &str = "CASE
    WHEN max(state='awaiting_review') THEN 'completed'
    WHEN max(state IN ('running','claimed')) THEN 'running'
    WHEN max(state='queued') THEN 'queued'
    WHEN max(state IN ('failed','cancelled')) THEN 'failed'
    ELSE 'acted_on' END";

pub fn group(conn: &Connection, id: &str) -> Result<Vec<WorkRow>> {
    let group = crate::delegated_work::resolve_group(conn, id)?;
    let mut stmt = conn.prepare(
        "SELECT * FROM delegated_work WHERE group_id=?1 OR legacy_job_id=?2 ORDER BY created_at,id",
    )?;
    Ok(stmt
        .query_map(params![group, id], work)?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn latest(conn: &Connection, statuses: Option<&[String]>, limit: i64) -> Result<Vec<WorkRow>> {
    let sql = format!(
        "WITH groups AS (
        SELECT group_id, {STATUS} AS group_status, max(created_at) AS newest
        FROM delegated_work GROUP BY group_id
        HAVING ?1 IS NULL OR group_status IN (SELECT value FROM json_each(?1))
        ORDER BY newest DESC, group_id LIMIT ?2
    ) SELECT w.* FROM groups JOIN delegated_work w USING(group_id) ORDER BY w.created_at,w.id"
    );
    let filter = statuses.map(|s| serde_json::to_string(s).expect("strings serialize"));
    let mut stmt = conn.prepare(&sql)?;
    Ok(stmt
        .query_map(params![filter, limit], work)?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn active_group(conn: &Connection, album: &str) -> Result<Option<String>> {
    Ok(conn.query_row("SELECT group_id FROM delegated_work WHERE album_id=?1
        AND library_epoch=(SELECT epoch FROM library_epoch) AND state IN ('queued','claimed','running','awaiting_review')
        ORDER BY created_at DESC,id LIMIT 1",[album],|r|r.get(0)).optional()?)
}

pub fn queue_counts(conn: &Connection) -> Result<(i64, i64)> {
    let sql = format!("WITH jobs(status) AS (
        SELECT status FROM ai_jobs WHERE NOT EXISTS (SELECT 1 FROM delegated_work WHERE legacy_job_id=ai_jobs.id)
        UNION ALL SELECT {STATUS} FROM delegated_work
        WHERE library_epoch=(SELECT epoch FROM library_epoch) GROUP BY group_id
    ) SELECT count(*) FILTER (WHERE status='running'), count(*) FILTER (WHERE status='queued') FROM jobs");
    Ok(conn.query_row(&sql, [], |r| Ok((r.get(0)?, r.get(1)?)))?)
}
