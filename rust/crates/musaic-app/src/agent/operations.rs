//! App-owned operation lifecycle and typed result persistence.

use musaic_commands::tools::operation::{
    ApplyResult, Counts, OperationGetOutput, OperationHandle, OperationResult, OperationState,
    POLL_INTERVAL_MS,
};
use musaic_commands::{ToolError, ToolErrorCode};
use musaic_db::agent_operations::{self as db, Finish, NewOperation, Operation};
use musaic_db::delegated_work;
use rusqlite::Connection;

use super::{run, to_json_text};
use crate::App;

pub(super) fn now(app: &App) -> i64 {
    app.now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub(super) fn guard(app: &App) -> Result<musaic_services::gate::WorkGuard, ToolError> {
    app.gate().begin_work().map_err(|_| {
        ToolError::new(
            ToolErrorCode::Restoring,
            "a restore is running; try again when it ends",
        )
    })
}

pub(super) fn checked(
    conn: &Connection,
    op: Option<Operation>,
    owner: &str,
) -> Result<Operation, ToolError> {
    let op = op
        .filter(|op| op.principal_id == owner)
        .ok_or_else(|| ToolError::new(ToolErrorCode::NotFound, "no such operation"))?;
    if op.library_epoch != delegated_work::epoch(conn).map_err(run::db_err)? {
        return Err(ToolError::new(
            ToolErrorCode::StaleContext,
            "the library has changed",
        ));
    }
    Ok(op)
}

pub(super) fn transact<T>(
    app: &App,
    f: impl FnOnce(&Connection) -> Result<T, ToolError>,
) -> Result<T, ToolError> {
    let _guard = guard(app)?;
    let conn = run::conn(app)?;
    let tx = musaic_db::immediate(&conn).map_err(run::db_err)?;
    let result = f(&tx)?;
    tx.commit().map_err(run::db_err)?;
    super::delegation::publish_pending(app, &conn);
    Ok(result)
}

pub(super) fn state(op: &Operation) -> Result<OperationState, ToolError> {
    serde_json::from_value(serde_json::Value::String(op.state.clone())).map_err(run::db_err)
}

pub(super) fn handle(op: &Operation) -> Result<OperationHandle, ToolError> {
    Ok(OperationHandle {
        operation_id: op.id.clone(),
        state: state(op)?,
        approval_id: op.approval_id.clone(),
        poll_interval_ms: POLL_INTERVAL_MS,
        counts: None,
        result: None,
        error: None,
    })
}

pub(super) fn output(op: Operation) -> Result<OperationGetOutput, ToolError> {
    let state = state(&op)?;
    Ok(OperationGetOutput {
        operation_id: op.id,
        tool: op.tool,
        state,
        counts: serde_json::from_str(&op.counts).map_err(run::db_err)?,
        approval_id: op.approval_id,
        result: stored(op.result)?,
        error: stored(op.error)?,
    })
}

fn stored<T: serde::de::DeserializeOwned>(value: Option<String>) -> Result<Option<T>, ToolError> {
    value
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(run::db_err)
}

pub(super) fn replay(
    conn: &Connection,
    owner: &str,
    tool: &str,
    key: &str,
    digest: &str,
) -> Result<Option<Operation>, ToolError> {
    let op = db::replay(conn, owner, tool, key).map_err(run::db_err)?;
    if op.as_ref().is_some_and(|op| op.input_digest != digest) {
        return Err(ToolError::new(
            ToolErrorCode::IdempotencyConflict,
            "that idempotency key was used with different input",
        ));
    }
    Ok(op)
}

pub(super) fn register(
    app: &App,
    operation: &NewOperation<'_>,
    approval: Option<&musaic_db::agent::ApprovalRow>,
) -> Result<(Operation, bool), ToolError> {
    transact(app, |conn| {
        if let Some(op) = replay(
            conn,
            operation.principal_id,
            operation.tool,
            operation.key,
            operation.input_digest,
        )? {
            return Ok((op, false));
        }
        if !musaic_db::agent::get_pairing(conn, operation.principal_id)
            .map_err(run::db_err)?
            .is_some_and(|p| p.revoked_at.is_none())
        {
            return Err(ToolError::new(
                ToolErrorCode::Forbidden,
                "the pairing was revoked",
            ));
        }
        db::insert(conn, operation, approval).map_err(run::db_err)?;
        let op = db::get(conn, operation.id)
            .map_err(run::db_err)?
            .expect("operation inserted in the same transaction");
        Ok((op, true))
    })
}

pub(super) fn finish_on(
    conn: &Connection,
    id: &str,
    state: OperationState,
    result: Option<&OperationResult>,
    error: Option<&ToolError>,
    at: i64,
) -> Result<bool, ToolError> {
    db::finish(
        conn,
        id,
        &Finish {
            state: state.as_str(),
            result: result.map(to_json_text).as_deref(),
            error: error.map(to_json_text).as_deref(),
            execution_failed: false,
            now: at,
        },
    )
    .map_err(run::db_err)
}

pub(super) fn finish_operation(
    app: &App,
    id: &str,
    state: OperationState,
    result: Option<OperationResult>,
    error: Option<ToolError>,
) {
    let _ = transact(app, |conn| {
        finish_on(conn, id, state, result.as_ref(), error.as_ref(), now(app))
    });
    app.0.agent.forget_cancellation_flag(id);
}

pub(super) fn progress(app: &App, id: &str, counts: Counts, partial: Option<OperationResult>) {
    let _ = transact(app, |conn| {
        db::progress(
            conn,
            id,
            now(app),
            &to_json_text(&counts),
            partial.as_ref().map(to_json_text).as_deref(),
        )
        .map_err(run::db_err)
    });
}

/// Running writes are never replayed on startup. Confirmed checkpoints are
/// kept, and an interrupted apply carries its backed-up change ID.
pub(super) fn recover(app: &App, conn: &Connection) -> Result<(), ToolError> {
    let tx = musaic_db::immediate(conn).map_err(run::db_err)?;
    db::expire_orphaned_approvals(&tx, &app.now_iso()).map_err(run::db_err)?;
    for (id, tool, checkpoint) in db::interrupted(&tx).map_err(run::db_err)? {
        let mut error = ToolError::new(
            ToolErrorCode::Unavailable,
            if tool == "musaic_fix_apply" {
                "Musaic stopped during apply; review confirmed changes and preview remaining edits again"
            } else {
                "Musaic stopped before this operation finished; its confirmed progress is available"
            },
        );
        let mut result = None;
        if let Some(change_id) = checkpoint {
            error = error.with_detail("changeId", &change_id);
            if let Some(change) =
                musaic_db::agent::get_change(&tx, &change_id).map_err(run::db_err)?
                && let Some(apply) = stored::<ApplyResult>(change.outcome)?
            {
                result = Some(OperationResult {
                    apply: Some(apply),
                    ..Default::default()
                });
                if change.state == "applied" {
                    finish_on(
                        &tx,
                        &id,
                        OperationState::Succeeded,
                        result.as_ref(),
                        None,
                        now(app),
                    )?;
                    continue;
                }
            }
        }
        finish_on(
            &tx,
            &id,
            OperationState::Failed,
            result.as_ref(),
            Some(&error),
            now(app),
        )?;
    }
    db::prune_tasks(&tx, now(app)).map_err(run::db_err)?;
    tx.commit().map_err(run::db_err)
}
