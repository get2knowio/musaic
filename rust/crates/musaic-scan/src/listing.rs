//! The one FLAC listing every layer uses.

use std::collections::HashSet;
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
pub fn py_resolve(path: &Path) -> PathBuf {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
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
