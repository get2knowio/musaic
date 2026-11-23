//! Durable Task mappings over the application operation lifecycle.

use std::collections::BTreeMap;
use std::time::{Duration, UNIX_EPOCH};

use musaic_commands::auth::random_hex;
use musaic_commands::tools::operation::{Cancellation, OperationCancelInput};
use musaic_commands::tools::task::{
    PROFILE, TaskCreateInput, TaskInput, TaskSnapshot, TaskUpdateInput,
};
use musaic_commands::{Decision, PermissionKind, ToolError, ToolErrorCode, Validate};
use musaic_db::agent_operations::{self as db, Operation, Task};
use rusqlite::Connection;

use super::{Principal, operations, run, to_json_text};
use crate::App;

fn paired(app: &App, principal: &Principal) -> Result<(), ToolError> {
    if run::authorize(app, principal, &[PermissionKind::LibraryRead])? == Decision::Allow {
        Ok(())
    } else {
        Err(ToolError::new(
            ToolErrorCode::Forbidden,
            "this pairing cannot read operations",
        ))
    }
}

fn snapshot(
    conn: &Connection,
    now: i64,
    task: Task,
    op: Operation,
) -> Result<TaskSnapshot, ToolError> {
    check_retention(&task, &op, now)?;
    let timestamp =
        |t: i64| musaic_db::backup::iso_seconds(UNIX_EPOCH + Duration::from_secs(t.max(0) as u64));
    let pending_inputs = if op.finished_at.is_none() {
        db::pending_inputs(conn, &task.id)
            .map_err(run::db_err)?
            .into_iter()
            .map(|(key, value)| Ok((key, serde_json::from_str(&value).map_err(run::db_err)?)))
            .collect::<Result<_, ToolError>>()?
    } else {
        BTreeMap::new()
    };
    Ok(TaskSnapshot {
        task_id: task.id,
        created_at: timestamp(task.created_at),
        updated_at: timestamp(op.updated_at.max(task.created_at)),
        ttl_ms: task
            .expires_at
            .map(|t| (t.saturating_sub(task.created_at).max(0) as u64).saturating_mul(1000)),
        execution_failed: op.execution_failed,
        operation: operations::output(op)?,
        pending_inputs,
    })
}

fn check_retention(task: &Task, op: &Operation, now: i64) -> Result<(), ToolError> {
    if task.expires_at.is_some_and(|expiry| expiry <= now) {
        return Err(ToolError::new(
            ToolErrorCode::TaskExpired,
            "the Task's retention period ended",
        )
        .with_detail("operationId", &op.id));
    }
    Ok(())
}

fn find(
    conn: &Connection,
    principal: &Principal,
    task_id: &str,
    now: i64,
) -> Result<(Task, Operation), ToolError> {
    let (task, id) = db::get_task(conn, task_id, &principal.pairing_id, PROFILE)
        .map_err(run::db_err)?
        .ok_or_else(|| ToolError::new(ToolErrorCode::NotFound, "no such task"))?;
    let op = operations::checked(
        conn,
        db::get(conn, &id).map_err(run::db_err)?,
        &principal.pairing_id,
    )?;
    check_retention(&task, &op, now)?;
    Ok((task, op))
}

pub fn create(
    app: &App,
    p: &Principal,
    input: &TaskCreateInput,
) -> Result<TaskSnapshot, ToolError> {
    input.validate()?;
    let _guard = operations::guard(app)?;
    paired(app, p)?;
    operations::transact(app, |conn| {
        let op = operations::checked(
            conn,
            db::get(conn, &input.operation_id).map_err(run::db_err)?,
            &p.pairing_id,
        )?;
        if !matches!(
            op.tool.as_str(),
            "musaic_scan_start" | "musaic_analysis_start" | "musaic_checks_start"
        ) {
            return Err(ToolError::new(
                ToolErrorCode::InvalidInput,
                "this operation has no native Task",
            ));
        }
        let now = operations::now(app);
        let task = db::create_task(conn, &format!("task_{}", random_hex(16)), &op, PROFILE, now)
            .map_err(run::db_err)?;
        snapshot(conn, now, task, op)
    })
}

pub fn get(app: &App, p: &Principal, input: &TaskInput) -> Result<TaskSnapshot, ToolError> {
    input.validate()?;
    let _guard = operations::guard(app)?;
    paired(app, p)?;
    let conn = run::conn(app)?;
    let tx = conn.unchecked_transaction().map_err(run::db_err)?;
    let now = operations::now(app);
    let (task, op) = find(&tx, p, &input.task_id, now)?;
    let result = snapshot(&tx, now, task, op)?;
    tx.commit().map_err(run::db_err)?;
    Ok(result)
}

pub fn update(app: &App, p: &Principal, input: &TaskUpdateInput) -> Result<(), ToolError> {
    input.validate()?;
    let _guard = operations::guard(app)?;
    paired(app, p)?;
    operations::transact(app, |conn| {
        let now = operations::now(app);
        let (task, op) = find(conn, p, &input.task_id, now)?;
        if op.finished_at.is_some() {
            return Ok(());
        }
        for (key, response) in &input.input_responses {
            let digest = run::input_digest(&[&to_json_text(response)]);
            if db::accept_input(conn, &task.id, key, &digest).map_err(run::db_err)? {
                musaic_db::delegated_work::record_change(
                    conn,
                    &op.library_epoch,
                    None,
                    Some(&op.id),
                    "task_input_received",
                    now.saturating_mul(1000),
                )
                .map_err(run::db_err)?;
            }
        }
        Ok(())
    })
}

pub fn cancel(app: &App, p: &Principal, input: &TaskInput) -> Result<(), ToolError> {
    input.validate()?;
    let _guard = operations::guard(app)?;
    paired(app, p)?;
    let conn = run::conn(app)?;
    let (_, op) = find(&conn, p, &input.task_id, operations::now(app))?;
    let result = run::operation_cancel(
        app,
        p,
        &OperationCancelInput {
            operation_id: op.id,
        },
    )?;
    if result.cancellation == Cancellation::NotCancellable {
        return Err(ToolError::new(
            ToolErrorCode::PolicyDenied,
            "the operation cannot be cancelled",
        ));
    }
    Ok(())
}
