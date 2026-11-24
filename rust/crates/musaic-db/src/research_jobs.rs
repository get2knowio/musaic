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

/// The requests of the newest cards (groups), oldest request first. A card
/// whose every request belongs to a bulk run's batch is left out: a batch is
/// listed as one entry of its own (`delegated_work::batches`), not as one
/// card per album (research D12).
pub fn latest(conn: &Connection, statuses: Option<&[String]>, limit: i64) -> Result<Vec<WorkRow>> {
    let sql = format!(
        "WITH groups AS (
        SELECT group_id, {STATUS} AS group_status, max(created_at) AS newest
        FROM delegated_work GROUP BY group_id
        HAVING max(batch_id IS NULL)
           AND (?1 IS NULL OR group_status IN (SELECT value FROM json_each(?1)))
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
    // Groups only: a history-only record of an old job is never queued or
    // running (nothing runs old jobs any more).
    let sql = format!("WITH jobs(status) AS (
        SELECT {STATUS} FROM delegated_work
        WHERE library_epoch=(SELECT epoch FROM library_epoch) GROUP BY group_id
    ) SELECT count(*) FILTER (WHERE status='running'), count(*) FILTER (WHERE status='queued') FROM jobs");
    Ok(conn.query_row(&sql, [], |r| Ok((r.get(0)?, r.get(1)?)))?)
}

/// The old job statuses an older build wrote; any other text on a history
/// record is an unsupported status, cleared with the failed ones.
const OLD_STATUSES: &str = "('queued','running','completed','failed','acted_on')";

/// What Clear removes for one old status (research D13): the request states
/// the jobs list shows under it, and which history-only records go with them.
/// `None` for `queued`, `running` and `completed`: an unresolved request is
/// cancelled, never deleted.
fn clear_scope(status: &str) -> Option<(&'static str, String)> {
    match status {
        "acted_on" => Some(("('applied','discarded')", "status='acted_on'".into())),
        "failed" => Some((
            "('failed','cancelled')",
            format!("(status='failed' OR status NOT IN {OLD_STATUSES})"),
        )),
        _ => None,
    }
}

/// How many requests and history-only records [`clear`] would remove for
/// `statuses` now (the desktop's Clear button count, `queue_state.clearable`).
pub fn clearable(conn: &Connection, statuses: &[&str]) -> Result<i64> {
    let mut count = 0;
    for (states, history) in statuses.iter().filter_map(|s| clear_scope(s)) {
        count += conn.query_row(
            &format!(
                "SELECT (SELECT count(*) FROM delegated_work WHERE state IN {states})
                      + (SELECT count(*) FROM research_history
                         WHERE {} AND {history})",
                crate::research_history::HISTORY_ONLY
            ),
            [],
            |r| r.get::<_, i64>(0),
        )?;
    }
    Ok(count)
}

/// Removes every request (any epoch) whose state the old `statuses` name,
/// with its attempts, results and replays, then the history records those
/// requests named and that no request names any more, and the history-only
/// records of those statuses. Returns requests removed plus history-only
/// records removed (a record whose requests went is not counted again).
/// One audit and notification row per removed request, in the same
/// transaction. Statuses other than `acted_on` and `failed` remove nothing.
/// Albums are never touched.
pub fn clear(conn: &Connection, statuses: &[String], now: i64) -> Result<usize> {
    let tx = crate::immediate(conn)?;
    let epoch = crate::delegated_work::epoch(&tx)?;
    let mut count = 0;
    for (states, history) in statuses.iter().filter_map(|s| clear_scope(s)) {
        count += tx.execute(
            &format!(
                "DELETE FROM research_history WHERE {} AND {history}",
                crate::research_history::HISTORY_ONLY
            ),
            [],
        )?;
        let rows: Vec<(String, Option<String>)> = {
            let mut stmt = tx.prepare(&format!(
                "SELECT id, legacy_job_id FROM delegated_work WHERE state IN {states} ORDER BY id"
            ))?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        count += remove_requests(&tx, &epoch, &rows, now)?;
    }
    tx.commit()?;
    Ok(count)
}

/// What [`delete_group`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupDelete {
    /// The group's requests (and the history record they named) are gone.
    Removed,
    /// A request of the group is unresolved: it is cancelled, never deleted.
    Unresolved,
    /// No request answers to that id.
    NotFound,
}

/// Removes the requests of one desktop card (a group id, an old group id, or
/// the id of the old job its requests name) as [`clear`] does, unless one of
/// them is unresolved.
pub fn delete_group(conn: &Connection, id: &str, now: i64) -> Result<GroupDelete> {
    let tx = crate::immediate(conn)?;
    let rows = group(&tx, id)?;
    if rows.is_empty() {
        return Ok(GroupDelete::NotFound);
    }
    if rows.iter().any(|w| {
        matches!(
            w.state.as_str(),
            "queued" | "claimed" | "running" | "awaiting_review"
        )
    }) {
        return Ok(GroupDelete::Unresolved);
    }
    let epoch = crate::delegated_work::epoch(&tx)?;
    let rows: Vec<(String, Option<String>)> =
        rows.into_iter().map(|w| (w.id, w.legacy_job_id)).collect();
    remove_requests(&tx, &epoch, &rows, now)?;
    tx.commit()?;
    Ok(GroupDelete::Removed)
}

/// Deletes each request `(id, legacy_job_id)` with its attempts, results and
/// replays, records the removal, then the history records they named that no
/// request names any more. Returns how many requests.
fn remove_requests(
    tx: &Connection,
    epoch: &str,
    rows: &[(String, Option<String>)],
    now: i64,
) -> Result<usize> {
    for (id, _) in rows {
        tx.execute("DELETE FROM work_attempts WHERE work_id=?1", [id])?;
        tx.execute("DELETE FROM work_results WHERE work_id=?1", [id])?;
        // A claim's saved answer names `work_id`, a completion's `id`; a
        // multi-item claim's lists several under `claims`, and goes as a whole
        // once one of them does (replaying it could not answer the same).
        tx.execute(
            "DELETE FROM work_replays
             WHERE coalesce(json_extract(response_json,'$.work_id'),
                            json_extract(response_json,'$.id'))=?1
                OR (operation='claim_many' AND EXISTS (
                    SELECT 1 FROM json_each(response_json,'$.claims') AS c
                    WHERE json_extract(c.value,'$.work_id')=?1))",
            [id],
        )?;
        tx.execute("DELETE FROM delegated_work WHERE id=?1", [id])?;
        crate::delegated_work::record_audit(tx, epoch, Some(id), None, "cleared", now)?;
    }
    for legacy in rows.iter().filter_map(|(_, l)| l.as_deref()) {
        tx.execute(
            &format!(
                "DELETE FROM research_history WHERE id=?1 AND {}",
                crate::research_history::HISTORY_ONLY
            ),
            [legacy],
        )?;
    }
    if !rows.is_empty() {
        tx.execute(
            "DELETE FROM work_group_aliases
             WHERE group_id NOT IN (SELECT group_id FROM delegated_work)",
            [],
        )?;
        crate::delegated_work::drop_empty_batches(tx)?;
    }
    Ok(rows.len())
}

/// What [`cancel_group`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupCancel {
    /// This many waiting or held requests of the card were cancelled (at
    /// least one).
    Cancelled(usize),
    /// The card has no waiting or held request: every one is awaiting review
    /// or finished.
    NothingToCancel,
    /// No request answers to that id.
    NotFound,
}

/// Cancels every waiting or held request (queued, claimed, running) of one
/// desktop card (a group id, an old group id, or the id of the old job its
/// requests name) in one transaction, as "Cancel research" does for one item:
/// one audit, outbox and event row per request (FR-036). Requests awaiting
/// review and finished ones are untouched.
pub fn cancel_group(conn: &Connection, id: &str, now: i64) -> Result<GroupCancel> {
    let tx = crate::immediate(conn)?;
    let rows = group(&tx, id)?;
    if rows.is_empty() {
        return Ok(GroupCancel::NotFound);
    }
    let cancelled = crate::delegated_work::cancel_rows_in(&tx, &rows, now)?;
    tx.commit()?;
    Ok(if cancelled == 0 {
        GroupCancel::NothingToCancel
    } else {
        GroupCancel::Cancelled(cancelled)
    })
}
