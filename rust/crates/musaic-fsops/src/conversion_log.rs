//! The record a multi-disc conversion keeps of what it did.
//!
//! A conversion removes the disc tags, so afterwards the files no longer
//! say which track was on which disc. Before it changes the first file it
//! writes the whole plan here (every file's old and new name and its old
//! and new numbering tags, the folder's old and new name, the album id
//! before and after), and then the outcome of each step as it happens. A
//! later "split back", a support request or an interrupted conversion reads
//! it back.
//!
//! One JSON file per conversion under `<config dir>/conversions/`, not a
//! database table: a database restore (or a snapshot from before the
//! conversion) must not take the only record of the old layout with it, and
//! a file needs no schema step. Each write goes to a temporary file that is
//! flushed to disk and then renamed over the record, so a crash leaves
//! either the previous version or the new one, never half of one. Nothing
//! secret is in it: paths, names and tag values of the user's own files.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// The folder under the config dir that holds the records.
pub const LOG_DIR_NAME: &str = "conversions";

/// `format` of every record, so a reader can tell it from other JSON.
pub const FORMAT: &str = "musaic-multi-disc-conversion";

/// The record format's version for a set kept in one folder.
pub const VERSION: i64 = 1;

/// The version of a record of a set gathered from several folders
/// (`kind: "folders"`): the members, each file's member and the folder
/// plans. A build that knows only version 1 skips such a record rather than
/// resuming it as a one-folder conversion.
pub const VERSION_FOLDERS: i64 = 2;

/// `kind` of a set kept in one folder.
pub const KIND_ONE_FOLDER: &str = "one_folder";
/// `kind` of a set gathered from several folders.
pub const KIND_FOLDERS: &str = "folders";

/// The tags a conversion reads and writes, as recorded (the keys are case
/// insensitive in a file; the record spells them in capitals).
pub const NUMBERING_KEYS: [&str; 6] = [
    "TRACKNUMBER",
    "TRACKTOTAL",
    "TOTALTRACKS",
    "DISCNUMBER",
    "DISCTOTAL",
    "TOTALDISCS",
];

/// Every value of each of [`NUMBERING_KEYS`], in that order (a key the
/// file does not have has no values).
pub type NumberingTags = Vec<(String, Vec<String>)>;

/// The numbering tags of a file whose tags `values(key)` lists.
pub fn numbering_of<'a>(values: impl Fn(&str) -> Vec<&'a str>) -> NumberingTags {
    NUMBERING_KEYS
        .iter()
        .map(|k| {
            (
                k.to_string(),
                values(&k.to_lowercase())
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
            )
        })
        .collect()
}

/// The values of `key` in `tags` (none when absent).
pub fn values_of<'a>(tags: &'a NumberingTags, key: &str) -> &'a [String] {
    tags.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map_or(&[], |(_, v)| v.as_slice())
}

/// What a file's numbering tags are after the conversion wrote `track` of
/// `total`: TOTALTRACKS only where the file had it.
pub fn converted_numbering(old: &NumberingTags, track: i64, total: usize) -> NumberingTags {
    let had_totaltracks = !values_of(old, "TOTALTRACKS").is_empty();
    NUMBERING_KEYS
        .iter()
        .map(|k| {
            let v = match *k {
                "TRACKNUMBER" => vec![track.to_string()],
                "TRACKTOTAL" => vec![total.to_string()],
                "TOTALTRACKS" if had_totaltracks => vec![total.to_string()],
                _ => Vec::new(),
            };
            (k.to_string(), v)
        })
        .collect()
}

/// One file of a conversion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedFile {
    /// The member album the file belonged to before the conversion (a
    /// `folders` record; empty in a one-folder record).
    pub album_id: String,
    pub disc: i64,
    /// Its number on its disc.
    pub track: i64,
    pub new_track: i64,
    pub old_name: String,
    pub new_name: String,
    pub old_tags: NumberingTags,
    pub new_tags: NumberingTags,
}

/// One member album of a `folders` conversion, as it was before.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedMember {
    pub album_id: String,
    /// The folder's full path before the conversion.
    pub folder: String,
    pub folder_name: String,
    pub disc: i64,
    /// Disc 1's folder: the one the others are gathered into.
    pub destination: bool,
}

/// What a `folders` conversion does with the entries of one member folder
/// that are not its FLACs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LoggedFolder {
    pub album_id: String,
    pub folder_name: String,
    /// `(name, action)`: `kept`, `moved`, `identical`, `different`.
    pub covers: Vec<(String, String)>,
    /// Operating-system clutter that goes to the app trash.
    pub junk: Vec<String>,
    /// The folder had a track-tag sidecar (merged, then put in the trash).
    pub sidecar: bool,
    /// Files that stay where they are.
    pub other_files: Vec<String>,
}

/// One attempt at running the conversion (a second Convert after an
/// interruption is a second attempt).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    pub started_at: String,
    pub finished_at: Option<String>,
    /// `completed`, `failed` or `refused`; `None` while it runs (or when the
    /// process stopped during it).
    pub outcome: Option<String>,
    /// One object per step: `{step, at, ok, message, results}`.
    pub steps: Vec<Value>,
}

/// A conversion's record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// Where it is on disk.
    pub path: PathBuf,
    pub kind: String,
    pub started_at: String,
    /// Set once every step is done.
    pub finished_at: Option<String>,
    pub library_path: String,
    pub album_id_before: String,
    /// The id the folder's new name gives, computed when planned.
    pub album_id_expected: String,
    /// The id it got (the name the folder lists after its rename).
    pub album_id_after: Option<String>,
    pub folder_before: String,
    pub folder_name_before: String,
    pub folder_name_after: String,
    pub template: String,
    pub dir_template: String,
    pub track_total: usize,
    pub files: Vec<LoggedFile>,
    pub attempts: Vec<Attempt>,
    /// A `folders` record: every member, in disc order (the destination
    /// first). Empty in a one-folder record.
    pub members: Vec<LoggedMember>,
    /// A `folders` record: the plan for each member folder's other entries.
    pub folders: Vec<LoggedFolder>,
    /// A `folders` record: rows Musaic keeps for files no longer on disk
    /// (`(album id, file name)`), carried to the surviving album under the
    /// same name so the fold loses nothing.
    pub orphan_rows: Vec<(String, String)>,
    /// Set when the user chose to stop treating an unfinished conversion
    /// as one (`finished_at` is set with it): the files stay as they are and
    /// scans read the folders again.
    pub abandoned_at: Option<String>,
}

fn tags_json(t: &NumberingTags) -> Value {
    Value::Object(
        t.iter()
            .map(|(k, v)| (k.clone(), json!(v)))
            .collect::<serde_json::Map<_, _>>(),
    )
}

fn tags_from(v: &Value) -> NumberingTags {
    NUMBERING_KEYS
        .iter()
        .map(|k| {
            let vals = v
                .get(*k)
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            (k.to_string(), vals)
        })
        .collect()
}

impl Record {
    /// The id it is filed under: the album id before and the start time.
    pub fn file_name(album_id: &str, started_at: &str) -> String {
        let stamp: String = started_at
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect();
        format!("{stamp}-{album_id}.json")
    }

    /// Whether this conversion is of the album `id` (before or after its
    /// folder was renamed).
    pub fn is_of(&self, id: &str) -> bool {
        self.album_id_before == id
            || self.album_id_expected == id
            || self.album_id_after.as_deref() == Some(id)
            || self.members.iter().any(|m| m.album_id == id)
    }

    /// A set gathered from several folders.
    pub fn is_folders(&self) -> bool {
        self.kind == KIND_FOLDERS
    }

    /// Every album id the conversion touches: the members (or the one
    /// album), and the id the destination has after its folder rename.
    pub fn album_ids(&self) -> Vec<String> {
        let mut v: Vec<String> = vec![self.album_id_before.clone()];
        v.extend(self.members.iter().map(|m| m.album_id.clone()));
        v.push(self.album_id_expected.clone());
        v.extend(self.album_id_after.clone());
        v.sort();
        v.dedup();
        v
    }

    /// Every folder path the conversion touches: each member's folder, and
    /// the destination under its old and its new name.
    pub fn folder_paths(&self) -> Vec<String> {
        let mut v: Vec<String> = vec![self.folder_before.clone()];
        v.extend(self.members.iter().map(|m| m.folder.clone()));
        if let Some(parent) = Path::new(&self.folder_before).parent() {
            v.push(
                parent
                    .join(&self.folder_name_after)
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        v.sort();
        v.dedup();
        v
    }

    pub fn to_json(&self) -> Value {
        let mut v = json!({
            "format": FORMAT,
            "version": if self.is_folders() { VERSION_FOLDERS } else { VERSION },
            "kind": self.kind,
            "started_at": self.started_at,
            "finished_at": self.finished_at,
            "library_path": self.library_path,
            "album_id_before": self.album_id_before,
            "album_id_expected": self.album_id_expected,
            "album_id_after": self.album_id_after,
            "folder_before": self.folder_before,
            "folder_name_before": self.folder_name_before,
            "folder_name_after": self.folder_name_after,
            "template": self.template,
            "dir_template": self.dir_template,
            "track_total": self.track_total,
            "files": self.files.iter().map(|f| json!({
                "album_id": f.album_id,
                "disc": f.disc,
                "track": f.track,
                "new_track": f.new_track,
                "old_name": f.old_name,
                "new_name": f.new_name,
                "old_tags": tags_json(&f.old_tags),
                "new_tags": tags_json(&f.new_tags),
            })).collect::<Vec<_>>(),
            "attempts": self.attempts.iter().map(|a| json!({
                "started_at": a.started_at,
                "finished_at": a.finished_at,
                "outcome": a.outcome,
                "steps": a.steps,
            })).collect::<Vec<_>>(),
        });
        if self.abandoned_at.is_some() {
            v["abandoned_at"] = json!(self.abandoned_at);
        }
        if self.is_folders() {
            v["members"] = json!(
                self.members
                    .iter()
                    .map(|m| json!({
                        "album_id": m.album_id,
                        "folder": m.folder,
                        "folder_name": m.folder_name,
                        "disc": m.disc,
                        "destination": m.destination,
                    }))
                    .collect::<Vec<_>>()
            );
            v["folders"] = json!(self.folders.iter().map(|f| json!({
                "album_id": f.album_id,
                "folder_name": f.folder_name,
                "covers": f.covers.iter().map(|(n, a)| json!({"name": n, "action": a})).collect::<Vec<_>>(),
                "junk": f.junk,
                "sidecar": f.sidecar,
                "other_files": f.other_files,
            })).collect::<Vec<_>>());
            v["orphan_rows"] = json!(
                self.orphan_rows
                    .iter()
                    .map(|(a, n)| json!({"album_id": a, "filename": n}))
                    .collect::<Vec<_>>()
            );
        }
        v
    }

    /// A record read back; `None` when it is not one (another format, a
    /// newer version, a missing field).
    pub fn from_json(path: &Path, v: &Value) -> Option<Record> {
        let version = v.get("version")?.as_i64()?;
        if v.get("format")?.as_str()? != FORMAT
            || !(version == VERSION || version == VERSION_FOLDERS)
        {
            return None;
        }
        let strs = |x: Option<&Value>| -> Vec<String> {
            x.and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        };
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        let files = v
            .get("files")?
            .as_array()?
            .iter()
            .map(|f| {
                Some(LoggedFile {
                    album_id: f
                        .get("album_id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    disc: f.get("disc")?.as_i64()?,
                    track: f.get("track")?.as_i64()?,
                    new_track: f.get("new_track")?.as_i64()?,
                    old_name: f.get("old_name")?.as_str()?.to_string(),
                    new_name: f.get("new_name")?.as_str()?.to_string(),
                    old_tags: tags_from(f.get("old_tags")?),
                    new_tags: tags_from(f.get("new_tags")?),
                })
            })
            .collect::<Option<Vec<_>>>()?;
        let attempts = v
            .get("attempts")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|x| Attempt {
                        started_at: x
                            .get("started_at")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        finished_at: x
                            .get("finished_at")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        outcome: x.get("outcome").and_then(Value::as_str).map(str::to_string),
                        steps: x
                            .get("steps")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let members = match v.get("members").and_then(Value::as_array) {
            None => Vec::new(),
            Some(a) => a
                .iter()
                .map(|m| {
                    Some(LoggedMember {
                        album_id: m.get("album_id")?.as_str()?.to_string(),
                        folder: m.get("folder")?.as_str()?.to_string(),
                        folder_name: m.get("folder_name")?.as_str()?.to_string(),
                        disc: m.get("disc")?.as_i64()?,
                        destination: m.get("destination")?.as_bool()?,
                    })
                })
                .collect::<Option<Vec<_>>>()?,
        };
        let folders = match v.get("folders").and_then(Value::as_array) {
            None => Vec::new(),
            Some(a) => a
                .iter()
                .map(|f| {
                    Some(LoggedFolder {
                        album_id: f.get("album_id")?.as_str()?.to_string(),
                        folder_name: f.get("folder_name")?.as_str()?.to_string(),
                        covers: f
                            .get("covers")
                            .and_then(Value::as_array)
                            .map(|c| {
                                c.iter()
                                    .filter_map(|c| {
                                        Some((
                                            c.get("name")?.as_str()?.to_string(),
                                            c.get("action")?.as_str()?.to_string(),
                                        ))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                        junk: strs(f.get("junk")),
                        sidecar: f.get("sidecar").and_then(Value::as_bool).unwrap_or(false),
                        other_files: strs(f.get("other_files")),
                    })
                })
                .collect::<Option<Vec<_>>>()?,
        };
        let orphan_rows = v
            .get("orphan_rows")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|o| {
                        Some((
                            o.get("album_id")?.as_str()?.to_string(),
                            o.get("filename")?.as_str()?.to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let kind = s("kind")?;
        // A version-1 record never describes several folders, and a
        // `folders` record without its members cannot be resumed.
        if (kind == KIND_FOLDERS) != (version == VERSION_FOLDERS)
            || (kind == KIND_FOLDERS && members.is_empty())
        {
            return None;
        }
        Some(Record {
            path: path.to_path_buf(),
            kind,
            started_at: s("started_at")?,
            finished_at: s("finished_at"),
            library_path: s("library_path")?,
            album_id_before: s("album_id_before")?,
            album_id_expected: s("album_id_expected")?,
            album_id_after: s("album_id_after"),
            folder_before: s("folder_before")?,
            folder_name_before: s("folder_name_before")?,
            folder_name_after: s("folder_name_after")?,
            template: s("template").unwrap_or_default(),
            dir_template: s("dir_template").unwrap_or_default(),
            track_total: v.get("track_total")?.as_u64()? as usize,
            files,
            attempts,
            members,
            folders,
            orphan_rows,
            abandoned_at: s("abandoned_at"),
        })
    }

    /// Write the record durably: a temporary file in the same folder,
    /// flushed to disk, then renamed over the record (and the folder
    /// flushed where the system allows it). Creates the folder.
    pub fn save(&self) -> std::io::Result<()> {
        let dir = self.path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir)?;
        let name = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let tmp = dir.join(format!(".{name}.tmp"));
        let text = serde_json::to_string_pretty(&self.to_json()).map_err(std::io::Error::other)?;
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(text.as_bytes())?;
            f.write_all(b"\n")?;
            f.sync_all()?;
        }
        // Replacing our own record is the point here; `tmp` is ours too.
        std::fs::rename(&tmp, &self.path)?;
        #[cfg(unix)]
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
        Ok(())
    }
}

/// `<config dir>/conversions`.
pub fn log_dir(config_dir: &Path) -> PathBuf {
    config_dir.join(LOG_DIR_NAME)
}

/// Every record in `dir`, oldest first (by start time, then file name).
/// A file that cannot be read or is not a record is skipped.
pub fn all(dir: &Path) -> Vec<Record> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<Record> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|x| x == "json")
                && !p
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with('.'))
        })
        .filter_map(|p| {
            let text = std::fs::read_to_string(&p).ok()?;
            let v: Value = serde_json::from_str(&text).ok()?;
            Record::from_json(&p, &v)
        })
        .collect();
    out.sort_by(|a, b| (&a.started_at, &a.path).cmp(&(&b.started_at, &b.path)));
    out
}

/// The records of conversions that did not finish.
pub fn unfinished(dir: &Path) -> Vec<Record> {
    all(dir)
        .into_iter()
        .filter(|r| r.finished_at.is_none())
        .collect()
}

/// The unfinished records of album `id`.
pub fn unfinished_of(dir: &Path, id: &str) -> Vec<Record> {
    unfinished(dir)
        .into_iter()
        .filter(|r| r.is_of(id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(track: &str, disc: &str) -> NumberingTags {
        let mut t = numbering_of(|_| Vec::new());
        t[0].1 = vec![track.to_string()];
        t[3].1 = vec![disc.to_string()];
        t
    }

    #[test]
    fn round_trip_and_durable_save() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = log_dir(tmp.path());
        let old = tags("5", "2");
        let r = Record {
            path: dir.join(Record::file_name("abc", "2026-01-02T03:04:05Z")),
            kind: "one_folder".into(),
            started_at: "2026-01-02T03:04:05Z".into(),
            finished_at: None,
            library_path: "/lib".into(),
            album_id_before: "abc".into(),
            album_id_expected: "def".into(),
            album_id_after: None,
            folder_before: "/lib/A (disc 1 of 2)".into(),
            folder_name_before: "A (disc 1 of 2)".into(),
            folder_name_after: "A".into(),
            template: "{tracknumber:02} - {title}".into(),
            dir_template: "{album}".into(),
            track_total: 9,
            files: vec![LoggedFile {
                album_id: String::new(),
                disc: 2,
                track: 5,
                new_track: 9,
                old_name: "2-05 - X.flac".into(),
                new_name: "09 - X.flac".into(),
                new_tags: converted_numbering(&old, 9, 9),
                old_tags: old,
            }],
            attempts: vec![Attempt {
                started_at: "2026-01-02T03:04:05Z".into(),
                finished_at: None,
                outcome: None,
                steps: vec![json!({"step": "rename_files", "ok": true})],
            }],
            members: Vec::new(),
            folders: Vec::new(),
            orphan_rows: Vec::new(),
            abandoned_at: None,
        };
        r.save().unwrap();
        assert_eq!(
            r.path.file_name().unwrap().to_string_lossy(),
            "20260102T030405Z-abc.json"
        );
        let back = all(&dir);
        assert_eq!(back, vec![r.clone()]);
        assert_eq!(unfinished_of(&dir, "def").len(), 1);
        assert!(unfinished_of(&dir, "zzz").is_empty());
        // No temporary file is left beside it.
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["20260102T030405Z-abc.json".to_string()]);
        let mut done = r;
        done.finished_at = Some("2026-01-02T03:05:00Z".into());
        done.save().unwrap();
        assert!(unfinished(&dir).is_empty());
        assert_eq!(all(&dir).len(), 1);
    }

    #[test]
    fn converted_numbering_keeps_totaltracks_only_where_present() {
        let mut old = tags("2-05", "2");
        let new = converted_numbering(&old, 17, 26);
        assert_eq!(values_of(&new, "TRACKNUMBER"), ["17"]);
        assert_eq!(values_of(&new, "TRACKTOTAL"), ["26"]);
        assert!(values_of(&new, "TOTALTRACKS").is_empty());
        assert!(values_of(&new, "DISCNUMBER").is_empty());
        old[2].1 = vec!["12".into()];
        assert_eq!(
            values_of(&converted_numbering(&old, 17, 26), "TOTALTRACKS"),
            ["26"]
        );
    }
}
