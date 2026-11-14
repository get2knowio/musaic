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

use std::path::{Path, PathBuf};

use musaic_core::album_id;
use musaic_core::pycompat::strip;
use musaic_db::albums::upsert_album;
use musaic_db::renames::{DirMigration, album_path, rename_album_directory, rename_track_files};
use musaic_scan::{TrackOrder, py_resolve, scan_album};
use rusqlite::Connection;

use crate::fsutil::rename_noclobber;
use crate::plan::TARGET;
use crate::pyrepr::py_repr;
use unicode_normalization::UnicodeNormalization;

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

/// `execute_renames`' return dict, plus the event to publish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameOutcome {
    pub ok: bool,
    pub message: String,
    pub renamed: usize,
    /// Present (`already_renamed: true`) when every real plan entry had
    /// already been applied by an earlier call.
    pub already_renamed: bool,
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
            detail: None,
            album_renamed: None,
        }
    }
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

/// Re-scan the album and upsert its row; a failed upsert is logged with
/// `what` (the Python app's message for that call site) and otherwise ignored,
/// as the Python app does (filename_mismatch.py:649, 731).
fn rescan(conn: &mut Connection, env: &RenameEnv, album_dir: &Path, what: &str) {
    let Some(lib) = env.library_dir else { return };
    if let Some(fresh) = scan_album(album_dir, env.track_order) {
        let lib = py_resolve(Path::new(lib)).to_string_lossy().into_owned();
        if let Err(e) = upsert_album(conn, &lib, env.now, &fresh) {
            // `log.exception`: ERROR; the error text stands in for the traceback.
            musaic_log::error(TARGET, &format!("{what}: {e}"));
        }
    }
}

const RESCAN_AFTER_DIR: &str = "execute_renames: post-dir-rename rescan failed (continuing)";
const RESCAN_AFTER_FILES: &str = "execute_renames: post-rename rescan failed (continuing)";

/// How a name compares on a case- and normalisation-insensitive volume.
fn fold(name: &str) -> String {
    name.nfc().collect::<String>().to_lowercase()
}

/// A free temporary name next to `old`: `.<name>.renaming.tmp`, then
/// `.<name>.renaming.<n>.tmp`; moved there without replacing anything.
fn stage(old: &Path) -> std::io::Result<PathBuf> {
    let name = name_of(old);
    for n in 1.. {
        let tmp = if n == 1 {
            old.with_file_name(format!(".{name}.renaming.tmp"))
        } else {
            old.with_file_name(format!(".{name}.renaming.{n}.tmp"))
        };
        match rename_noclobber(old, &tmp) {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && n < 1000 => continue,
            r => return r.map(|()| tmp),
        }
    }
    unreachable!()
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
    if env.library_dir.is_none_or(str::is_empty) {
        return RenameOutcome::fail("No library configured");
    }
    let row_path = match album_path(conn, album_id_in) {
        Ok(Some(p)) => p,
        _ => return RenameOutcome::fail("Album not found"),
    };
    let mut album_dir = PathBuf::from(&row_path);
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
    let mut already_done = 0;
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
            if new_p.is_file() {
                already_done += 1;
            } else {
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
        // normalisation-insensitive volume it changes the on-disk spelling.
        if let Err(e) = std::fs::rename(&album_dir, &target) {
            return RenameOutcome::fail(format!("Couldn't rename directory: {e}"));
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
                if let Err(e) = std::fs::rename(&target, &album_dir) {
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
        if already_done > 0 && already_done == real {
            return RenameOutcome {
                ok: true,
                renamed: 0,
                already_renamed: true,
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
            message: "Nothing to rename".into(),
            album_renamed: event,
            ..RenameOutcome::fail("")
        };
    }
    if cleaned.is_empty() {
        rescan(conn, env, &album_dir, RESCAN_AFTER_DIR);
        return RenameOutcome {
            ok: true,
            message: "Renamed album directory".into(),
            renamed: 0,
            already_renamed: false,
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
        let old_p = album_dir.join(old);
        match stage(&old_p) {
            Ok(tmp) => staged.push((old_p, tmp, album_dir.join(new))),
            Err(e) => {
                // filename_mismatch.py:710 (`log.exception`: ERROR).
                musaic_log::error(
                    TARGET,
                    &format!("execute_renames: stage phase failed; rolling back: {e}"),
                );
                for (o, t, _) in &staged {
                    let _ = rename_noclobber(t, o);
                }
                // After a dir rename this still reports it.
                let message = format!("Rename staging failed: {e}");
                return match dir_migration {
                    Some(_) => {
                        rescan(conn, env, &album_dir, RESCAN_AFTER_DIR);
                        RenameOutcome {
                            ok: false,
                            message: message.clone(),
                            renamed: 0,
                            already_renamed: false,
                            detail: Some(RenameDetail {
                                errors: vec![message],
                                dir_renamed: true,
                                new_album_id,
                                dir_migration,
                            }),
                            album_renamed,
                        }
                    }
                    None => RenameOutcome::fail(message),
                };
            }
        }
    }
    let mut renamed = 0;
    let mut errors = Vec::new();
    for (old_p, tmp, new_p) in &staged {
        match rename_noclobber(tmp, new_p) {
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
                if let Err(e) = rename_noclobber(tmp, old_p) {
                    // filename_mismatch.py:706 (`log.exception`: ERROR).
                    musaic_log::error(
                        TARGET,
                        &format!(
                            "execute_renames: rollback failed for {}: {e}",
                            old_p.display()
                        ),
                    );
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
    rescan(conn, env, &album_dir, RESCAN_AFTER_FILES);

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
    } else {
        "Renamed album directory".to_string()
    };
    if !errors.is_empty() {
        message.push_str(&format!("; {} error(s)", errors.len()));
    }
    RenameOutcome {
        ok: (renamed > 0 || dir_renamed) && errors.is_empty(),
        message: strip(&message).to_string(),
        renamed,
        already_renamed: false,
        detail: Some(RenameDetail {
            errors,
            dir_renamed,
            new_album_id,
            dir_migration,
        }),
        album_renamed,
    }
}
