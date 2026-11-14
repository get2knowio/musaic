//! On-disk cover lookup.
//!
//! Discovery itself (`scanner._find_cover_file`: the first of
//! cover/folder/front .jpg/.jpeg/.png that `is_file()`, then a
//! case-insensitive fallback over the directory listing) lives in
//! `musaic-scan` and lands in `Album::cover_file`. This module is the
//! `thumbs.py` side: turning that name into a path and a MIME type. The
//! embedded-picture fallback (largest PICTURE block) is not ported here.

use std::path::{Path, PathBuf};

/// `thumbs.get_disk_cover_path`: `album_path/cover_file` if it is a file.
pub fn disk_cover_path(album_path: &str, cover_file: Option<&str>) -> Option<PathBuf> {
    let cover_file = cover_file.filter(|c| !c.is_empty())?;
    if album_path.is_empty() {
        return None;
    }
    let p = Path::new(album_path).join(cover_file);
    p.is_file().then_some(p)
}

/// `get_source_image`'s MIME for a disk cover: PNG when the suffix is
/// `.png` in any case (Python's suffix rule: a leading-dot name has none),
/// JPEG otherwise.
pub fn disk_cover_mime(path: &Path) -> &'static str {
    if path
        .extension()
        .is_some_and(|e| e.to_string_lossy().to_lowercase() == "png")
    {
        "image/png"
    } else {
        "image/jpeg"
    }
}
