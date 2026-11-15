//! `cover.jpg` sidecar.
//!
//! Port of `tags.save_cover_to_album_dir`: write `.cover.jpg.tmp`, rename it
//! over `cover.jpg` (atomic on one volume; no fsync, like the Python app), then
//! delete the competing `cover.{jpeg,png,JPG,JPEG,PNG}` siblings unless the
//! OS says they are the very file just written (`os.path.samefile`, which on
//! a case-insensitive volume is true for `cover.JPG`). `folder.*` and
//! `front.*` are never touched.

use std::fs;
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
