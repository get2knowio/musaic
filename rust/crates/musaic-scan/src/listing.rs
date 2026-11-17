//! The one FLAC listing every layer uses.

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

/// FLAC files directly inside `album_dir`: regular files (symlinks followed)
/// with a `.flac` extension in any case, AppleDouble `._*` names skipped,
/// sorted by name in code point order, and same-inode duplicates collapsed
/// to the first occurrence (SMB shares can list one file under two names).
/// An unreadable directory yields an empty list.
pub fn list_flacs(album_dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(album_dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for name in names {
        if name.starts_with("._") || !has_flac_suffix(&name) {
            continue;
        }
        let path = album_dir.join(&name);
        let Ok(meta) = std::fs::metadata(&path) else {
            continue; // broken symlink etc.: Path.is_file() is False
        };
        if !meta.is_file() {
            continue;
        }
        if let Some(key) = inode_key(&meta)
            && !seen.insert(key)
        {
            continue;
        }
        out.push(path);
    }
    out
}

/// The names `dir` lists, in the order the listing returns them. `Err` when
/// the directory, or an entry while listing it, cannot be read.
pub fn dir_names(dir: &Path) -> std::io::Result<Vec<OsString>> {
    std::fs::read_dir(dir)?
        .map(|e| e.map(|e| e.file_name()))
        .collect()
}

/// The name under which a directory lists an entry that was just created or
/// renamed as `name`: `name` itself when `names` (the directory's listing)
/// has it, else the one listed entry that is the same file as `entry` (a
/// handle on it, taken right after the rename; same device and inode or
/// file index, never a comparison of strings). A volume that stores names
/// in another form (HFS+ keeps them decomposed, NFD) lists the entry under
/// that spelling, and the scanner derives album ids and file rows from the
/// listing, so whatever records the entry has to use the same spelling.
///
/// `None` when no entry, or more than one (hard links), is the same file,
/// or there is no handle to compare with: the caller cannot tell which
/// spelling is there and must not guess.
pub fn listed_spelling(
    dir: &Path,
    name: &str,
    names: &[OsString],
    entry: Option<&same_file::Handle>,
) -> Option<OsString> {
    if names.iter().any(|n| n == name) {
        return Some(OsString::from(name));
    }
    let entry = entry?;
    let mut same = names
        .iter()
        .filter(|n| same_file::Handle::from_path(dir.join(n)).is_ok_and(|h| h == *entry));
    match (same.next(), same.next()) {
        (Some(n), None) => Some(n.clone()),
        _ => None,
    }
}

/// `Path(name).suffix.lower() == ".flac"`: the suffix starts at the last dot,
/// and a name that starts or ends with its only dot has none.
fn has_flac_suffix(name: &str) -> bool {
    match name.rfind('.') {
        Some(i) if i > 0 && i < name.len() - 1 => name[i..].to_lowercase() == ".flac",
        _ => false,
    }
}

#[cfg(unix)]
fn inode_key(meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn inode_key(_meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    None
}

/// `Path.resolve()` as Python does it: make the path absolute, drop `.` and
/// `..`, and replace symlinks with their targets, WITHOUT asking the OS for
/// the canonical spelling. On a case-insensitive filesystem the caller's own
/// spelling of each name survives, which matters because the album id hashes
/// this string.
///
/// On Windows, Python's `Path.resolve()` does ask the OS (`ntpath.realpath`):
/// 8.3 short names are expanded, each name gets its on-disk case, and no
/// `\\?\` prefix is left. That is followed there
/// ([`musaic_core::config::windows_realpath`]), so one folder has one id
/// however its path was typed.
pub fn py_resolve(path: &Path) -> PathBuf {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    if cfg!(windows) {
        return musaic_core::config::windows_realpath(&abs);
    }
    resolve_abs(&abs, 0)
}

fn resolve_abs(abs: &Path, hops: u32) -> PathBuf {
    let comps: Vec<Component> = abs.components().collect();
    let mut out = PathBuf::new();
    for (i, c) in comps.iter().enumerate() {
        match c {
            Component::Prefix(_) | Component::RootDir => out.push(c.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(name) => {
                let candidate = out.join(name);
                match std::fs::read_link(&candidate) {
                    // A symlink: splice its target in and start over, so the
                    // target's own symlinks and `..` are handled too.
                    Ok(target) if hops < 40 => {
                        let mut next = if target.is_absolute() {
                            target
                        } else {
                            out.join(target)
                        };
                        for rest in &comps[i + 1..] {
                            next.push(rest.as_os_str());
                        }
                        return resolve_abs(&next, hops + 1);
                    }
                    _ => out.push(name),
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact name wins; otherwise the one same-file entry; two
    /// same-file entries (hard links) or no handle say nothing.
    #[test]
    fn listed_spelling_takes_the_exact_name_or_the_one_same_file_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("a.flac"), b"a").unwrap();
        std::fs::write(dir.join("other.flac"), b"b").unwrap();
        let a = same_file::Handle::from_path(dir.join("a.flac")).unwrap();
        let names = dir_names(dir).unwrap();
        let s = |name: &str, names: &[OsString], h: Option<&same_file::Handle>| {
            listed_spelling(dir, name, names, h)
        };
        assert_eq!(s("a.flac", &names, None), Some("a.flac".into()));
        // Listed under another spelling (as a volume that rewrites names
        // would): the entry that is the same file.
        assert_eq!(s("A.FLAC", &names, Some(&a)), Some("a.flac".into()));
        assert_eq!(s("A.FLAC", &names, None), None);
        std::fs::hard_link(dir.join("a.flac"), dir.join("b.flac")).unwrap();
        let names = dir_names(dir).unwrap();
        assert_eq!(s("A.FLAC", &names, Some(&a)), None);
        assert_eq!(s("b.flac", &names, Some(&a)), Some("b.flac".into()));
    }
}
