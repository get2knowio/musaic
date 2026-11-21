//! The before-image of an agent-caused apply (FR-054): what the files hold
//! now, recorded before the first byte is written; the after-image of what
//! the change left; and what the desktop's Undo (FR-055) derives from them.
//!
//! For each file the change will touch, the prior value (or absence) of every
//! tag field it names (and of the alias spellings a removal deletes with the
//! field); the prior file and folder names when it renames; and,
//! when it replaces the cover, a byte-for-byte copy of everything a cover
//! apply overwrites or deletes, under `<config dir>/agent-changes/<id>/`:
//! every PICTURE block of every FLAC file (the apply drops them all and embeds
//! one) and every `cover.jpg` / `cover.jpeg` / `cover.png` sidecar, in any
//! case (the apply writes `cover.jpg` and deletes the others). Picture data is
//! stored once per distinct content, so an album whose files share one
//! picture keeps one copy, while files that differ each keep their own. The
//! record is a row of `agent_changes`
//! (state `recorded`, then `applied`, `partial` or `failed`, and `undone`
//! once the desktop user undid it) and a `before.json` beside the copy, so it
//! survives a restart and a lost database row alike. If any part cannot be
//! recorded the apply is refused and nothing is written. Rows older than 90
//! days are pruned with their directories (at startup).
//!
//! Once the change finished, its after-image is stored beside it
//! (`after.json` and the row's `after`): for every file it touched, the name
//! the file has now and the values of the same fields, digests of the
//! pictures and sidecars a cover change wrote, and the folder's name. An undo
//! compares the files with it ([`changed_since`]) and refuses when anything
//! differs, so it never overwrites a later edit.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use musaic_commands::auth::random_hex;
use musaic_commands::tools::fix::{FixDiff, ProposedPatch};
use musaic_commands::{ToolError, ToolErrorCode};
use musaic_scan::flacmeta;
use musaic_tags::apply::{FIELD_TO_VORBIS, TRACK_FIELD_TO_VORBIS};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::agent_read::file_facts;
use crate::albums::AlbumRow;
use crate::apply::RestoreRequest;
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
    /// File name -> Vorbis key -> prior value, for the other spellings a
    /// removal deletes with its field (`ALBUM_ARTIST` with `album_artist`,
    /// `YEAR` with `year`); only for fields the change removes.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub aliases: BTreeMap<String, BTreeMap<String, Prior>>,
    pub renames: RenamesBefore,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<CoverBefore>,
}

impl BeforeImage {
    /// Every file the record names, by its name before the change.
    pub fn file_names(&self) -> BTreeSet<String> {
        let mut out: BTreeSet<String> = self.files.keys().cloned().collect();
        out.extend(self.aliases.keys().cloned());
        out.extend(self.renames.files.iter().map(|n| n.from.clone()));
        if let Some(c) = &self.cover {
            out.extend(c.embedded.keys().cloned());
        }
        out
    }

    /// The album-level fields the change touched (`genre`, `year`, ...).
    pub fn album_fields(&self) -> Vec<String> {
        let set: BTreeSet<String> = self
            .files
            .values()
            .flat_map(|f| f.keys())
            .filter_map(|k| k.strip_prefix(ALBUM_PREFIX))
            .map(str::to_string)
            .collect();
        set.into_iter().collect()
    }

    /// What the change did, in a few words for the desktop's list of changes.
    pub fn summary(&self) -> String {
        let label = |f: &str| f.replace('_', " ");
        let mut parts = Vec::new();
        let album = self.album_fields();
        if !album.is_empty() {
            parts.push(format!(
                "album {}",
                album
                    .iter()
                    .map(|f| label(f))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        let mut track_fields: BTreeSet<String> = BTreeSet::new();
        let mut track_files = 0;
        for fields in self.files.values() {
            let here: Vec<&str> = fields
                .keys()
                .filter_map(|k| k.strip_prefix(TRACK_PREFIX))
                .collect();
            if !here.is_empty() {
                track_files += 1;
                track_fields.extend(here.into_iter().map(label));
            }
        }
        if track_files > 0 {
            parts.push(format!(
                "track {} on {} file{}",
                track_fields.into_iter().collect::<Vec<_>>().join(", "),
                track_files,
                if track_files == 1 { "" } else { "s" }
            ));
        }
        let renamed = self.renames.files.len();
        match (renamed, self.renames.folder.is_some()) {
            (0, false) => {}
            (0, true) => parts.push("renamed the folder".into()),
            (n, folder) => parts.push(format!(
                "renamed {n} file{}{}",
                if n == 1 { "" } else { "s" },
                if folder { " and the folder" } else { "" }
            )),
        }
        if self.cover.is_some() {
            parts.push("replaced the cover".into());
        }
        if parts.is_empty() {
            "no tag, name or cover change".into()
        } else {
            let mut s = parts.join("; ");
            if let Some(first) = s.get_mut(0..1) {
                first.make_ascii_uppercase();
            }
            s
        }
    }
}

pub struct ChangeInput<'a> {
    pub pairing_id: &'a str,
    pub proposal_id: &'a str,
    pub approval_id: Option<&'a str>,
    pub now: &'a str,
}

/// What a before-image covers, per file by the name it has now: the
/// `album.<field>` / `track.<field>` keys and the raw alias keys.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileKeys {
    pub fields: Vec<String>,
    pub aliases: Vec<String>,
}

/// Everything a before-image is recorded for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageSpec {
    pub keys: BTreeMap<String, FileKeys>,
    pub renames: RenamesBefore,
    pub cover: bool,
}

impl ImageSpec {
    /// What applying `patch` to the files `names` touches, with the renames
    /// `diff` predicts.
    pub fn of_patch(names: &[String], patch: &ProposedPatch, diff: &FixDiff) -> ImageSpec {
        let mut keys = BTreeMap::new();
        if let Some(tags) = &patch.tags {
            let album: Vec<(&String, bool)> = tags
                .album
                .iter()
                .flatten()
                .map(|(f, v)| (f, v.is_none()))
                .collect();
            for name in names {
                let track: Vec<&String> = tags
                    .tracks
                    .as_ref()
                    .and_then(|t| t.get(name))
                    .map(|m| m.keys().collect())
                    .unwrap_or_default();
                if album.is_empty() && track.is_empty() {
                    continue;
                }
                let mut k = FileKeys::default();
                for (f, clears) in &album {
                    k.fields.push(format!("{ALBUM_PREFIX}{f}"));
                    if *clears && let Some(vk) = vorbis_of(&format!("{ALBUM_PREFIX}{f}")) {
                        k.aliases.extend(
                            musaic_tags::removal_aliases(vk)
                                .iter()
                                .map(|a| a.to_string()),
                        );
                    }
                }
                for f in track {
                    k.fields.push(format!("{TRACK_PREFIX}{f}"));
                }
                keys.insert(name.clone(), k);
            }
        }
        ImageSpec {
            keys,
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
            cover: patch.cover.is_some(),
        }
    }
}

/// The Vorbis key of an `album.<field>` / `track.<field>` key.
pub fn vorbis_of(key: &str) -> Option<&'static str> {
    let find = |table: &[(&'static str, &'static str)], f: &str| {
        table.iter().find(|(name, _)| *name == f).map(|(_, v)| *v)
    };
    if let Some(f) = key.strip_prefix(ALBUM_PREFIX) {
        find(&FIELD_TO_VORBIS, f)
    } else if let Some(f) = key.strip_prefix(TRACK_PREFIX) {
        find(&TRACK_FIELD_TO_VORBIS, f)
    } else {
        None
    }
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

type Priors = BTreeMap<String, BTreeMap<String, Prior>>;

/// Read the prior values of the keys `spec` names, from the files now.
fn read_prior(album_dir: &str, spec: &ImageSpec) -> Result<(Priors, Priors), ToolError> {
    let mut files = BTreeMap::new();
    let mut aliases = BTreeMap::new();
    for (name, keys) in &spec.keys {
        let meta = flacmeta::read(&Path::new(album_dir).join(name)).map_err(refuse)?;
        let read = |vk: &str| Prior::of(meta.values(vk).map(str::to_string).collect());
        // `album.<field>` and `track.<field>`: the two vocabularies share names
        // (`title` is ALBUM at album level and TITLE on a track).
        let fields: BTreeMap<String, Prior> = keys
            .fields
            .iter()
            .filter_map(|k| vorbis_of(k).map(|vk| (k.clone(), read(vk))))
            .collect();
        if !fields.is_empty() {
            files.insert(name.clone(), fields);
        }
        let alias: BTreeMap<String, Prior> =
            keys.aliases.iter().map(|k| (k.clone(), read(k))).collect();
        if !alias.is_empty() {
            aliases.insert(name.clone(), alias);
        }
    }
    Ok((files, aliases))
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
        .filter(|n| musaic_tags::is_cover_sidecar_name(n) && album_dir.join(n).is_file())
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
    let names: Vec<String> = file_facts(&row.album.path)
        .into_iter()
        .map(|(n, _, _)| n)
        .collect();
    let spec = ImageSpec::of_patch(&names, patch, diff);
    record_spec(conn, config_dir, input, row, &spec, None)
}

/// [`record`] for an explicit [`ImageSpec`]; `undoes` names the change an
/// undo puts back (the undo records what it overwrites like any change).
pub fn record_spec(
    conn: &Connection,
    config_dir: &Path,
    input: &ChangeInput<'_>,
    row: &AlbumRow,
    spec: &ImageSpec,
    undoes: Option<&str>,
) -> Result<String, ToolError> {
    let id = format!("ch_{}", random_hex(8));
    let dir: PathBuf = config_dir.join(DIR).join(&id);
    let result = (|| -> Result<(), ToolError> {
        std::fs::create_dir_all(&dir).map_err(refuse)?;
        let names: Vec<String> = file_facts(&row.album.path)
            .into_iter()
            .map(|(n, _, _)| n)
            .collect();
        let (files, aliases) = read_prior(&row.album.path, spec)?;
        let before = BeforeImage {
            files,
            aliases,
            renames: spec.renames.clone(),
            cover: if spec.cover {
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
                after: None,
                undoes: undoes.map(str::to_string),
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

// ------------------------------------------------------------- after-image

/// One file as the change left it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileAfter {
    /// The bare name the folder lists for it now.
    pub name: String,
    /// The same keys as the before-image, with the values the file holds.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, Prior>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub aliases: BTreeMap<String, Prior>,
    /// Digests of its PICTURE blocks in order, when the change replaced the
    /// cover.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pictures: Option<Vec<String>>,
    /// The file could not be read when the change finished: an undo cannot
    /// tell whether it changed since and refuses.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unread: bool,
}

/// What a finished change left on disk (`agent_changes.after`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AfterImage {
    /// The album's id once the change finished (a folder rename changes it).
    #[serde(rename = "albumId")]
    pub album_id: String,
    /// The folder's bare name.
    pub folder: String,
    /// By the file's name before the change.
    pub files: BTreeMap<String, FileAfter>,
    /// Name -> digest of every cover sidecar, when the change replaced the
    /// cover.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidecars: Option<BTreeMap<String, String>>,
}

fn hex16(bytes: &[u8]) -> String {
    bytes[..16].iter().map(|b| format!("{b:02x}")).collect()
}

/// A digest of a PICTURE block: its fields and its data.
fn picture_digest(p: &Picture) -> String {
    let mut h = Sha256::new();
    for n in [p.kind, p.width, p.height, p.depth, p.colors] {
        h.update(n.to_be_bytes());
    }
    for s in [&p.mime, &p.desc] {
        h.update((s.len() as u64).to_be_bytes());
        h.update(s.as_bytes());
    }
    h.update(&p.data);
    hex16(&h.finalize())
}

fn sidecar_digests(album_dir: &Path) -> BTreeMap<String, String> {
    std::fs::read_dir(album_dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| musaic_tags::is_cover_sidecar_name(n))
        .filter_map(|n| {
            let bytes = std::fs::read(album_dir.join(&n)).ok()?;
            Some((n, hex16(&Sha256::digest(&bytes))))
        })
        .collect()
}

/// The name the folder lists for `name` (asked for by the rename, possibly
/// spelled differently on disk), by same-file identity.
fn listed_name(dir: &Path, name: &str) -> Option<String> {
    let path = dir.join(name);
    if !path.is_file() {
        return None;
    }
    let handle = same_file::Handle::from_path(&path).ok();
    let names = musaic_scan::dir_names(dir).ok()?;
    musaic_scan::listed_spelling(dir, name, &names, handle.as_ref())
        .map(|n| n.to_string_lossy().into_owned())
}

/// The values of `keys` and, when `pictures`, the picture digests of the
/// file at `path`; `None` when it cannot be read.
fn read_file(
    path: &Path,
    fields: &BTreeMap<String, Prior>,
    aliases: &BTreeMap<String, Prior>,
    pictures: bool,
) -> Option<FileAfter> {
    let flac = FlacFile::open(path).ok()?;
    let read = |vk: &str| {
        Prior::of(
            flac.tags()
                .map(|t| t.values(vk).map(str::to_string).collect())
                .unwrap_or_default(),
        )
    };
    Some(FileAfter {
        name: String::new(),
        fields: fields
            .keys()
            .filter_map(|k| vorbis_of(k).map(|vk| (k.clone(), read(vk))))
            .collect(),
        aliases: aliases.keys().map(|k| (k.clone(), read(k))).collect(),
        pictures: pictures.then(|| {
            flac.pictures()
                .into_iter()
                .map(picture_digest)
                .collect::<Vec<_>>()
        }),
        unread: false,
    })
}

/// Read what a change left in `album_dir` (the album's folder now, under
/// `album_id`). `renamed` maps a file's name before the change to the name
/// the pipeline's own track-number rename gave it, when it did.
pub fn read_after(
    album_id: &str,
    album_dir: &Path,
    before: &BeforeImage,
    renamed: &BTreeMap<String, String>,
) -> AfterImage {
    let empty = BTreeMap::new();
    let cover = before.cover.is_some();
    let mut files = BTreeMap::new();
    for orig in before.file_names() {
        // The predicted final name first: a swap of two numbers leaves a
        // different file under the original name.
        let predicted = before
            .renames
            .files
            .iter()
            .find(|n| n.from == orig)
            .map(|n| n.to.clone());
        let candidates = predicted
            .iter()
            .chain(renamed.get(&orig))
            .chain(std::iter::once(&orig));
        let mut candidates = candidates;
        let name = candidates
            .find_map(|c| listed_name(album_dir, c))
            .unwrap_or_else(|| predicted.clone().unwrap_or_else(|| orig.clone()));
        let fields = before.files.get(&orig).unwrap_or(&empty);
        let aliases = before.aliases.get(&orig).unwrap_or(&empty);
        let file = match read_file(&album_dir.join(&name), fields, aliases, cover) {
            Some(f) => FileAfter { name, ..f },
            None => FileAfter {
                name,
                unread: true,
                ..FileAfter::default()
            },
        };
        files.insert(orig, file);
    }
    AfterImage {
        album_id: album_id.to_string(),
        folder: album_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        files,
        sidecars: cover.then(|| sidecar_digests(album_dir)),
    }
}

/// The before-image of a recorded change, when its row is there and readable.
pub fn load_before(conn: &Connection, id: &str) -> Option<BeforeImage> {
    let row = musaic_db::agent::get_change(conn, id).ok()??;
    serde_json::from_str(&row.before).ok()
}

/// Store the after-image of a finished change (the row and `after.json`).
/// A failure is logged: the change stands, but it cannot be undone.
pub fn save_after(conn: &Connection, config_dir: &Path, id: &str, after: &AfterImage) {
    let text = match serde_json::to_string(after) {
        Ok(t) => t,
        Err(e) => {
            musaic_log::error(
                "musaic.agent",
                &format!("change {id}: the after-image could not be written: {e}"),
            );
            return;
        }
    };
    let _ = std::fs::write(change_dir(config_dir, id).join("after.json"), &text);
    if let Err(e) = musaic_db::agent::set_change_after(conn, id, &text) {
        musaic_log::error(
            "musaic.agent",
            &format!("change {id}: the after-image could not be stored: {e}"),
        );
    }
}

/// What differs between the album's files now (in `album_dir`) and what the
/// change left, in plain words, one entry per file and field; empty when
/// nothing does. File identity is the folder's own (same-file), never a
/// comparison of path strings.
pub fn changed_since(album_dir: &Path, after: &AfterImage) -> Vec<String> {
    let mut out = Vec::new();
    let same_folder = album_dir
        .parent()
        .map(|p| p.join(&after.folder))
        .is_some_and(|p| same_file::is_same_file(&p, album_dir).unwrap_or(false));
    if !same_folder {
        out.push("the album folder was renamed or moved".to_string());
    }
    for f in after.files.values() {
        if f.unread {
            out.push(format!(
                "{}: could not be read when the change finished",
                f.name
            ));
            continue;
        }
        let path = album_dir.join(&f.name);
        if listed_name(album_dir, &f.name).is_none() {
            out.push(format!("{}: renamed, moved or deleted", f.name));
            continue;
        }
        let Some(now) = read_file(&path, &f.fields, &f.aliases, f.pictures.is_some()) else {
            out.push(format!("{}: could not be read", f.name));
            continue;
        };
        for (k, v) in &f.fields {
            if now.fields.get(k) != Some(v) {
                let field = k
                    .strip_prefix(ALBUM_PREFIX)
                    .or_else(|| k.strip_prefix(TRACK_PREFIX))
                    .unwrap_or(k);
                out.push(format!("{}: {}", f.name, field.replace('_', " ")));
            }
        }
        for (k, v) in &f.aliases {
            if now.aliases.get(k) != Some(v) {
                out.push(format!("{}: {k}", f.name));
            }
        }
        if f.pictures.is_some() && now.pictures != f.pictures {
            out.push(format!("{}: embedded cover", f.name));
        }
    }
    if let Some(sidecars) = &after.sidecars
        && sidecar_digests(album_dir) != *sidecars
    {
        out.push("the cover image files in the folder".to_string());
    }
    out.dedup();
    out
}

/// A path saved in a record, resolved under the config dir; `None` when it
/// would leave the change's records directory.
fn saved_path(config_dir: &Path, saved_as: &str) -> Option<PathBuf> {
    let rel = Path::new(saved_as);
    let inside =
        rel.starts_with(DIR) && rel.components().all(|c| matches!(c, Component::Normal(_)));
    inside.then(|| config_dir.join(rel))
}

fn read_saved(config_dir: &Path, saved_as: &str) -> Result<Vec<u8>, String> {
    let path = saved_path(config_dir, saved_as)
        .ok_or_else(|| format!("{saved_as} is not a path of a change record"))?;
    std::fs::read(&path).map_err(|e| format!("{saved_as} could not be read: {e}"))
}

/// What putting `before` back writes, by the files' names before the change
/// (an undo renames them back first): every recorded key set to its
/// recorded values, every recorded picture and sidecar written back byte for
/// byte from the record's copies.
pub fn restore_request(config_dir: &Path, before: &BeforeImage) -> Result<RestoreRequest, String> {
    let mut tags: BTreeMap<String, BTreeMap<String, Vec<String>>> = BTreeMap::new();
    for (file, fields) in &before.files {
        for (k, prior) in fields {
            if let Some(vk) = vorbis_of(k) {
                tags.entry(file.clone())
                    .or_default()
                    .insert(vk.to_string(), prior.values());
            }
        }
    }
    for (file, aliases) in &before.aliases {
        for (k, prior) in aliases {
            tags.entry(file.clone())
                .or_default()
                .insert(k.clone(), prior.values());
        }
    }
    let mut req = RestoreRequest {
        tags: musaic_tags::Restore {
            tags: tags
                .into_iter()
                .map(|(f, keys)| (f, keys.into_iter().collect()))
                .collect(),
            pictures: Vec::new(),
        },
        sidecars: None,
        fields: before.album_fields(),
    };
    if let Some(cover) = &before.cover {
        for (file, pictures) in &cover.embedded {
            let mut blocks = Vec::new();
            for p in pictures {
                blocks.push(p.to_picture(read_saved(config_dir, &p.saved_as)?));
            }
            req.tags.pictures.push((file.clone(), blocks));
        }
        let mut sidecars = Vec::new();
        for s in &cover.sidecars {
            sidecars.push((s.name.clone(), read_saved(config_dir, &s.saved_as)?));
        }
        req.sidecars = Some(sidecars);
    }
    Ok(req)
}
