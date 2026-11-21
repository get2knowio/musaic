//! The agent read tools over the services' read functions. None of them takes
//! the work guard, does network work or writes the database (FR-053).

use std::collections::BTreeMap;

use musaic_commands::auth::config_dir_id;
use musaic_commands::tools::album::{AlbumGetInput, AlbumGetOutput};
use musaic_commands::tools::capabilities::{CapabilitiesOutput, ConfiguredRoot};
use musaic_commands::tools::findings::{FindingsListInput, FindingsListOutput};
use musaic_commands::tools::library::{LibraryListInput, LibraryListOutput};
use musaic_commands::tools::resolution::{ResolutionGetInput, ResolutionGetOutput};
use musaic_commands::tools::{DEFAULT_LIMIT, Validate};
use musaic_commands::{
    CONTRACT_VERSION, PROTOCOL_PROFILES, PermissionKind, ToolError, ToolErrorCode, kinds,
};
use musaic_services::{agent_read, library_ops};

use super::Principal;
use crate::App;
use crate::ops::agent::policy_of;

fn internal(e: impl std::fmt::Display) -> ToolError {
    musaic_log::error("musaic.agent", &format!("agent read failed: {e}"));
    ToolError::new(ToolErrorCode::Internal, "the library could not be read")
}

fn conn(app: &App) -> Result<rusqlite::Connection, ToolError> {
    musaic_db::open_connection(app.db_path()).map_err(internal)
}

/// The configured library folder, resolved the way an album id is.
fn resolved_library(app: &App) -> Option<String> {
    app.library_dir().filter(|l| !l.is_empty()).map(|l| {
        musaic_scan::py_resolve(std::path::Path::new(&l))
            .to_string_lossy()
            .into_owned()
    })
}

pub fn capabilities(app: &App, principal: &Principal) -> Result<CapabilitiesOutput, ToolError> {
    let conn = conn(app)?;
    let stored = policy_of(&conn, &principal.pairing_id)
        .map_err(|_| ToolError::new(ToolErrorCode::Internal, "the policy could not be read"))?;
    let policy: BTreeMap<PermissionKind, musaic_commands::Decision> = PermissionKind::ALL
        .into_iter()
        .map(|k| {
            let d = if principal.scopes.contains(&k) {
                kinds::policy_decision(&stored, k)
            } else {
                musaic_commands::Decision::Refuse
            };
            (k, d)
        })
        .collect();
    let configured_roots = resolved_library(app)
        .map(|lib| {
            let name = std::path::Path::new(&lib)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            vec![ConfiguredRoot {
                root_id: config_dir_id(&lib),
                name,
            }]
        })
        .unwrap_or_default();
    Ok(CapabilitiesOutput {
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        contract_version: CONTRACT_VERSION.to_string(),
        registry_digest: musaic_commands::catalog::registry_digest(),
        instance_id: app.0.agent.instance_id().to_string(),
        scopes: principal.scopes.clone(),
        policy,
        profiles: PROTOCOL_PROFILES.iter().map(|p| p.to_string()).collect(),
        configured_roots,
    })
}

pub fn library_list(app: &App, input: &LibraryListInput) -> Result<LibraryListOutput, ToolError> {
    input.validate()?;
    let mut conn = conn(app)?;
    agent_read::list_albums_page(
        &mut conn,
        app.library_dir().as_deref(),
        input.cursor.as_deref(),
        input.limit.unwrap_or(DEFAULT_LIMIT),
        input.query.as_deref(),
    )
}

pub fn album_get(app: &App, input: &AlbumGetInput) -> Result<AlbumGetOutput, ToolError> {
    let conn = conn(app)?;
    agent_read::album_get(&conn, app.library_dir().as_deref(), &input.album_id)
}

pub fn resolution_get(
    app: &App,
    input: &ResolutionGetInput,
) -> Result<ResolutionGetOutput, ToolError> {
    let conn = conn(app)?;
    let stored = library_ops::stored_album_resolution(
        &conn,
        app.library_dir().as_deref(),
        &app.0.resolver,
        &input.album_id,
    )
    .map_err(|e| match e {
        musaic_services::OpError::NotFound { .. } => {
            ToolError::new(ToolErrorCode::NotFound, "no such album")
        }
        musaic_services::OpError::NoLibrary { .. } => {
            ToolError::new(ToolErrorCode::NoLibrary, "no library folder is configured")
        }
        other => internal(other.message()),
    })?;
    Ok(agent_read::resolution_output(stored))
}

pub fn findings_list(
    app: &App,
    input: &FindingsListInput,
) -> Result<FindingsListOutput, ToolError> {
    input.validate()?;
    let conn = conn(app)?;
    agent_read::findings_page(
        &conn,
        app.library_dir().as_deref(),
        input.album_id.as_deref(),
        input.cursor.as_deref(),
        input.limit.unwrap_or(DEFAULT_LIMIT),
    )
}
