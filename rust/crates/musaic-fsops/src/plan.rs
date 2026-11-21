//! Filename-mismatch planning (port of `checks/filename_mismatch.py`:
//! `_check_album`, `_check_dir`, `_summarize`, `derive_plan`).
//!
//! One deliberate change from the Python app: the
//! current and template-rendered names are compared after NFC normalisation,
//! so a name that differs from its target only in NFC/NFD form yields no
//! rename.
//!
//! A name that differs from its target only in case is a mismatch, as in
//! the Python app: `execute_renames` carries out case-only renames on every
//! volume, so the check's finding and the fix agree. Deliberate change: two
//! targets that differ only in case collide (`collides_with`), since
//! `execute_renames` refuses such a pair; so does a target that equals the
//! name of a file that is not being renamed.

use std::path::Path;

use musaic_core::album_fields::display_artist;
use musaic_core::pycompat::{is_py_space, strip};
use musaic_db::renames::{RenamePlanRow, rename_plan_row};
use musaic_db::settings;
use musaic_scan::{flacmeta, list_flacs};
use rusqlite::Connection;
use unicode_general_category::{GeneralCategory as G, get_general_category};
use unicode_normalization::UnicodeNormalization;

use crate::fsutil::fold;
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

    pub fn for_row(&self, row: &RenamePlanRow) -> (&str, &str) {
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

pub(crate) fn same_nfc(a: &str, b: &str) -> bool {
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

/// The tags of one file a filename template reads.
#[derive(Debug, Clone, Copy)]
pub struct TrackTags<'a> {
    pub title: &'a str,
    pub artist: &'a str,
    pub albumartist: &'a str,
}

/// One file's name under `template`, without the extension: the file's own
/// title and artists (ARTIST falling back to ALBUMARTIST, ALBUMARTIST to
/// the row's), the row's album and year, and the given track number and
/// disc. The filename check and the multi-disc conversion preview both
/// render through this, so they propose the same names.
pub fn render_track_stem(
    template: &str,
    tags: &TrackTags<'_>,
    row: &RenamePlanRow,
    tracknumber: &str,
    disc: Option<i64>,
) -> Result<String, RenderError> {
    let artist_value = if !tags.artist.is_empty() {
        tags.artist
    } else {
        tags.albumartist
    };
    let albumartist_value = if !tags.albumartist.is_empty() {
        tags.albumartist
    } else {
        opt(&row.album_artist)
    };
    let mut values = Values::new();
    values.insert("title", Value::from(tags.title));
    values.insert("artist", Value::from(artist_value));
    values.insert("albumartist", Value::from(albumartist_value));
    values.insert("album", Value::from(opt(&row.title)));
    values.insert("year", Value::from(opt(&row.year)));
    values.insert("tracknumber", Value::from(tracknumber));
    values.insert("disc", Value::from(disc));
    render(template, &values)
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
    // Every FLAC in the folder; the ones left out of the plan keep their
    // names (see `render_file_targets`).
    let mut all_names: Vec<String> = Vec::new();
    // Read once: the disc rule needs every file before any name is rendered.
    let mut files: Vec<(std::path::PathBuf, flacmeta::FlacMeta)> = Vec::new();
    for flac in list_flacs(album_path) {
        all_names.push(file_name(&flac));
        match flacmeta::read(&flac) {
            Ok(m) => files.push((flac, m)),
            Err(e) => {
                // filename_mismatch.py:213; the reason is this build's
                // reader error, not mutagen's.
                musaic_log::warning(
                    TARGET,
                    &format!("filename-mismatch: couldn't open {}: {e}", flac.display()),
                );
            }
        }
    }
    // The rule a renumber uses, so the two agree on every file's prefix.
    let multi_disc = flacmeta::set_spans_discs(row.disc_number, row.disc_total, || {
        let mut ev = flacmeta::DiscEvidence::default();
        for (_, m) in &files {
            ev.add(m);
        }
        ev
    });
    let targets: Vec<TargetFile> = files
        .iter()
        .map(|(flac, meta)| {
            let tracknumber = first_tag(meta, "tracknumber");
            let disc = if multi_disc {
                flacmeta::file_disc(meta.values("discnumber"), meta.values("tracknumber"))
                    .or(row.disc_number)
            } else {
                None
            };
            TargetFile {
                name: file_name(flac),
                tracknumber,
                title: first_tag(meta, "title"),
                artist: first_tag(meta, "artist"),
                albumartist: first_tag(meta, "albumartist"),
                disc,
            }
        })
        .collect();
    render_file_targets(&targets, &all_names, template, row)
}

/// One file as the filename template sees it: its listed name, the tags the
/// template reads and the disc rule's answer for it (`None` outside a
/// multi-disc set).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetFile {
    pub name: String,
    pub tracknumber: String,
    pub title: String,
    pub artist: String,
    pub albumartist: String,
    pub disc: Option<i64>,
}

/// The renames a set of files would need under `template`, from their tags as
/// given (the tags on disc now, or the tags a proposed change would write):
/// the shared core of [`check_album`] and the agent bridge's rename
/// prediction. `all_names` are every FLAC of the folder; the ones left out of
/// the plan (no title or track number, empty render) keep their names.
pub fn render_file_targets(
    files: &[TargetFile],
    all_names: &[String],
    template: &str,
    row: &RenamePlanRow,
) -> Result<Vec<RenameEntry>, RenderError> {
    let mut renames = Vec::new();
    let mut seen_targets: std::collections::HashMap<String, String> = Default::default();
    for f in files {
        let TargetFile {
            name,
            tracknumber,
            title,
            artist,
            albumartist,
            disc,
        } = f;
        let disc = *disc;
        if title.is_empty() || tracknumber.is_empty() {
            continue;
        }
        let artist_value = if !artist.is_empty() {
            artist.clone()
        } else {
            albumartist.clone()
        };
        // The disc part of a "2-05" track number is left out only where
        // `{disc}` renders it, or it would show twice. Under a pattern
        // without `{disc}` the tag passes through whole, as it always has:
        // dropping the disc from such names without renumbering the tracks
        // would leave two files numbered 05 in one folder.
        let disc_renders =
            disc.is_some() && (template.contains("{disc}") || template.contains("{disc:"));
        let track_part = if disc_renders {
            flacmeta::split_disc_track(tracknumber).map_or(tracknumber.as_str(), |t| t.1)
        } else {
            tracknumber.as_str()
        };
        let tags = TrackTags {
            title,
            artist,
            albumartist,
        };
        let rendered = render_track_stem(template, &tags, row, track_part, disc)?;
        if rendered.is_empty() {
            continue;
        }
        let expected = format!("{rendered}.flac");
        // A normalisation-only difference is not a mismatch.
        if same_nfc(&expected, name) {
            continue;
        }
        // Keyed as an insensitive volume compares names, so two targets
        // that differ only in case are a collision here as they are for
        // `execute_renames` (which refuses them on every volume).
        let collides_with = seen_targets.insert(fold(&expected), name.clone());
        renames.push(RenameEntry {
            filename: name.clone(),
            expected,
            tracknumber: tracknumber.clone(),
            title: title.clone(),
            artist: artist_value,
            collides_with,
        });
    }
    // A target that equals (folded) the name of a file that stays where it
    // is (already correct, or one the plan skips) would land on that file:
    // `execute_renames` refuses it, so the preview flags it too. A file that
    // is itself being renamed does not count, since it moves away (and a
    // case-only rename of a file onto its own spelling is no collision).
    let renamed: std::collections::HashSet<&str> =
        renames.iter().map(|r| r.filename.as_str()).collect();
    let staying: std::collections::HashMap<String, &String> = all_names
        .iter()
        .filter(|n| !renamed.contains(n.as_str()))
        .map(|n| (fold(n), n))
        .collect();
    for r in &mut renames {
        if let Some(existing) = staying.get(&fold(&r.expected)) {
            r.collides_with = Some((*existing).clone());
        }
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
    dir_rename_for(&file_name(album_path), template, row)
}

/// [`check_dir`] for a folder known by its current name.
pub fn dir_rename_for(
    current: &str,
    template: &str,
    row: &RenamePlanRow,
) -> Result<Option<DirRename>, RenderError> {
    let Some(rendered) = render_dir_name(template, row)? else {
        return Ok(None);
    };
    // A normalisation-only difference is not a mismatch.
    if same_nfc(&rendered, current) {
        return Ok(None);
    }
    Ok(Some(DirRename {
        old: current.to_string(),
        new: rendered,
    }))
}

/// The names an album would get under the user's templates, from tag values
/// as given: the file renames and the folder rename, as [`derive_plan`]
/// derives them from what is on disc, so a prediction made from the values a
/// change would write agrees with the plan the rename then derives.
pub fn render_targets(
    templates: &Templates,
    row: &RenamePlanRow,
    files: &[TargetFile],
    all_names: &[String],
    current_dir_name: &str,
) -> Result<(Vec<RenameEntry>, Option<DirRename>), RenderError> {
    let (template, dir_template) = templates.for_row(row);
    let renames = render_file_targets(files, all_names, template, row)?;
    let dir = dir_rename_for(current_dir_name, dir_template, row)?;
    Ok((renames, dir))
}

/// The album folder's name under the directory `template`, with the
/// `(disc n of total)` suffix when the row names a disc of a larger set.
/// `None` when the row has no title or the template renders nothing.
pub fn render_dir_name(template: &str, row: &RenamePlanRow) -> Result<Option<String>, RenderError> {
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
    // A disc number with no total is still a disc: leaving it out would
    // propose one folder name for every disc of the set. A total of one is a
    // single disc and needs no suffix.
    let suffix = match (row.disc_number, row.disc_total) {
        (Some(n), Some(total)) if total > 1 && n != 0 => Some(format!("(disc {n} of {total})")),
        (Some(n), None) if n != 0 => Some(format!("(disc {n})")),
        _ => None,
    };
    if let Some(suffix) = suffix {
        rendered = match trailing_tag_start(&rendered) {
            Some(i) => format!("{} {suffix}{}", &rendered[..i], &rendered[i..]),
            None => format!("{rendered} {suffix}"),
        };
        rendered = sanitize_for_filename(&rendered);
    }
    Ok(Some(rendered))
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

#[cfg(test)]
mod render_targets_tests {
    use super::*;

    fn row() -> RenamePlanRow {
        RenamePlanRow {
            id: "0123456789abcdef".into(),
            path: Some("/x/Old Name".into()),
            dir_name: Some("Old Name".into()),
            album_artist: Some("Artist".into()),
            artist: Some("Artist".into()),
            dir_artist: None,
            title: Some("Album".into()),
            year: Some("2001".into()),
            is_compilation: false,
            disc_number: None,
            disc_total: None,
        }
    }

    fn templates() -> Templates {
        Templates {
            filename_single: "{tracknumber:02} - {title}".into(),
            filename_compilation: "{tracknumber:02} - {artist} - {title}".into(),
            dir_single: "{albumartist} - {album} [FLAC]".into(),
            dir_compilation: "Various Artists - {album} [FLAC]".into(),
        }
    }

    fn file(name: &str, n: &str, title: &str) -> TargetFile {
        TargetFile {
            name: name.into(),
            tracknumber: n.into(),
            title: title.into(),
            artist: "Artist".into(),
            albumartist: "Artist".into(),
            disc: None,
        }
    }

    #[test]
    fn targets_follow_the_values_given_not_the_disc() {
        let files = [
            file("a.flac", "1", "Song One"),
            file("b.flac", "2", "Song Two"),
        ];
        let names: Vec<String> = files.iter().map(|f| f.name.clone()).collect();
        let (renames, dir) =
            render_targets(&templates(), &row(), &files, &names, "Old Name").unwrap();
        assert_eq!(
            renames
                .iter()
                .map(|r| (r.filename.as_str(), r.expected.as_str()))
                .collect::<Vec<_>>(),
            [
                ("a.flac", "01 - Song One.flac"),
                ("b.flac", "02 - Song Two.flac")
            ]
        );
        assert_eq!(
            dir,
            Some(DirRename {
                old: "Old Name".into(),
                new: "Artist - Album [FLAC]".into()
            })
        );
        // A file already named right, and a folder already named right, are no rename.
        let ok = [file("01 - Song One.flac", "1", "Song One")];
        let (renames, dir) = render_targets(
            &templates(),
            &row(),
            &ok,
            &["01 - Song One.flac".into()],
            "Artist - Album [FLAC]",
        )
        .unwrap();
        assert!(renames.is_empty() && dir.is_none());
    }

    #[test]
    fn two_files_rendering_to_one_name_collide() {
        let files = [file("a.flac", "1", "Same"), file("b.flac", "1", "Same")];
        let names: Vec<String> = files.iter().map(|f| f.name.clone()).collect();
        let (renames, _) = render_targets(&templates(), &row(), &files, &names, "x").unwrap();
        assert_eq!(renames[1].collides_with.as_deref(), Some("a.flac"));
    }
}
