//! App-owned delegated work. SQLite commits each transition and its audit and
//! notification together. No guard survives a tool call.

use std::time::{Duration, SystemTime};

use musaic_commands::auth::{hex_decode, random_hex, work_lease_token};
use musaic_commands::tools::work::*;
use musaic_commands::{
    Decision, PermissionKind, ToolCall, ToolError, ToolErrorCode, ToolOutput, Validate,
};
use musaic_db::delegated_work::{
    self as db, Claim, ClaimMany, Completion, Lease, List, Progress, QueueResult, Refusal, WorkRow,
};
use musaic_events::{AgentWhat, Event};
use rusqlite::Connection;

use crate::App;
use crate::agent::{Principal, run, to_json_text};
use crate::ops::agent::config_id;
use crate::research;

pub(crate) fn now_ms(app: &App) -> i64 {
    app.now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

pub(crate) fn uuid() -> String {
    let hex = random_hex(16);
    format!(
        "{}-{}-4{}-8{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[13..16],
        &hex[17..20],
        &hex[20..]
    )
}

fn iso(ms: i64) -> String {
    musaic_db::backup::iso_seconds(SystemTime::UNIX_EPOCH + Duration::from_millis(ms.max(0) as u64))
}

fn database_error(error: impl std::fmt::Display) -> ToolError {
    musaic_log::error(
        "musaic.agent",
        &format!("delegated work database step failed: {error}"),
    );
    ToolError::new(
        ToolErrorCode::Internal,
        "the research queue could not be read or written",
    )
}

fn checked<T>(r: QueueResult<T>) -> Result<T, ToolError> {
    r.map_err(database_error)?.map_err(|refusal| {
        let (code, message) = match refusal {
            Refusal::NotFound => (ToolErrorCode::NotFound, "no such work"),
            Refusal::AlreadyClaimed => (
                ToolErrorCode::AlreadyClaimed,
                "this work already has an active claim",
            ),
            Refusal::LeaseExpired => (ToolErrorCode::LeaseExpired, "the work lease expired"),
            Refusal::StaleGeneration => (
                ToolErrorCode::StaleGeneration,
                "the work attempt is no longer current",
            ),
            Refusal::StaleContext => (
                ToolErrorCode::StaleContext,
                "the library, album, or queued work changed",
            ),
            Refusal::InvalidResult => (
                ToolErrorCode::InvalidResult,
                "the work result could not be accepted",
            ),
            Refusal::PolicyDenied => (
                ToolErrorCode::PolicyDenied,
                "the requested transition is unavailable",
            ),
            Refusal::RateLimited => (
                ToolErrorCode::RateLimited,
                "this work is waiting for its next retry",
            ),
            Refusal::IdempotencyConflict => (
                ToolErrorCode::IdempotencyConflict,
                "that key was already used for different input",
            ),
        };
        ToolError::new(code, message)
    })
}

fn summary(row: WorkRow) -> Result<WorkSummary, ToolError> {
    let corrupt = || ToolError::new(ToolErrorCode::Internal, "the work record could not be read");
    Ok(WorkSummary {
        work_id: row.id,
        album_id: row.album_id,
        kind: WorkKind::parse(&row.kind).ok_or_else(corrupt)?,
        state: WorkState::parse(&row.state).ok_or_else(corrupt)?,
        revision: row.revision,
        attempts: row.attempts,
        max_attempts: row.max_attempts,
        created_at: iso(row.created_at),
        updated_at: iso(row.updated_at),
        available_at: iso(row.available_at),
        result_id: row.result_id,
        batch_id: row.batch_id,
    })
}

struct Session<'a> {
    app: &'a App,
    principal: &'a Principal,
    conn: Connection,
}

impl Session<'_> {
    fn list(&self, input: WorkListInput) -> Result<ToolOutput, ToolError> {
        input.validate()?;
        let epoch = db::epoch(&self.conn).map_err(database_error)?;
        let state = input.state.map(WorkState::as_str);
        let kind = input.kind.map(WorkKind::as_str);
        let batch = input.batch_id.as_deref();
        // A filter-free cursor keeps the key it had before batches existed.
        let key = match batch {
            None => research::digest(&to_json_text(&(
                &self.principal.pairing_id,
                epoch,
                state,
                kind,
            ))),
            Some(batch) => research::digest(&to_json_text(&(
                &self.principal.pairing_id,
                epoch,
                state,
                kind,
                batch,
            ))),
        };
        let after = match input.cursor {
            None => None,
            Some(ref cursor) => {
                let Some((id, digest)) = cursor.split_once(':') else {
                    return Err(cursor_error());
                };
                if !is_uuid(id) || digest != key {
                    return Err(cursor_error());
                }
                Some(id)
            }
        };
        let limit = input.limit() as usize;
        let mut rows = db::list(
            &self.conn,
            List {
                principal_id: &self.principal.pairing_id,
                state,
                kind,
                batch_id: batch,
                after,
                limit: i64::from(input.limit()) + 1,
            },
        )
        .map_err(database_error)?;
        let next_cursor = (rows.len() > limit).then(|| format!("{}:{key}", rows[limit - 1].id));
        rows.truncate(limit);
        let items = rows.into_iter().map(summary).collect::<Result<_, _>>()?;
        Ok(ToolOutput::WorkList(WorkListOutput { items, next_cursor }))
    }

    fn visible(&self, work_id: &str) -> Result<WorkRow, ToolError> {
        db::get(&self.conn, work_id)
            .map_err(database_error)?
            .filter(|row| db::visible(row, &self.principal.pairing_id))
            .ok_or_else(|| ToolError::new(ToolErrorCode::NotFound, "no such work"))
    }

    fn get(&self, input: WorkGetInput) -> Result<ToolOutput, ToolError> {
        input.validate()?;
        let row = self.visible(&input.work_id)?;
        if db::epoch(&self.conn).map_err(database_error)? != row.library_epoch {
            return Err(ToolError::new(
                ToolErrorCode::StaleContext,
                "this work is from an earlier library epoch",
            ));
        }
        let context: WorkContext = serde_json::from_str(&row.input_json).map_err(|_| {
            ToolError::new(
                ToolErrorCode::StaleContext,
                "the work has no current context",
            )
        })?;
        Ok(ToolOutput::WorkGet(WorkGetOutput {
            schema_version: SCHEMA_VERSION,
            library_epoch: row.library_epoch.clone(),
            base_revision: row.base_revision.clone(),
            input_digest: row.input_digest.clone(),
            result_schema: "musaic_work_complete#/properties/result".to_owned(),
            context,
            work: summary(row)?,
        }))
    }

    /// The pairing's secret, from which every lease token is derived.
    fn lease_secret(&self) -> Result<Vec<u8>, ToolError> {
        musaic_secrets::agent::get_secret(
            self.app.0.agent_secrets.as_ref(),
            &config_id(self.app),
            &self.principal.pairing_id,
        )
        .ok()
        .flatten()
        .and_then(|secret| hex_decode(&secret))
        .ok_or_else(|| ToolError::new(ToolErrorCode::PolicyDenied, "the pairing cannot claim work"))
    }

    fn claim(&self, input: WorkClaimInput) -> Result<ToolOutput, ToolError> {
        input.validate()?;
        let epoch = db::epoch(&self.conn).map_err(database_error)?;
        let secret = self.lease_secret()?;
        let token = work_lease_token(
            &secret,
            &self.principal.pairing_id,
            &epoch,
            &input.work_id,
            &input.idempotency_key,
        );
        let digest = research::digest(&to_json_text(&input));
        let claim = checked(db::claim_with_context(
            &self.conn,
            Claim {
                work_id: &input.work_id,
                principal_id: &self.principal.pairing_id,
                idempotency_key: &input.idempotency_key,
                request_digest: &digest,
                lease_token_hash: &research::digest(&token),
                now: now_ms(self.app),
            },
            |conn, row| Ok(research::current(self.app, conn, row).is_ok()),
        ))?;
        Ok(ToolOutput::WorkClaim(WorkClaimOutput {
            work_id: claim.work_id,
            claim_generation: claim.claim_generation,
            lease_token: token,
            lease_expires_at: iso(claim.lease_expires_at),
            input_digest: claim.input_digest,
        }))
    }

    /// Up to `max` items in one call, each with its own lease (FR-017). The
    /// token of each is derived from the call's key and the item, as a single
    /// claim's is, so a replay answers the same tokens without storing any.
    fn claim_many(&self, input: WorkClaimManyInput) -> Result<ToolOutput, ToolError> {
        input.validate()?;
        let epoch = db::epoch(&self.conn).map_err(database_error)?;
        let secret = self.lease_secret()?;
        let token = |work_id: &str| {
            work_lease_token(
                &secret,
                &self.principal.pairing_id,
                &epoch,
                work_id,
                &input.idempotency_key,
            )
        };
        let digest = research::digest(&to_json_text(&input));
        let claimed = checked(db::claim_many_with_context(
            &self.conn,
            ClaimMany {
                principal_id: &self.principal.pairing_id,
                idempotency_key: &input.idempotency_key,
                request_digest: &digest,
                batch_id: input.batch_id.as_deref(),
                kind: input.kind.map(WorkKind::as_str),
                max: i64::from(input.max()),
                now: now_ms(self.app),
            },
            |work_id| research::digest(&token(work_id)),
            |conn, row| Ok(research::current(self.app, conn, row).is_ok()),
        ))?;
        let claims = claimed
            .claims
            .into_iter()
            .map(|c| WorkClaimOutput {
                lease_token: token(&c.work_id),
                work_id: c.work_id,
                claim_generation: c.claim_generation,
                lease_expires_at: iso(c.lease_expires_at),
                input_digest: c.input_digest,
            })
            .collect();
        Ok(ToolOutput::WorkClaimMany(WorkClaimManyOutput { claims }))
    }

    /// Renews each lease as `musaic_work_renew` does; a refused one does not
    /// stop the others.
    fn renew_many(&self, input: WorkRenewManyInput) -> Result<ToolOutput, ToolError> {
        input.validate()?;
        let verifiers: Vec<String> = input
            .leases
            .iter()
            .map(|l| research::digest(&l.lease_token))
            .collect();
        let leases: Vec<Lease<'_>> = input
            .leases
            .iter()
            .zip(&verifiers)
            .map(|(l, verifier)| self.lease(l.lease(), verifier))
            .collect();
        let answers = db::renew_many(&self.conn, &leases, |conn, row| {
            Ok(research::current(self.app, conn, row).is_ok())
        })
        .map_err(database_error)?;
        let mut out = WorkRenewManyOutput {
            renewed: Vec::new(),
            refused: Vec::new(),
        };
        for (lease, answer) in input.leases.iter().zip(answers) {
            match answer {
                Ok(expiry) => out.renewed.push(WorkRenewOutput {
                    work_id: lease.work_id.clone(),
                    lease_expires_at: iso(expiry),
                }),
                Err(refusal) => out.refused.push(WorkRenewRefusal {
                    work_id: lease.work_id.clone(),
                    code: checked::<()>(Ok(Err(refusal)))
                        .expect_err("a refusal is an error")
                        .code,
                }),
            }
        }
        Ok(ToolOutput::WorkRenewMany(out))
    }

    fn lease<'a>(&'a self, input: LeaseRef<'a>, verifier: &'a str) -> Lease<'a> {
        Lease {
            work_id: input.work_id,
            principal_id: &self.principal.pairing_id,
            claim_generation: input.claim_generation,
            lease_token_hash: verifier,
            now: now_ms(self.app),
        }
    }

    fn leased(
        &self,
        input: LeaseRef<'_>,
        change: impl FnOnce(Lease<'_>) -> Result<ToolOutput, ToolError>,
    ) -> Result<ToolOutput, ToolError> {
        let verifier = research::digest(input.lease_token);
        let lease = self.lease(input, &verifier);
        let row = checked(db::check_lease(&self.conn, lease))?;
        research::current(self.app, &self.conn, &row)?;
        change(self.lease(input, &verifier))
    }

    fn complete(&self, input: WorkCompleteInput) -> Result<ToolOutput, ToolError> {
        input.validate()?;
        let verifier = research::digest(&input.lease_token);
        let result_json = to_json_text(&input.result);
        let request_digest = research::digest(&to_json_text(&(
            &input.work_id,
            input.claim_generation,
            &verifier,
            &input.input_digest,
            &result_json,
        )));
        let replay = || {
            checked(db::completion_replay(
                &self.conn,
                &self.principal.pairing_id,
                &input.idempotency_key,
                &request_digest,
            ))
        };
        if let Some(first) = replay()? {
            return Ok(ToolOutput::WorkStatus(summary(first)?));
        }
        let verified = (|| {
            let row = checked(db::check_lease(
                &self.conn,
                self.lease(input.lease(), &verifier),
            ))?;
            research::verify_result(self.app, &self.conn, &row, &input.result)
        })();
        let validation = match verified {
            Ok(v) => v,
            Err(error) => {
                // The same request could have committed while we were validating.
                return match replay()? {
                    Some(first) => Ok(ToolOutput::WorkStatus(summary(first)?)),
                    None => Err(error),
                };
            }
        };
        check_policy(self.app, self.principal, RESEARCH_SCOPE)?;
        self.status(db::complete_with_context(
            &self.conn,
            self.lease(input.lease(), &verifier),
            Completion {
                key: &input.idempotency_key,
                request_digest: &request_digest,
                input_digest: &input.input_digest,
                id: &uuid(),
                digest: &research::digest(&result_json),
                result_json: &result_json,
                validation,
            },
            |conn, row| Ok(research::current(self.app, conn, row).is_ok()),
        ))
    }

    fn status(&self, result: QueueResult<WorkRow>) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::WorkStatus(summary(checked(result)?)?))
    }
}

fn cursor_error() -> ToolError {
    ToolError::new(
        ToolErrorCode::CursorStale,
        "start again without this cursor",
    )
}

const READ_SCOPE: &[PermissionKind] = &[PermissionKind::LibraryRead];
const RESEARCH_SCOPE: &[PermissionKind] =
    &[PermissionKind::LibraryRead, PermissionKind::ProposalSubmit];
const CANCEL_SCOPE: &[PermissionKind] = &[PermissionKind::LibraryRead, PermissionKind::WorkCancel];

fn check_policy(
    app: &App,
    principal: &Principal,
    scopes: &[PermissionKind],
) -> Result<(), ToolError> {
    match run::authorize(app, principal, scopes) {
        Ok(Decision::Allow) => Ok(()),
        Ok(_)
        | Err(ToolError {
            code: ToolErrorCode::Forbidden,
            ..
        }) => Err(ToolError::new(
            ToolErrorCode::PolicyDenied,
            "this pairing cannot perform that research action",
        )),
        Err(error) => Err(error),
    }
}

pub(crate) fn handles(call: &ToolCall) -> bool {
    call.tool_id().starts_with("musaic_work_")
}

pub(crate) fn call(
    app: &App,
    principal: &Principal,
    call: ToolCall,
) -> Result<ToolOutput, ToolError> {
    if app.is_shutting_down() {
        return Err(ToolError::new(
            ToolErrorCode::Unavailable,
            "Musaic is closing",
        ));
    }
    let _guard = app
        .gate()
        .begin_work()
        .map_err(|_| ToolError::new(ToolErrorCode::Restoring, "the library is being replaced"))?;
    let tool = call.tool_id();
    let result = dispatch(app, principal, call);
    run::record_activity(
        app,
        &principal.pairing_id,
        &principal.pairing_name,
        tool,
        &match &result {
            Ok(_) => "ok".to_owned(),
            Err(error) => format!("refused:{}", error.code),
        },
    );
    result
}

fn dispatch(app: &App, principal: &Principal, call: ToolCall) -> Result<ToolOutput, ToolError> {
    let scope = match call {
        ToolCall::WorkList(_) | ToolCall::WorkGet(_) => READ_SCOPE,
        ToolCall::WorkCancel(_) => CANCEL_SCOPE,
        _ => RESEARCH_SCOPE,
    };
    check_policy(app, principal, scope)?;
    let s = Session {
        app,
        principal,
        conn: musaic_db::open_connection(app.db_path()).map_err(database_error)?,
    };
    db::expire(&s.conn, now_ms(app)).map_err(database_error)?;
    let output = match call {
        ToolCall::WorkList(i) => s.list(i),
        ToolCall::WorkGet(i) => s.get(i),
        ToolCall::WorkClaim(i) => s.claim(i),
        ToolCall::WorkClaimMany(i) => s.claim_many(i),
        ToolCall::WorkRenewMany(i) => s.renew_many(i),
        ToolCall::WorkRenew(i) => {
            i.validate()?;
            s.leased(i.lease(), |lease| {
                let expiry = checked(db::renew(&s.conn, lease))?;
                Ok(ToolOutput::WorkRenew(WorkRenewOutput {
                    work_id: i.work_id.clone(),
                    lease_expires_at: iso(expiry),
                }))
            })
        }
        ToolCall::WorkProgress(i) => {
            i.validate()?;
            s.leased(i.lease(), |lease| {
                s.status(db::progress(
                    &s.conn,
                    lease,
                    Progress {
                        sequence: i.sequence,
                        stage: &i.stage,
                        completed: i.completed,
                        total: i.total,
                        message: i.message.as_deref(),
                    },
                ))
            })
        }
        ToolCall::WorkComplete(i) => s.complete(i),
        ToolCall::WorkFail(i) => {
            i.validate()?;
            s.leased(i.lease(), |lease| {
                s.status(db::fail(&s.conn, lease, &i.code, &i.message, i.retryable))
            })
        }
        ToolCall::WorkRelease(i) => {
            i.validate()?;
            s.leased(i.lease(), |lease| {
                s.status(db::release(&s.conn, lease, &i.reason))
            })
        }
        ToolCall::WorkCancel(i) => {
            i.validate()?;
            s.status(db::cancel(
                &s.conn,
                &i.work_id,
                &principal.pairing_id,
                i.expected_revision,
                now_ms(app),
            ))
        }
        _ => unreachable!("only work tools reach this dispatch"),
    };
    publish_pending(app, &s.conn);
    output
}

/// How many outbox rows one `AgentStateChanged` event names at most.
pub(crate) const PUBLISH_PAGE: i64 = 128;

/// Publishes every committed, unsent outbox row: one `AgentStateChanged`
/// per family (`work`, `operations`) per page of up to [`PUBLISH_PAGE`]
/// rows, not one per row, so a bulk run's hundreds of requests reach the
/// relay as a handful of events. Each page is acknowledged after it is
/// published.
pub(crate) fn publish_pending(app: &App, conn: &Connection) {
    let result = (|| -> musaic_db::Result<()> {
        let mut woke = false;
        loop {
            let page = db::outbox(conn, PUBLISH_PAGE)?;
            let Some(last) = page.last() else {
                break;
            };
            let mut work: Vec<String> = Vec::new();
            let mut operations: Vec<String> = Vec::new();
            for row in &page {
                match (&row.work_id, &row.operation_id) {
                    (Some(id), _) => work.push(id.clone()),
                    (_, Some(id)) => operations.push(id.clone()),
                    _ => {}
                }
            }
            for (what, ids) in [
                (AgentWhat::Work, &mut work),
                (AgentWhat::Operations, &mut operations),
            ] {
                if ids.is_empty() {
                    continue;
                }
                // One id per row as before, without repeats within a page.
                let mut seen = std::collections::HashSet::new();
                ids.retain(|id| seen.insert(id.clone()));
                app.bus().publish(&Event::AgentStateChanged {
                    what,
                    ids: std::mem::take(ids),
                });
                woke |= what == AgentWhat::Work;
            }
            db::ack_outbox(conn, &last.library_epoch, last.sequence)?;
            if (page.len() as i64) < PUBLISH_PAGE {
                break;
            }
        }
        if woke {
            app.ai_jobs_changed();
            // Wake parked event polls; they read the table, not this batch.
            app.0.agent.notify_events();
        }
        Ok(())
    })();
    if let Err(error) = result {
        let _ = database_error(error);
    }
}

pub(crate) fn tick(app: &App) {
    let Ok(_guard) = app.gate().begin_work() else {
        return;
    };
    let result = (|| -> musaic_db::Result<()> {
        let conn = musaic_db::open_connection(app.db_path())?;
        db::expire(&conn, now_ms(app))?;
        publish_pending(app, &conn);
        crate::agent::events::prune(app, &conn);
        Ok(())
    })();
    if let Err(error) = result {
        let _ = database_error(error);
    }
}
