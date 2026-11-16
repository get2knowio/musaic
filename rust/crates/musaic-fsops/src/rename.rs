//! Filename-mismatch execution (port of `filename_mismatch.execute_renames`).
//!
//! The Python app publishes `AlbumRenamed(old, new)` from inside this function;
//! here the event is returned in [`RenameOutcome::album_renamed`] for the
//! caller to publish:
//! - after a successful directory rename (old id -> new id), also when a
//!   file rename then fails with an I/O error;
//! - when nothing was left to rename and no directory was renamed
//!   (id -> id), including "Nothing to rename" and "already renamed";
//! - after at least one file rename without a directory rename (id -> id).
//!
//! Deliberate changes from the Python app:
//! - A plan in which two entries share a target (equal after NFC
//!   normalisation and lower-casing, i.e. equal on a case- or
//!   normalisation-insensitive volume; conservatively also refused on a
//!   case-sensitive one) is refused as a whole.
//! - Every check (separators, duplicate targets, collisions with
//!   existing files) runs before the directory is renamed, so a refusal
//!   changes nothing. A failure that still happens after the directory
//!   rename (an I/O error) returns `ok: false` with `detail.dir_renamed:
//!   true`, the new id and the event; the caller publishes the event and
//!   returns the outcome as a result (the Python app's API already answers 200,
//!   not 409, when `dir_renamed` is set) so the client refreshes to the new id.
//! - An existing target that is the same file (samefile) as one of
//!   the plan's sources is not a collision, so case-only and
//!   normalisation-only renames work on insensitive volumes. Every move
//!   goes through a temporary name with no-clobber semantics, so the file
//!   always has a name and nothing is overwritten.
//! - track_fingerprints rows follow each file rename (with
//!   track_tags, one transaction).
//! - After file renames the track-tag sidecar is rewritten (only
//!   when the album has track tags, as apply.py does).
//! - A Missing album (`missing_since` set) is refused before anything
//!   is touched; the Python app went ahead and failed at the filesystem.
//! - The post-rename upsert files the album under its own row's
//!   `library_path`, not the configured library dir (they differ after
//!   the library setting changes).
//! - Plan entries that are skipped (source not found, or already renamed)
//!   are returned in [`RenameOutcome::skipped`] and summed up in the
//!   message, not only logged. A file that failed, or that could not be
//!   moved back from its temporary name, is named in the message.
//! - Case-only renames (`one.flac` -> `One.flac`, `abba - x` -> `ABBA - x`)
//!   are real renames on every volume. Files always move through a
//!   temporary name; a directory does when the target is the same directory
//!   under another spelling (same-file), and otherwise is never moved onto
//!   an existing name. After each rename the parent directory is read back:
//!   a volume that took the rename but kept another spelling (one that does
//!   not preserve case) has the file or directory put back under its old
//!   name, and the entry is reported as not renamed, never as success. On
//!   an insensitive volume a source whose new spelling is already the one
//!   listed is "already renamed".

use std::path::{Path, PathBuf};

use musaic_core::album_id;
use musaic_core::pycompat::strip;
use musaic_db::albums::upsert_album;
use musaic_db::renames::{
    DirMigration, album_location, rename_album_directory, rename_track_files,
};
use musaic_scan::{TrackOrder, py_resolve, scan_album};
use rusqlite::Connection;

use crate::fsutil::{fold, listed, rename_dir, rename_noclobber, temp_name};
use crate::plan::TARGET;
use crate::pyrepr::py_repr;

/// One `{"old": filename, "new": filename}` plan entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanEntry {
    pub old: String,
    pub new: String,
}

/// `AlbumRenamed(old_album_id, new_album_id)`; equal ids mean "files renamed,
/// identity unchanged".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumRenamed {
    pub old_album_id: String,
    pub new_album_id: String,
}

/// The keys the Python app returns only on its "something happened" branches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameDetail {
    pub errors: Vec<String>,
    pub dir_renamed: bool,
    pub new_album_id: String,
    /// `None` renders as `{}` (no directory rename).
    pub dir_migration: Option<DirMigration>,
}

/// Why a plan entry was not attempted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// Neither the old nor the new name is a file in the album directory.
    SourceMissing,
    /// The old name is gone and the new one exists: an earlier call did it.
    AlreadyRenamed,
}

impl SkipReason {
    pub fn as_str(self) -> &'static str {
        match self {
            SkipReason::SourceMissing => "source_missing",
            SkipReason::AlreadyRenamed => "already_renamed",
        }
    }
}

/// A plan entry left alone, with the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedEntry {
    pub old: String,
    pub new: String,
    pub reason: SkipReason,
}

/// `execute_renames`' return dict, plus the event to publish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameOutcome {
    pub ok: bool,
    pub message: String,
    pub renamed: usize,
    /// Present (`already_renamed: true`) when every real plan entry had
    /// already been applied by an earlier call.
    pub already_renamed: bool,
    /// Plan entries not attempted (not in the Python app's dict).
    pub skipped: Vec<SkippedEntry>,
    pub detail: Option<RenameDetail>,
    pub album_renamed: Option<AlbumRenamed>,
}

impl RenameOutcome {
    fn fail(message: impl Into<String>) -> Self {
        RenameOutcome {
            ok: false,
            message: message.into(),
            renamed: 0,
            already_renamed: false,
            skipped: Vec::new(),
            detail: None,
            album_renamed: None,
        }
    }
}

/// How many names a message lists before it says "and N more".
const NAMES_IN_MESSAGE: usize = 3;

fn name_list(names: &[&str]) -> String {
    let mut s = names
        .iter()
        .take(NAMES_IN_MESSAGE)
        .map(|n| py_repr(n))
        .collect::<Vec<_>>()
        .join(", ");
    if names.len() > NAMES_IN_MESSAGE {
        s.push_str(&format!(" and {} more", names.len() - NAMES_IN_MESSAGE));
    }
    s
}

/// The message suffix for skipped entries ("" when there are none), so the
/// one line the dialog shows says what was left alone.
fn skipped_suffix(skipped: &[SkippedEntry]) -> String {
    let of = |r: SkipReason| -> Vec<&str> {
        skipped
            .iter()
            .filter(|e| e.reason == r)
            .map(|e| e.old.as_str())
            .collect()
    };
    let mut out = String::new();
    let missing = of(SkipReason::SourceMissing);
    if !missing.is_empty() {
        out.push_str(&format!(
            "; {} not found, skipped: {}",
            missing.len(),
            name_list(&missing)
        ));
    }
    let done = of(SkipReason::AlreadyRenamed);
    if !done.is_empty() {
        out.push_str(&format!("; {} already renamed", done.len()));
    }
    out
}

/// What `execute_renames` needs from outside the call.
#[derive(Debug, Clone)]
pub struct RenameEnv<'a> {
    /// `load_config().library_dir` (as stored; resolved before the upsert).
    pub library_dir: Option<&'a str>,
    /// Timestamp for a `libraries` row the post-rename upsert may create.
    pub now: &'a str,
    /// Track order for the post-rename rescan (the app uses
    /// [`TrackOrder::DiscTrack`]; the Python app's is [`TrackOrder::Filename`]).
    pub track_order: TrackOrder,
}

fn has_sep(s: &str) -> bool {
    s.contains('/') || s.contains('\\')
}

fn name_of(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `os.path.samefile`, False when either side cannot be stat'ed
/// (used exactly where the Python app uses it, the directory rename).
fn samefile(a: &Path, b: &Path) -> bool {
    same_file::is_same_file(a, b).unwrap_or(false)
}

/// Re-scan the album and upsert its row under `library` (the row's own
/// library); a failed upsert is logged with `what` (the Python app's message
/// for that call site) and otherwise ignored, as the Python app does
/// (filename_mismatch.py:649, 731).
fn rescan(conn: &mut Connection, env: &RenameEnv, library: &str, album_dir: &Path, what: &str) {
    if let Some(fresh) = scan_album(album_dir, env.track_order)
        && let Err(e) = upsert_album(conn, library, env.now, &fresh)
    {
        // `log.exception`: ERROR; the error text stands in for the traceback.
        musaic_log::error(TARGET, &format!("{what}: {e}"));
    }
}

const RESCAN_AFTER_DIR: &str = "execute_renames: post-dir-rename rescan failed (continuing)";
const RESCAN_AFTER_FILES: &str = "execute_renames: post-rename rescan failed (continuing)";

/// `dir/name` as the directory spells it: on a case- or
/// normalisation-insensitive volume `name` may open an entry listed under
/// another spelling (same-file and equal when folded); that entry's path.
/// `dir/name` itself when it is listed, nothing matches, or the folder
/// cannot be read.
fn spelled_on_disk(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    if !matches!(listed(dir, name), Ok(false)) {
        return path;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return path;
    };
    rd.flatten()
        .map(|e| e.path())
        .find(|p| fold(&name_of(p)) == fold(name) && samefile(p, &path))
        .unwrap_or(path)
}

/// A free temporary name next to `old`: `.<name>.renaming.tmp`, then
/// `.<name>.renaming.<n>.tmp`; moved there without replacing anything.
fn stage(old: &Path) -> std::io::Result<PathBuf> {
    let name = name_of(old);
    for n in 1.. {
        let tmp = old.with_file_name(temp_name(&name, n));
        match rename_noclobber(old, &tmp) {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && n < 1000 => continue,
            r => return r.map(|()| tmp),
        }
    }
    unreachable!()
}

/// Move every staged file back to its own name. Returns a line for each
/// file that could not be moved back (it keeps its temporary name).
fn roll_back(staged: &[(PathBuf, PathBuf, PathBuf)]) -> Vec<String> {
    let mut stuck = Vec::new();
    for (o, t, _) in staged {
        if let Err(e) = rename_noclobber(t, o) {
            musaic_log::error(
                TARGET,
                &format!("execute_renames: rollback failed for {}: {e}", o.display()),
            );
            stuck.push(format!("{} is left as {}: {e}", name_of(o), name_of(t)));
        }
    }
    stuck
}

/// Apply a rename plan (and optionally a directory rename) to an album.
///
/// Preflight (nothing changes on refusal): directory target validity and
/// existence (samefile = same dir, allowed), file targets without
/// separators, no two targets folding to the same name, and no target that
/// exists unless it is the same file as one of the plan's sources. Then the
/// directory is renamed (its rows move to the new id in one transaction,
/// renamed back if that fails), then files move in two passes through
/// temporary names, each followed by its track_tags / track_fingerprints
/// rows, then the sidecar is rewritten and the album rescanned.
pub fn execute_renames(
    conn: &mut Connection,
    env: &RenameEnv,
    album_id_in: &str,
    plan: &[PlanEntry],
    dir_rename: Option<&PlanEntry>,
) -> RenameOutcome {
    execute_renames_with(conn, env, album_id_in, plan, dir_rename, &listed)
}

/// "Did the volume keep this spelling?": `(parent dir, new name)`, asked
/// after each rename. The real one reads the directory ([`listed`]); tests
/// stand in a volume that does not preserve case, or a folder that cannot
/// be read back. `Err`: the folder could not be read, so it is unknown.
type KeptSpelling<'a> = &'a dyn Fn(&Path, &str) -> std::io::Result<bool>;

const NOT_KEPT: &str = "the volume did not keep the new spelling of the name (it does not \
                        preserve case); not renamed";

/// Put a file whose new spelling the volume did not keep back under its
/// old name (through a temporary name, as it got there). `Err` says where
/// it is left.
fn put_back(at: &Path, old: &Path) -> Result<(), String> {
    let tmp = stage(at).map_err(|e| format!("{} could not be put back: {e}", name_of(old)))?;
    rename_noclobber(&tmp, old)
        .map_err(|e| format!("{} is left as {}: {e}", name_of(old), name_of(&tmp)))
}

/// [`execute_renames`] with the read-back given. A rename whose result
/// could not be read back is kept (the OS reported it done; undoing it
/// would be a second rename that could not be verified either) and is
/// named at the end of the message, so it is never silently reported as
/// verified.
fn execute_renames_with(
    conn: &mut Connection,
    env: &RenameEnv,
    album_id_in: &str,
    plan: &[PlanEntry],
    dir_rename: Option<&PlanEntry>,
    kept: KeptSpelling,
) -> RenameOutcome {
    let mut unverified: Vec<String> = Vec::new();
    let mut out = run_renames(
        conn,
        env,
        album_id_in,
        plan,
        dir_rename,
        kept,
        &mut unverified,
    );
    if !unverified.is_empty() {
        out.message.push_str(&format!(
            "; could not read the folder back to check the new spelling of {}",
            name_list(&unverified.iter().map(String::as_str).collect::<Vec<_>>())
        ));
    }
    out
}

/// Whether the new spelling is on disk: `true` also when the folder
/// could not be read back (recorded in `unverified` and logged).
fn check_kept(kept: KeptSpelling, dir: &Path, name: &str, unverified: &mut Vec<String>) -> bool {
    match kept(dir, name) {
        Ok(k) => k,
        Err(e) => {
            musaic_log::warning(
                TARGET,
                &format!(
                    "execute_renames: could not read {} back to check {}: {e}",
                    dir.display(),
                    py_repr(name)
                ),
            );
            unverified.push(name.to_string());
            true
        }
    }
}

fn run_renames(
    conn: &mut Connection,
    env: &RenameEnv,
    album_id_in: &str,
    plan: &[PlanEntry],
    dir_rename: Option<&PlanEntry>,
    kept: KeptSpelling,
    unverified: &mut Vec<String>,
) -> RenameOutcome {
    if env.library_dir.is_none_or(str::is_empty) {
        return RenameOutcome::fail("No library configured");
    }
    let row = match album_location(conn, album_id_in) {
        Ok(Some(r)) => r,
        _ => return RenameOutcome::fail("Album not found"),
    };
    if let Some(since) = row.missing_since.as_deref().filter(|s| !s.is_empty()) {
        return RenameOutcome::fail(format!(
            "Album is missing from disk (missing since {since}); nothing was renamed"
        ));
    }
    // The row's own library; the configured one only for a row without it.
    let library = if row.library_path.is_empty() {
        let lib = env.library_dir.unwrap_or_default();
        py_resolve(Path::new(lib)).to_string_lossy().into_owned()
    } else {
        row.library_path
    };
    let mut album_dir = PathBuf::from(&row.path);
    if !album_dir.is_dir() {
        return RenameOutcome::fail(format!("Album directory missing: {}", album_dir.display()));
    }

    // ---- preflight: directory ----
    let mut new_dir = None;
    if let Some(dr) = dir_rename {
        let new_base = strip(&dr.new);
        if new_base.is_empty() {
            return RenameOutcome::fail("dir_rename.new is empty");
        }
        if has_sep(new_base) {
            return RenameOutcome::fail(format!(
                "Refusing dir rename target with path separator: {}",
                py_repr(new_base)
            ));
        }
        let parent = album_dir
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let target = parent.join(new_base);
        if target.exists() && !samefile(&target, &album_dir) {
            return RenameOutcome::fail(format!(
                "Refusing to overwrite existing directory: {}",
                name_of(&target)
            ));
        }
        new_dir = Some(target);
    }

    // ---- preflight: files (checked in the current dir; a dir rename does
    // not change its contents) ----
    let mut cleaned: Vec<(String, String)> = Vec::new();
    let mut skipped: Vec<SkippedEntry> = Vec::new();
    for entry in plan {
        let (old, new) = (strip(&entry.old), strip(&entry.new));
        if old.is_empty() || new.is_empty() || old == new {
            continue;
        }
        if has_sep(new) {
            return RenameOutcome::fail(format!(
                "Refusing rename target with path separator: {}",
                py_repr(new)
            ));
        }
        let (old_p, new_p) = (album_dir.join(old), album_dir.join(new));
        if !old_p.is_file() {
            let skip = |reason| SkippedEntry {
                old: old.to_string(),
                new: new.to_string(),
                reason,
            };
            if new_p.is_file() {
                skipped.push(skip(SkipReason::AlreadyRenamed));
            } else {
                skipped.push(skip(SkipReason::SourceMissing));
                // filename_mismatch.py:602. The Python app checks after the
                // directory rename, so its path names the new directory;
                // this preflight runs before it and names the
                // current one.
                musaic_log::warning(
                    TARGET,
                    &format!(
                        "execute_renames: source missing, skipping: {}",
                        old_p.display()
                    ),
                );
            }
            continue;
        }
        // On an insensitive volume the old spelling still "is a file" after
        // a case-only rename; the listing says which spelling is there.
        // Only for a case- or normalisation-only entry: two hard links
        // (`X.flac`, `one.flac`) are also the same file, and `x.flac` ->
        // `one.flac` is not done while `X.flac` is there. A listing that
        // cannot be read is not evidence: the entry is attempted.
        if fold(old) == fold(new)
            && samefile(&old_p, &new_p)
            && matches!(listed(&album_dir, old), Ok(false))
            && matches!(listed(&album_dir, new), Ok(true))
        {
            skipped.push(SkippedEntry {
                old: old.to_string(),
                new: new.to_string(),
                reason: SkipReason::AlreadyRenamed,
            });
            continue;
        }
        cleaned.push((old.to_string(), new.to_string()));
    }
    // Two entries onto one name.
    let mut targets = std::collections::HashSet::new();
    for (_, new) in &cleaned {
        if !targets.insert(fold(new)) {
            return RenameOutcome::fail(format!(
                "Refusing rename: more than one file would be renamed to {}",
                py_repr(new)
            ));
        }
    }
    // Collisions with existing files: the same file as a source
    // (that source moves away first) is not a collision.
    for (_, new) in &cleaned {
        let new_p = album_dir.join(new);
        if new_p.exists()
            && !cleaned
                .iter()
                .any(|(old, _)| samefile(&new_p, &album_dir.join(old)))
        {
            return RenameOutcome::fail(format!(
                "Refusing to overwrite an existing unrelated file: {}",
                name_of(&new_p)
            ));
        }
    }

    // ---- directory ----
    let mut new_album_id = album_id_in.to_string();
    let mut dir_migration = None;
    let mut album_renamed = None;
    if let Some(target) = new_dir {
        // Issued even onto the same physical dir: on a case- or
        // normalisation-insensitive volume it changes the on-disk spelling
        // (through a temporary name, see `rename_dir`).
        if let Err(e) = rename_dir(&album_dir, &target) {
            return RenameOutcome::fail(format!("Couldn't rename directory: {e}"));
        }
        let parent = target.parent().map(Path::to_path_buf).unwrap_or_default();
        if !check_kept(kept, &parent, &name_of(&target), unverified) {
            // Nothing moved in the database yet: put the folder back and
            // keep the album's id.
            let mut message = format!(
                "Couldn't rename directory to {}: {NOT_KEPT}",
                py_repr(&name_of(&target))
            );
            if let Err(e) = rename_dir(&target, &album_dir) {
                musaic_log::error(
                    TARGET,
                    &format!("execute_renames: putting the dir back failed: {e}"),
                );
                message.push_str(&format!("; the folder could not be put back: {e}"));
            }
            return RenameOutcome::fail(message);
        }
        let id = album_id(&py_resolve(&target).to_string_lossy());
        match rename_album_directory(
            conn,
            album_id_in,
            &id,
            &target.to_string_lossy(),
            &name_of(&target),
        ) {
            Ok(m) => dir_migration = Some(m),
            Err(e) => {
                // filename_mismatch.py:540 / 544 (`log.exception`: ERROR;
                // the error text stands in for the traceback).
                musaic_log::error(
                    TARGET,
                    &format!(
                        "execute_renames: cache migration failed; rolling back dir rename: {e}"
                    ),
                );
                if let Err(e) = rename_dir(&target, &album_dir) {
                    musaic_log::error(
                        TARGET,
                        &format!("execute_renames: rollback of dir rename also failed: {e}"),
                    );
                }
                return RenameOutcome::fail(
                    "Cache migration failed during dir rename; rolled back",
                );
            }
        }
        new_album_id = id;
        album_dir = target;
        album_renamed = Some(AlbumRenamed {
            old_album_id: album_id_in.to_string(),
            new_album_id: new_album_id.clone(),
        });
    }
    let dir_renamed = dir_migration.is_some();

    if cleaned.is_empty() && !dir_renamed {
        let event = Some(AlbumRenamed {
            old_album_id: new_album_id.clone(),
            new_album_id: new_album_id.clone(),
        });
        let real = plan
            .iter()
            .filter(|e| {
                let (o, n) = (strip(&e.old), strip(&e.new));
                !o.is_empty() && !n.is_empty() && o != n
            })
            .count();
        let already_done = skipped
            .iter()
            .filter(|e| e.reason == SkipReason::AlreadyRenamed)
            .count();
        if already_done > 0 && already_done == real {
            return RenameOutcome {
                ok: true,
                renamed: 0,
                already_renamed: true,
                // `already_renamed` already says it, for every entry.
                skipped: Vec::new(),
                message: format!(
                    "All {already_done} file(s) were already renamed by an earlier click; nothing to do. \
                     Re-running the check to clear the stale finding."
                ),
                detail: None,
                album_renamed: event,
            };
        }
        return RenameOutcome {
            ok: true,
            message: format!("Nothing to rename{}", skipped_suffix(&skipped)),
            album_renamed: event,
            skipped,
            ..RenameOutcome::fail("")
        };
    }
    if cleaned.is_empty() {
        rescan(conn, env, &library, &album_dir, RESCAN_AFTER_DIR);
        return RenameOutcome {
            ok: true,
            message: format!("Renamed album directory{}", skipped_suffix(&skipped)),
            renamed: 0,
            already_renamed: false,
            skipped,
            detail: Some(RenameDetail {
                errors: Vec::new(),
                dir_renamed: true,
                new_album_id,
                dir_migration,
            }),
            album_renamed,
        };
    }

    // ---- files: stage every source to a temporary name, then finalise ----
    let mut staged: Vec<(PathBuf, PathBuf, PathBuf)> = Vec::new();
    for (old, new) in &cleaned {
        // The name as the folder spells it, so a file that has to go back
        // (a failed second step) gets its own spelling back, not the plan's.
        let old_p = spelled_on_disk(&album_dir, old);
        match stage(&old_p) {
            Ok(tmp) => staged.push((old_p, tmp, album_dir.join(new))),
            Err(e) => {
                // filename_mismatch.py:710 (`log.exception`: ERROR).
                musaic_log::error(
                    TARGET,
                    &format!("execute_renames: stage phase failed; rolling back: {e}"),
                );
                // The Python app's OSError text names the file; io::Error's
                // does not, so the name is added here.
                let mut message = format!("Rename staging failed for {}: {e}", py_repr(old));
                let stuck = roll_back(&staged);
                if stuck.is_empty() {
                    message.push_str("; no file was renamed");
                } else {
                    message.push_str(&format!("; {}", stuck.join("; ")));
                }
                // After a dir rename this still reports it.
                return match dir_migration {
                    Some(_) => {
                        rescan(conn, env, &library, &album_dir, RESCAN_AFTER_DIR);
                        RenameOutcome {
                            ok: false,
                            message: message.clone(),
                            renamed: 0,
                            already_renamed: false,
                            skipped,
                            detail: Some(RenameDetail {
                                errors: vec![message],
                                dir_renamed: true,
                                new_album_id,
                                dir_migration,
                            }),
                            album_renamed,
                        }
                    }
                    None => RenameOutcome {
                        skipped,
                        ..RenameOutcome::fail(message)
                    },
                };
            }
        }
    }
    let mut renamed = 0;
    let mut errors = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    let mut stuck: Vec<String> = Vec::new();
    let mut not_kept = 0;
    for (old_p, tmp, new_p) in &staged {
        match rename_noclobber(tmp, new_p) {
            Ok(()) if !check_kept(kept, &album_dir, &name_of(new_p), unverified) => {
                not_kept += 1;
                errors.push(format!(
                    "{} → {}: {NOT_KEPT}",
                    name_of(old_p),
                    name_of(new_p)
                ));
                failed.push(name_of(old_p));
                if let Err(line) = put_back(new_p, old_p) {
                    musaic_log::error(TARGET, &format!("execute_renames: {line}"));
                    errors.push(line.clone());
                    stuck.push(line);
                }
            }
            Ok(()) => {
                renamed += 1;
                if let Err(e) =
                    rename_track_files(conn, &new_album_id, &name_of(old_p), &name_of(new_p))
                {
                    // filename_mismatch.py:618 (`log.exception`: ERROR).
                    musaic_log::error(
                        TARGET,
                        &format!(
                            "execute_renames: track-tag migration failed for {} \u{2192} {} (continuing): {e}",
                            name_of(old_p),
                            name_of(new_p)
                        ),
                    );
                }
            }
            Err(e) => {
                errors.push(format!("{} → {}: {e}", name_of(old_p), name_of(new_p)));
                failed.push(name_of(old_p));
                if let Err(e) = rename_noclobber(tmp, old_p) {
                    // filename_mismatch.py:706 (`log.exception`: ERROR).
                    musaic_log::error(
                        TARGET,
                        &format!(
                            "execute_renames: rollback failed for {}: {e}",
                            old_p.display()
                        ),
                    );
                    // The file is only under its temporary name now: say so.
                    let line = format!("{} is left as {}: {e}", name_of(old_p), name_of(tmp));
                    errors.push(line.clone());
                    stuck.push(line);
                }
            }
        }
    }

    if renamed > 0
        && let Err(e) = crate::sidecar::resync_after_renames(conn, &new_album_id, &album_dir)
    {
        // The sidecar rewrite borrows apply.py's step 5; its failure log is
        // services/apply.py:169 (`_log.exception`: ERROR, on
        // `musaic.services.apply`). An I/O failure of the write itself is
        // logged by `write_sidecar`.
        musaic_log::error(
            "musaic.services.apply",
            &format!(
                "Failed to resync track-tag sidecar after rename for album {new_album_id}: {e}"
            ),
        );
    }
    rescan(conn, env, &library, &album_dir, RESCAN_AFTER_FILES);

    if renamed > 0 && !dir_renamed {
        album_renamed = Some(AlbumRenamed {
            old_album_id: new_album_id.clone(),
            new_album_id: new_album_id.clone(),
        });
    }
    let files = |n: usize| format!("{n} file{}", if n != 1 { "s" } else { "" });
    let mut message = if renamed > 0 {
        format!(
            "{}{}",
            if dir_renamed {
                "Renamed album directory + "
            } else {
                ""
            },
            files(renamed)
        )
    } else if dir_renamed {
        "Renamed album directory".to_string()
    } else {
        // Every file failed (for instance the volume kept none of the new
        // spellings): say so instead of naming a directory rename.
        "No file was renamed".to_string()
    };
    if !failed.is_empty() {
        // One per failed file, as the Python app counted its errors.
        message.push_str(&format!("; {} error(s)", failed.len()));
        // Deliberate addition: which files kept their old names, and any
        // left under a temporary name.
        let names: Vec<&str> = failed.iter().map(String::as_str).collect();
        message.push_str(&format!(": not renamed {}", name_list(&names)));
        if not_kept > 0 {
            message.push_str(&format!(
                "; the volume did not keep the new spelling of {} (it does not preserve case)",
                files(not_kept)
            ));
        }
        for line in &stuck {
            message.push_str(&format!("; {line}"));
        }
    }
    message.push_str(&skipped_suffix(&skipped));
    RenameOutcome {
        ok: (renamed > 0 || dir_renamed) && errors.is_empty(),
        message: strip(&message).to_string(),
        renamed,
        already_renamed: false,
        skipped,
        detail: Some(RenameDetail {
            errors,
            dir_renamed,
            new_album_id,
            dir_migration,
        }),
        album_renamed,
    }
}

#[cfg(test)]
mod tests {
    //! A volume that takes a rename but does not keep the new spelling
    //! cannot be made in a temp dir, so the read-back is replaced by one
    //! that always answers "not kept"; the renames themselves are real.

    use super::*;
    use musaic_db::{Db, albums};
    use musaic_scan::scan_library;

    const NOW: &str = "2026-01-01T00:00:00Z";

    fn flac_bytes(title: &str) -> Vec<u8> {
        let mut si = Vec::new();
        si.extend_from_slice(&[0x10, 0, 0x10, 0, 0, 0, 0, 0, 0, 0]);
        let packed: u64 = (44100u64 << 44) | (1 << 41) | (15 << 36) | 44100;
        si.extend_from_slice(&packed.to_be_bytes());
        si.extend_from_slice(&[0; 16]);
        let mut vc = Vec::new();
        vc.extend_from_slice(&4u32.to_le_bytes());
        vc.extend_from_slice(b"test");
        let tags = [("TRACKNUMBER", "1"), ("TITLE", title), ("ALBUM", "X")];
        vc.extend_from_slice(&(tags.len() as u32).to_le_bytes());
        for (k, v) in tags {
            let e = format!("{k}={v}");
            vc.extend_from_slice(&(e.len() as u32).to_le_bytes());
            vc.extend_from_slice(e.as_bytes());
        }
        let mut out = b"fLaC".to_vec();
        out.push(0);
        out.extend_from_slice(&(si.len() as u32).to_be_bytes()[1..]);
        out.extend_from_slice(&si);
        out.push(0x84);
        out.extend_from_slice(&(vc.len() as u32).to_be_bytes()[1..]);
        out.extend_from_slice(&vc);
        out
    }

    fn listing(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    fn library(base: &Path) -> (Db, String, String) {
        let album = base.join("lib/abba - x");
        std::fs::create_dir_all(&album).unwrap();
        std::fs::write(album.join("01 - one.flac"), flac_bytes("One")).unwrap();
        let db_path = base.join("library.db");
        std::fs::copy(musaic_testdata::public("db/v16-empty.db"), &db_path).unwrap();
        let (mut db, _) = Db::open(&db_path).unwrap();
        let lib = py_resolve(&base.join("lib")).to_string_lossy().into_owned();
        let scanned = scan_library(&base.join("lib"), TrackOrder::Filename);
        albums::save_snapshot(db.connection_mut(), &lib, NOW, NOW, &scanned).unwrap();
        let id = scanned[0].id.clone();
        (db, lib, id)
    }

    fn env(lib: &str) -> RenameEnv<'_> {
        RenameEnv {
            library_dir: Some(lib),
            now: NOW,
            track_order: TrackOrder::Filename,
        }
    }

    fn entry(old: &str, new: &str) -> PlanEntry {
        PlanEntry {
            old: old.into(),
            new: new.into(),
        }
    }

    const NEVER_KEPT: KeptSpelling = &|_, _| Ok(false);
    const UNREADABLE: KeptSpelling = &|_, _| {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "denied",
        ))
    };

    /// The file is put back under its old name and reported as not
    /// renamed: no success, no AlbumRenamed, its rows untouched.
    #[test]
    fn a_file_spelling_the_volume_did_not_keep_is_reported_and_put_back() {
        let tmp = tempfile::tempdir().unwrap();
        let base = musaic_testdata::canonical(tmp.path());
        let (mut db, lib, id) = library(&base);
        let album = base.join("lib/abba - x");
        let before = std::fs::read(album.join("01 - one.flac")).unwrap();
        let out = execute_renames_with(
            db.connection_mut(),
            &env(&lib),
            &id,
            &[entry("01 - one.flac", "01 - One.flac")],
            None,
            NEVER_KEPT,
        );
        assert!(!out.ok);
        assert_eq!(out.renamed, 0);
        assert!(out.album_renamed.is_none());
        assert!(
            out.message
                .starts_with("No file was renamed; 1 error(s): not renamed '01 - one.flac'"),
            "{}",
            out.message
        );
        assert!(
            out.message.contains("did not keep the new spelling"),
            "{}",
            out.message
        );
        let d = out.detail.unwrap();
        assert!(d.errors[0].contains(NOT_KEPT), "{:?}", d.errors);
        assert_eq!(listing(&album), ["01 - one.flac"]);
        assert_eq!(std::fs::read(album.join("01 - one.flac")).unwrap(), before);
    }

    /// The folder is put back and keeps its id; nothing in the database
    /// moved.
    #[test]
    fn a_folder_spelling_the_volume_did_not_keep_is_reported_and_put_back() {
        let tmp = tempfile::tempdir().unwrap();
        let base = musaic_testdata::canonical(tmp.path());
        let (mut db, lib, id) = library(&base);
        let out = execute_renames_with(
            db.connection_mut(),
            &env(&lib),
            &id,
            &[],
            Some(&entry("abba - x", "ABBA - x")),
            NEVER_KEPT,
        );
        assert!(!out.ok);
        assert!(out.album_renamed.is_none());
        assert_eq!(
            out.message,
            format!("Couldn't rename directory to 'ABBA - x': {NOT_KEPT}")
        );
        assert_eq!(listing(&base.join("lib")), ["abba - x"]);
        let loc = album_location(db.connection(), &id).unwrap().unwrap();
        assert_eq!(
            loc.path,
            base.join("lib").join("abba - x").to_string_lossy()
        );
    }

    /// A folder that cannot be read back: the renames the OS reported done
    /// are kept (file and folder, new id), and the message says they could
    /// not be checked, instead of calling them "not kept".
    #[test]
    fn a_rename_that_cannot_be_read_back_is_kept_and_reported_unverified() {
        let tmp = tempfile::tempdir().unwrap();
        let base = musaic_testdata::canonical(tmp.path());
        let (mut db, lib, id) = library(&base);
        let out = execute_renames_with(
            db.connection_mut(),
            &env(&lib),
            &id,
            &[entry("01 - one.flac", "01 - One.flac")],
            Some(&entry("abba - x", "ABBA - x")),
            UNREADABLE,
        );
        assert!(out.ok, "{}", out.message);
        assert_eq!(out.renamed, 1);
        assert_eq!(
            out.message,
            "Renamed album directory + 1 file; could not read the folder back to check \
             the new spelling of 'ABBA - x', '01 - One.flac'"
        );
        assert!(!out.message.contains(NOT_KEPT));
        let ev = out.album_renamed.expect("AlbumRenamed");
        assert_ne!(ev.new_album_id, id);
        assert_eq!(listing(&base.join("lib")), ["ABBA - x"]);
        assert_eq!(listing(&base.join("lib/ABBA - x")), ["01 - One.flac"]);
    }
}
