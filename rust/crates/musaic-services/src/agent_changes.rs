//! The before-image of an agent-caused apply (FR-054): what the files hold
//! now, recorded before the first byte is written.
//!
//! For each file the change will touch, the prior value (or absence) of every
//! tag field it names; the prior file and folder names when it renames; and,
//! when it replaces the cover, a byte-for-byte copy of everything a cover
//! apply overwrites or deletes, under `<config dir>/agent-changes/<id>/`:
//! every PICTURE block of every FLAC file (the apply drops them all and embeds
//! one) and every `cover.jpg` / `cover.jpeg` / `cover.png` sidecar, in any
//! case (the apply writes `cover.jpg` and deletes the others). Picture data is
//! stored once per distinct content, so an album whose files share one
//! picture keeps one copy, while files that differ each keep their own. The
//! record is a row of `agent_changes`
//! (state `recorded`, then `applied`, `partial` or `failed`) and a
//! `before.json` beside the copy, so it survives a restart and a lost
//! database row alike. If any part cannot be recorded the apply is refused
//! and nothing is written. Rows older than 90 days are pruned with their
//! directories (at startup).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use musaic_commands::auth::random_hex;
use musaic_commands::tools::fix::{FixDiff, ProposedPatch};
use musaic_commands::{ToolError, ToolErrorCode};
use musaic_scan::flacmeta;
use musaic_tags::apply::{FIELD_TO_VORBIS, TRACK_FIELD_TO_VORBIS};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::agent_read::file_facts;
use crate::albums::AlbumRow;
use musaic_tags::flac::{FlacFile, Picture};
use sha2::{Digest, Sha256};

/// The directory holding every change's record.
pub const DIR: &str = "agent-changes";

/// Key prefixes of a field in [`BeforeImage::files`].
pub const ALBUM_PREFIX: &str = "album.";
pub const TRACK_PREFIX: &str = "track.";

/// A tag field's prior value: absent, one value, or several.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Prior {
    Absent(()),
    One(String),
    Many(Vec<String>),
}

impl Prior {
    fn of(mut values: Vec<String>) -> Prior {
        match values.len() {
            0 => Prior::Absent(()),
            1 => Prior::One(values.remove(0)),
            _ => Prior::Many(values),
        }
    }

    /// The values it stands for.
    pub fn values(&self) -> Vec<String> {
        match self {
            Prior::Absent(()) => Vec::new(),
            Prior::One(v) => vec![v.clone()],
            Prior::Many(v) => v.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamePair {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenamesBefore {
    pub files: Vec<NamePair>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<NamePair>,
}

/// One prior sidecar image, saved byte for byte.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidecarBefore {
    /// The bare name the album folder listed (`cover.jpg`, `cover.PNG`, ...).
    pub name: String,
    /// `agent-changes/<id>/sidecars/<name>`, relative to the config dir.
    #[serde(rename = "savedAs")]
    pub saved_as: String,
}

/// One prior PICTURE block of a FLAC file: its fields, and its image data
/// saved byte for byte, so the block can be rebuilt exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PictureBefore {
    /// `agent-changes/<id>/pictures/<sha256 prefix>.<ext>`, relative to the
    /// config dir; files whose picture data is identical share one copy.
    #[serde(rename = "savedAs")]
    pub saved_as: String,
    /// The ID3v2 APIC picture type (3 = front cover).
    #[serde(rename = "type")]
    pub kind: u32,
    pub mime: String,
    pub description: String,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub colors: u32,
}

impl PictureBefore {
    /// The PICTURE block this stands for, given its saved data.
    pub fn to_picture(&self, data: Vec<u8>) -> Picture {
        Picture {
            kind: self.kind,
            mime: self.mime.clone(),
            desc: self.description.clone(),
            width: self.width,
            height: self.height,
            depth: self.depth,
            colors: self.colors,
            data,
        }
    }
}

/// The prior cover art: everything a cover apply replaces.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverBefore {
    /// Every cover sidecar the folder held (empty: none). A restore writes
    /// these back and removes a `cover.jpg` that is not among them.
    pub sidecars: Vec<SidecarBefore>,
    /// Every FLAC file's PICTURE blocks in file order, by bare file name; an
    /// empty list is a file that had none.
    pub embedded: BTreeMap<String, Vec<PictureBefore>>,
}

/// The before-image as stored in `agent_changes.before`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BeforeImage {
    /// File name -> `album.<field>` / `track.<field>` -> prior value. Names
    /// are bare names.
    pub files: BTreeMap<String, BTreeMap<String, Prior>>,
    pub renames: RenamesBefore,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<CoverBefore>,
}

pub struct ChangeInput<'a> {
    pub pairing_id: &'a str,
    pub proposal_id: &'a str,
    pub approval_id: Option<&'a str>,
    pub now: &'a str,
}

fn refuse(why: impl std::fmt::Display) -> ToolError {
    musaic_log::error(
        "musaic.agent",
        &format!("the before-image could not be recorded: {why}"),
    );
    ToolError::new(
        ToolErrorCode::Unavailable,
        "the record of the current state could not be saved, so nothing was changed",
    )
}

fn ext_of(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        _ => "bin",
    }
}

/// Read the prior values of the fields `patch` touches, from the files now.
fn read_prior(
    album_dir: &str,
    names: &[String],
    patch: &ProposedPatch,
) -> Result<BTreeMap<String, BTreeMap<String, Prior>>, ToolError> {
    let mut out = BTreeMap::new();
    let Some(tags) = &patch.tags else {
        return Ok(out);
    };
    let album_fields: Vec<&String> = tags.album.iter().flat_map(|m| m.keys()).collect();
    for name in names {
        let track_fields: Vec<&String> = tags
            .tracks
            .as_ref()
            .and_then(|t| t.get(name))
            .map(|m| m.keys().collect())
            .unwrap_or_default();
        if album_fields.is_empty() && track_fields.is_empty() {
            continue;
        }
        let meta = flacmeta::read(&Path::new(album_dir).join(name)).map_err(refuse)?;
        let mut fields = BTreeMap::new();
        let read = |table: &[(&str, &str)], field: &str| {
            table
                .iter()
                .find(|(f, _)| *f == field)
                .map(|(_, v)| meta.values(v).map(str::to_string).collect::<Vec<_>>())
        };
        // `album.<field>` and `track.<field>`: the two vocabularies share names
        // (`title` is ALBUM at album level and TITLE on a track).
        for f in &album_fields {
            if let Some(values) = read(&FIELD_TO_VORBIS, f) {
                fields.insert(format!("{ALBUM_PREFIX}{f}"), Prior::of(values));
            }
        }
        for f in &track_fields {
            if let Some(values) = read(&TRACK_FIELD_TO_VORBIS, f) {
                fields.insert(format!("{TRACK_PREFIX}{f}"), Prior::of(values));
            }
        }
        out.insert(name.clone(), fields);
    }
    Ok(out)
}

/// The sidecar names a cover apply writes or deletes (`musaic_tags::sidecar`),
/// matched without regard to case so that nothing it could replace on a
/// case-insensitive volume is missed.
fn is_cover_sidecar(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "cover.jpg" | "cover.jpeg" | "cover.png"
    )
}

fn cover_copy(
    dir: &Path,
    change_id: &str,
    album_dir: &Path,
    names: &[String],
) -> Result<CoverBefore, ToolError> {
    let mut out = CoverBefore::default();
    // The names as the folder lists them: on a case-insensitive volume
    // `cover.JPG` and `cover.jpg` are one file and must be saved once.
    let mut sidecar_names: Vec<String> = std::fs::read_dir(album_dir)
        .map_err(refuse)?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| is_cover_sidecar(n) && album_dir.join(n).is_file())
        .collect();
    sidecar_names.sort();
    for name in sidecar_names {
        let bytes = std::fs::read(album_dir.join(&name)).map_err(refuse)?;
        std::fs::create_dir_all(dir.join("sidecars")).map_err(refuse)?;
        std::fs::write(dir.join("sidecars").join(&name), &bytes).map_err(refuse)?;
        out.sidecars.push(SidecarBefore {
            saved_as: format!("{DIR}/{change_id}/sidecars/{name}"),
            name,
        });
    }
    let pictures = dir.join("pictures");
    for name in names {
        let flac = FlacFile::open(&album_dir.join(name)).map_err(refuse)?;
        let mut saved = Vec::new();
        for p in flac.pictures() {
            // Named by content: one copy per distinct picture across files.
            let digest = Sha256::digest(&p.data);
            let hex: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
            let file = format!("{hex}.{}", ext_of(&p.mime));
            let target = pictures.join(&file);
            if !target.is_file() {
                std::fs::create_dir_all(&pictures).map_err(refuse)?;
                std::fs::write(&target, &p.data).map_err(refuse)?;
            }
            saved.push(PictureBefore {
                saved_as: format!("{DIR}/{change_id}/pictures/{file}"),
                kind: p.kind,
                mime: p.mime.clone(),
                description: p.desc.clone(),
                width: p.width,
                height: p.height,
                depth: p.depth,
                colors: p.colors,
            });
        }
        out.embedded.insert(name.clone(), saved);
    }
    Ok(out)
}

/// Record the before-image of applying `patch` (with the renames `diff`
/// predicts) to the album, and return the change's id. Everything is written
/// before this returns; on any failure nothing is left behind and the caller
/// must not write.
pub fn record(
    conn: &Connection,
    config_dir: &Path,
    input: &ChangeInput<'_>,
    row: &AlbumRow,
    patch: &ProposedPatch,
    diff: &FixDiff,
) -> Result<String, ToolError> {
    let id = format!("ch_{}", random_hex(8));
    let dir: PathBuf = config_dir.join(DIR).join(&id);
    let result = (|| -> Result<(), ToolError> {
        std::fs::create_dir_all(&dir).map_err(refuse)?;
        let names: Vec<String> = file_facts(&row.album.path)
            .into_iter()
            .map(|(n, _, _)| n)
            .collect();
        let before = BeforeImage {
            files: read_prior(&row.album.path, &names, patch)?,
            renames: RenamesBefore {
                files: diff
                    .files
                    .iter()
                    .map(|n| NamePair {
                        from: n.from.clone(),
                        to: n.to.clone(),
                    })
                    .collect(),
                folder: diff.folder.as_ref().map(|n| NamePair {
                    from: n.from.clone(),
                    to: n.to.clone(),
                }),
            },
            cover: if patch.cover.is_some() {
                Some(cover_copy(&dir, &id, Path::new(&row.album.path), &names)?)
            } else {
                None
            },
        };
        let text = serde_json::to_string(&before).map_err(refuse)?;
        std::fs::write(dir.join("before.json"), &text).map_err(refuse)?;
        musaic_db::agent::insert_change(
            conn,
            &musaic_db::agent::ChangeRow {
                id: id.clone(),
                pairing_id: input.pairing_id.to_string(),
                proposal_id: input.proposal_id.to_string(),
                approval_id: input.approval_id.map(str::to_string),
                album_id: row.id().to_string(),
                created_at: input.now.to_string(),
                state: "recorded".into(),
                before: text,
                outcome: None,
            },
        )
        .map_err(refuse)?;
        Ok(())
    })();
    match result {
        Ok(()) => Ok(id),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            Err(e)
        }
    }
}

/// Mark the change `applied`, `partial` or `failed` with the outcome that was
/// reported to the agent.
pub fn finish(conn: &Connection, id: &str, state: &str, outcome: &str) {
    if let Err(e) = musaic_db::agent::finish_change(conn, id, state, outcome) {
        musaic_log::error(
            "musaic.agent",
            &format!("change {id} could not be finished: {e}"),
        );
    }
}

/// The directory of one change's record.
pub fn change_dir(config_dir: &Path, id: &str) -> PathBuf {
    config_dir.join(DIR).join(id)
}
