//! `cover.jpg` sidecar.
//!
//! Port of `tags.save_cover_to_album_dir`: write `.cover.jpg.tmp`, rename it
//! over `cover.jpg` (atomic on one volume; no fsync, like the Python app), then
//! delete the competing `cover.{jpeg,png,JPG,JPEG,PNG}` siblings unless the
//! OS says they are the very file just written (`os.path.samefile`, which on
//! a case-insensitive volume is true for `cover.JPG`). `folder.*` and
//! `front.*` are never touched.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The Python app's `_COMPETING_COVER_FILES`, in its order.
pub const COMPETING_COVER_FILES: [&str; 5] = [
    "cover.jpeg",
    "cover.png",
    "cover.JPG",
    "cover.JPEG",
    "cover.PNG",
];

/// Write `jpeg_bytes` to `<album_dir>/cover.jpg`. Returns the path written,
/// or `None` when `album_dir` is not a directory or the write failed (the
/// temp file is removed in that case; both are logged on `musaic.tags` as
/// the Python app logs them). Cleanup failures are ignored.
pub fn save_cover_to_album_dir(album_dir: &Path, jpeg_bytes: &[u8]) -> Option<PathBuf> {
    if !album_dir.is_dir() {
        // tags.py:445
        musaic_log::warning(
            "musaic.tags",
            &format!(
                "save_cover_to_album_dir: not a directory: {}",
                album_dir.display()
            ),
        );
        return None;
    }
    let target = album_dir.join("cover.jpg");
    let tmp = album_dir.join(".cover.jpg.tmp");
    if let Err(e) = fs::write(&tmp, jpeg_bytes).and_then(|()| fs::rename(&tmp, &target)) {
        // tags.py:453; `e` is Rust's io::Error text, not Python's OSError repr.
        musaic_log::warning(
            "musaic.tags",
            &format!("failed to write {}: {e}", target.display()),
        );
        let _ = fs::remove_file(&tmp);
        return None;
    }
    for name in COMPETING_COVER_FILES {
        let stale = album_dir.join(name);
        // Path.is_file() follows symlinks and is False for anything missing.
        if !stale.is_file() {
            continue;
        }
        match same_file::is_same_file(&stale, &target) {
            Ok(true) => continue,
            Ok(false) => {
                let _ = fs::remove_file(&stale);
            }
            Err(_) => {} // samefile raised OSError: the Python app skips the name
        }
    }
    Some(target)
}

/// Whether `name` is a sidecar a cover apply writes or deletes
/// (`cover.jpg`, `cover.jpeg`, `cover.png`), in any case, so that nothing it
/// could replace on a case-insensitive volume is missed.
pub fn is_cover_sidecar_name(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "cover.jpg" | "cover.jpeg" | "cover.png"
    )
}

/// Put the folder's cover sidecars back to exactly `saved` (bare name, bytes
/// byte for byte): a cover sidecar the folder lists that is not among them
/// by name is deleted, then each saved one is written (through a temporary
/// name, renamed over). Returns whether anything changed on disk. Used by an
/// undo; an empty `saved` leaves the folder without any cover sidecar.
pub fn restore_cover_sidecars(album_dir: &Path, saved: &[(String, Vec<u8>)]) -> io::Result<bool> {
    let mut changed = false;
    let listed: Vec<String> = fs::read_dir(album_dir)?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| is_cover_sidecar_name(n) && album_dir.join(n).is_file())
        .collect();
    for name in &listed {
        if !saved.iter().any(|(s, _)| s == name) {
            fs::remove_file(album_dir.join(name))?;
            changed = true;
        }
    }
    for (name, bytes) in saved {
        let target = album_dir.join(name);
        if listed.contains(name) && fs::read(&target).is_ok_and(|b| b == *bytes) {
            continue;
        }
        let tmp = album_dir.join(format!(".{name}.restore.tmp"));
        if let Err(e) = fs::write(&tmp, bytes).and_then(|()| fs::rename(&tmp, &target)) {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        changed = true;
    }
    Ok(changed)
}
