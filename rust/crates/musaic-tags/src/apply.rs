//! Album-wide apply: port of `tags.apply_to_album_iter` plus the cover steps
//! of `services/apply.apply_album_changes_iter`.
//!
//! Events are delivered to a callback in the Python app's order, so the HTTP/SSE
//! layer can forward them as they happen. Ordered maps (Python dicts) are
//! `Vec<(key, value)>`; a repeated key behaves like a dict assignment (value
//! replaced, first position kept).
//!
//! # Removing a tag
//!
//! A suggestion only ever sets a value: an empty, whitespace-only or null
//! suggestion is "no change", so a source whose data is missing (a resolver
//! field it could not fill, a model that answered `""`) never clears a tag.
//! Removal is a separate, explicit request ([`Removals`]); the Python app
//! had no way to remove a tag. Deliberate change: a whitespace-only
//! suggestion is skipped like an empty one; the Python app stripped it after
//! the emptiness test and wrote an empty tag.

use std::io;
use std::path::{Path, PathBuf};

use musaic_core::pycompat;

use crate::cover::{build_picture, normalize_to_jpeg};
use crate::flac::{FlacFile, Picture};
use crate::sidecar::save_cover_to_album_dir;

/// `_FIELD_TO_VORBIS`: album-level suggestion key -> Vorbis key, as written
/// (lower case: mutagen stores the key spelling it is given).
pub const FIELD_TO_VORBIS: [(&str, &str); 8] = [
    ("title", "album"),
    ("album_artist", "albumartist"),
    ("artist", "artist"),
    ("year", "date"),
    ("genre", "genre"),
    ("album_artist_sort", "albumartistsort"),
    ("artist_sort", "artistsort"),
    ("album_sort", "albumsort"),
];

/// `_TRACK_FIELD_TO_VORBIS`: per-track suggestion key -> Vorbis key.
pub const TRACK_FIELD_TO_VORBIS: [(&str, &str); 4] = [
    ("title", "title"),
    ("artist", "artist"),
    ("tracknumber", "tracknumber"),
    ("artist_sort", "artistsort"),
];

/// Suggestion values as the Python app receives them, already `str()`-ed by the
/// caller; `None` stands for JSON null. `None`, `""` and whitespace-only
/// values are skipped: a suggestion never removes a tag (see [`Removals`]).
pub type Suggestions = Vec<(String, Option<String>)>;
/// Filename -> per-track suggestions.
pub type TrackSuggestions = Vec<(String, Suggestions)>;

/// Spellings the scanner reads as the same field as a key this module
/// writes (`musaic-scan`'s tag fallbacks). Removing the field removes them
/// too, or the scanner would show the field again from the other spelling.
/// ORIGINALDATE / ORIGINALYEAR are another field (the first release) and
/// stay.
const REMOVAL_ALIASES: [(&str, &[&str]); 2] =
    [("albumartist", &["album_artist"]), ("date", &["year"])];

/// Per-track fields that cannot be removed: the scanner orders tracks by
/// TRACKNUMBER and file renames key on it. Correct it by setting a number.
pub const TRACK_FIELDS_NOT_REMOVABLE: [&str; 1] = ["tracknumber"];

/// Whether `field` names an album-level field a [`Removals`] can remove (a
/// key of [`FIELD_TO_VORBIS`]).
pub fn is_removable_album_field(field: &str) -> bool {
    FIELD_TO_VORBIS.iter().any(|(f, _)| *f == field)
}

/// The album-level field names a [`Removals`] can remove, in table order.
pub fn removable_album_fields() -> Vec<&'static str> {
    FIELD_TO_VORBIS.iter().map(|(f, _)| *f).collect()
}

/// The per-track field names a [`Removals`] can remove, in table order.
pub fn removable_track_fields() -> Vec<&'static str> {
    TRACK_FIELD_TO_VORBIS
        .iter()
        .map(|(f, _)| *f)
        .filter(|f| is_removable_track_field(f))
        .collect()
}

/// Whether `field` names a per-track field a [`Removals`] can remove (a key
/// of [`TRACK_FIELD_TO_VORBIS`] other than [`TRACK_FIELDS_NOT_REMOVABLE`]).
pub fn is_removable_track_field(field: &str) -> bool {
    TRACK_FIELD_TO_VORBIS.iter().any(|(f, _)| *f == field)
        && !TRACK_FIELDS_NOT_REMOVABLE.contains(&field)
}

/// Fields to remove from the files: only ever an explicit request (a user
/// blanking a field), never derived from a source's missing data.
///
/// A removal deletes every value of the field's Vorbis key under any
/// spelling of the key (`GENRE`, `genre`, ...), plus the aliases the scanner
/// reads as the same field (ALBUM_ARTIST for album_artist, YEAR for year).
/// A file without the tag is left alone. Names that are not removable
/// fields are ignored here (the HTTP layer refuses them). For one file and
/// one Vorbis key, a per-track entry overrides an album-level one, as with
/// suggestions, and a removal overrides a suggestion given with it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Removals {
    /// Album-level field names (keys of [`FIELD_TO_VORBIS`]), removed from
    /// every FLAC of the album.
    pub album: Vec<String>,
    /// Filename -> per-track field names (see [`is_removable_track_field`]).
    pub tracks: Vec<(String, Vec<String>)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diff {
    /// The value before: the first value of the key ("" when absent). For a
    /// removal, every value the key held, joined with "; " (the way the
    /// scanner shows a multi-valued genre).
    pub old: String,
    /// The value written, or `None` when the tag is removed (JSON null).
    pub new: Option<String>,
}

/// One planned edit of a Vorbis key.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Edit {
    Set(String),
    Remove,
}

/// `FileChange` as `change_to_dict` renders it, plus `renamed_to`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: PathBuf,
    pub filename: String,
    pub diffs: Vec<(String, Diff)>,
    pub art_changed: bool,
    pub saved: bool,
    /// The Python app's text is mutagen's exception message; only presence is
    /// part of the contract.
    pub error: Option<String>,
    pub renamed_to: Option<String>,
    /// What the save could not carry over from the file it rewrote
    /// ([`crate::SaveReport::warnings`]); the tags were written. Not in the
    /// Python app's event (it shifted the audio in place).
    pub warnings: Vec<String>,
}

/// A file whose TRACKNUMBER was written but which kept its old name (its
/// `FileDone` carried `renamed_to`). Not in the Python app's events, which
/// only logged these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameFailure {
    /// The name the file has now.
    pub filename: String,
    /// The name it was to get.
    pub new_filename: String,
    /// Why it was not renamed.
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    CoverDownloaded {
        source_bytes: usize,
        source_mime: String,
        bytes: usize,
        mime: String,
    },
    Start {
        album_path: PathBuf,
        dry_run: bool,
        file_count: usize,
        suggestions_applied: Vec<(String, String)>,
        /// Album-level fields removed (user-facing names). Not part of
        /// the Python app's payload; sent only when not empty.
        cleared: Vec<String>,
        has_cover_change: bool,
        track_change_count: usize,
    },
    FileStarted {
        index: usize,
        filename: String,
        path: PathBuf,
    },
    FileDone(FileChange),
    FileRenamed {
        old_path: PathBuf,
        new_path: PathBuf,
        old_filename: String,
        new_filename: String,
    },
    Done {
        album_path: PathBuf,
        dry_run: bool,
        saved_count: usize,
        art_changed_count: usize,
        renamed_count: usize,
        errors: usize,
        suggestions_applied: Vec<(String, String)>,
        /// As in [`Event::Start`].
        cleared: Vec<String>,
        /// Files whose tags were written but whose rename did not happen.
        rename_failures: Vec<RenameFailure>,
    },
    CoverSaved {
        path: PathBuf,
        filename: String,
    },
}

impl Event {
    /// The Python app's event name.
    pub fn name(&self) -> &'static str {
        match self {
            Event::CoverDownloaded { .. } => "cover_downloaded",
            Event::Start { .. } => "start",
            Event::FileStarted { .. } => "file_started",
            Event::FileDone(_) => "file_done",
            Event::FileRenamed { .. } => "file_renamed",
            Event::Done { .. } => "done",
            Event::CoverSaved { .. } => "cover_saved",
        }
    }
}

fn dict_set<V>(map: &mut Vec<(String, V)>, key: &str, value: V) {
    match map.iter_mut().find(|(k, _)| k == key) {
        Some(slot) => slot.1 = value,
        None => map.push((key.to_string(), value)),
    }
}

/// `{table[k]: str(v).strip() for k, v in sug.items() if k in table and v not in (None, "")}`,
/// except that a value that strips to nothing is skipped too.
fn translate(sug: &Suggestions, table: &[(&str, &str)]) -> Vec<(String, Edit)> {
    // The caller's dict first: a repeated key keeps its first position.
    let mut dict: Vec<(String, Option<String>)> = Vec::new();
    for (k, v) in sug {
        match dict.iter_mut().find(|(dk, _)| dk == k) {
            Some(slot) => slot.1 = v.clone(),
            None => dict.push((k.clone(), v.clone())),
        }
    }
    let mut out = Vec::new();
    for (k, v) in dict {
        let Some(v) = v.map(|v| pycompat::strip(&v).to_string()) else {
            continue;
        };
        // Deliberate change: the Python app tested for "" before stripping
        // and wrote an empty tag for a whitespace-only value.
        if v.is_empty() {
            continue;
        }
        if let Some((_, vk)) = table.iter().find(|(f, _)| *f == k) {
            dict_set(&mut out, vk, Edit::Set(v));
        }
    }
    out
}

/// Add the removals of `fields` (those `removable` accepts) to `out`.
fn add_removals(
    out: &mut Vec<(String, Edit)>,
    fields: &[String],
    table: &[(&str, &str)],
    removable: fn(&str) -> bool,
) {
    for f in fields {
        if !removable(f) {
            continue;
        }
        if let Some((_, vk)) = table.iter().find(|(name, _)| name == f) {
            dict_set(out, vk, Edit::Remove);
        }
    }
}

/// The user-facing field name of an album-level Vorbis key.
fn album_field_of(vk: &str) -> &str {
    FIELD_TO_VORBIS
        .iter()
        .rev()
        .find(|(_, x)| *x == vk)
        .map_or(vk, |(f, _)| f)
}

/// `_proposed_filename`: replace a leading numeric prefix matched by
/// `^(?P<prefix>\d+(?:[-.]\d+)?)(?P<sep>[\s\-_.]+)` with `f"{int(n):02d}"`.
pub fn proposed_filename(current_name: &str, new_tracknumber: &str) -> Option<String> {
    let prefix_end = filename_prefix_end(current_name)?;
    let n = pycompat::int(new_tracknumber)?;
    Some(format!("{n:02}{}", &current_name[prefix_end..]))
}

fn filename_prefix_end(name: &str) -> Option<usize> {
    let is_digit = |c: char| pycompat::decimal_digit_value(c).is_some();
    let is_sep = |c: char| pycompat::is_py_space(c) || matches!(c, '-' | '_' | '.');
    let digits_end = |from: usize| {
        name[from..]
            .char_indices()
            .find(|(_, c)| !is_digit(*c))
            .map_or(name.len(), |(i, _)| from + i)
    };
    let sep_at = |i: usize| name[i..].chars().next().is_some_and(is_sep);
    let a = digits_end(0);
    if a == 0 {
        return None;
    }
    // Greedy optional `[-.]\d+`, then backtrack to the bare digits.
    if let Some(c @ ('-' | '.')) = name[a..].chars().next() {
        let b = digits_end(a + c.len_utf8());
        if b > a + 1 && sep_at(b) {
            return Some(b);
        }
    }
    sep_at(a).then_some(a)
}

/// `apply_to_album_iter(album_dir, suggestions, track_suggestions=...,
/// cover_bytes=..., cover_mime=..., dry_run=...)`, plus `removals`.
///
/// `cover` is the (already normalised) image bytes and the MIME type to
/// record in the PICTURE block. Errors only when `album_dir` is not a
/// directory (the Python app's `FileNotFoundError`); per-file failures become
/// `FileDone.error`.
pub fn apply_to_album_iter(
    album_dir: &Path,
    suggestions: &Suggestions,
    track_suggestions: &TrackSuggestions,
    removals: &Removals,
    cover: Option<(&[u8], &str)>,
    dry_run: bool,
    emit: &mut dyn FnMut(Event),
) -> io::Result<()> {
    if !album_dir.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("Not a directory: {}", album_dir.display()),
        ));
    }
    let mut album_applied = translate(suggestions, &FIELD_TO_VORBIS);
    add_removals(
        &mut album_applied,
        &removals.album,
        &FIELD_TO_VORBIS,
        is_removable_album_field,
    );
    let user_friendly: Vec<(String, String)> = album_applied
        .iter()
        .filter_map(|(vk, e)| match e {
            Edit::Set(v) => Some((album_field_of(vk).to_string(), v.clone())),
            Edit::Remove => None,
        })
        .collect();
    let cleared: Vec<String> = album_applied
        .iter()
        .filter(|(_, e)| *e == Edit::Remove)
        .map(|(vk, _)| album_field_of(vk).to_string())
        .collect();
    // The caller's dict of filename -> suggestions (last value of a repeated
    // filename wins, first position kept), then translated per file.
    let mut raw_by_file: Vec<(&String, &Suggestions)> = Vec::new();
    for (filename, raw) in track_suggestions {
        match raw_by_file.iter_mut().find(|(f, _)| *f == filename) {
            Some(slot) => slot.1 = raw,
            None => raw_by_file.push((filename, raw)),
        }
    }
    let mut per_file: Vec<(String, Vec<(String, Edit)>)> = raw_by_file
        .into_iter()
        .map(|(f, raw)| (f.clone(), translate(raw, &TRACK_FIELD_TO_VORBIS)))
        .collect();
    for (filename, fields) in &removals.tracks {
        let i = match per_file.iter().position(|(f, _)| f == filename) {
            Some(i) => i,
            None => {
                per_file.push((filename.clone(), Vec::new()));
                per_file.len() - 1
            }
        };
        add_removals(
            &mut per_file[i].1,
            fields,
            &TRACK_FIELD_TO_VORBIS,
            is_removable_track_field,
        );
    }
    per_file.retain(|(_, t)| !t.is_empty());

    let flacs = musaic_scan::list_flacs(album_dir);
    let pic: Option<Picture> = cover.map(|(bytes, mime)| build_picture(bytes.to_vec(), mime));

    emit(Event::Start {
        album_path: album_dir.to_path_buf(),
        dry_run,
        file_count: flacs.len(),
        suggestions_applied: user_friendly.clone(),
        cleared: cleared.clone(),
        has_cover_change: pic.is_some(),
        track_change_count: per_file.len(),
    });
    let done =
        |saved_count, art_changed_count, renamed_count, errors, rename_failures| Event::Done {
            album_path: album_dir.to_path_buf(),
            dry_run,
            saved_count,
            art_changed_count,
            renamed_count,
            errors,
            suggestions_applied: user_friendly.clone(),
            cleared: cleared.clone(),
            rename_failures,
        };
    if flacs.is_empty() {
        emit(done(0, 0, 0, 0, Vec::new()));
        return Ok(());
    }

    let (mut saved_count, mut art_changed_count, mut errors) = (0, 0, 0);
    let mut pending_renames: Vec<(PathBuf, String)> = Vec::new();
    for (index, flac_path) in flacs.iter().enumerate() {
        let filename = flac_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        emit(Event::FileStarted {
            index,
            filename: filename.clone(),
            path: flac_path.clone(),
        });
        let mut change = FileChange {
            path: flac_path.clone(),
            filename: filename.clone(),
            diffs: Vec::new(),
            art_changed: false,
            saved: false,
            error: None,
            renamed_to: None,
            warnings: Vec::new(),
        };
        let file_specific = per_file
            .iter()
            .find(|(f, _)| *f == filename)
            .map(|(_, s)| s.as_slice())
            .unwrap_or(&[]);
        let result = (|| -> io::Result<()> {
            let mut audio = FlacFile::open(flac_path)?;
            let mut combined = album_applied.clone();
            for (k, v) in file_specific {
                dict_set(&mut combined, k, v.clone());
            }
            for (vkey, edit) in &combined {
                match edit {
                    Edit::Set(new_val) => {
                        let old_val = audio
                            .tags()
                            .and_then(|t| t.first(vkey))
                            .unwrap_or("")
                            .to_string();
                        if old_val != *new_val {
                            change.diffs.push((
                                vkey.clone(),
                                Diff {
                                    old: old_val,
                                    new: Some(new_val.clone()),
                                },
                            ));
                            if !dry_run {
                                audio.tags_mut().set(vkey, new_val);
                            }
                        }
                    }
                    Edit::Remove => {
                        let aliases = REMOVAL_ALIASES
                            .iter()
                            .find(|(k, _)| k == vkey)
                            .map_or(&[][..], |(_, a)| *a);
                        for key in std::iter::once(vkey.as_str()).chain(aliases.iter().copied()) {
                            let old: Vec<String> = audio
                                .tags()
                                .map(|t| t.values(key).map(str::to_string).collect())
                                .unwrap_or_default();
                            // Absent: nothing to remove. A present but empty
                            // value is removed (and shown as a diff).
                            if old.is_empty() {
                                continue;
                            }
                            change.diffs.push((
                                key.to_string(),
                                Diff {
                                    old: old.join("; "),
                                    new: None,
                                },
                            ));
                            if !dry_run {
                                audio.tags_mut().remove(key);
                            }
                        }
                    }
                }
            }
            if let Some(pic) = &pic {
                change.art_changed = true;
                if !dry_run {
                    audio.clear_pictures();
                    audio.add_picture(pic.clone());
                }
            }
            if !dry_run && (!change.diffs.is_empty() || change.art_changed) {
                change.warnings = audio.save()?.warnings;
                change.saved = true;
                saved_count += 1;
                if change.art_changed {
                    art_changed_count += 1;
                }
            }
            if let Some((_, Edit::Set(n))) = file_specific.iter().find(|(k, _)| k == "tracknumber")
                && let Some(proposed) = proposed_filename(&filename, n)
                && proposed != filename
            {
                pending_renames.push((flac_path.clone(), proposed.clone()));
                change.renamed_to = Some(proposed);
            }
            Ok(())
        })();
        if let Err(e) = result {
            change.error = Some(e.to_string());
            errors += 1;
        }
        emit(Event::FileDone(change));
    }

    let mut renamed_count = 0;
    let mut rename_failures: Vec<RenameFailure> = Vec::new();
    if !pending_renames.is_empty() && !dry_run {
        let name_of = |p: &Path| {
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        // Two-pass rename so swapped numbers never collide mid-flight.
        let mut staged: Vec<(PathBuf, PathBuf, PathBuf)> = Vec::new();
        // Per staged entry: settled (renamed, or rolled back and reported).
        let mut settled: Vec<bool> = Vec::new();
        // The entry the batch stopped at, and why.
        let mut failed: Option<(String, io::Error)> = None;
        for (orig, target_name) in &pending_renames {
            let tmp = orig.with_file_name(format!(".{}.renaming.tmp", name_of(orig)));
            if let Err(e) = std::fs::rename(orig, &tmp) {
                failed = Some((name_of(orig), e));
                break;
            }
            staged.push((orig.clone(), tmp, orig.with_file_name(target_name)));
            settled.push(false);
        }
        if failed.is_none() {
            for (k, (orig, tmp, fin)) in staged.iter().enumerate() {
                if fin.exists() {
                    // tags.py:277
                    musaic_log::warning(
                        "musaic.tags",
                        &format!("rename target exists, rolling back: {}", fin.display()),
                    );
                    if let Err(e) = std::fs::rename(tmp, orig) {
                        failed = Some((name_of(orig), e));
                        break;
                    }
                    errors += 1;
                    settled[k] = true;
                    rename_failures.push(RenameFailure {
                        filename: name_of(orig),
                        new_filename: name_of(fin),
                        error: format!("{} already exists", name_of(fin)),
                    });
                    continue;
                }
                if let Err(e) = std::fs::rename(tmp, fin) {
                    failed = Some((name_of(orig), e));
                    break;
                }
                settled[k] = true;
                renamed_count += 1;
                emit(Event::FileRenamed {
                    old_path: orig.clone(),
                    new_path: fin.clone(),
                    old_filename: name_of(orig),
                    new_filename: name_of(fin),
                });
            }
        }
        if let Some((at, e)) = failed {
            // tags.py:290 `logger.exception(...)`: ERROR with a traceback;
            // here the error text stands in for the traceback.
            musaic_log::error(
                "musaic.tags",
                &format!("rename batch failed; attempting rollback: {e}"),
            );
            errors += 1;
            // Every file not renamed is reported by name: its TRACKNUMBER
            // was written, its name was not changed.
            for (k, (orig, tmp, fin)) in staged.iter().enumerate() {
                if settled[k] {
                    continue;
                }
                let mut error = if name_of(orig) == at {
                    e.to_string()
                } else {
                    format!("not renamed: the batch stopped at {at}")
                };
                if tmp.exists() && !orig.exists() {
                    if let Err(e2) = std::fs::rename(tmp, orig) {
                        error = format!("{error}; the file is left as {}: {e2}", name_of(tmp));
                    }
                } else if tmp.exists() {
                    error = format!(
                        "{error}; the file is left as {}: its name is taken",
                        name_of(tmp)
                    );
                }
                rename_failures.push(RenameFailure {
                    filename: name_of(orig),
                    new_filename: name_of(fin),
                    error,
                });
            }
            // Entries the first pass never reached keep their names too.
            for (orig, target_name) in &pending_renames[staged.len()..] {
                rename_failures.push(RenameFailure {
                    filename: name_of(orig),
                    new_filename: target_name.clone(),
                    error: if name_of(orig) == at {
                        e.to_string()
                    } else {
                        format!("not renamed: the batch stopped at {at}")
                    },
                });
            }
        }
    }
    emit(done(
        saved_count,
        art_changed_count,
        renamed_count,
        errors,
        rename_failures,
    ));
    Ok(())
}

/// A cover supplied with the apply (`cover_bytes` + `cover_mime`, the
/// manual-upload path; the URL path downloads first and then does the same).
#[derive(Debug, Clone, Copy)]
pub struct CoverInput<'a> {
    pub bytes: &'a [u8],
    pub mime: Option<&'a str>,
}

/// The file-mutating steps of `apply_album_changes_iter` (1b, 2, 3): normalise
/// the cover to JPEG q95, apply tags + picture to every FLAC, then write the
/// `cover.jpg` sidecar unless `dry_run`. The DB upsert, rescan and bus events
/// belong to other layers.
pub fn apply_album_files(
    album_dir: &Path,
    suggestions: &Suggestions,
    track_suggestions: &TrackSuggestions,
    removals: &Removals,
    cover: Option<CoverInput<'_>>,
    dry_run: bool,
    emit: &mut dyn FnMut(Event),
) -> io::Result<()> {
    let normalized = cover.map(|c| {
        let out = normalize_to_jpeg(c.bytes, 95);
        emit(Event::CoverDownloaded {
            source_bytes: c.bytes.len(),
            source_mime: c.mime.unwrap_or("image/jpeg").to_string(),
            bytes: out.len(),
            mime: "image/jpeg".to_string(),
        });
        out
    });
    apply_to_album_iter(
        album_dir,
        suggestions,
        track_suggestions,
        removals,
        normalized.as_deref().map(|b| (b, "image/jpeg")),
        dry_run,
        emit,
    )?;
    if dry_run {
        return Ok(());
    }
    if let Some(bytes) = &normalized
        && let Some(saved) = save_cover_to_album_dir(album_dir, bytes)
    {
        let filename = saved
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        emit(Event::CoverSaved {
            path: saved,
            filename,
        });
    }
    Ok(())
}
