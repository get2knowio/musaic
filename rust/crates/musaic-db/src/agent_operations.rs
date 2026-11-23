//! Durable, owner-scoped agent operation and MCP Task records. JSON payloads
//! are typed by the application. Functions that change several records run
//! inside an immediate transaction supplied by the caller.

use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::{Result, delegated_work};

pub const TASK_RETENTION_SECS: i64 = 7 * 24 * 60 * 60;

#[derive(Debug)]
pub struct Operation {
    pub id: String,
    pub principal_id: String,
    pub library_epoch: String,
    pub tool: String,
    pub input_digest: String,
    pub state: String,
    pub cancellable: bool,
    pub cancel_requested: bool,
    pub approval_id: Option<String>,
    pub updated_at: i64,
    pub finished_at: Option<i64>,
    pub counts: String,
    pub result: Option<String>,
    pub error: Option<String>,
    pub execution_failed: bool,
}

pub struct NewOperation<'a> {
    pub id: &'a str,
    pub principal_id: &'a str,
    pub tool: &'a str,
    pub key: &'a str,
    pub input_digest: &'a str,
    pub command: &'a str,
    pub cancellable: bool,
    pub approval_id: Option<&'a str>,
    pub now: i64,
}

const COLS: &str = "id, principal_id, library_epoch, tool, input_digest, state, cancellable,     cancel_requested, approval_id, updated_at, finished_at, counts, result, error, execution_failed";

fn operation(r: &Row<'_>) -> rusqlite::Result<Operation> {
    Ok(Operation {
        id: r.get(0)?,
        principal_id: r.get(1)?,
        library_epoch: r.get(2)?,
        tool: r.get(3)?,
        input_digest: r.get(4)?,
        state: r.get(5)?,
        cancellable: r.get(6)?,
        cancel_requested: r.get(7)?,
        approval_id: r.get(8)?,
        updated_at: r.get(9)?,
        finished_at: r.get(10)?,
        counts: r.get(11)?,
        result: r.get(12)?,
        error: r.get(13)?,
        execution_failed: r.get(14)?,
    })
}

pub fn get(conn: &Connection, id: &str) -> Result<Option<Operation>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLS} FROM operations WHERE id=?1"),
            [id],
            operation,
        )
        .optional()?)
}

pub fn replay(conn: &Connection, owner: &str, tool: &str, key: &str) -> Result<Option<Operation>> {
    Ok(conn.query_row(
        &format!("SELECT {COLS} FROM operations WHERE principal_id=?1 AND tool=?2 \
            AND library_epoch=(SELECT epoch FROM library_epoch WHERE singleton=1) AND idempotency_key=?3"),
        params![owner, tool, key],
        operation,
    ).optional()?)
}

/// The operation, its optional approval and the outbox row commit together.
pub fn insert(
    conn: &Connection,
    op: &NewOperation<'_>,
    approval: Option<&crate::agent::ApprovalRow>,
) -> Result<()> {
    let epoch = delegated_work::epoch(conn)?;
    let state = if approval.is_some() {
        "awaiting_approval"
    } else {
        "running"
    };
    conn.execute(
        "INSERT INTO operations(id,principal_id,library_epoch,tool,idempotency_key,input_digest,\
         command,state,cancellable,approval_id,created_at,updated_at) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?11)",
        params![
            op.id,
            op.principal_id,
            epoch,
            op.tool,
            op.key,
            op.input_digest,
            op.command,
            state,
            op.cancellable,
            op.approval_id,
            op.now
        ],
    )?;
    if let Some(approval) = approval {
        crate::agent::insert_approval(conn, approval)?;
    }
    delegated_work::record_change(
        conn,
        &epoch,
        None,
        Some(op.id),
        state,
        op.now.saturating_mul(1000),
    )
}

pub fn start(conn: &Connection, id: &str, now: i64) -> Result<bool> {
    let epoch = delegated_work::epoch(conn)?;
    let changed = conn.execute(
        "UPDATE operations SET state='running', updated_at=?3 \
         WHERE id=?1 AND library_epoch=?2 AND state='awaiting_approval'",
        params![id, epoch, now],
    )? != 0;
    if changed {
        delegated_work::record_change(
            conn,
            &epoch,
            None,
            Some(id),
            "running",
            now.saturating_mul(1000),
        )?;
    }
    Ok(changed)
}

pub fn progress(
    conn: &Connection,
    id: &str,
    now: i64,
    counts: &str,
    partial: Option<&str>,
) -> Result<()> {
    let epoch = delegated_work::epoch(conn)?;
    if conn.execute(
        "UPDATE operations SET counts=?3, result=COALESCE(?4,result), updated_at=?5 \
         WHERE id=?1 AND library_epoch=?2 AND state='running'
         AND (counts<>?3 OR (?4 IS NOT NULL AND result IS NOT ?4))",
        params![id, epoch, counts, partial, now],
    )? != 0
    {
        delegated_work::record_change(
            conn,
            &epoch,
            None,
            Some(id),
            "progress",
            now.saturating_mul(1000),
        )?;
    }
    Ok(())
}

pub fn checkpoint(conn: &Connection, id: &str, change_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE operations SET checkpoint=?2 WHERE id=?1 AND state='running' \
         AND library_epoch=(SELECT epoch FROM library_epoch WHERE singleton=1)",
        params![id, change_id],
    )?;
    Ok(())
}

pub struct Finish<'a> {
    pub state: &'a str,
    pub result: Option<&'a str>,
    pub error: Option<&'a str>,
    pub execution_failed: bool,
    pub now: i64,
}

pub fn finish(conn: &Connection, id: &str, end: &Finish<'_>) -> Result<bool> {
    let epoch = delegated_work::epoch(conn)?;
    let changed = conn.execute(
        "UPDATE operations SET state=?3, finished_at=?4, updated_at=?4, \
         result=COALESCE(?5,result), error=?6, execution_failed=?7 \
         WHERE id=?1 AND library_epoch=?2 AND finished_at IS NULL",
        params![
            id,
            epoch,
            end.state,
            end.now,
            end.result,
            end.error,
            end.execution_failed
        ],
    )? != 0;
    if changed {
        conn.execute(
            "UPDATE mcp_tasks SET expires_at=?2 WHERE operation_id=?1",
            params![id, end.now.saturating_add(TASK_RETENTION_SECS)],
        )?;
        delegated_work::record_change(
            conn,
            &epoch,
            None,
            Some(id),
            end.state,
            end.now.saturating_mul(1000),
        )?;
    }
    Ok(changed)
}

/// Record cancellation intent before signalling a worker.
pub fn request_cancel(conn: &Connection, id: &str, now: i64) -> Result<()> {
    let epoch = delegated_work::epoch(conn)?;
    if conn.execute(
        "UPDATE operations SET cancel_requested=1, updated_at=?3 WHERE id=?1 AND library_epoch=?2 \
         AND finished_at IS NULL AND cancel_requested=0",
        params![id, epoch, now],
    )? != 0
    {
        delegated_work::record_change(
            conn,
            &epoch,
            None,
            Some(id),
            "cancellation_requested",
            now.saturating_mul(1000),
        )?;
    }
    Ok(())
}

pub fn interrupted(conn: &Connection) -> Result<Vec<(String, String, Option<String>)>> {
    let mut stmt = conn.prepare(
        "SELECT id, tool, checkpoint FROM operations WHERE state='running' \
         AND library_epoch=(SELECT epoch FROM library_epoch WHERE singleton=1)",
    )?;
    Ok(stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

#[derive(Debug)]
pub struct Task {
    pub id: String,
    pub created_at: i64,
    pub expires_at: Option<i64>,
}

fn task(r: &Row<'_>) -> rusqlite::Result<Task> {
    Ok(Task {
        id: r.get(0)?,
        created_at: r.get(1)?,
        expires_at: r.get(2)?,
    })
}

pub fn create_task(
    conn: &Connection,
    id: &str,
    op: &Operation,
    profile: &str,
    now: i64,
) -> Result<Task> {
    if conn.execute(
        "INSERT INTO mcp_tasks(id,operation_id,principal_id,profile,created_at,expires_at) \
         VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT (operation_id) DO NOTHING",
        params![
            id,
            op.id,
            op.principal_id,
            profile,
            now,
            op.finished_at
                .map(|t| t.saturating_add(TASK_RETENTION_SECS))
        ],
    )? != 0
    {
        delegated_work::record_change(
            conn,
            &op.library_epoch,
            None,
            Some(&op.id),
            "task_created",
            now.saturating_mul(1000),
        )?;
    }
    Ok(conn.query_row(
        "SELECT id,created_at,expires_at FROM mcp_tasks WHERE operation_id=?1",
        [&op.id],
        task,
    )?)
}

pub fn get_task(
    conn: &Connection,
    id: &str,
    owner: &str,
    profile: &str,
) -> Result<Option<(Task, String)>> {
    Ok(conn
        .query_row(
            "SELECT id,created_at,expires_at,operation_id FROM mcp_tasks \
         WHERE id=?1 AND principal_id=?2 AND profile=?3",
            params![id, owner, profile],
            |r| Ok((task(r)?, r.get(3)?)),
        )
        .optional()?)
}

/// Retain a small owner-scoped tombstone for expiry recovery through
/// operationId, and remove any saved interaction payloads.
pub fn prune_tasks(conn: &Connection, now: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM mcp_task_inputs WHERE task_id IN \
         (SELECT id FROM mcp_tasks WHERE expires_at<=?1 AND expired=0)",
        [now],
    )?;
    conn.execute(
        "UPDATE mcp_tasks SET expired=1 WHERE expires_at<=?1 AND expired=0",
        [now],
    )?;
    Ok(())
}

pub fn pending_inputs(conn: &Connection, id: &str) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT input_key,request FROM mcp_task_inputs WHERE task_id=?1 AND response_digest IS NULL",
    )?;
    Ok(stmt
        .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Acknowledge only the first response to an outstanding request. A retry,
/// an unknown key, or a response after completion has no effect.
pub fn accept_input(conn: &Connection, id: &str, key: &str, digest: &str) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE mcp_task_inputs SET response_digest=?3
         WHERE task_id=?1 AND input_key=?2 AND response_digest IS NULL",
        params![id, key, digest],
    )? != 0)
}

/// A restored or replaced collection cannot resume approval cards or workers
/// from its previous epoch. Their references remain available for diagnosis.
pub(crate) fn invalidate_epoch(conn: &Connection, epoch: &str, now_ms: i64) -> Result<()> {
    let now = now_ms / 1000;
    let at = crate::backup::iso_seconds(
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(now.max(0) as u64),
    );
    // Preserve the historical diff on approval cards but never allow a preview
    // from a replaced collection to authorize another write.
    conn.execute(
        "UPDATE agent_proposals SET expires_at=?1 WHERE expires_at>?1",
        [&at],
    )?;
    conn.execute(
        "UPDATE agent_approvals SET state='expired', reason='the library changed', decided_at=?1
         WHERE state='pending'",
        [&at],
    )?;
    let mut stmt =
        conn.prepare("SELECT id FROM operations WHERE library_epoch=?1 AND finished_at IS NULL")?;
    let ids = stmt
        .query_map([epoch], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for id in ids {
        finish(
            conn,
            &id,
            &Finish {
                state: "expired",
                result: None,
                error: None,
                execution_failed: false,
                now,
            },
        )?;
    }
    Ok(())
}

pub fn expire_orphaned_approvals(conn: &Connection, now: &str) -> Result<()> {
    conn.execute(
        "UPDATE agent_approvals SET state='expired', reason='operation unavailable', decided_at=?1
         WHERE state='pending' AND NOT EXISTS (SELECT 1 FROM operations WHERE id=operation_id)",
        [now],
    )?;
    Ok(())
}
