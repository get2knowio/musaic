//! Soft delete into the app trash (port of the Python app's `trash.py`).
//!
//! Layout: `<trash root>/<album dir name, '/' and '\' replaced by '_'>/<file
//! name>`; on a name collision `<stem>__<YYYY-MM-DDTHH-MM-SS><suffix>` with
//! Python's stem/suffix split. Nothing is unlinked: a failed move is an error.
//!
//! Deliberate change: the Python app reused the
//! stamped name on a further collision within the same second and the move
//! replaced the earlier trashed file. Here further collisions get a counter
//! after the stamp (`<stem>__<stamp>-2<suffix>`, `-3`, ...), and every
//! candidate name is claimed with no-clobber semantics (hard link, or a copy
//! into a `create_new` file across volumes), so nothing already in the trash
//! is ever replaced, even by a concurrent trash.

use std::path::{Path, PathBuf};

use crate::fsutil::move_noclobber;
use crate::pyrepr::py_repr;

/// The trash root under a config dir (`<config>/trash`).
pub fn trash_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("trash")
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct TrashError(pub String);

/// `datetime.strftime("%Y-%m-%dT%H-%M-%S")` from civil (local) time parts.
pub fn collision_stamp(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> String {
    format!("{year:04}-{month:02}-{day:02}T{hour:02}-{minute:02}-{second:02}")
}

/// Python `PurePath.suffix` / `.stem` for a final component: the suffix starts
/// at the last dot unless that dot is the first or the last character.
fn split_suffix(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 && i < name.len() - 1 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// `send_to_trash(path, album_dir_name=...)`: move `path` (a regular file,
/// symlinks followed for the check) into `trash_root`. `stamp` is called only
/// on a name collision and must return local time as [`collision_stamp`]
/// formats it. Returns the new path.
pub fn send_to_trash(
    trash_root: &Path,
    path: &Path,
    album_dir_name: Option<&str>,
    stamp: impl FnOnce() -> String,
) -> Result<PathBuf, TrashError> {
    if !path.is_file() {
        return Err(TrashError(format!(
            "File doesn't exist (or isn't a regular file): {}",
            path.display()
        )));
    }
    let parent_name = path
        .parent()
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let sub = match album_dir_name {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => parent_name,
    }
    .replace(['/', '\\'], "_");
    let sub = if sub.is_empty() {
        "untitled".to_string()
    } else {
        sub
    };
    let target_dir = trash_root.join(sub);
    std::fs::create_dir_all(&target_dir).map_err(|e| {
        TrashError(format!(
            "Couldn't create trash directory {}: {e}",
            target_dir.display()
        ))
    })?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (stem, suffix) = split_suffix(&name);
    let mut stamp = Some(stamp);
    let mut stamped = String::new();
    for n in 0usize.. {
        let target = match n {
            0 => target_dir.join(&name),
            1 => {
                stamped = (stamp.take().unwrap())();
                target_dir.join(format!("{stem}__{stamped}{suffix}"))
            }
            _ => target_dir.join(format!("{stem}__{stamped}-{n}{suffix}")),
        };
        match move_noclobber(path, &target) {
            Ok(()) => return Ok(target),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(TrashError(format!(
                    "Couldn't move {} to trash: {e}",
                    py_repr(&name)
                )));
            }
        }
    }
    unreachable!()
}
