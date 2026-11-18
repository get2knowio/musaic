//! Which vanished album rows a scan adopts into a folder it found under
//! another spelling (CLAUDE.md rules 3 and 6).
//!
//! The scan matches its cached rows to the folders it found by path string.
//! A folder the volume now lists under another spelling than its row holds
//! (a decomposed name on HFS+, a case change made outside the app on a
//! case-insensitive volume, a share whose normalisation changed) would look
//! like one album gone Missing and a new one with a new id, without the
//! old one's fingerprints, anchor, ignored findings or blocklist. Where the
//! filesystem says the old spelling and the new one are the same folder,
//! the row moves to the new id and path instead
//! (`musaic_db::albums::save_snapshot_adopting`).
//!
//! A row is adopted only when all of this holds, and refused otherwise:
//!
//! * it was Present before the scan and no folder was found at its stored
//!   path (a Missing row is never adopted);
//! * its stored path still opens as a directory (on an unmounted drive it
//!   does not, so the sanity check sees the same vanished albums as
//!   before; on a volume that tells names apart byte for byte, such as
//!   ext4, an NFC and an NFD spelling are two names and the old one does
//!   not open the new folder);
//! * among the folders found whose path differs from it only in case or
//!   Unicode normalisation, exactly one is the same file as it
//!   (`same_file`, never a string comparison);
//! * that folder was read into an album by this scan, has no album row of
//!   its own (in any library), and no table holds rows under its id;
//! * no other vanished row is the same file as that folder.
//!
//! The comparison of spellings only picks which folders to ask the
//! filesystem about: a row whose folder now has a different name is a
//! rename, which this does not try to follow.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use musaic_core::Album;
use musaic_db::albums::Adoption;
use rusqlite::Connection;
use unicode_normalization::UnicodeNormalization;

/// The key under which spellings of one path that a case-insensitive or
/// normalisation-insensitive volume could treat as one name meet.
fn spelling_key(path: &str) -> String {
    path.nfc().collect::<String>().to_lowercase()
}

/// A folder the scan found.
pub struct Found<'a> {
    pub path: &'a str,
    /// The album this scan read from it; `None` when it gave none (no
    /// readable FLAC), so it gets no row and cannot take one over.
    pub album: Option<&'a Album>,
    /// The library's cache already has a row at this path.
    pub has_row: bool,
}

/// The adoptions for this scan, from `removed` (`(id, path)` of the rows
/// that were Present and whose path no folder was found at) and `found`
/// (every folder the scan found). `conn` answers whether a new id is free;
/// a database error refuses that adoption.
pub fn plan(conn: &Connection, removed: &[(String, String)], found: &[Found<'_>]) -> Vec<Adoption> {
    if removed.is_empty() {
        return Vec::new();
    }
    let mut by_key: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, f) in found.iter().enumerate() {
        by_key.entry(spelling_key(f.path)).or_default().push(i);
    }
    // found index -> the removed rows that are the same file as it.
    let mut claims: HashMap<usize, Vec<usize>> = HashMap::new();
    for (r, (_, old_path)) in removed.iter().enumerate() {
        let Some(group) = by_key.get(&spelling_key(old_path)) else {
            continue;
        };
        let old = Path::new(old_path);
        if !std::fs::metadata(old).is_ok_and(|m| m.is_dir()) {
            continue;
        }
        let Ok(handle) = same_file::Handle::from_path(old) else {
            continue;
        };
        let same: Vec<usize> = group
            .iter()
            .copied()
            .filter(|&i| same_file::Handle::from_path(found[i].path).is_ok_and(|h| h == handle))
            .collect();
        // Two folders found that are both this one: a listing that repeats
        // a folder under two spellings. Neither is trusted.
        if let [only] = same[..] {
            claims.entry(only).or_default().push(r);
        }
    }
    let mut out = Vec::new();
    let mut taken: HashSet<&str> = HashSet::new();
    let mut indices: Vec<usize> = claims.keys().copied().collect();
    indices.sort_unstable();
    for i in indices {
        let rows = &claims[&i];
        let f = &found[i];
        let [r] = rows[..] else {
            continue;
        };
        let Some(album) = f.album else {
            continue;
        };
        if f.has_row
            || album.id.is_empty()
            || album.path != f.path
            || !taken.insert(album.id.as_str())
        {
            continue;
        }
        match musaic_db::renames::album_key_in_use(conn, &album.id, &album.path) {
            Ok(false) => {}
            Ok(true) | Err(_) => continue,
        }
        let (old_id, old_path) = &removed[r];
        out.push(Adoption {
            old_id: old_id.clone(),
            old_path: old_path.clone(),
            new_id: album.id.clone(),
            new_path: album.path.clone(),
            new_dir_name: album.dir_name.clone(),
        });
    }
    // In the order the rows were cached, so events and logs follow the
    // scan's own order.
    let pos: HashMap<&str, usize> = removed
        .iter()
        .enumerate()
        .map(|(i, (id, _))| (id.as_str(), i))
        .collect();
    out.sort_by_key(|a| pos.get(a.old_id.as_str()).copied().unwrap_or(usize::MAX));
    out
}
