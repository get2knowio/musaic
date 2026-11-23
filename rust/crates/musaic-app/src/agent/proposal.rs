//! `musaic_fix_preview`: validate a proposed change against the album's
//! current revision and store it as an immutable proposal. Nothing is written
//! to the collection.

use std::time::Duration;

use musaic_commands::auth::random_hex;
use musaic_commands::tools::fix::{FixPreviewInput, FixPreviewOutput};
use musaic_commands::{ToolError, ToolErrorCode};
use musaic_services::{agent_proposal, agent_read};

use super::Principal;
use super::operations::guard;
use super::run::conn;
use crate::App;

/// How long a proposal, and an approval card, stay valid.
pub const PROPOSAL_TTL: Duration = Duration::from_secs(30 * 60);

pub fn fix_preview(
    app: &App,
    principal: &Principal,
    input: &FixPreviewInput,
) -> Result<FixPreviewOutput, ToolError> {
    // A restore swaps the database file; do not read through it.
    let _guard = guard(app)?;
    let mut conn = conn(app)?;
    let lib = app.library_dir();
    let row = agent_read::cached_row(&conn, lib.as_deref(), &input.album_id)?;
    if row.missing_since.as_deref().is_some_and(|s| !s.is_empty()) {
        return Err(ToolError::new(
            ToolErrorCode::MissingAlbum,
            "the album is missing",
        ));
    }
    if agent_read::revision_of(&row) != input.base_revision {
        return Err(ToolError::new(
            ToolErrorCode::StaleRevision,
            "the album changed since baseRevision; read it again",
        ));
    }
    let plan = app.with_apply_env(&mut conn, |env| {
        agent_proposal::plan(env, &row, &input.base_revision, &input.proposed_patch)
    })?;
    let now = app.now();
    let expires = now + PROPOSAL_TTL;
    let id = format!("pr_{}", random_hex(8));
    let stored = musaic_db::agent::ProposalRow {
        id: id.clone(),
        pairing_id: principal.pairing_id.clone(),
        album_id: input.album_id.clone(),
        base_revision: input.base_revision.clone(),
        patch: plan.patch_text.clone(),
        digest: plan.digest.clone(),
        kinds: super::to_json_text(&plan.kinds.iter().map(|k| k.as_str()).collect::<Vec<_>>()),
        diff: super::to_json_text(&plan.diff),
        created_at: musaic_db::backup::iso_seconds(now),
        expires_at: musaic_db::backup::iso_seconds(expires),
    };
    musaic_db::agent::insert_proposal(&conn, &stored).map_err(|e| {
        musaic_log::error(
            "musaic.agent",
            &format!("a proposal could not be stored: {e}"),
        );
        ToolError::new(ToolErrorCode::Internal, "the proposal could not be stored")
    })?;
    Ok(FixPreviewOutput {
        proposal_id: id,
        digest: plan.digest,
        diff: plan.diff,
        effects: plan.effects,
        kinds: plan.kinds,
        expires_at: stored.expires_at,
    })
}
