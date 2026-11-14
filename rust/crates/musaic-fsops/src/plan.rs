//! Filename-mismatch planning (port of `checks/filename_mismatch.py`:
//! `_check_album`, `_check_dir`, `_summarize`, `derive_plan`).
//!
//! One deliberate change from the Python app: the
//! current and template-rendered names are compared after NFC normalisation,
//! so a name that differs from its target only in NFC/NFD form yields no
//! rename.

use std::path::Path;

use musaic_core::album_fields::display_artist;
use musaic_core::pycompat::{is_py_space, strip};
use musaic_db::renames::{RenamePlanRow, rename_plan_row};
use musaic_db::settings;
use musaic_scan::{flacmeta, list_flacs};
use rusqlite::Connection;
use unicode_general_category::{GeneralCategory as G, get_general_category};
use unicode_normalization::UnicodeNormalization;

use crate::pyrepr::py_repr;
use crate::template::{RenderError, Value, Values, render, sanitize_for_filename};

/// The Python app's logger (`musaic.checks.filename_mismatch`).
pub(crate) const TARGET: &str = "musaic.checks.filename_mismatch";

pub const KEY_FILENAME_SINGLE: &str = "filename_template_single_artist";
pub const KEY_FILENAME_COMPILATION: &str = "filename_template_compilation";
pub const KEY_DIR_SINGLE: &str = "dir_template_single_artist";
pub const KEY_DIR_COMPILATION: &str = "dir_template_compilation";

/// The four user templates, as the settings accessors return them (stored
/// value stripped; empty or missing -> default).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Templates {
    pub filename_single: String,
    pub filename_compilation: String,
    pub dir_single: String,
    pub dir_compilation: String,
}

impl Templates {
    pub fn from_settings(conn: &Connection) -> musaic_db::Result<Self> {
        let get = |key: &str| -> musaic_db::Result<String> {
            let raw = settings::get_raw(conn, key)?.unwrap_or_default();
            let raw = strip(&raw);
            Ok(if raw.is_empty() {
                settings::default_for(key).unwrap_or("").to_string()
            } else {
                raw.to_string()
            })
        };
        Ok(Templates {
            filename_single: get(KEY_FILENAME_SINGLE)?,
            filename_compilation: get(KEY_FILENAME_COMPILATION)?,
            dir_single: get(KEY_DIR_SINGLE)?,
            dir_compilation: get(KEY_DIR_COMPILATION)?,
        })
    }

    fn for_row(&self, row: &RenamePlanRow) -> (&str, &str) {
        if row.is_compilation {
            (&self.filename_compilation, &self.dir_compilation)
        } else {
            (&self.filename_single, &self.dir_single)
        }
    }
}

/// One proposed file rename (`detail.renames[]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameEntry {
    pub filename: String,
    pub expected: String,
    /// The raw TRACKNUMBER tag value.
    pub tracknumber: String,
    pub title: String,
    pub artist: String,
    /// The earlier file in this plan that renders to the same name.
    pub collides_with: Option<String>,
}

/// `{"old": current basename, "new": rendered basename}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirRename {
    pub old: String,
    pub new: String,
}

/// `derive_plan`'s result (the finding-detail shape).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub dir_name: Option<String>,
    pub is_compilation: bool,
    pub template: String,
    pub dir_template: String,
    pub headline: String,
    pub renames: Vec<RenameEntry>,
    pub dir_rename: Option<DirRename>,
    /// The row's path is not a directory on disk (`album_missing: true`).
    pub album_missing: bool,
    /// Rendering raised (`error: "Couldn't re-derive plan; see server log"`).
    pub error: Option<String>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.renames.is_empty() && self.dir_rename.is_none()
    }
}

fn same_nfc(a: &str, b: &str) -> bool {
    a == b || a.nfc().eq(b.nfc())
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn first_tag(meta: &flacmeta::FlacMeta, key: &str) -> String {
    meta.values(key).next().unwrap_or("").to_string()
}

fn opt(s: &Option<String>) -> &str {
    s.as_deref().unwrap_or("")
}

/// `_check_album`: render the expected filename of every FLAC (shared listing:
/// `._` skipped, inode-deduped, name order) and list the ones that differ.
/// Tracks without a title or track number, unreadable files and empty
/// renders are skipped.
pub fn check_album(
    album_path: &Path,
    template: &str,
    row: &RenamePlanRow,
) -> Result<Vec<RenameEntry>, RenderError> {
    let mut renames = Vec::new();
    let mut seen_targets: std::collections::HashMap<String, String> = Default::default();
    for flac in list_flacs(album_path) {
        let meta = match flacmeta::read(&flac) {
            Ok(m) => m,
            Err(e) => {
                // filename_mismatch.py:213; the reason is this build's
                // reader error, not mutagen's.
                musaic_log::warning(
                    TARGET,
                    &format!("filename-mismatch: couldn't open {}: {e}", flac.display()),
                );
                continue;
            }
        };
        let tracknumber = first_tag(&meta, "tracknumber");
        let title = first_tag(&meta, "title");
        let artist = first_tag(&meta, "artist");
        let albumartist = first_tag(&meta, "albumartist");
        if title.is_empty() || tracknumber.is_empty() {
            continue;
        }
        let artist_value = if !artist.is_empty() {
            artist.clone()
        } else {
            albumartist.clone()
        };
        let albumartist_value = if !albumartist.is_empty() {
            albumartist.clone()
        } else {
            opt(&row.album_artist).to_string()
        };
        let mut values = Values::new();
        values.insert("tracknumber", Value::from(tracknumber.as_str()));
        values.insert("title", Value::from(title.as_str()));
        values.insert("artist", Value::from(artist_value.as_str()));
        values.insert("albumartist", Value::from(albumartist_value));
        values.insert("album", Value::from(opt(&row.title)));
        values.insert("year", Value::from(opt(&row.year)));
        values.insert("disc", Value::from(row.disc_number));
        let rendered = render(template, &values)?;
        if rendered.is_empty() {
            continue;
        }
        let expected = format!("{rendered}.flac");
        let name = file_name(&flac);
        // A normalisation-only difference is not a mismatch.
        if same_nfc(&expected, &name) {
            continue;
        }
        let collides_with = seen_targets.insert(expected.clone(), name.clone());
        renames.push(RenameEntry {
            filename: name,
            expected,
            tracknumber,
            title,
            artist: artist_value,
            collides_with,
        });
    }
    Ok(renames)
}

/// Python `\w` for one character: alphanumeric (str.isalnum) or '_'.
fn is_py_word(c: char) -> bool {
    c == '_'
        || matches!(
            get_general_category(c),
            G::UppercaseLetter
                | G::LowercaseLetter
                | G::TitlecaseLetter
                | G::ModifierLetter
                | G::OtherLetter
                | G::DecimalNumber
                | G::LetterNumber
                | G::OtherNumber
        )
}

/// Start of a match of `\s+\[\w+\]\s*$` (`re.search`), if any.
fn trailing_tag_start(s: &str) -> Option<usize> {
    let body = s.trim_end_matches(is_py_space);
    let inner = body.strip_suffix(']')?;
    let word_start = inner
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_py_word(*c))
        .last()
        .map(|(i, _)| i)?;
    let before = inner[..word_start].strip_suffix('[')?;
    let ws_start = before.trim_end_matches(is_py_space).len();
    (ws_start < before.len()).then_some(ws_start)
}

/// `_check_dir`: the album directory's expected basename, when it differs.
pub fn check_dir(
    album_path: &Path,
    template: &str,
    row: &RenamePlanRow,
) -> Result<Option<DirRename>, RenderError> {
    let current = file_name(album_path);
    let title = opt(&row.title);
    if strip(title).is_empty() {
        return Ok(None);
    }
    let aa = display_artist(
        row.album_artist.as_deref(),
        row.artist.as_deref(),
        row.dir_artist.as_deref(),
        true,
    );
    let mut values = Values::new();
    values.insert("title", Value::from(title));
    values.insert("album", Value::from(title));
    values.insert("artist", Value::from(aa.as_str()));
    values.insert("albumartist", Value::from(aa));
    values.insert("year", Value::from(opt(&row.year)));
    let mut rendered = render(template, &values)?;
    if rendered.is_empty() {
        return Ok(None);
    }
    if let (Some(n), Some(total)) = (row.disc_number, row.disc_total)
        && total > 1
        && n != 0
    {
        let suffix = format!("(disc {n} of {total})");
        rendered = match trailing_tag_start(&rendered) {
            Some(i) => format!("{} {suffix}{}", &rendered[..i], &rendered[i..]),
            None => format!("{rendered} {suffix}"),
        };
        rendered = sanitize_for_filename(&rendered);
    }
    // A normalisation-only difference is not a mismatch.
    if same_nfc(&rendered, &current) {
        return Ok(None);
    }
    Ok(Some(DirRename {
        old: current,
        new: rendered,
    }))
}

/// `derive_plan(album_id)`: re-derive the plan against current disk + DB +
/// settings. `None` when the album is unknown or Missing.
pub fn derive_plan(conn: &Connection, album_id: &str) -> musaic_db::Result<Option<Plan>> {
    let templates = Templates::from_settings(conn)?;
    let Some(row) = rename_plan_row(conn, album_id)? else {
        return Ok(None);
    };
    let headline = match row.dir_name.as_deref() {
        Some(d) if !d.is_empty() => d.to_string(),
        _ => album_id.to_string(),
    };
    let mut plan = Plan {
        dir_name: row.dir_name.clone(),
        is_compilation: row.is_compilation,
        template: String::new(),
        dir_template: String::new(),
        headline,
        renames: Vec::new(),
        dir_rename: None,
        album_missing: false,
        error: None,
    };
    let path = opt(&row.path);
    if path.is_empty() || !Path::new(path).is_dir() {
        plan.album_missing = true;
        return Ok(Some(plan));
    }
    let (template, dir_template) = templates.for_row(&row);
    plan.template = template.to_string();
    plan.dir_template = dir_template.to_string();
    let album_path = Path::new(path);
    match check_album(album_path, template, &row)
        .and_then(|r| Ok((r, check_dir(album_path, dir_template, &row)?)))
    {
        Ok((renames, dir_rename)) => {
            plan.renames = renames;
            plan.dir_rename = dir_rename;
        }
        Err(e) => {
            // filename_mismatch.py:419 `log.exception(...)`: ERROR; the
            // error text stands in for the traceback.
            musaic_log::error(
                TARGET,
                &format!("filename-mismatch: derive_plan error for {path}: {e}"),
            );
            plan.error = Some("Couldn't re-derive plan; see server log".into());
        }
    }
    Ok(Some(plan))
}

/// `_summarize`: the finding summary line.
pub fn summarize(dir_name: Option<&str>, renames: usize, has_dir_rename: bool) -> String {
    let label = py_repr(match dir_name {
        Some(d) if !d.is_empty() => d,
        _ => "(unknown album)",
    });
    let files = |n: usize| format!("{n} file{}", if n != 1 { "s" } else { "" });
    if has_dir_rename && renames > 0 {
        return format!(
            "Album dir + {} in {label} don't match the templates",
            files(renames)
        );
    }
    if has_dir_rename {
        return format!("Album dir {label} doesn't match the template");
    }
    format!("{} in {label} don't match the template", files(renames))
}
