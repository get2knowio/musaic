//! Changes and compute an agent asks for: apply a proposal, start a scan, a
//! check run or an analysis, poll and cancel them, and what the desktop user
//! decides about the ones that wait for approval.
//!
//! The order of an apply is fixed: authorization, then (at the moment of
//! running) revalidation of everything the proposal rested on, then the
//! before-image, then the existing apply pipeline, then the rename service.
//! Anything that fails before the before-image is recorded writes nothing.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use musaic_commands::auth::{hex_encode, random_hex};
use musaic_commands::tools::analysis::{AnalysisKind, AnalysisStartInput};
use musaic_commands::tools::checks::ChecksStartInput;
use musaic_commands::tools::fix::{FixApplyInput, FixApplyOutput, ProposedPatch};
use musaic_commands::tools::operation::{
    AnalysisItem, ApplyResult, Cancellation, CheckRunSummary, Counts, FileOutcome, FileStatus,
    OperationCancelInput, OperationCancelOutput, OperationGetInput, OperationGetOutput,
    OperationHandle, OperationResult, OperationState,
};
use musaic_commands::tools::scan::ScanStartInput;
use musaic_commands::{Decision, PermissionKind, ToolError, ToolErrorCode};
use musaic_events::{AgentWhat, Event};
use musaic_services::agent_changes::{self, ChangeInput};
use musaic_services::agent_proposal::{self, apply_parts};
use musaic_services::agent_read::{cached_row, file_facts, revision_of};
use musaic_services::apply::{ApplyEvent, ApplyRequest, TagEvent, apply_album_changes};
use musaic_services::gate::{Busy, WorkKind};
use musaic_services::library_ops::{RenameParts, rename_album_to_templates};
use sha2::{Digest, Sha256};

use super::proposal::PROPOSAL_TTL;
use super::{Operation, OperationSnapshot, Principal, new_operation_id, parse_kinds, to_json_text};
use crate::App;

const LOG_TARGET: &str = "musaic.agent";

pub fn conn(app: &App) -> Result<rusqlite::Connection, ToolError> {
    musaic_db::open_connection(app.db_path()).map_err(|e| {
        musaic_log::error(
            LOG_TARGET,
            &format!("the database could not be opened: {e}"),
        );
        ToolError::new(ToolErrorCode::Internal, "the database could not be opened")
    })
}

fn db_err(e: impl std::fmt::Display) -> ToolError {
    musaic_log::error(LOG_TARGET, &format!("a database step failed: {e}"));
    ToolError::new(
        ToolErrorCode::Internal,
        "the database could not be read or written",
    )
}

/// A restore running refuses work: nothing is read through the file it swaps.
pub fn check_not_restoring(app: &App) -> Result<(), ToolError> {
    let (_, restoring) = app.0.gate.snapshot();
    if restoring {
        Err(ToolError::new(
            ToolErrorCode::Restoring,
            "a restore is running; try again when it ends",
        ))
    } else {
        Ok(())
    }
}

pub(crate) fn apply_guard(app: &App) -> Result<musaic_services::gate::WorkGuard, ToolError> {
    app.gate()
        .begin(WorkKind::Apply)
        .map_err(|busy| match busy {
            Busy::Restoring => ToolError::new(
                ToolErrorCode::Restoring,
                "a restore is running; try again when it ends",
            ),
            Busy::WorkRunning => ToolError::new(ToolErrorCode::Busy, "the app is busy"),
        })
}

fn tidy_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_control() || c.is_whitespace() {
                '_'
            } else {
                c
            }
        })
        .take(64)
        .collect()
}

/// Record one call: a row of the pairing's activity (tool, outcome, time; never
/// an argument value) and one process-log line.
pub fn record_activity(app: &App, pairing_id: &str, pairing_name: &str, tool: &str, outcome: &str) {
    app.log().info(
        LOG_TARGET,
        &format!(
            "agent pairing={} tool={tool} outcome={outcome}",
            tidy_name(pairing_name)
        ),
    );
    if let Ok(conn) = conn(app) {
        let _ = musaic_db::agent::insert_activity(&conn, pairing_id, &app.now_iso(), tool, outcome);
    }
    app.bus().publish(&Event::AgentStateChanged {
        what: AgentWhat::Activity,
        ids: vec![pairing_id.to_string()],
    });
}

fn publish_approvals(app: &App, ids: Vec<String>) {
    app.bus().publish(&Event::AgentStateChanged {
        what: AgentWhat::Approvals,
        ids,
    });
}

/// The strictest decision for `kinds`, from the scope and the policy now.
/// A pairing revoked since its token was issued (an exchange racing the
/// revoke can still hand one out) is refused here, not only at the token.
pub fn authorize(
    app: &App,
    principal: &Principal,
    kinds: &[PermissionKind],
) -> Result<Decision, ToolError> {
    let conn = conn(app)?;
    let active = musaic_db::agent::get_pairing(&conn, &principal.pairing_id)
        .map_err(db_err)?
        .is_some_and(|p| p.revoked_at.is_none());
    if !active {
        // FORBIDDEN, as `revalidate` answers a revoked pairing.
        return Err(ToolError::new(
            ToolErrorCode::Forbidden,
            "the pairing was revoked",
        ));
    }
    let policy = crate::ops::agent::policy_of(&conn, &principal.pairing_id)
        .map_err(|_| ToolError::new(ToolErrorCode::Internal, "the policy could not be read"))?;
    Ok(musaic_commands::decide(&principal.scopes, &policy, kinds))
}

pub fn refuse_forbidden(app: &App, principal: &Principal, tool: &str) -> ToolError {
    record_activity(
        app,
        &principal.pairing_id,
        &principal.pairing_name,
        tool,
        "refused:FORBIDDEN",
    );
    ToolError::new(
        ToolErrorCode::Forbidden,
        "this pairing is not allowed to do that; its scope or policy refuses it",
    )
}

fn without_paths(app: &App, text: &str, album_path: &str) -> String {
    let lib = app.library_dir().unwrap_or_default();
    let mut out = text.to_string();
    for p in [album_path, lib.as_str()]
        .into_iter()
        .filter(|p| p.len() > 1)
    {
        out = out.replace(p, "<folder>");
    }
    out
}

fn input_digest(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update((p.len() as u64).to_be_bytes());
        h.update(p.as_bytes());
    }
    hex_encode(&h.finalize())
}

fn snapshot_handle(s: &OperationSnapshot) -> OperationHandle {
    OperationHandle {
        operation_id: s.id.clone(),
        state: s.state,
        approval_id: s.approval_id.clone(),
    }
}

// ----------------------------------------------------------------- apply

struct Report {
    result: ApplyResult,
    error: Option<ToolError>,
    change_state: &'static str,
}

/// Everything an apply needs to be sure of at the moment it runs (FR-036).
struct Validated {
    row: musaic_services::albums::AlbumRow,
    patch: ProposedPatch,
    proposal: musaic_db::agent::ProposalRow,
}

/// An approved start's last check: the pairing is not revoked and no kind it
/// needs became Refuse (the user's approval stands in for Ask).
fn start_still_allowed(
    conn: &rusqlite::Connection,
    pairing_id: &str,
    kinds: &[PermissionKind],
) -> Result<(), ToolError> {
    let pairing = musaic_db::agent::get_pairing(conn, pairing_id)
        .map_err(db_err)?
        .filter(|p| p.revoked_at.is_none())
        .ok_or_else(|| ToolError::new(ToolErrorCode::Forbidden, "the pairing was revoked"))?;
    let scopes: Vec<PermissionKind> = parse_kinds(&pairing.scopes);
    let policy = crate::ops::agent::policy_of(conn, pairing_id)
        .map_err(|_| ToolError::new(ToolErrorCode::Internal, "the policy could not be read"))?;
    if musaic_commands::decide(&scopes, &policy, kinds) == Decision::Refuse {
        return Err(ToolError::new(
            ToolErrorCode::Forbidden,
            "the pairing is no longer allowed to do that",
        ));
    }
    Ok(())
}

/// The checks of FR-036, in order. Nothing is written. `decision_ok` is
/// false when the pairing's authority is gone.
fn revalidate(
    app: &App,
    conn: &rusqlite::Connection,
    pairing_id: &str,
    proposal_id: &str,
    approved: bool,
) -> Result<Validated, ToolError> {
    let pairing = musaic_db::agent::get_pairing(conn, pairing_id)
        .map_err(db_err)?
        .filter(|p| p.revoked_at.is_none())
        .ok_or_else(|| ToolError::new(ToolErrorCode::Forbidden, "the pairing was revoked"))?;
    let proposal = musaic_db::agent::get_proposal(conn, proposal_id)
        .map_err(db_err)?
        .filter(|p| p.pairing_id == pairing_id)
        .ok_or_else(|| ToolError::new(ToolErrorCode::NotFound, "no such proposal"))?;
    if proposal.expires_at <= app.now_iso() {
        return Err(ToolError::new(
            ToolErrorCode::ProposalExpired,
            "the proposal expired; preview it again",
        ));
    }
    let patch: ProposedPatch = serde_json::from_str(&proposal.patch).map_err(|_| {
        ToolError::new(
            ToolErrorCode::DigestMismatch,
            "the stored proposal is not readable",
        )
    })?;
    if agent_proposal::proposal_digest(&proposal.patch, &proposal.album_id, &proposal.base_revision)
        != proposal.digest
    {
        return Err(ToolError::new(
            ToolErrorCode::DigestMismatch,
            "the proposal does not match its digest",
        ));
    }
    // Still allowed: the scope covers every kind, and the policy has not
    // become Refuse since it was asked.
    let kinds = parse_kinds(&proposal.kinds);
    let scopes: Vec<PermissionKind> = parse_kinds(&pairing.scopes);
    let policy = crate::ops::agent::policy_of(conn, pairing_id)
        .map_err(|_| ToolError::new(ToolErrorCode::Internal, "the policy could not be read"))?;
    let decision = musaic_commands::decide(&scopes, &policy, &kinds);
    if decision == Decision::Refuse || (!approved && decision == Decision::Ask) {
        return Err(ToolError::new(
            ToolErrorCode::Forbidden,
            "the pairing is no longer allowed to do that",
        ));
    }
    let lib = app.library_dir();
    let row = cached_row(conn, lib.as_deref(), &proposal.album_id)?;
    if row.missing_since.as_deref().is_some_and(|s| !s.is_empty()) {
        return Err(ToolError::new(
            ToolErrorCode::MissingAlbum,
            "the album is missing",
        ));
    }
    // The folder and every file the change names are the ones on disc now.
    let dir = Path::new(&row.album.path);
    if !dir.is_dir() {
        return Err(ToolError::new(
            ToolErrorCode::MissingAlbum,
            "the album's folder is gone",
        ));
    }
    let names: Vec<String> = file_facts(&row.album.path)
        .into_iter()
        .map(|(n, _, _)| n)
        .collect();
    let named: BTreeSet<&String> = patch
        .tags
        .iter()
        .filter_map(|t| t.tracks.as_ref())
        .flat_map(|t| t.keys())
        .collect();
    for n in named {
        let p = dir.join(n);
        // Presence only: the identity of each file (name, size, mtime) is
        // held by the revision compared below.
        if !names.contains(n) || !p.is_file() {
            return Err(ToolError::new(
                ToolErrorCode::StaleRevision,
                "a file the change names is gone",
            ));
        }
    }
    if revision_of(&row) != proposal.base_revision {
        return Err(ToolError::new(
            ToolErrorCode::StaleRevision,
            "the album changed since the proposal was made; preview it again",
        ));
    }
    Ok(Validated {
        row,
        patch,
        proposal,
    })
}

fn status_of(c: &musaic_tags::FileChange) -> FileStatus {
    if c.error.is_some() {
        FileStatus::Failed
    } else if c.saved {
        FileStatus::Ok
    } else {
        FileStatus::Skipped
    }
}

/// Run an approved or allowed apply under the work guard: revalidate, record
/// the before-image, write through the pipeline, rename. `Err` means nothing
/// was written.
fn run_apply(
    app: &App,
    principal_id: &str,
    proposal_id: &str,
    approval_id: Option<&str>,
) -> Result<Report, ToolError> {
    let _guard = apply_guard(app)?;
    let mut conn = conn(app)?;
    let v = revalidate(app, &conn, principal_id, proposal_id, approval_id.is_some())?;
    let lib = app.library_dir();
    let names: Vec<String> = file_facts(&v.row.album.path)
        .into_iter()
        .map(|(n, _, _)| n)
        .collect();
    let parts = apply_parts(&v.patch, &names)?;
    // Derive the change again from the files as they are: the stored diff is a
    // label only.
    let plan = app.with_apply_env(&mut conn, |env| {
        agent_proposal::plan(env, &v.row, &v.proposal.base_revision, &v.patch)
    })?;
    if plan.digest != v.proposal.digest {
        return Err(ToolError::new(
            ToolErrorCode::DigestMismatch,
            "the change no longer matches its digest",
        ));
    }
    let now = app.now_iso();
    let change_id = agent_changes::record(
        &conn,
        app.config_dir(),
        &ChangeInput {
            pairing_id: principal_id,
            proposal_id,
            approval_id,
            now: &now,
        },
        &v.row,
        &v.patch,
        &plan.diff,
    )?;

    let album_id = v.row.id().to_string();
    let album_path = v.row.album.path.clone();
    let mut files: Vec<FileOutcome> = Vec::new();
    let mut cover: Option<FileOutcome> = None;
    let mut first_error: Option<String> = None;
    let mut wrote = false;
    // The pipeline's own track-number renames, for the after-image.
    let mut step1_renamed: std::collections::BTreeMap<String, String> = Default::default();
    let cover_url = match &v.patch.cover {
        None => None,
        Some(musaic_commands::tools::fix::CoverPatch::Url { url }) => Some(url.clone()),
        Some(musaic_commands::tools::fix::CoverPatch::Resolution) => {
            match agent_proposal::anchored_cover(&conn, &album_id) {
                Some(u) => Some(u),
                None => {
                    first_error = Some("the album no longer has a cover candidate".into());
                    None
                }
            }
        }
    };
    if first_error.is_none() && (v.patch.tags.is_some() || cover_url.is_some()) {
        let req = ApplyRequest {
            album_id: album_id.clone(),
            album_path: album_path.clone(),
            library_dir: Some(lib.clone().unwrap_or_default()),
            suggestions: parts
                .suggestions
                .iter()
                .map(|(k, x)| (k.clone(), Some(x.clone())))
                .collect(),
            track_suggestions: parts
                .track_suggestions
                .iter()
                .map(|(f, s)| {
                    (
                        f.clone(),
                        s.iter()
                            .map(|(k, x)| (k.clone(), Some(x.clone())))
                            .collect(),
                    )
                })
                .collect(),
            removals: musaic_tags::Removals {
                album: parts.clear.clone(),
                tracks: parts.track_clear.clone(),
            },
            cover_url,
            dry_run: false,
            ..Default::default()
        };
        app.with_apply_env(&mut conn, |env| {
            apply_album_changes(env, &req, &mut |ev| {
                match ev {
                    ApplyEvent::Tags(TagEvent::FileDone(c)) => {
                        if c.saved {
                            wrote = true;
                        }
                        files.push(FileOutcome {
                            file_name: c.filename.clone(),
                            status: status_of(&c),
                            message: c
                                .error
                                .as_deref()
                                .map(|m| without_paths(app, m, &album_path)),
                        });
                    }
                    ApplyEvent::Tags(TagEvent::FileRenamed {
                        old_filename,
                        new_filename,
                        ..
                    }) => {
                        step1_renamed.insert(old_filename, new_filename);
                    }
                    ApplyEvent::Tags(TagEvent::Done {
                        rename_failures, ..
                    }) => {
                        for f in rename_failures {
                            files.push(FileOutcome {
                                file_name: f.filename,
                                status: FileStatus::Failed,
                                message: Some(without_paths(app, &f.user_message, &album_path)),
                            });
                        }
                    }
                    ApplyEvent::CoverSaved { .. } => {
                        wrote = true;
                        cover = Some(FileOutcome {
                            file_name: "cover".into(),
                            status: FileStatus::Ok,
                            message: None,
                        });
                    }
                    ApplyEvent::Error { detail } => {
                        first_error.get_or_insert(without_paths(app, &detail, &album_path));
                    }
                    _ => {}
                }
                std::ops::ControlFlow::Continue(())
            })
        });
    }
    let tag_failed = first_error.is_some() || files.iter().any(|f| f.status == FileStatus::Failed);
    let mut renamed = false;
    let mut album_id_after = None;
    let mut rename_error: Option<String> = None;
    if v.patch.rename == Some(true) && !tag_failed {
        let now = app.now_iso();
        match rename_album_to_templates(
            RenameParts {
                conn: &mut conn,
                bus: app.bus(),
                library_dir: lib.as_deref(),
                now: &now,
                track_order: app.0.track_order,
            },
            &album_id,
        ) {
            Ok(out) => {
                renamed = out.renamed > 0 || out.detail.as_ref().is_some_and(|d| d.dir_renamed);
                if let Some(ev) = out.album_renamed.filter(|e| e.new_album_id != album_id) {
                    album_id_after = Some(ev.new_album_id);
                }
            }
            Err(e) => rename_error = Some(without_paths(app, e.message(), &album_path)),
        }
    }
    let error_text = first_error.or(rename_error).or_else(|| {
        files
            .iter()
            .find(|f| f.status == FileStatus::Failed)
            .map(|f| {
                f.message
                    .clone()
                    .unwrap_or_else(|| "a file could not be written".into())
            })
    });
    let ok = error_text.is_none();
    let change_state = if ok {
        "applied"
    } else if wrote || renamed {
        "partial"
    } else {
        "failed"
    };
    // What the change left, for the desktop's Undo to compare with later.
    if change_state != "failed"
        && let Some(before) = agent_changes::load_before(&conn, &change_id)
    {
        let final_id = album_id_after.clone().unwrap_or_else(|| album_id.clone());
        let final_path = cached_row(&conn, lib.as_deref(), &final_id)
            .map(|r| r.album.path.clone())
            .unwrap_or_else(|_| album_path.clone());
        let after =
            agent_changes::read_after(&final_id, Path::new(&final_path), &before, &step1_renamed);
        agent_changes::save_after(&conn, app.config_dir(), &change_id, &after);
    }
    let result = ApplyResult {
        files,
        cover,
        renamed,
        album_id_after,
    };
    agent_changes::finish(&conn, &change_id, change_state, &to_json_text(&result));
    Ok(Report {
        result,
        error: error_text.map(|m| {
            ToolError::new(
                ToolErrorCode::Unavailable,
                format!("not everything was applied: {m}"),
            )
        }),
        change_state,
    })
}

/// Finish an operation from the outcome of [`run_apply`]; returns its state.
fn finish_apply_operation(
    app: &App,
    op_id: &str,
    outcome: Result<Report, ToolError>,
) -> OperationState {
    match outcome {
        Ok(rep) => {
            let state = if rep.error.is_none() {
                OperationState::Succeeded
            } else {
                OperationState::Failed
            };
            app.0.agent.finish_operation(
                op_id,
                state,
                Some(OperationResult {
                    apply: Some(rep.result),
                    ..Default::default()
                }),
                rep.error,
            );
            state
        }
        Err(e) => {
            app.0
                .agent
                .finish_operation(op_id, OperationState::Failed, None, Some(e));
            OperationState::Failed
        }
    }
}

/// `musaic_fix_apply`.
pub fn fix_apply(
    app: &App,
    p: &Principal,
    input: &FixApplyInput,
) -> Result<FixApplyOutput, ToolError> {
    let hub = &app.0.agent;
    let conn = conn(app)?;
    let proposal = musaic_db::agent::get_proposal(&conn, &input.proposal_id)
        .map_err(db_err)?
        .filter(|pr| pr.pairing_id == p.pairing_id)
        .ok_or_else(|| ToolError::new(ToolErrorCode::NotFound, "no such proposal"))?;
    let digest_in = input_digest(&[&input.proposal_id, &input.digest]);
    if let Some(found) = hub.idempotent(&p.pairing_id, &input.idempotency_key, &digest_in) {
        return match found {
            Ok(op_id) => {
                let s = hub.operation(&op_id, &p.pairing_id).ok_or_else(|| {
                    ToolError::new(ToolErrorCode::NotFound, "that operation is gone")
                })?;
                Ok(FixApplyOutput {
                    operation_id: s.id,
                    state: s.state,
                    approval_id: s.approval_id,
                    result: s.result.and_then(|r| r.apply),
                })
            }
            Err(()) => Err(ToolError::new(
                ToolErrorCode::IdempotencyConflict,
                "that idempotency key was used with different input",
            )),
        };
    }
    if proposal.digest != input.digest {
        record_activity(
            app,
            &p.pairing_id,
            &p.pairing_name,
            "musaic_fix_apply",
            "refused:DIGEST_MISMATCH",
        );
        return Err(ToolError::new(
            ToolErrorCode::DigestMismatch,
            "the digest is not the proposal's",
        ));
    }
    if proposal.expires_at <= app.now_iso() {
        record_activity(
            app,
            &p.pairing_id,
            &p.pairing_name,
            "musaic_fix_apply",
            "refused:PROPOSAL_EXPIRED",
        );
        return Err(ToolError::new(
            ToolErrorCode::ProposalExpired,
            "the proposal expired; preview it again",
        ));
    }
    let kinds = parse_kinds(&proposal.kinds);
    let decision = authorize(app, p, &kinds)?;
    if decision == Decision::Refuse {
        return Err(refuse_forbidden(app, p, "musaic_fix_apply"));
    }
    check_not_restoring(app)?;
    let op_id = new_operation_id();
    let op = Operation::new(&op_id, &p.pairing_id, "musaic_fix_apply", app.now());
    hub.register_operation(op);
    hub.remember_idempotent(&p.pairing_id, &input.idempotency_key, &digest_in, &op_id);
    match decision {
        Decision::Ask => {
            let now = app.now();
            let card_id = format!("ap_{}", random_hex(8));
            let inserted = musaic_db::agent::insert_approval(
                &conn,
                &musaic_db::agent::ApprovalRow {
                    id: card_id.clone(),
                    pairing_id: p.pairing_id.clone(),
                    action: "fix_apply".into(),
                    proposal_id: Some(proposal.id.clone()),
                    request: None,
                    request_digest: proposal.digest.clone(),
                    album_revision: Some(proposal.base_revision.clone()),
                    kinds: proposal.kinds.clone(),
                    state: "pending".into(),
                    reason: None,
                    created_at: musaic_db::backup::iso_seconds(now),
                    expires_at: musaic_db::backup::iso_seconds(now + PROPOSAL_TTL),
                    decided_at: None,
                    operation_id: Some(op_id.clone()),
                },
            )
            .map_err(db_err);
            if let Err(e) = inserted {
                abandon_start(app, &p.pairing_id, &input.idempotency_key, &op_id, &e);
                return Err(e);
            }
            hub.update_operation(&op_id, |o| {
                o.state = OperationState::AwaitingApproval;
                o.approval_id = Some(card_id.clone());
            });
            record_activity(
                app,
                &p.pairing_id,
                &p.pairing_name,
                "musaic_fix_apply",
                "awaiting_approval",
            );
            publish_approvals(app, vec![card_id.clone()]);
            Ok(FixApplyOutput {
                operation_id: op_id,
                state: OperationState::AwaitingApproval,
                approval_id: Some(card_id),
                result: None,
            })
        }
        _ => {
            let outcome = run_apply(app, &p.pairing_id, &proposal.id, None);
            let outcome_text = match &outcome {
                Ok(r) if r.error.is_none() => "ok".to_string(),
                Ok(r) => format!("failed:{}", r.change_state),
                Err(e) => format!("refused:{}", e.code),
            };
            // A refusal before any write is the call's own error.
            if let Err(e) = &outcome {
                abandon_start(app, &p.pairing_id, &input.idempotency_key, &op_id, e);
                record_activity(
                    app,
                    &p.pairing_id,
                    &p.pairing_name,
                    "musaic_fix_apply",
                    &outcome_text,
                );
                return Err(e.clone());
            }
            let state = finish_apply_operation(app, &op_id, outcome);
            record_activity(
                app,
                &p.pairing_id,
                &p.pairing_name,
                "musaic_fix_apply",
                &outcome_text,
            );
            let s = hub.operation(&op_id, &p.pairing_id);
            Ok(FixApplyOutput {
                operation_id: op_id,
                state,
                approval_id: None,
                result: s.and_then(|s| s.result).and_then(|r| r.apply),
            })
        }
    }
}

// ----------------------------------------------------------------- starts

/// What a worker thread runs.
#[derive(Clone)]
pub enum Compute {
    Scan,
    Checks(ChecksStartInput),
    Analysis(AnalysisStartInput),
}

impl Compute {
    fn action(&self) -> &'static str {
        match self {
            Compute::Scan => "scan_start",
            Compute::Checks(_) => "checks_start",
            Compute::Analysis(_) => "analysis_start",
        }
    }

    fn tool(&self) -> &'static str {
        match self {
            Compute::Scan => "musaic_scan_start",
            Compute::Checks(_) => "musaic_checks_start",
            Compute::Analysis(_) => "musaic_analysis_start",
        }
    }

    fn kinds(&self) -> Vec<PermissionKind> {
        match self {
            Compute::Scan | Compute::Checks(_) => vec![PermissionKind::ComputeScan],
            Compute::Analysis(_) => vec![PermissionKind::ComputeAnalyze, PermissionKind::TagsWrite],
        }
    }

    /// The request as stored on a card, and read back on approval.
    fn request_text(&self) -> String {
        match self {
            Compute::Scan => "{}".to_string(),
            Compute::Checks(i) => to_json_text(i),
            Compute::Analysis(i) => to_json_text(i),
        }
    }

    pub fn from_card(action: &str, request: Option<&str>) -> Option<Compute> {
        match action {
            "scan_start" => Some(Compute::Scan),
            "checks_start" => serde_json::from_str(request?).ok().map(Compute::Checks),
            "analysis_start" => serde_json::from_str(request?).ok().map(Compute::Analysis),
            _ => None,
        }
    }
}

/// Spawn the worker of an operation. Refused once shutdown began; the thread
/// is joined at shutdown.
fn spawn_worker(app: &App, op_id: &str, work: Compute) -> Result<(), ToolError> {
    if app.is_shutting_down() {
        return Err(ToolError::new(
            ToolErrorCode::Unavailable,
            "Musaic is shutting down",
        ));
    }
    let app2 = app.clone();
    let id = op_id.to_string();
    let handle = std::thread::Builder::new()
        .name(format!("agent-op-{op_id}"))
        .spawn(move || worker(&app2, &id, work))
        .map_err(|_| ToolError::new(ToolErrorCode::Unavailable, "the work could not be started"))?;
    app.0.agent.keep_worker(handle);
    Ok(())
}

fn op_error(code: ToolErrorCode, message: &str) -> Option<ToolError> {
    Some(ToolError::new(code, message))
}

fn cancelled(app: &App, id: &str) -> bool {
    app.0
        .agent
        .operation_any(id)
        .is_some_and(|s| s.cancel.load(Ordering::SeqCst))
}

/// The operation's cancel flag, handed to the services so a check run stops
/// between albums and an analysis between files.
fn cancel_flag(app: &App, id: &str) -> Arc<AtomicBool> {
    app.0
        .agent
        .operation_any(id)
        .map(|s| s.cancel)
        .unwrap_or_default()
}

fn worker(app: &App, op_id: &str, work: Compute) {
    app.0.agent.set_state(op_id, OperationState::Running);
    let hub = &app.0.agent;
    let (pairing_id, tool) = hub
        .operation_any(op_id)
        .map(|s| (s.pairing_id, s.tool))
        .unwrap_or_default();
    let (state, result, error) = match work {
        Compute::Scan => run_scan(app, op_id),
        Compute::Checks(i) => run_checks(app, op_id, &i),
        Compute::Analysis(i) => run_analysis(app, op_id, &i),
    };
    hub.finish_operation(op_id, state, result, error);
    let name = conn(app)
        .ok()
        .and_then(|c| {
            musaic_db::agent::get_pairing(&c, &pairing_id)
                .ok()
                .flatten()
        })
        .map(|p| p.name)
        .unwrap_or_default();
    let outcome = match state {
        OperationState::Succeeded => "ok".to_string(),
        OperationState::Cancelled => "cancelled".to_string(),
        _ => "failed:INTERNAL".to_string(),
    };
    record_activity(app, &pairing_id, &name, tool, &outcome);
}

type Finished = (OperationState, Option<OperationResult>, Option<ToolError>);

fn run_scan(app: &App, op_id: &str) -> Finished {
    use musaic_services::ScanStatus;
    let hub = &app.0.agent;
    if let Err(e) = app.ops().scan().start(false) {
        let code = match e {
            crate::ops::OpError::Busy { .. } => ToolErrorCode::Busy,
            crate::ops::OpError::NoLibrary { .. } => ToolErrorCode::NoLibrary,
            _ => ToolErrorCode::Internal,
        };
        return (
            OperationState::Failed,
            None,
            op_error(code, "the scan could not be started"),
        );
    }
    let mut cancel_sent = false;
    let mut idle_polls = 0;
    loop {
        let st = app.ops().scan().status();
        hub.set_counts(
            op_id,
            Counts {
                done: st.scanned as u32,
                total: st.total as u32,
                failed: 0,
            },
        );
        if cancelled(app, op_id) && !cancel_sent {
            cancel_sent = true;
            app.ops().scan().cancel();
        }
        match st.status {
            ScanStatus::Running => {}
            ScanStatus::Idle if idle_polls < 50 => idle_polls += 1,
            ScanStatus::Done => {
                return (
                    OperationState::Succeeded,
                    Some(OperationResult {
                        scan_albums: Some(st.total as u32),
                        ..Default::default()
                    }),
                    None,
                );
            }
            ScanStatus::Cancelled => return (OperationState::Cancelled, None, None),
            ScanStatus::Aborted => {
                return (
                    OperationState::Failed,
                    None,
                    op_error(
                        ToolErrorCode::Unavailable,
                        "the scan was stopped as untrustworthy; nothing was saved",
                    ),
                );
            }
            _ => {
                return (
                    OperationState::Failed,
                    None,
                    op_error(ToolErrorCode::Unavailable, "the scan failed"),
                );
            }
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn run_checks(app: &App, op_id: &str, input: &ChecksStartInput) -> Finished {
    let hub = &app.0.agent;
    let ids: Vec<String> = match &input.check_ids {
        Some(ids) => ids.clone(),
        None => app
            .0
            .checks_env
            .registry
            .all_meta()
            .iter()
            .map(|m| m.id.to_string())
            .collect(),
    };
    let (scope, albums) = match &input.album_ids {
        Some(a) if !a.is_empty() => ("albums", Some(a.as_slice())),
        _ => ("all", None),
    };
    let total = ids.len() as u32;
    let (mut done, mut failed) = (0u32, 0u32);
    let mut summaries = Vec::new();
    let cancel = cancel_flag(app, op_id);
    let cancelled_run = musaic_checks::CheckError::Cancelled.to_string();
    hub.set_counts(
        op_id,
        Counts {
            done,
            total,
            failed,
        },
    );
    let stop = |summaries| {
        (
            OperationState::Cancelled,
            Some(OperationResult {
                checks: Some(summaries),
                ..Default::default()
            }),
            None,
        )
    };
    for id in &ids {
        // Between checks; inside one, the runner stops before its next album.
        if cancelled(app, op_id) {
            return stop(summaries);
        }
        let (findings, bad, stopped) = match app
            .ops()
            .checks()
            .run_cancellable(id, scope, albums, &cancel)
        {
            Ok(run) => (
                run.issue_count.max(0) as u32,
                run.status == "failed",
                run.status == "failed" && run.error.as_deref() == Some(&cancelled_run),
            ),
            Err(_) => (0, true, false),
        };
        if stopped {
            // The check stopped part-way: its findings so far, and `failed`
            // because it did not complete. Not counted as done or failed.
            summaries.push(CheckRunSummary {
                check_id: id.clone(),
                findings,
                failed: true,
            });
            return stop(summaries);
        }
        done += 1;
        failed += bad as u32;
        summaries.push(CheckRunSummary {
            check_id: id.clone(),
            findings,
            failed: bad,
        });
        hub.set_counts(
            op_id,
            Counts {
                done,
                total,
                failed,
            },
        );
    }
    let state = if failed == 0 {
        OperationState::Succeeded
    } else {
        OperationState::Failed
    };
    (
        state,
        Some(OperationResult {
            checks: Some(summaries),
            ..Default::default()
        }),
        (failed > 0).then(|| ToolError::new(ToolErrorCode::Unavailable, "some checks did not run")),
    )
}

fn run_analysis(app: &App, op_id: &str, input: &AnalysisStartInput) -> Finished {
    use crate::ops::analyze::AnalyzeOutcome as Outcome;
    use musaic_services::analyze::AnalysisKind as Kind;
    let hub = &app.0.agent;
    let total = input.album_ids.len() as u32;
    let (mut done, mut failed) = (0u32, 0u32);
    let mut items = Vec::new();
    hub.set_counts(
        op_id,
        Counts {
            done,
            total,
            failed,
        },
    );
    let conn = conn(app).ok();
    let cancel = cancel_flag(app, op_id);
    let stop = |items| {
        (
            OperationState::Cancelled,
            Some(OperationResult {
                analysis: Some(items),
                ..Default::default()
            }),
            None,
        )
    };
    for album in &input.album_ids {
        // Between albums; inside one, the analysis stops before its next file.
        if cancelled(app, op_id) {
            return stop(items);
        }
        let missing = conn
            .as_ref()
            .and_then(|c| musaic_db::albums::missing_since(c, album).ok().flatten())
            .is_some_and(|s| !s.is_empty());
        for kind in &input.kinds {
            if cancelled(app, op_id) {
                return stop(items);
            }
            let name = match kind {
                AnalysisKind::Bpm => "bpm",
                AnalysisKind::Replaygain => "replaygain",
            };
            let mut stopped = false;
            let status = if missing {
                FileStatus::Skipped
            } else {
                let k = match kind {
                    AnalysisKind::Bpm => Kind::Bpm,
                    AnalysisKind::Replaygain => Kind::ReplayGain,
                };
                match app
                    .ops()
                    .analyze()
                    .run_cancellable(album, k, &cancel, &mut |_| crate::ops::Listening::Yes)
                {
                    Ok(Outcome::Finished(s)) if s.errors == 0 => FileStatus::Ok,
                    // Stopped part-way: some files written, the album not
                    // finished (not counted as failed). None written: skipped.
                    Ok(Outcome::Cancelled(s)) => {
                        stopped = true;
                        if s.analyzed_count + s.errors == 0 {
                            FileStatus::Skipped
                        } else {
                            FileStatus::Failed
                        }
                    }
                    _ => FileStatus::Failed,
                }
            };
            if stopped {
                items.push(AnalysisItem {
                    album_id: album.clone(),
                    kind: name.into(),
                    status,
                });
                return stop(items);
            }
            if status == FileStatus::Failed {
                failed += 1;
            }
            items.push(AnalysisItem {
                album_id: album.clone(),
                kind: name.into(),
                status,
            });
        }
        done += 1;
        hub.set_counts(
            op_id,
            Counts {
                done,
                total,
                failed,
            },
        );
    }
    let state = if failed == 0 {
        OperationState::Succeeded
    } else {
        OperationState::Failed
    };
    (
        state,
        Some(OperationResult {
            analysis: Some(items),
            ..Default::default()
        }),
        (failed > 0).then(|| {
            ToolError::new(
                ToolErrorCode::Unavailable,
                "some albums could not be analysed",
            )
        }),
    )
}

/// A start that failed after its operation was registered: the operation ends
/// `failed` and its idempotency key is free again, so a retry is not handed
/// an operation that never runs.
fn abandon_start(app: &App, pairing_id: &str, key: &str, op_id: &str, e: &ToolError) {
    let hub = &app.0.agent;
    hub.forget_idempotent(pairing_id, key);
    hub.finish_operation(op_id, OperationState::Failed, None, Some(e.clone()));
}

/// Start (or queue for approval) a scan, a check run or an analysis. The same
/// idempotency key and input return the same handle; the same key with other
/// input is refused. `validate` runs after the idempotency lookup and before
/// anything is created.
pub fn start_compute(
    app: &App,
    p: &Principal,
    work: Compute,
    key: &str,
    validate: impl FnOnce() -> Result<(), ToolError>,
) -> Result<OperationHandle, ToolError> {
    let hub = &app.0.agent;
    let request = work.request_text();
    let digest_in = input_digest(&[work.tool(), &request]);
    if let Some(found) = hub.idempotent(&p.pairing_id, key, &digest_in) {
        return match found {
            Ok(op_id) => hub
                .operation(&op_id, &p.pairing_id)
                .map(|s| snapshot_handle(&s))
                .ok_or_else(|| ToolError::new(ToolErrorCode::NotFound, "that operation is gone")),
            Err(()) => Err(ToolError::new(
                ToolErrorCode::IdempotencyConflict,
                "that idempotency key was used with different input",
            )),
        };
    }
    validate()?;
    let kinds = work.kinds();
    let decision = authorize(app, p, &kinds)?;
    if decision == Decision::Refuse {
        return Err(refuse_forbidden(app, p, work.tool()));
    }
    let op_id = new_operation_id();
    hub.register_operation(Operation::new(
        &op_id,
        &p.pairing_id,
        work.tool(),
        app.now(),
    ));
    hub.remember_idempotent(&p.pairing_id, key, &digest_in, &op_id);
    if decision == Decision::Ask {
        let now = app.now();
        let card_id = format!("ap_{}", random_hex(8));
        let inserted = conn(app).and_then(|conn| {
            musaic_db::agent::insert_approval(
                &conn,
                &musaic_db::agent::ApprovalRow {
                    id: card_id.clone(),
                    pairing_id: p.pairing_id.clone(),
                    action: work.action().into(),
                    proposal_id: None,
                    request: Some(request.clone()),
                    request_digest: input_digest(&[work.action(), &request]),
                    album_revision: None,
                    kinds: to_json_text(&kinds.iter().map(|k| k.as_str()).collect::<Vec<_>>()),
                    state: "pending".into(),
                    reason: None,
                    created_at: musaic_db::backup::iso_seconds(now),
                    expires_at: musaic_db::backup::iso_seconds(now + PROPOSAL_TTL),
                    decided_at: None,
                    operation_id: Some(op_id.clone()),
                },
            )
            .map_err(db_err)
        });
        if let Err(e) = inserted {
            abandon_start(app, &p.pairing_id, key, &op_id, &e);
            return Err(e);
        }
        hub.update_operation(&op_id, |o| {
            o.state = OperationState::AwaitingApproval;
            o.approval_id = Some(card_id.clone());
        });
        record_activity(
            app,
            &p.pairing_id,
            &p.pairing_name,
            work.tool(),
            "awaiting_approval",
        );
        publish_approvals(app, vec![card_id.clone()]);
        return Ok(OperationHandle {
            operation_id: op_id,
            state: OperationState::AwaitingApproval,
            approval_id: Some(card_id),
        });
    }
    record_activity(app, &p.pairing_id, &p.pairing_name, work.tool(), "started");
    if let Err(e) = spawn_worker(app, &op_id, work) {
        abandon_start(app, &p.pairing_id, key, &op_id, &e);
        return Err(e);
    }
    Ok(OperationHandle {
        operation_id: op_id,
        state: OperationState::Running,
        approval_id: None,
    })
}

pub fn scan_start(
    app: &App,
    p: &Principal,
    input: &ScanStartInput,
) -> Result<OperationHandle, ToolError> {
    let root = input.configured_root_id.clone();
    start_compute(app, p, Compute::Scan, &input.idempotency_key, || {
        let lib = app.library_dir().filter(|l| !l.is_empty()).ok_or_else(|| {
            ToolError::new(ToolErrorCode::NoLibrary, "no library folder is configured")
        })?;
        let resolved = musaic_scan::py_resolve(Path::new(&lib))
            .to_string_lossy()
            .into_owned();
        if musaic_commands::auth::config_dir_id(&resolved) == root {
            Ok(())
        } else {
            Err(ToolError::new(
                ToolErrorCode::NotFound,
                "no such library root",
            ))
        }
    })
}

pub fn checks_start(
    app: &App,
    p: &Principal,
    input: &ChecksStartInput,
) -> Result<OperationHandle, ToolError> {
    start_compute(
        app,
        p,
        Compute::Checks(input.clone()),
        &input.idempotency_key,
        || {
            for id in input.check_ids.iter().flatten() {
                if app.0.checks_env.registry.get(id).is_none() {
                    return Err(ToolError::invalid("checkIds", "no such check"));
                }
            }
            Ok(())
        },
    )
}

pub fn analysis_start(
    app: &App,
    p: &Principal,
    input: &AnalysisStartInput,
) -> Result<OperationHandle, ToolError> {
    start_compute(
        app,
        p,
        Compute::Analysis(input.clone()),
        &input.idempotency_key,
        || Ok(()),
    )
}

// -------------------------------------------------------------- operations

fn output_of(s: OperationSnapshot) -> OperationGetOutput {
    OperationGetOutput {
        operation_id: s.id,
        tool: s.tool.to_string(),
        state: s.state,
        counts: s.counts,
        approval_id: s.approval_id,
        result: s.result,
        error: s.error,
    }
}

pub fn operation_get(
    app: &App,
    p: &Principal,
    input: &OperationGetInput,
) -> Result<OperationGetOutput, ToolError> {
    app.0
        .agent
        .operation(&input.operation_id, &p.pairing_id)
        .map(output_of)
        .ok_or_else(|| ToolError::new(ToolErrorCode::NotFound, "no such operation"))
}

pub fn operation_cancel(
    app: &App,
    p: &Principal,
    input: &OperationCancelInput,
) -> Result<OperationCancelOutput, ToolError> {
    let hub = &app.0.agent;
    let s = hub
        .operation(&input.operation_id, &p.pairing_id)
        .ok_or_else(|| ToolError::new(ToolErrorCode::NotFound, "no such operation"))?;
    let out = |state, cancellation| OperationCancelOutput {
        operation_id: s.id.clone(),
        state,
        cancellation,
    };
    if s.state.is_finished() {
        return Ok(out(s.state, Cancellation::AlreadyFinished));
    }
    if s.state == OperationState::AwaitingApproval {
        // Cancelled before the user decided: the card is denied, nothing runs.
        // Only a deny that won the card ends the operation here; if the user
        // decided first (approved: the work is starting), it is treated as
        // running below, so it never reads `cancelled` while it writes.
        let denied = s.approval_id.as_ref().is_some_and(|card| {
            conn(app).ok().is_some_and(|c| {
                musaic_db::agent::decide_approval(
                    &c,
                    card,
                    "denied",
                    Some("cancelled by assistant"),
                    &app.now_iso(),
                )
                .unwrap_or(false)
            })
        });
        if denied {
            if let Some(card) = &s.approval_id {
                publish_approvals(app, vec![card.clone()]);
            }
            hub.finish_operation(&s.id, OperationState::Cancelled, None, None);
            return Ok(out(OperationState::Cancelled, Cancellation::Requested));
        }
        let now = hub
            .operation(&input.operation_id, &p.pairing_id)
            .ok_or_else(|| ToolError::new(ToolErrorCode::NotFound, "no such operation"))?;
        if now.state.is_finished() {
            return Ok(out(now.state, Cancellation::AlreadyFinished));
        }
    }
    // An apply that has started cannot be stopped (a tag write in progress is
    // not interruptible); everything else stops at its next boundary: a scan
    // through its own cancel, a check run before its next album, an analysis
    // before its next file.
    if s.tool == "musaic_fix_apply" {
        return Ok(out(s.state, Cancellation::NotCancellable));
    }
    s.cancel.store(true, Ordering::SeqCst);
    Ok(out(s.state, Cancellation::Requested))
}

// --------------------------------------------------------------- approvals

/// Cards whose time has come become `expired` ("timed out"), and the
/// operations waiting on them end `expired`. Returns the card ids.
pub fn expire_due(app: &App) -> Vec<String> {
    let Ok(conn) = conn(app) else {
        return Vec::new();
    };
    let now = app.now_iso();
    let ids = musaic_db::agent::expire_due(&conn, &now).unwrap_or_default();
    for id in &ids {
        if let Ok(Some(card)) = musaic_db::agent::get_approval(&conn, id)
            && let Some(op) = card.operation_id
        {
            app.0.agent.finish_operation(
                &op,
                OperationState::Expired,
                None,
                Some(ToolError::new(
                    ToolErrorCode::ApprovalExpired,
                    "the approval timed out",
                )),
            );
        }
    }
    if !ids.is_empty() {
        publish_approvals(app, ids.clone());
    }
    ids
}

/// The user's decision on a card. Approving runs what was waiting (an apply
/// runs now, revalidated; a start spawns its worker); denying writes
/// nothing. `Ok(false)` when the card is no longer pending.
pub fn decide_approval(
    app: &App,
    card_id: &str,
    approve: bool,
) -> Result<ApprovalOutcome, ToolError> {
    let conn = conn(app)?;
    // A card past its time is not decidable, whether or not the ticker ran.
    expire_due(app);
    let Some(card) = musaic_db::agent::get_approval(&conn, card_id).map_err(db_err)? else {
        return Err(ToolError::new(ToolErrorCode::NotFound, "no such approval"));
    };
    let now = app.now_iso();
    let state = if approve { "approved" } else { "denied" };
    let reason = (!approve).then_some("denied by the user");
    if !musaic_db::agent::decide_approval(&conn, card_id, state, reason, &now).map_err(db_err)? {
        return Ok(ApprovalOutcome::NotPending);
    }
    let hub = &app.0.agent;
    let op_id = card.operation_id.clone().unwrap_or_default();
    let pairing_name = musaic_db::agent::get_pairing(&conn, &card.pairing_id)
        .ok()
        .flatten()
        .map(|p| p.name)
        .unwrap_or_default();
    let tool = match card.action.as_str() {
        "fix_apply" => "musaic_fix_apply",
        "scan_start" => "musaic_scan_start",
        "checks_start" => "musaic_checks_start",
        _ => "musaic_analysis_start",
    };
    if !approve {
        hub.finish_operation(
            &op_id,
            OperationState::Denied,
            None,
            Some(ToolError::new(ToolErrorCode::Denied, "the user denied it")),
        );
        record_activity(app, &card.pairing_id, &pairing_name, tool, "refused:DENIED");
        publish_approvals(app, vec![card_id.to_string()]);
        return Ok(ApprovalOutcome::Decided);
    }
    publish_approvals(app, vec![card_id.to_string()]);
    match card.action.as_str() {
        "fix_apply" => {
            hub.set_state(&op_id, OperationState::Running);
            let proposal_id = card.proposal_id.clone().unwrap_or_default();
            let outcome = run_apply(app, &card.pairing_id, &proposal_id, Some(card_id));
            let text = match &outcome {
                Ok(r) if r.error.is_none() => "ok".to_string(),
                Ok(r) => format!("failed:{}", r.change_state),
                Err(e) => format!("refused:{}", e.code),
            };
            finish_apply_operation(app, &op_id, outcome);
            record_activity(app, &card.pairing_id, &pairing_name, tool, &text);
        }
        action => {
            let Some(work) = Compute::from_card(action, card.request.as_deref()) else {
                hub.finish_operation(
                    &op_id,
                    OperationState::Failed,
                    None,
                    op_error(ToolErrorCode::Internal, "the request could not be read"),
                );
                return Ok(ApprovalOutcome::Decided);
            };
            // As an apply is revalidated: a pairing revoked, or a kind turned
            // to Refuse, since the card was made starts nothing.
            if let Err(e) = start_still_allowed(&conn, &card.pairing_id, &work.kinds()) {
                record_activity(
                    app,
                    &card.pairing_id,
                    &pairing_name,
                    tool,
                    &format!("refused:{}", e.code),
                );
                hub.finish_operation(&op_id, OperationState::Failed, None, Some(e));
                return Ok(ApprovalOutcome::Decided);
            }
            record_activity(app, &card.pairing_id, &pairing_name, tool, "started");
            if let Err(e) = spawn_worker(app, &op_id, work) {
                hub.finish_operation(&op_id, OperationState::Failed, None, Some(e));
            }
        }
    }
    Ok(ApprovalOutcome::Decided)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalOutcome {
    Decided,
    NotPending,
}
