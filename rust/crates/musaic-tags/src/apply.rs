//! Album-wide apply: port of `tags.apply_to_album_iter` plus the cover steps
//! of `services/apply.apply_album_changes_iter`.
//!
//! Events are delivered to a callback in the Python app's order, so the HTTP/SSE
//! layer can forward them as they happen. Ordered maps (Python dicts) are
//! `Vec<(key, value)>`; a repeated key behaves like a dict assignment (value
//! replaced, first position kept).

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
/// caller; `None` stands for JSON null.
pub type Suggestions = Vec<(String, Option<String>)>;
/// Filename -> per-track suggestions.
pub type TrackSuggestions = Vec<(String, Suggestions)>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diff {
    pub old: String,
    pub new: String,
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

fn dict_set(map: &mut Vec<(String, String)>, key: &str, value: String) {
    match map.iter_mut().find(|(k, _)| k == key) {
        Some(slot) => slot.1 = value,
        None => map.push((key.to_string(), value)),
    }
}

/// `{table[k]: str(v).strip() for k, v in sug.items() if k in table and v not in (None, "")}`
fn translate(sug: &Suggestions, table: &[(&str, &str)]) -> Vec<(String, String)> {
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
        let Some(v) = v.filter(|v| !v.is_empty()) else {
            continue;
        };
        if let Some((_, vk)) = table.iter().find(|(f, _)| *f == k) {
            dict_set(&mut out, vk, pycompat::strip(&v).to_string());
        }
    }
    out
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
/// cover_bytes=..., cover_mime=..., dry_run=...)`.
///
/// `cover` is the (already normalised) image bytes and the MIME type to
/// record in the PICTURE block. Errors only when `album_dir` is not a
/// directory (the Python app's `FileNotFoundError`); per-file failures become
/// `FileDone.error`.
pub fn apply_to_album_iter(
    album_dir: &Path,
    suggestions: &Suggestions,
    track_suggestions: &TrackSuggestions,
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
    let album_applied = translate(suggestions, &FIELD_TO_VORBIS);
    let user_friendly: Vec<(String, String)> = album_applied
        .iter()
        .map(|(vk, v)| {
            let field = FIELD_TO_VORBIS
                .iter()
                .rev()
                .find(|(_, x)| x == vk)
                .map_or(vk.as_str(), |(f, _)| f);
            (field.to_string(), v.clone())
        })
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
    let per_file: Vec<(String, Vec<(String, String)>)> = raw_by_file
        .into_iter()
        .map(|(f, raw)| (f.clone(), translate(raw, &TRACK_FIELD_TO_VORBIS)))
        .filter(|(_, t)| !t.is_empty())
        .collect();

    let flacs = musaic_scan::list_flacs(album_dir);
    let pic: Option<Picture> = cover.map(|(bytes, mime)| build_picture(bytes.to_vec(), mime));

    emit(Event::Start {
        album_path: album_dir.to_path_buf(),
        dry_run,
        file_count: flacs.len(),
        suggestions_applied: user_friendly.clone(),
        has_cover_change: pic.is_some(),
        track_change_count: per_file.len(),
    });
    let done = |saved_count, art_changed_count, renamed_count, errors| Event::Done {
        album_path: album_dir.to_path_buf(),
        dry_run,
        saved_count,
        art_changed_count,
        renamed_count,
        errors,
        suggestions_applied: user_friendly.clone(),
    };
    if flacs.is_empty() {
        emit(done(0, 0, 0, 0));
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
            for (vkey, new_val) in &combined {
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
                            new: new_val.clone(),
                        },
                    ));
                    if !dry_run {
                        audio.tags_mut().set(vkey, new_val);
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
                audio.save()?;
                change.saved = true;
                saved_count += 1;
                if change.art_changed {
                    art_changed_count += 1;
                }
            }
            if let Some((_, n)) = file_specific.iter().find(|(k, _)| k == "tracknumber")
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
    if !pending_renames.is_empty() && !dry_run {
        // Two-pass rename so swapped numbers never collide mid-flight.
        let mut staged: Vec<(PathBuf, PathBuf, PathBuf)> = Vec::new();
        let result = (|| -> io::Result<()> {
            for (orig, target_name) in &pending_renames {
                let name = orig
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let tmp = orig.with_file_name(format!(".{name}.renaming.tmp"));
                std::fs::rename(orig, &tmp)?;
                staged.push((orig.clone(), tmp, orig.with_file_name(target_name)));
            }
            for (orig, tmp, fin) in &staged {
                if fin.exists() {
                    // tags.py:277
                    musaic_log::warning(
                        "musaic.tags",
                        &format!("rename target exists, rolling back: {}", fin.display()),
                    );
                    std::fs::rename(tmp, orig)?;
                    errors += 1;
                    continue;
                }
                std::fs::rename(tmp, fin)?;
                renamed_count += 1;
                emit(Event::FileRenamed {
                    old_path: orig.clone(),
                    new_path: fin.clone(),
                    old_filename: orig
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    new_filename: fin
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                });
            }
            Ok(())
        })();
        if let Err(e) = result {
            // tags.py:290 `logger.exception(...)`: ERROR with a traceback;
            // here the error text stands in for the traceback.
            musaic_log::error(
                "musaic.tags",
                &format!("rename batch failed; attempting rollback: {e}"),
            );
            errors += 1;
            for (orig, tmp, _) in &staged {
                if tmp.exists() && !orig.exists() {
                    let _ = std::fs::rename(tmp, orig);
                }
            }
        }
    }
    emit(done(saved_count, art_changed_count, renamed_count, errors));
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
