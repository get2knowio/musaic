//! A proposed change to one album, validated against its current state and
//! turned into a diff, without writing anything (FR-040, FR-041).
//!
//! The diff comes from a dry run of the apply pipeline, so a preview shows
//! what the pipeline would write; the names a rename would give come from the
//! user's templates over the values the change would write
//! ([`musaic_fsops::plan::render_targets`]). Stored proposals are used for
//! labels only: apply derives everything again.

use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::path::Path;

use musaic_commands::auth::hex_encode;
use musaic_commands::tools::fix::{
    CoverPatch, Effect, FixDiff, NameChange, ProposedPatch, TagChange,
};
use musaic_commands::{PermissionKind, ToolError, ToolErrorCode};
use musaic_fsops::plan::{TargetFile, Templates, render_targets};
use musaic_resolve::db_store::DbStore;
use musaic_scan::flacmeta;
use musaic_tags::apply::{FIELD_TO_VORBIS, TRACK_FIELD_TO_VORBIS};
use sha2::{Digest, Sha256};

use crate::agent_read::file_facts;
use crate::albums::AlbumRow;
use crate::apply::{ApplyEnv, ApplyEvent, ApplyRequest, TagEvent, apply_album_changes};

/// What a validated proposal is.
#[derive(Debug, Clone, PartialEq)]
pub struct ProposalPlan {
    /// The patch as stored (BTreeMap keys: canonical).
    pub patch_text: String,
    pub kinds: Vec<PermissionKind>,
    pub diff: FixDiff,
    pub effects: Vec<Effect>,
    pub digest: String,
}

fn invalid(field: &str, message: impl Into<String>) -> ToolError {
    ToolError::invalid(field, message)
}

/// `SHA-256(canonical patch ‖ albumId ‖ baseRevision)`.
pub fn proposal_digest(patch_text: &str, album_id: &str, base_revision: &str) -> String {
    let mut h = Sha256::new();
    for part in [patch_text, album_id, base_revision] {
        h.update((part.len() as u64).to_be_bytes());
        h.update(part.as_bytes());
    }
    hex_encode(&h.finalize())
}

/// The cover the resolver's stored anchor names on the Cover Art Archive,
/// derived from the database (never from the caller); no network.
pub fn anchored_cover(conn: &rusqlite::Connection, album_id: &str) -> Option<String> {
    let store = DbStore {
        conn,
        now_iso: String::new(),
        read: Default::default(),
    };
    musaic_resolve::resolver::anchored_cover_url(&store, album_id)
        .ok()
        .flatten()
}

/// The pieces of a patch the apply pipeline takes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplyParts {
    pub suggestions: Vec<(String, String)>,
    pub track_suggestions: Vec<(String, Vec<(String, String)>)>,
    pub clear: Vec<String>,
    pub track_clear: Vec<(String, Vec<String>)>,
}

/// Split the tag part of a patch into sets and clears, after checking every
/// field and file name against the album.
pub fn apply_parts(patch: &ProposedPatch, file_names: &[String]) -> Result<ApplyParts, ToolError> {
    let mut parts = ApplyParts::default();
    let Some(tags) = &patch.tags else {
        return Ok(parts);
    };
    for (field, value) in tags.album.iter().flatten() {
        if !musaic_tags::is_removable_album_field(field) {
            return Err(invalid(
                "proposedPatch.tags.album",
                format!("{field} is not an album tag that can be changed"),
            ));
        }
        match value.as_deref().map(str::trim) {
            Some("") => {
                return Err(invalid(
                    "proposedPatch.tags.album",
                    format!("{field} is empty; use null to clear a tag"),
                ));
            }
            Some(v) => parts.suggestions.push((field.clone(), v.to_string())),
            None => parts.clear.push(field.clone()),
        }
    }
    for (file, fields) in tags.tracks.iter().flatten() {
        if !file_names.contains(file) {
            return Err(invalid(
                "proposedPatch.tags.tracks",
                "a file name is not one of the album's files",
            ));
        }
        let mut sets = Vec::new();
        let mut clears = Vec::new();
        for (field, value) in fields {
            if !TRACK_FIELD_TO_VORBIS.iter().any(|(f, _)| f == field) {
                return Err(invalid(
                    "proposedPatch.tags.tracks",
                    format!("{field} is not a track tag that can be changed"),
                ));
            }
            match value.as_deref().map(str::trim) {
                Some("") => {
                    return Err(invalid(
                        "proposedPatch.tags.tracks",
                        format!("{field} is empty; use null to clear a tag"),
                    ));
                }
                Some(v) => sets.push((field.clone(), v.to_string())),
                None if musaic_tags::is_removable_track_field(field) => clears.push(field.clone()),
                None => {
                    return Err(invalid(
                        "proposedPatch.tags.tracks",
                        format!("{field} cannot be cleared; set a value instead"),
                    ));
                }
            }
        }
        if !sets.is_empty() {
            parts.track_suggestions.push((file.clone(), sets));
        }
        if !clears.is_empty() {
            parts.track_clear.push((file.clone(), clears));
        }
    }
    Ok(parts)
}

fn vorbis_to_field(key: &str, track_scope: bool) -> String {
    let table: &[(&str, &str)] = if track_scope {
        &TRACK_FIELD_TO_VORBIS
    } else {
        &FIELD_TO_VORBIS
    };
    table
        .iter()
        .find(|(_, v)| *v == key)
        .map(|(f, _)| f.to_string())
        .or_else(|| match key {
            // The scanner's other spellings of the same fields.
            "album_artist" => Some("album_artist".to_string()),
            "year" => Some("year".to_string()),
            _ => None,
        })
        .unwrap_or_else(|| key.to_string())
}

/// Validate `patch` against the album and derive its diff, effects and
/// digest. Nothing is written and nothing is fetched.
pub fn plan(
    env: &mut ApplyEnv<'_>,
    row: &AlbumRow,
    base_revision: &str,
    patch: &ProposedPatch,
) -> Result<ProposalPlan, ToolError> {
    patch.validate_shape()?;
    let album_id = row.id().to_string();
    let files = file_facts(&row.album.path);
    let names: Vec<String> = files.iter().map(|(n, _, _)| n.clone()).collect();
    if names.is_empty() {
        return Err(ToolError::new(
            ToolErrorCode::MissingAlbum,
            "the album's folder has no files",
        ));
    }
    let parts = apply_parts(patch, &names)?;
    let mut effects = Vec::new();
    let mut diff = FixDiff::default();

    // Cover.
    match &patch.cover {
        None => {}
        Some(CoverPatch::Resolution) => {
            if anchored_cover(env.conn, &album_id).is_none() {
                return Err(invalid(
                    "proposedPatch.cover",
                    "the album has no cover candidate from a stored resolution",
                ));
            }
            diff.cover = Some("the cover from the album's MusicBrainz match".into());
            effects.extend([Effect::WritesCover, Effect::FetchesNetwork]);
        }
        Some(CoverPatch::Url { url }) => {
            let host = musaic_net::preview::check_public_url(url).map_err(|why| {
                invalid(
                    "proposedPatch.cover.url",
                    format!("the URL cannot be used: {why}"),
                )
            })?;
            diff.cover = Some(format!("a cover from {host}"));
            effects.extend([Effect::WritesCover, Effect::FetchesNetwork]);
        }
    }

    // Tags: a dry run of the pipeline, so the diff is what it would write.
    let mut changed_tracknumber = false;
    let mut step1_renames: BTreeMap<String, String> = BTreeMap::new();
    if patch.tags.is_some() {
        effects.insert(0, Effect::WritesTags);
        let req = ApplyRequest {
            album_id: album_id.clone(),
            album_path: row.album.path.clone(),
            library_dir: None,
            suggestions: parts
                .suggestions
                .iter()
                .map(|(k, v)| (k.clone(), Some(v.clone())))
                .collect(),
            track_suggestions: parts
                .track_suggestions
                .iter()
                .map(|(f, s)| {
                    (
                        f.clone(),
                        s.iter()
                            .map(|(k, v)| (k.clone(), Some(v.clone())))
                            .collect(),
                    )
                })
                .collect(),
            removals: musaic_tags::Removals {
                album: parts.clear.clone(),
                tracks: parts.track_clear.clone(),
            },
            dry_run: true,
            ..Default::default()
        };
        let mut failure: Option<String> = None;
        let mut album_seen: BTreeMap<String, TagChange> = BTreeMap::new();
        let mut track_changes: Vec<TagChange> = Vec::new();
        let tracks_patch = patch.tags.as_ref().and_then(|t| t.tracks.as_ref());
        apply_album_changes(env, &req, &mut |ev| {
            match ev {
                ApplyEvent::Tags(TagEvent::FileDone(c)) => {
                    if let Some(e) = &c.error {
                        failure = Some(e.clone());
                    }
                    let file_fields = tracks_patch.and_then(|t| t.get(&c.filename));
                    for (key, d) in &c.diffs {
                        let track_field = vorbis_to_field(key, true);
                        let is_track = file_fields.is_some_and(|f| f.contains_key(&track_field));
                        let change = |scope: String, field: String| TagChange {
                            scope,
                            field,
                            before: Some(d.old.clone()).filter(|o| !o.is_empty()),
                            after: d.new.clone(),
                        };
                        if is_track {
                            track_changes.push(change(c.filename.clone(), track_field));
                        } else {
                            let field = vorbis_to_field(key, false);
                            album_seen
                                .entry(field.clone())
                                .or_insert_with(|| change("album".into(), field));
                        }
                    }
                    if let Some(to) = &c.renamed_to {
                        step1_renames.insert(c.filename.clone(), to.clone());
                    }
                }
                ApplyEvent::Error { detail } => failure = Some(detail),
                _ => {}
            }
            ControlFlow::Continue(())
        });
        if let Some(why) = failure {
            musaic_log::warning("musaic.agent", &format!("a proposal preview failed: {why}"));
            return Err(ToolError::new(
                ToolErrorCode::Unavailable,
                "the album's files could not be checked; nothing was written",
            ));
        }
        diff.tags = album_seen.into_values().chain(track_changes).collect();
        changed_tracknumber = tracks_patch
            .into_iter()
            .flatten()
            .any(|(_, f)| f.contains_key("tracknumber"));
        if diff.tags.is_empty() && patch.cover.is_none() && patch.rename != Some(true) {
            return Err(invalid(
                "proposedPatch",
                "the change would not alter any tag",
            ));
        }
    }

    // File names: the track-number rename the pipeline does itself, then the
    // template rename when asked for.
    let mut final_names: BTreeMap<String, String> = step1_renames.clone();
    if patch.rename == Some(true) {
        let Some(mut rp) = musaic_db::renames::rename_plan_row(env.conn, &album_id)
            .map_err(|_| ToolError::new(ToolErrorCode::Internal, "the album could not be read"))?
        else {
            return Err(ToolError::new(
                ToolErrorCode::MissingAlbum,
                "the album is missing",
            ));
        };
        // The row as the scan that follows an apply would leave it: the
        // title prefers the folder name's own album part, the artist is the
        // album artist tag first, the year is the date's first four digits.
        let dir_titled = row.album.dir_album.is_some();
        for (field, value) in &parts.suggestions {
            match field.as_str() {
                "title" if !dir_titled => rp.title = Some(value.clone()),
                "album_artist" => rp.album_artist = Some(value.clone()),
                "artist" => {
                    if rp.album_artist.as_deref().is_none_or(str::is_empty) {
                        rp.artist = Some(value.clone());
                    }
                }
                "year" => {
                    let head: String = value.chars().take(4).collect();
                    rp.year = Some(
                        if head.chars().count() == 4 && head.chars().all(|c| c.is_ascii_digit()) {
                            head
                        } else {
                            value.clone()
                        },
                    );
                }
                _ => {}
            }
        }
        for field in &parts.clear {
            match field.as_str() {
                "title" if !dir_titled => rp.title = None,
                "album_artist" => rp.album_artist = None,
                "year" => rp.year = None,
                _ => {}
            }
        }
        let album_set = |f: &str| {
            parts
                .suggestions
                .iter()
                .find(|(k, _)| k == f)
                .map(|(_, v)| v.clone())
        };
        let album_cleared = |f: &str| parts.clear.iter().any(|k| k == f);
        let mut metas = Vec::new();
        for (name, _, _) in &files {
            if let Ok(m) = flacmeta::read(&Path::new(&row.album.path).join(name)) {
                metas.push((name.clone(), m));
            }
        }
        let multi_disc = flacmeta::set_spans_discs(rp.disc_number, rp.disc_total, || {
            let mut ev = flacmeta::DiscEvidence::default();
            for (_, m) in &metas {
                ev.add(m);
            }
            ev
        });
        let targets: Vec<TargetFile> = metas
            .iter()
            .map(|(name, m)| {
                let first = |k: &str| m.values(k).next().unwrap_or("").to_string();
                let track_set = |f: &str| {
                    parts
                        .track_suggestions
                        .iter()
                        .find(|(n, _)| n == name)
                        .and_then(|(_, s)| s.iter().find(|(k, _)| k == f))
                        .map(|(_, v)| v.clone())
                };
                let track_cleared = |f: &str| {
                    parts
                        .track_clear
                        .iter()
                        .any(|(n, fs)| n == name && fs.iter().any(|k| k == f))
                };
                let artist = track_set("artist").or_else(|| {
                    if track_cleared("artist") {
                        Some(String::new())
                    } else {
                        album_set("artist").or_else(|| album_cleared("artist").then(String::new))
                    }
                });
                let tracknumber = track_set("tracknumber").unwrap_or_else(|| first("tracknumber"));
                let disc = if multi_disc {
                    flacmeta::file_disc(m.values("discnumber"), m.values("tracknumber"))
                        .or(rp.disc_number)
                } else {
                    None
                };
                TargetFile {
                    name: step1_renames
                        .get(name)
                        .cloned()
                        .unwrap_or_else(|| name.clone()),
                    tracknumber,
                    title: track_set("title").unwrap_or_else(|| first("title")),
                    artist: artist.unwrap_or_else(|| first("artist")),
                    albumartist: album_set("album_artist")
                        .or_else(|| album_cleared("album_artist").then(String::new))
                        .unwrap_or_else(|| first("albumartist")),
                    disc,
                }
            })
            .collect();
        let after1: Vec<String> = names
            .iter()
            .map(|n| step1_renames.get(n).cloned().unwrap_or_else(|| n.clone()))
            .collect();
        let templates = Templates::from_settings(env.conn).map_err(|_| {
            ToolError::new(ToolErrorCode::Internal, "the templates could not be read")
        })?;
        let (renames, dir) =
            render_targets(&templates, &rp, &targets, &after1, &row.album.dir_name).map_err(
                |_| {
                    ToolError::new(
                        ToolErrorCode::Unavailable,
                        "the filename templates could not be applied",
                    )
                },
            )?;
        for r in &renames {
            if r.collides_with.is_some() {
                return Err(invalid(
                    "proposedPatch.rename",
                    "two files would get the same name under the templates",
                ));
            }
            // Chain onto the original name of the file.
            let original = names
                .iter()
                .find(|n| step1_renames.get(*n).unwrap_or(n) == &r.filename)
                .cloned()
                .unwrap_or_else(|| r.filename.clone());
            final_names.insert(original, r.expected.clone());
        }
        diff.folder = dir.map(|d| NameChange {
            from: d.old,
            to: d.new,
        });
        if diff.folder.is_some() {
            effects.push(Effect::RenamesFolder);
        }
        if final_names.is_empty()
            && diff.folder.is_none()
            && patch.tags.is_none()
            && patch.cover.is_none()
        {
            return Err(invalid(
                "proposedPatch.rename",
                "the album already matches the templates",
            ));
        }
    }
    diff.files = final_names
        .iter()
        .filter(|(from, to)| from != to)
        .map(|(from, to)| NameChange {
            from: from.clone(),
            to: to.clone(),
        })
        .collect();
    if !diff.files.is_empty() {
        effects.push(Effect::RenamesFiles);
    }

    let mut kinds = patch.kinds();
    // A track-number change renames the file through the pipeline itself; the
    // rename needs its own permission.
    if (changed_tracknumber && !step1_renames.is_empty())
        && !kinds.contains(&PermissionKind::FilesRename)
    {
        kinds.push(PermissionKind::FilesRename);
    }
    let patch_text = serde_json::to_string(patch).expect("a patch is JSON");
    let digest = proposal_digest(&patch_text, &album_id, base_revision);
    Ok(ProposalPlan {
        patch_text,
        kinds,
        diff,
        effects,
        digest,
    })
}
