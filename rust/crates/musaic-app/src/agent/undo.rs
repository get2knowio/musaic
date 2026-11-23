//! The desktop user's Undo of an assistant's change (FR-055), and the list
//! of recent changes Settings shows it on.
//!
//! An undo replays the change's before-image: names back through the rename
//! service, then tags, pictures and cover files back through the apply
//! pipeline, so it publishes what any rename and apply publish (rule 1).
//! It runs only when the files still hold exactly what the change left (its
//! after-image, compared file by file and field by field); anything changed
//! since refuses it, listing what, and nothing is written. An undo records
//! its own before-image like any change, so it can be audited, and it is
//! never offered to an agent: only the desktop reaches it.

use std::collections::BTreeMap;
use std::path::Path;

use musaic_commands::{ToolError, ToolErrorCode};
use musaic_events::{AgentWhat, Event};
use musaic_fsops::rename::PlanEntry;
use musaic_services::agent_changes::{
    self, AfterImage, BeforeImage, ChangeInput, FileKeys, ImageSpec, NamePair, RenamesBefore,
};
use musaic_services::agent_read::cached_row;
use musaic_services::albums::AlbumRow;
use musaic_services::apply::{ApplyEvent, RestoreRequest, TagEvent, apply_restore};
use musaic_services::library_ops::{RenameParts, rename_album_with_plan};

use super::run::{apply_guard, conn, record_activity};
use crate::App;
use crate::ops::agent::{AgentError, ChangeRecord, UndoOutcome, album_label};

/// The most changes one listing returns.
pub const LIST_LIMIT: u32 = 20;

const LOG_TARGET: &str = "musaic.agent";

/// Why a change cannot be undone now.
enum Blocked {
    /// The change's state or record rules it out.
    State(String),
    /// The album is gone or Missing.
    Album(String),
    /// The files changed since; what changed.
    Changed(Vec<String>),
}

impl Blocked {
    fn reason(&self) -> String {
        match self {
            Blocked::State(m) | Blocked::Album(m) => m.clone(),
            Blocked::Changed(what) => format!("Changed since: {}", what.join("; ")),
        }
    }

    fn into_error(self) -> AgentError {
        match self {
            Blocked::State(message) | Blocked::Album(message) => AgentError::Conflict { message },
            Blocked::Changed(changed) => AgentError::ChangedSince {
                message: format!(
                    "The album changed since the assistant's change, so nothing was put back: {}",
                    changed.join("; ")
                ),
                changed,
            },
        }
    }
}

/// Everything an undo rests on, checked in order. Nothing is written.
struct Ready {
    before: BeforeImage,
    after: AfterImage,
    album: AlbumRow,
}

fn check(
    app: &App,
    conn: &rusqlite::Connection,
    row: &musaic_db::agent::ChangeRow,
) -> Result<Ready, Blocked> {
    if row.undoes.is_some() {
        return Err(Blocked::State("An undo cannot itself be undone.".into()));
    }
    match row.state.as_str() {
        "applied" | "partial" => {}
        "undone" => return Err(Blocked::State("Already undone.".into())),
        "failed" => {
            return Err(Blocked::State(
                "Nothing was changed, so there is nothing to undo.".into(),
            ));
        }
        _ => return Err(Blocked::State("The change has not finished.".into())),
    }
    let before: BeforeImage = serde_json::from_str(&row.before)
        .map_err(|_| Blocked::State("The record of this change cannot be read.".into()))?;
    let after: AfterImage = row
        .after
        .as_deref()
        .and_then(|t| serde_json::from_str(t).ok())
        .ok_or_else(|| {
            Blocked::State(
                "What this change left was not recorded, so it cannot be undone safely.".into(),
            )
        })?;
    let lib = app.library_dir();
    let album = cached_row(conn, lib.as_deref(), &after.album_id)
        .map_err(|_| Blocked::Album("The album was renamed, moved or removed since.".into()))?;
    if album
        .missing_since
        .as_deref()
        .is_some_and(|s| !s.is_empty())
        || !Path::new(&album.album.path).is_dir()
    {
        return Err(Blocked::Album(
            "The album is missing from disk; rescan once its folder is back.".into(),
        ));
    }
    let changed = agent_changes::changed_since(Path::new(&album.album.path), &after);
    if !changed.is_empty() {
        return Err(Blocked::Changed(changed));
    }
    Ok(Ready {
        before,
        after,
        album,
    })
}

fn record_of(
    app: &App,
    conn: &rusqlite::Connection,
    row: &musaic_db::agent::ChangeRow,
    names: &mut BTreeMap<String, String>,
) -> ChangeRecord {
    let before: Option<BeforeImage> = serde_json::from_str(&row.before).ok();
    let album_id = row
        .after
        .as_deref()
        .and_then(|t| serde_json::from_str::<AfterImage>(t).ok())
        .map(|a| a.album_id)
        .unwrap_or_else(|| row.album_id.clone());
    let pairing_name = match &row.pairing_id {
        Some(id) => names
            .entry(id.clone())
            .or_insert_with(|| {
                musaic_db::agent::get_pairing(conn, id)
                    .ok()
                    .flatten()
                    .map(|p| p.name)
                    .unwrap_or_default()
            })
            .clone(),
        None => "Musaic".into(),
    };
    let blocked = check(app, conn, row).err();
    let summary = before
        .as_ref()
        .map(|b| b.summary())
        .unwrap_or_else(|| "a change".into());
    ChangeRecord {
        id: row.id.clone(),
        pairing_id: row.pairing_id.clone(),
        pairing_name,
        album_label: album_label(conn, app.library_dir().as_deref(), &album_id),
        album_id,
        created_at: row.created_at.clone(),
        state: row.state.clone(),
        summary: if row.undoes.is_some() {
            format!("Undo: {summary}")
        } else {
            summary
        },
        undoes: row.undoes.clone(),
        undoable: blocked.is_none(),
        reason: blocked.map(|b| b.reason()),
    }
}

pub(crate) fn research_change_undoable(
    app: &App,
    conn: &rusqlite::Connection,
    change: &musaic_db::agent::ChangeRow,
) -> bool {
    check(app, conn, change).is_ok()
}

/// `agent.list_changes`.
pub fn list_changes(app: &App, pairing_id: Option<&str>) -> Result<Vec<ChangeRecord>, AgentError> {
    let conn = conn(app)?;
    if let Some(id) = pairing_id
        && musaic_db::agent::get_pairing(&conn, id)?.is_none()
    {
        return Err(AgentError::Tool(ToolError::new(
            ToolErrorCode::NotFound,
            "no such pairing",
        )));
    }
    let rows = musaic_db::agent::list_changes_of(&conn, pairing_id, LIST_LIMIT)?;
    let mut names = BTreeMap::new();
    Ok(rows
        .iter()
        .map(|r| record_of(app, &conn, r, &mut names))
        .collect())
}

/// The undo's own before-image: the keys the change touched, by the names
/// the files have now, and the renames that put the old names back.
fn undo_spec(before: &BeforeImage, after: &AfterImage) -> ImageSpec {
    let now_name = |orig: &str| {
        after
            .files
            .get(orig)
            .map(|f| f.name.clone())
            .unwrap_or_else(|| orig.to_string())
    };
    let mut keys: BTreeMap<String, FileKeys> = BTreeMap::new();
    for (orig, fields) in &before.files {
        keys.entry(now_name(orig))
            .or_default()
            .fields
            .extend(fields.keys().cloned());
    }
    for (orig, aliases) in &before.aliases {
        keys.entry(now_name(orig))
            .or_default()
            .aliases
            .extend(aliases.keys().cloned());
    }
    let files = after
        .files
        .iter()
        .filter(|(orig, f)| *orig != &f.name)
        .map(|(orig, f)| NamePair {
            from: f.name.clone(),
            to: orig.clone(),
        })
        .collect();
    let folder = before
        .renames
        .folder
        .as_ref()
        .filter(|p| p.from != after.folder)
        .map(|p| NamePair {
            from: after.folder.clone(),
            to: p.from.clone(),
        });
    ImageSpec {
        keys,
        renames: RenamesBefore { files, folder },
        cover: before.cover.is_some(),
    }
}

/// `agent.undo_change`.
pub fn undo_change(app: &App, change_id: &str) -> Result<UndoOutcome, AgentError> {
    let _guard = apply_guard(app)?;
    let mut conn = conn(app)?;
    let row = musaic_db::agent::get_change(&conn, change_id)?.ok_or_else(|| {
        AgentError::Tool(ToolError::new(ToolErrorCode::NotFound, "no such change"))
    })?;
    let ready = check(app, &conn, &row).map_err(Blocked::into_error)?;
    // The saved copies are read before anything is written: a record whose
    // copies are gone refuses with nothing changed.
    let restore: RestoreRequest = agent_changes::restore_request(app.config_dir(), &ready.before)
        .map_err(|why| {
        app.log().warn(
            LOG_TARGET,
            &format!("change {change_id} cannot be undone: {why}"),
        );
        AgentError::Conflict {
            message:
                "The saved copy of what this change replaced is missing, so it cannot be undone."
                    .into(),
        }
    })?;
    let spec = undo_spec(&ready.before, &ready.after);
    let now = app.now_iso();
    let undo_id = agent_changes::record_spec(
        &conn,
        app.config_dir(),
        &ChangeInput {
            pairing_id: row.pairing_id.as_deref(),
            proposal_id: &row.proposal_id,
            approval_id: None,
            now: &now,
        },
        &ready.album,
        &spec,
        Some(&row.id),
    )?;

    let lib = app.library_dir();
    let mut album_id = ready.album.id().to_string();
    let mut album_path = ready.album.album.path.clone();
    let mut wrote = false;
    let mut error: Option<String> = None;

    // 1. Names back (keyed by the names the files have now).
    let files: Vec<PlanEntry> = spec
        .renames
        .files
        .iter()
        .map(|p| PlanEntry {
            old: p.from.clone(),
            new: p.to.clone(),
        })
        .collect();
    let dir = spec.renames.folder.as_ref().map(|p| PlanEntry {
        old: p.from.clone(),
        new: p.to.clone(),
    });
    if !files.is_empty() || dir.is_some() {
        match rename_album_with_plan(
            RenameParts {
                conn: &mut conn,
                bus: app.bus(),
                library_dir: lib.as_deref(),
                now: &now,
                track_order: app.0.track_order,
            },
            &album_id,
            &files,
            dir.as_ref(),
        ) {
            Ok(out) => {
                wrote = out.renamed > 0 || out.detail.as_ref().is_some_and(|d| d.dir_renamed);
                if let Some(ev) = &out.album_renamed {
                    album_id = ev.new_album_id.clone();
                }
                if !out.ok {
                    error = Some(out.user_message.clone().unwrap_or(out.message.clone()));
                }
            }
            Err(e) => error = Some(e.message().to_string()),
        }
        match cached_row(&conn, lib.as_deref(), &album_id) {
            Ok(r) => album_path = r.album.path.clone(),
            Err(_) => {
                error.get_or_insert_with(|| "the album could not be found after renaming".into());
            }
        }
    }

    // 2. Tags, pictures and cover files, by the names before the change. A
    // change that only renamed ran no apply, and neither does its undo.
    let restores_anything = !restore.tags.tags.is_empty()
        || !restore.tags.pictures.is_empty()
        || restore.sidecars.is_some();
    if error.is_none() && restores_anything {
        app.with_apply_env(&mut conn, |env| {
            apply_restore(
                env,
                &album_id,
                &album_path,
                lib.as_deref(),
                &restore,
                &mut |ev| {
                    match ev {
                        ApplyEvent::Tags(TagEvent::FileDone(c)) => {
                            if c.saved {
                                wrote = true;
                            }
                            if let Some(e) = &c.error {
                                error.get_or_insert(format!("{}: {e}", c.filename));
                            }
                        }
                        ApplyEvent::Error { detail } => {
                            error.get_or_insert(detail);
                        }
                        _ => {}
                    }
                    std::ops::ControlFlow::Continue(())
                },
            );
        });
    }

    let state = match (&error, wrote) {
        (None, _) => "applied",
        (Some(_), true) => "partial",
        (Some(_), false) => "failed",
    };
    let outcome = serde_json::json!({ "albumId": album_id, "error": error }).to_string();
    agent_changes::finish(&conn, &undo_id, state, &outcome)?;
    if state != "failed" {
        let _ = musaic_db::agent::mark_change_undone(&conn, &row.id);
    }
    if let Some(pairing_id) = row.pairing_id.as_deref() {
        let pairing_name = musaic_db::agent::get_pairing(&conn, pairing_id)?
            .map(|p| p.name)
            .unwrap_or_default();
        record_activity(
            app,
            pairing_id,
            &pairing_name,
            "undo",
            if state == "applied" { "ok" } else { state },
        );
    }
    app.bus().publish(&Event::AgentStateChanged {
        what: AgentWhat::Changes,
        ids: row.pairing_id.iter().cloned().collect(),
    });
    if musaic_db::delegated_work::get(&conn, &row.proposal_id)?.is_some() {
        app.bus().publish(&Event::AgentStateChanged {
            what: AgentWhat::Work,
            ids: vec![row.proposal_id.clone()],
        });
    }
    if state == "failed" {
        return Err(AgentError::Tool(ToolError::new(
            ToolErrorCode::Unavailable,
            format!(
                "nothing was put back: {}",
                error.unwrap_or_else(|| "the undo failed".into())
            ),
        )));
    }
    let fresh = musaic_db::agent::get_change(&conn, &row.id)?.unwrap_or(row);
    let mut names = BTreeMap::new();
    Ok(UndoOutcome {
        change: record_of(app, &conn, &fresh, &mut names),
        undo_id,
        complete: error.is_none(),
        message: error,
    })
}
