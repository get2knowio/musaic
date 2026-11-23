//! The `agent` group: pairings and their policy, the handshake an agent's CLI
//! makes, the twelve tools, and the approval cards the desktop decides.
//!
//! Every agent route is a face over these operations, and so is a future MCP
//! server: they take typed input ([`musaic_commands::ToolCall`], parsed by
//! the face) and return typed output ([`musaic_commands::ToolOutput`]). The
//! rules are the contract's:
//!
//! * An agent is a [`Principal`] resolved from a token. Every tool call is
//!   decided from the pairing's scope and policy before anything runs
//!   ([`musaic_commands::decide`]): a kind outside the scope is refused, the
//!   strictest kind wins, and nothing the agent sends can widen either.
//! * Reads (capabilities, list, get, findings, stored resolution) take no
//!   work guard, do no network work and write nothing.
//! * Changes go through the existing apply pipeline and rename service, after
//!   a revalidation and a recorded before-image; an Ask becomes an approval
//!   card only the desktop user can decide. Denied, expired, refused,
//!   stale and cancelled requests change no file.
//! * Every start, apply and authorization refusal is recorded (pairing,
//!   tool, outcome, time; never an argument value) and logged once.
//! * Mutations publish [`musaic_events::Event::AgentStateChanged`].

use std::collections::BTreeMap;
use std::time::SystemTime;

use musaic_commands::auth::{
    ExchangeRequest, ExchangeResponse, HelloResponse, config_dir_id, hex_decode, hex_encode,
    random_hex, server_proof, verify_client_proof,
};
use musaic_commands::tools::fix::FixDiff;
use musaic_commands::{
    CONTRACT_VERSION, Decision, PROTOCOL_PROFILES, PermissionKind, ToolCall, ToolError,
    ToolErrorCode, ToolOutput,
};
use musaic_events::{AgentWhat, Event};
use sha2::{Digest, Sha256};

use super::error::OpError;
use crate::App;
pub use crate::agent::Principal;
pub use crate::agent::hosts::{HostRecord, PairingDiagnostics};

/// Pairings that may be active at once.
pub const MAX_ACTIVE_PAIRINGS: i64 = 20;
/// The log target of the agent lines.
const LOG_TARGET: &str = "musaic.agent";

/// Why an agent operation refused.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentError {
    /// A contract error: the code says what.
    Tool(ToolError),
    /// The state forbids it: 20 pairings are active, or a card is no longer
    /// pending.
    Conflict { message: String },
    /// An undo found the album changed since the assistant's change: what
    /// changed, one entry per file and field. Nothing was written.
    ChangedSince {
        message: String,
        changed: Vec<String>,
    },
    /// Any other operation failure (the database, the library).
    Op(OpError),
}

impl From<ToolError> for AgentError {
    fn from(e: ToolError) -> Self {
        AgentError::Tool(e)
    }
}

impl From<OpError> for AgentError {
    fn from(e: OpError) -> Self {
        AgentError::Op(e)
    }
}

impl From<musaic_db::Error> for AgentError {
    fn from(e: musaic_db::Error) -> Self {
        AgentError::Op(OpError::from(e))
    }
}

impl From<rusqlite::Error> for AgentError {
    fn from(e: rusqlite::Error) -> Self {
        AgentError::Op(OpError::internal(e))
    }
}

pub(crate) fn tool_err(code: ToolErrorCode, message: &str) -> AgentError {
    AgentError::Tool(ToolError::new(code, message))
}

/// A pairing as the desktop shows it. Never a secret.
#[derive(Debug, Clone, PartialEq)]
pub struct PairingRecord {
    pub id: String,
    pub name: String,
    pub scopes: Vec<PermissionKind>,
    /// The effective decision per kind (stored, else the default).
    pub policy: BTreeMap<PermissionKind, Decision>,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub revoked_at: Option<String>,
}

/// How to connect an assistant to this pairing: a command to run, never the
/// secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectHint {
    pub command: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CreatedPairing {
    pub pairing: PairingRecord,
    pub connect: ConnectHint,
}

/// One recorded call of a pairing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityRecord {
    pub at: String,
    pub tool: String,
    pub outcome: String,
}

/// An approval card, as the Review inbox shows it. The diff is a label only;
/// what is applied is derived again from the files at the moment of approval.
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalRecord {
    pub id: String,
    pub pairing_id: String,
    pub pairing_name: String,
    /// `fix_apply`, `scan_start`, `checks_start` or `analysis_start`.
    pub action: String,
    pub album_id: Option<String>,
    /// "Artist - Title" of the album the card is about, when it names one.
    pub album_label: Option<String>,
    pub kinds: Vec<PermissionKind>,
    /// `pending`, `approved`, `denied` or `expired`.
    pub state: String,
    pub reason: Option<String>,
    pub created_at: String,
    pub expires_at: String,
    pub decided_at: Option<String>,
    pub diff: Option<FixDiff>,
}

/// One recorded change an assistant made (or an undo of one), as Settings
/// lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeRecord {
    pub id: String,
    pub pairing_id: Option<String>,
    pub pairing_name: String,
    /// The album's id now (after a folder rename, the new one).
    pub album_id: String,
    pub album_label: Option<String>,
    pub created_at: String,
    /// `recorded`, `applied`, `partial`, `failed` or `undone`.
    pub state: String,
    /// What it did, in a few words (labels only).
    pub summary: String,
    /// For an undo's own record: the change it put back.
    pub undoes: Option<String>,
    /// Whether Undo would run now; when not, `reason` says why.
    pub undoable: bool,
    pub reason: Option<String>,
}

/// What an undo did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoOutcome {
    /// The change, now `undone`.
    pub change: ChangeRecord,
    /// The undo's own record (its before-image).
    pub undo_id: String,
    /// Everything was put back; when not, `message` says what was not.
    pub complete: bool,
    pub message: Option<String>,
}

/// The tool catalog and the registry's identity, as the catalog route
/// renders them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryInfo {
    /// The generated catalog text (`docs/agent-tools.json`).
    pub catalog: String,
    /// SHA-256 (hex) of `catalog`.
    pub registry_digest: String,
}

/// Which cards [`Agent::list_approvals`] lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalFilter {
    Pending,
    All,
}

/// The `agent` operations.
pub struct Agent<'a> {
    pub(super) app: &'a App,
}

pub(crate) fn conn(app: &App) -> Result<rusqlite::Connection, AgentError> {
    Ok(musaic_db::open_connection(app.db_path())?)
}

/// The id of this app's config dir: keyring accounts and the CLI's credential
/// lookup both derive from it, so two config dirs never share a pairing.
pub(crate) fn config_id(app: &App) -> String {
    let dir = app.config_dir();
    let resolved = musaic_core::config::resolve_path(dir).unwrap_or_else(|_| dir.to_path_buf());
    config_dir_id(&resolved.to_string_lossy())
}

pub(crate) fn iso(t: SystemTime) -> String {
    musaic_db::backup::iso_seconds(t)
}

fn kinds_json(kinds: &[PermissionKind]) -> String {
    let names: Vec<&str> = kinds.iter().map(|k| k.as_str()).collect();
    crate::agent::to_json_text(&names)
}

fn parse_scopes(stored: &str) -> Vec<PermissionKind> {
    crate::agent::parse_kinds(stored)
}

pub(crate) fn policy_of(
    conn: &rusqlite::Connection,
    pairing_id: &str,
) -> Result<BTreeMap<PermissionKind, Decision>, AgentError> {
    let mut out = BTreeMap::new();
    for (kind, decision) in musaic_db::agent::policies(conn, pairing_id)? {
        if let (Some(k), Some(d)) = (PermissionKind::parse(&kind), Decision::parse(&decision)) {
            out.insert(k, d);
        }
    }
    Ok(out)
}

fn record_of(
    conn: &rusqlite::Connection,
    row: musaic_db::agent::PairingRow,
) -> Result<PairingRecord, AgentError> {
    let stored = policy_of(conn, &row.id)?;
    let policy = PermissionKind::ALL
        .into_iter()
        .map(|k| (k, musaic_commands::kinds::policy_decision(&stored, k)))
        .collect();
    Ok(PairingRecord {
        scopes: parse_scopes(&row.scopes),
        id: row.id,
        name: row.name,
        policy,
        created_at: row.created_at,
        last_used_at: row.last_used_at,
        revoked_at: row.revoked_at,
    })
}

/// "Artist - Title" of an album, for a label.
pub(crate) fn album_label(
    conn: &rusqlite::Connection,
    lib: Option<&str>,
    id: &str,
) -> Option<String> {
    musaic_services::agent_read::cached_row(conn, lib, id)
        .ok()
        .map(|r| {
            let a = musaic_core::album_fields::display_artist(
                r.album.album_artist.as_deref(),
                r.album.artist.as_deref(),
                r.album.dir_artist.as_deref(),
                true,
            );
            let t = r
                .album
                .title
                .clone()
                .unwrap_or_else(|| r.album.dir_name.clone());
            if a.is_empty() {
                t
            } else {
                format!("{a} - {t}")
            }
        })
}

/// One card as the Review inbox shows it: its pairing's name, and the album
/// and diff of its proposal when it has one (labels only).
fn approval_record(
    conn: &rusqlite::Connection,
    lib: Option<&str>,
    card: musaic_db::agent::ApprovalRow,
) -> Result<ApprovalRecord, AgentError> {
    let pairing_name = musaic_db::agent::get_pairing(conn, &card.pairing_id)?
        .map(|p| p.name)
        .unwrap_or_default();
    let proposal = match &card.proposal_id {
        Some(id) => musaic_db::agent::get_proposal(conn, id)?,
        None => None,
    };
    let album_id = proposal.as_ref().map(|p| p.album_id.clone());
    let album_label = album_id
        .as_deref()
        .and_then(|id| album_label(conn, lib, id));
    Ok(ApprovalRecord {
        id: card.id,
        pairing_id: card.pairing_id,
        pairing_name,
        action: card.action,
        album_id,
        album_label,
        kinds: crate::agent::parse_kinds(&card.kinds),
        state: card.state,
        reason: card.reason,
        created_at: card.created_at,
        expires_at: card.expires_at,
        decided_at: card.decided_at,
        diff: crate::agent::diff_of(proposal.as_ref()),
    })
}

impl Agent<'_> {
    fn publish(&self, what: AgentWhat, ids: Vec<String>) {
        self.app
            .bus()
            .publish(&Event::AgentStateChanged { what, ids });
    }

    // ----------------------------------------------------------- pairings

    /// Create a pairing: a named, scoped, revocable grant. The secret is made
    /// here, goes only to the agent keyring (and, in a scratch run with memory
    /// secrets, to the dev credential file), and is never returned.
    ///
    /// Refusals, nothing changed: a name that is empty or over 64 characters
    /// once trimmed, or no scope (`InvalidInput`); 20 active pairings
    /// (`Conflict`).
    pub fn create_pairing(
        &self,
        name: &str,
        scopes: &[PermissionKind],
    ) -> Result<CreatedPairing, AgentError> {
        let name = name.trim();
        let chars = name.chars().count();
        if chars == 0 || chars > 64 {
            return Err(AgentError::Tool(ToolError::invalid(
                "name",
                "the name must be 1 to 64 characters",
            )));
        }
        let mut scopes: Vec<PermissionKind> = scopes.to_vec();
        scopes.sort();
        scopes.dedup();
        if scopes.is_empty() {
            return Err(AgentError::Tool(ToolError::invalid(
                "scopes",
                "choose at least one permission",
            )));
        }
        let app = self.app;
        let conn = conn(app)?;
        if musaic_db::agent::count_active_pairings(&conn)? >= MAX_ACTIVE_PAIRINGS {
            return Err(AgentError::Conflict {
                message: "20 pairings are active; revoke one first".into(),
            });
        }
        let id = format!("p_{}", random_hex(8));
        let secret = random_hex(32);
        let secret_bytes = hex_decode(&secret).unwrap_or_default();
        let cfg = config_id(app);
        musaic_secrets::agent::put_secret(app.0.agent_secrets.as_ref(), &cfg, &id, &secret)
            .map_err(|e| {
                AgentError::Op(OpError::Unavailable {
                    message: format!("The credential could not be stored: {e}"),
                })
            })?;
        if app.0.agent_dev_credentials
            && let Err(e) = crate::agent::write_dev_credential(app.config_dir(), &id, &secret)
        {
            let _ = musaic_secrets::agent::delete_secret(app.0.agent_secrets.as_ref(), &cfg, &id);
            return Err(AgentError::Op(OpError::Unavailable {
                message: format!("The development credential could not be written: {e}"),
            }));
        }
        let row = musaic_db::agent::PairingRow {
            id: id.clone(),
            name: name.to_string(),
            scopes: kinds_json(&scopes),
            secret_fingerprint: hex_encode(&Sha256::digest(&secret_bytes)),
            created_at: app.now_iso(),
            last_used_at: None,
            revoked_at: None,
        };
        let inserted = musaic_db::agent::insert_pairing(&conn, &row, MAX_ACTIVE_PAIRINGS);
        if !matches!(inserted, Ok(true)) {
            let _ = musaic_secrets::agent::delete_secret(app.0.agent_secrets.as_ref(), &cfg, &id);
            crate::agent::remove_dev_credential(app.config_dir(), &id);
            return match inserted {
                Ok(_) => Err(AgentError::Conflict {
                    message: "20 pairings are active; revoke one first".into(),
                }),
                Err(e) => Err(e.into()),
            };
        }
        self.sync_index(&conn);
        app.log().info(LOG_TARGET, &format!("pairing {id} created"));
        self.publish(AgentWhat::Pairings, vec![id.clone()]);
        let pairing = record_of(&conn, row)?;
        Ok(CreatedPairing {
            pairing,
            connect: self.connect_hint(),
        })
    }

    /// Keep the keyring's index of active pairing ids in step (the CLI reads
    /// it; there are no secrets in it).
    fn sync_index(&self, conn: &rusqlite::Connection) {
        // A failed read must not write an empty index: that would delete the
        // entry and hide every pairing from the CLI.
        let rows = match musaic_db::agent::list_pairings(conn) {
            Ok(rows) => rows,
            Err(e) => {
                self.app.log().warn(
                    LOG_TARGET,
                    &format!("the pairing index was not updated: {e}"),
                );
                return;
            }
        };
        let ids: Vec<String> = rows
            .into_iter()
            .filter(|p| p.revoked_at.is_none())
            .map(|p| p.id)
            .collect();
        if let Err(e) = musaic_secrets::agent::put_index(
            self.app.0.agent_index_store.as_ref(),
            &config_id(self.app),
            &ids,
        ) {
            self.app.log().warn(
                LOG_TARGET,
                &format!("the pairing index could not be stored: {e}"),
            );
        }
    }

    fn connect_hint(&self) -> ConnectHint {
        let mut args = vec!["setup".to_string()];
        let default = musaic_core::config::config_dir()
            .and_then(|d| musaic_core::config::resolve_path(&d).ok());
        let here = musaic_core::config::resolve_path(self.app.config_dir()).ok();
        if default.is_none() || default != here {
            args.push("--config-dir".into());
            args.push(self.app.config_dir().to_string_lossy().into_owned());
        }
        // The bundled CLI sits beside the app's executable (macOS
        // `Contents/MacOS`, next to `Musaic.exe` on Windows), which is not on
        // PATH: name it by its absolute path when it is there.
        let bundled = std::env::current_exe().ok().and_then(|exe| {
            let name = if cfg!(windows) {
                "musaic-cli.exe"
            } else {
                "musaic-cli"
            };
            let p = exe.parent()?.join(name);
            p.is_file().then(|| p.to_string_lossy().into_owned())
        });
        ConnectHint {
            command: bundled.unwrap_or_else(|| "musaic-cli".into()),
            args,
        }
    }

    /// Every pairing, oldest first, revoked ones included.
    pub fn list_pairings(&self) -> Result<Vec<PairingRecord>, AgentError> {
        let conn = conn(self.app)?;
        musaic_db::agent::list_pairings(&conn)?
            .into_iter()
            .map(|r| record_of(&conn, r))
            .collect()
    }

    /// Revoke a pairing: its tokens stop working at once, its keyring entry
    /// and dev credential are deleted, and the approval cards still waiting on
    /// it are denied. `NotFound` for an unknown id; revoking twice is `Ok`.
    pub fn revoke_pairing(&self, id: &str) -> Result<PairingRecord, AgentError> {
        let app = self.app;
        let _guard = app.gate().begin_work().map_err(OpError::from)?;
        let conn = conn(app)?;
        let tx = musaic_db::immediate(&conn)?;
        let Some(row) = musaic_db::agent::get_pairing(&tx, id)? else {
            return Err(tool_err(ToolErrorCode::NotFound, "no such pairing"));
        };
        let now = app.now_iso();
        let revoked = musaic_db::agent::revoke_pairing(&tx, id, &now)?;
        let cards = crate::agent::deny_cards_of(app, &tx, id)?;
        tx.commit()?;
        app.0.agent.drop_tokens_of(id);
        app.0.agent.forget_pairing(id);
        let _ = musaic_db::agent_hosts::delete(&conn, id);
        // Parked event polls re-check their pairing and answer Forbidden.
        app.0.agent.notify_events();
        let cfg = config_id(app);
        let _ = musaic_secrets::agent::delete_secret(app.0.agent_secrets.as_ref(), &cfg, id);
        crate::agent::remove_dev_credential(app.config_dir(), id);
        if revoked {
            self.sync_index(&conn);
            if !cards.is_empty() {
                self.publish(AgentWhat::Approvals, cards);
            }
            crate::agent::delegation::publish_pending(app, &conn);
            app.log().info(LOG_TARGET, &format!("pairing {id} revoked"));
            self.publish(AgentWhat::Pairings, vec![id.to_string()]);
        }
        let row = musaic_db::agent::get_pairing(&conn, id)?.unwrap_or(row);
        record_of(&conn, row)
    }

    /// Set the decision of some kinds for a pairing; the others keep theirs.
    /// A kind outside the pairing's scope cannot be set (`InvalidInput`).
    pub fn set_policy(
        &self,
        id: &str,
        changes: &[(PermissionKind, Decision)],
    ) -> Result<PairingRecord, AgentError> {
        let conn = conn(self.app)?;
        let Some(row) = musaic_db::agent::get_pairing(&conn, id)? else {
            return Err(tool_err(ToolErrorCode::NotFound, "no such pairing"));
        };
        if row.revoked_at.is_some() {
            return Err(tool_err(ToolErrorCode::NotFound, "that pairing is revoked"));
        }
        let scopes = parse_scopes(&row.scopes);
        // Reads and previews have no card to wait on: Ask means nothing there.
        if let Some((k, _)) = changes.iter().find(|(k, d)| {
            *d == Decision::Ask
                && matches!(
                    k,
                    PermissionKind::LibraryRead
                        | PermissionKind::ProposalSubmit
                        | PermissionKind::WorkCancel
                )
        }) {
            return Err(AgentError::Tool(ToolError::invalid(
                "policy",
                format!("{k} can be Allow or Refuse, not Ask"),
            )));
        }
        if let Some((k, _)) = changes.iter().find(|(k, _)| !scopes.contains(k)) {
            return Err(AgentError::Tool(ToolError::invalid(
                "policy",
                format!("{k} is outside this pairing's scope"),
            )));
        }
        let set: Vec<(String, String)> = changes
            .iter()
            .map(|(k, d)| (k.as_str().to_string(), d.as_str().to_string()))
            .collect();
        musaic_db::agent::set_policies(&conn, id, &set)?;
        self.publish(AgentWhat::Pairings, vec![id.to_string()]);
        record_of(&conn, row)
    }

    /// The last 100 calls a pairing recorded, newest first.
    pub fn activity(&self, id: &str) -> Result<Vec<ActivityRecord>, AgentError> {
        let conn = conn(self.app)?;
        if musaic_db::agent::get_pairing(&conn, id)?.is_none() {
            return Err(tool_err(ToolErrorCode::NotFound, "no such pairing"));
        }
        Ok(musaic_db::agent::list_activity(&conn, id)?
            .into_iter()
            .map(|r| ActivityRecord {
                at: r.at,
                tool: r.tool,
                outcome: r.outcome,
            })
            .collect())
    }

    // ---------------------------------------------------------- handshake

    /// The first half of the handshake: this instance's id and a single-use
    /// nonce. Needs no credential, and reveals nothing but those.
    pub fn hello(&self) -> HelloResponse {
        HelloResponse {
            instance_id: self.app.0.agent.instance_id().to_string(),
            server_nonce: self.app.0.agent.issue_nonce(),
            profiles: PROTOCOL_PROFILES.iter().map(|p| p.to_string()).collect(),
            contract_version: CONTRACT_VERSION.to_string(),
        }
    }

    /// The second half: the client proves it holds the pairing's secret, and
    /// this side proves it back. Any failure is the same `NOT_PAIRED`, which
    /// does not say which part failed, and counts toward the lockout; while
    /// locked out the answer is `LOCKED_OUT` with `retryAfterSeconds`.
    pub fn exchange(&self, req: &ExchangeRequest) -> Result<ExchangeResponse, AgentError> {
        let app = self.app;
        let hub = &app.0.agent;
        if let Some(wait) = hub.locked_for() {
            let secs = wait.as_secs().max(1);
            return Err(AgentError::Tool(
                ToolError::new(ToolErrorCode::LockedOut, "too many failed attempts")
                    .with_detail("retryAfterSeconds", secs.to_string()),
            ));
        }
        match self.verified_pairing(req) {
            Some((row, secret)) => {
                hub.record_success();
                let principal = Principal {
                    pairing_id: row.id.clone(),
                    pairing_name: row.name.clone(),
                    scopes: parse_scopes(&row.scopes),
                    instance_id: hub.instance_id().to_string(),
                };
                let scopes: Vec<String> = principal
                    .scopes
                    .iter()
                    .map(|k| k.as_str().to_string())
                    .collect();
                let (token, expires) = hub.issue_token(principal);
                let proof = server_proof(
                    &secret,
                    &req.pairing_id,
                    &req.instance_id,
                    &req.server_nonce,
                    &req.client_nonce,
                );
                if let Ok(conn) = conn(app) {
                    let now = app.now_iso();
                    let _ = musaic_db::agent::touch_pairing(&conn, &row.id, &now);
                    let _ =
                        musaic_db::agent::insert_activity(&conn, &row.id, &now, "exchange", "ok");
                }
                crate::agent::hosts::announce(app, &row.id);
                self.publish(AgentWhat::Activity, vec![row.id]);
                Ok(ExchangeResponse {
                    token,
                    expires_at: iso(expires),
                    scopes,
                    server_proof: proof,
                })
            }
            None => {
                hub.record_failure();
                // The presented credential is never logged.
                app.log().warn(LOG_TARGET, "a token exchange failed");
                Err(tool_err(ToolErrorCode::NotPaired, "not paired"))
            }
        }
    }

    /// Every check of an exchange, in one place so every failure is the same.
    /// The nonce is spent first, whatever follows.
    fn verified_pairing(
        &self,
        req: &ExchangeRequest,
    ) -> Option<(musaic_db::agent::PairingRow, Vec<u8>)> {
        let app = self.app;
        let hub = &app.0.agent;
        let nonce_ok = hub.take_nonce(&req.server_nonce);
        let instance_ok = req.instance_id == hub.instance_id();
        let conn = conn(app).ok()?;
        let row = musaic_db::agent::get_pairing(&conn, &req.pairing_id).ok()??;
        let secret_hex = musaic_secrets::agent::get_secret(
            app.0.agent_secrets.as_ref(),
            &config_id(app),
            &row.id,
        )
        .ok()??;
        let secret = hex_decode(&secret_hex)?;
        let fingerprint = hex_encode(&Sha256::digest(&secret));
        let fingerprint_ok =
            musaic_commands::auth::ct_eq(fingerprint.as_bytes(), row.secret_fingerprint.as_bytes());
        let proof_ok = verify_client_proof(
            &secret,
            &req.client_proof,
            &req.pairing_id,
            &req.instance_id,
            &req.server_nonce,
            &req.client_nonce,
        );
        (nonce_ok && instance_ok && row.revoked_at.is_none() && fingerprint_ok && proof_ok)
            .then_some((row, secret))
    }

    /// The caller a bearer token names, or `NOT_PAIRED`.
    pub fn authenticate(&self, token: &str) -> Result<Principal, AgentError> {
        self.app
            .0
            .agent
            .lookup_token(token)
            .ok_or_else(|| tool_err(ToolErrorCode::NotPaired, "the token is not valid"))
    }

    /// The tool catalog and its digest, for the catalog route.
    pub fn list_tools(&self, _principal: &Principal) -> RegistryInfo {
        RegistryInfo {
            catalog: musaic_commands::catalog::catalog_json(),
            registry_digest: musaic_commands::catalog::registry_digest(),
        }
    }

    pub fn task_create(
        &self,
        p: &Principal,
        input: &musaic_commands::tools::task::TaskCreateInput,
    ) -> Result<musaic_commands::tools::task::TaskSnapshot, ToolError> {
        crate::agent::tasks::create(self.app, p, input)
    }

    pub fn task_get(
        &self,
        p: &Principal,
        input: &musaic_commands::tools::task::TaskInput,
    ) -> Result<musaic_commands::tools::task::TaskSnapshot, ToolError> {
        crate::agent::tasks::get(self.app, p, input)
    }

    pub fn task_update(
        &self,
        p: &Principal,
        input: &musaic_commands::tools::task::TaskUpdateInput,
    ) -> Result<(), ToolError> {
        crate::agent::tasks::update(self.app, p, input)
    }

    pub fn task_cancel(
        &self,
        p: &Principal,
        input: &musaic_commands::tools::task::TaskInput,
    ) -> Result<(), ToolError> {
        crate::agent::tasks::cancel(self.app, p, input)
    }

    /// The event types this app offers (`events/list`). The pairing must be
    /// active and allowed to read work (`EVENT_FORBIDDEN` otherwise).
    pub fn events_list(
        &self,
        p: &Principal,
    ) -> Result<musaic_commands::events::ListResult, ToolError> {
        crate::agent::events::events_list(self.app, p)
    }

    /// One poll of the delegated-work history after a cursor. With
    /// `wait_ms` set and nothing to report it parks, holding no work guard
    /// and no connection, until a delegation publish, a revocation, shutdown
    /// or the wait ends, then looks once more. At most four polls of one
    /// pairing park (`EVENT_LIMIT`).
    pub fn events_poll(
        &self,
        p: &Principal,
        params: &musaic_commands::events::PollParams,
    ) -> Result<musaic_commands::events::PollResult, ToolError> {
        crate::agent::events::events_poll(self.app, p, params)
    }

    /// Record what a paired host's stdio process reports. Never fails a
    /// session: bad values are cleaned or dropped. Publishes
    /// `AgentStateChanged(hosts)` when what is stored changed.
    pub fn observe_host(
        &self,
        p: &Principal,
        observation: &musaic_commands::events::HostObservation,
    ) {
        crate::agent::hosts::observe_host(self.app, p, observation);
    }

    /// The pairing's own stored host observation and subscription state, for
    /// `doctor`.
    pub fn session_status(
        &self,
        p: &Principal,
    ) -> Result<musaic_commands::events::SessionStatus, AgentError> {
        crate::agent::hosts::session_status(self.app, p)
    }

    /// Per pairing, four separate facts with times and what the host reported
    /// (desktop only). Nothing here says "ready".
    pub fn diagnostics(&self) -> Result<Vec<PairingDiagnostics>, AgentError> {
        crate::agent::hosts::diagnostics(self.app)
    }

    // --------------------------------------------------- authorization

    // ------------------------------------------------------------ tools

    /// Run one tool call for `principal`. The call is authorized first; a
    /// refusal for scope or policy is recorded and answers `FORBIDDEN`. Read
    /// tools, `musaic_fix_preview` and the operation tools need `Allow`: an
    /// `Ask` has no card to wait on there, so it refuses as well.
    pub fn call_tool(
        &self,
        principal: &Principal,
        call: ToolCall,
    ) -> Result<ToolOutput, ToolError> {
        let app = self.app;
        let tool = call.tool_id();
        if crate::agent::delegation::handles(&call) {
            return crate::agent::delegation::call(app, principal, call);
        }
        let kinds: &[PermissionKind] = match &call {
            ToolCall::Capabilities(_)
            | ToolCall::LibraryList(_)
            | ToolCall::AlbumGet(_)
            | ToolCall::ResolutionGet(_)
            | ToolCall::FindingsList(_)
            | ToolCall::OperationGet(_)
            | ToolCall::OperationCancel(_) => &[PermissionKind::LibraryRead],
            ToolCall::FixPreview(_) => &[PermissionKind::ProposalSubmit],
            // These decide for themselves: Ask is a card for them.
            ToolCall::FixApply(_)
            | ToolCall::ScanStart(_)
            | ToolCall::ChecksStart(_)
            | ToolCall::AnalysisStart(_) => &[],
            _ => unreachable!("delegation tools were dispatched above"),
        };
        if !kinds.is_empty() {
            let decision = crate::agent::run::authorize(app, principal, kinds)?;
            if decision != Decision::Allow {
                return Err(crate::agent::run::refuse_forbidden(app, principal, tool));
            }
        }
        use crate::agent::{proposal, read, run};
        Ok(match call {
            ToolCall::Capabilities(_) => {
                ToolOutput::Capabilities(read::capabilities(app, principal)?)
            }
            ToolCall::LibraryList(i) => ToolOutput::LibraryList(read::library_list(app, &i)?),
            ToolCall::AlbumGet(i) => ToolOutput::AlbumGet(read::album_get(app, &i)?),
            ToolCall::ResolutionGet(i) => ToolOutput::ResolutionGet(read::resolution_get(app, &i)?),
            ToolCall::FindingsList(i) => ToolOutput::FindingsList(read::findings_list(app, &i)?),
            ToolCall::FixPreview(i) => {
                ToolOutput::FixPreview(proposal::fix_preview(app, principal, &i)?)
            }
            ToolCall::FixApply(i) => ToolOutput::FixApply(run::fix_apply(app, principal, &i)?),
            ToolCall::ScanStart(i) => ToolOutput::Handle(run::scan_start(app, principal, &i)?),
            ToolCall::ChecksStart(i) => ToolOutput::Handle(run::checks_start(app, principal, &i)?),
            ToolCall::AnalysisStart(i) => {
                ToolOutput::Handle(run::analysis_start(app, principal, &i)?)
            }
            ToolCall::OperationGet(i) => {
                ToolOutput::OperationGet(run::operation_get(app, principal, &i)?)
            }
            ToolCall::OperationCancel(i) => {
                ToolOutput::OperationCancel(run::operation_cancel(app, principal, &i)?)
            }
            _ => unreachable!("delegation tools were dispatched above"),
        })
    }

    // -------------------------------------------------------- approvals

    /// The approval cards, newest first: the pending ones, or all of them.
    pub fn list_approvals(
        &self,
        filter: ApprovalFilter,
    ) -> Result<Vec<ApprovalRecord>, AgentError> {
        let app = self.app;
        let conn = conn(app)?;
        let rows = musaic_db::agent::list_approvals(
            &conn,
            matches!(filter, ApprovalFilter::Pending).then_some("pending"),
        )?;
        let lib = app.library_dir();
        rows.into_iter()
            .map(|card| approval_record(&conn, lib.as_deref(), card))
            .collect()
    }

    /// The desktop user's decision. Only a face the user is at reaches this:
    /// no agent tool calls it. Approving runs what was waiting (an apply runs
    /// now, revalidated against the files as they are); denying changes
    /// nothing. A card no longer pending is a `Conflict`, `NotFound` for an
    /// unknown id.
    pub fn decide_approval(&self, id: &str, approve: bool) -> Result<ApprovalRecord, AgentError> {
        use crate::agent::run::{ApprovalOutcome, decide_approval};
        match decide_approval(self.app, id, approve)? {
            ApprovalOutcome::NotPending => Err(AgentError::Conflict {
                message: "that approval is no longer pending".into(),
            }),
            ApprovalOutcome::Decided => {
                // This card only: the history of cards is never pruned.
                let conn = conn(self.app)?;
                let card = musaic_db::agent::get_approval(&conn, id)?
                    .ok_or_else(|| tool_err(ToolErrorCode::NotFound, "no such approval"))?;
                approval_record(&conn, self.app.library_dir().as_deref(), card)
            }
        }
    }

    // ---------------------------------------------------------- changes

    /// The recent changes assistants made (a pairing's, or every pairing's),
    /// newest first, at most 20 per call, each with whether Undo would run
    /// now. Reads only: nothing is written and no guard is taken. `NotFound`
    /// for an unknown pairing.
    pub fn list_changes(&self, pairing_id: Option<&str>) -> Result<Vec<ChangeRecord>, AgentError> {
        crate::agent::undo::list_changes(self.app, pairing_id)
    }

    /// The desktop user's Undo of an assistant's change: no agent tool
    /// reaches it. Under the apply guard (a restore running refuses it), in
    /// this order: the change exists (`NotFound`); it was applied or partly
    /// applied and not undone, and is not itself an undo (`Conflict`); its
    /// album is present (`Conflict`, rule 6); the files still hold exactly
    /// what the change left (`ChangedSince`, listing what differs; nothing
    /// is written). Then it records its own before-image, renames back
    /// through the rename service, puts the tags, pictures and cover files
    /// back through the apply pipeline (the same events as any rename and
    /// apply), marks the change `undone`, records an activity row and
    /// publishes [`AgentWhat::Changes`].
    pub fn undo_change(&self, change_id: &str) -> Result<UndoOutcome, AgentError> {
        crate::agent::undo::undo_change(self.app, change_id)
    }

    /// The app's periodic delegation upkeep: expire lapsed leases, publish
    /// what is pending, drop event history older than seven days, and publish
    /// the diagnostics refreshes that are owed (`AgentStateChanged(hosts)`: one
    /// the rate limit held back, or a subscription that lapsed). The app calls
    /// it every 30 seconds.
    pub fn sweep_delegation(&self) {
        crate::agent::delegation::tick(self.app);
        crate::agent::hosts::flush(self.app);
    }

    /// Expire the cards whose time has come ("timed out"). The app calls it
    /// every 30 seconds; returns the cards that expired.
    pub fn expire_due(&self) -> Vec<String> {
        crate::agent::run::expire_due(self.app)
    }
}
