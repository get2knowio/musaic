//! `cache.import_track_tag_sidecars` + `track_tags_sidecar.read_sidecar` +
//! `cache.replace_album_track_tags`: after a committed scan, an album with no
//! `track_tags` rows takes its tags from `.media-manager-tags.json`. The scan
//! passes only the albums whose row it newly inserted, so a returning album
//! whose tag rows were deliberately removed does not re-import its sidecar
//! (a deliberate change: the Python app passed every scanned album).

use std::collections::BTreeMap;
use std::path::Path;

use musaic_core::pycompat::strip;
use rusqlite::Connection;

/// The Python app's logger for `track_tags_sidecar.py`.
pub const SIDECAR_LOG: &str = "musaic.track_tags_sidecar";
/// The Python app's logger for `cache.py`.
pub const CACHE_LOG: &str = "musaic.cache";

/// `read_sidecar(album_dir)`: `{filename: [tag, ...]}`, empty when the file is
/// absent, unreadable, malformed or of the wrong shape. Tags are stripped,
/// empties dropped, de-duplicated case-insensitively keeping the first
/// spelling; files left without tags are dropped.
///
/// The three failures are logged as the Python app logged them
/// (track_tags_sidecar.py:64, 71, 74: WARNING on `musaic.track_tags_sidecar`;
/// the interpolated error is io::Error's / serde_json's text, not Python's).
/// A file that is not UTF-8 is `Err`: in the Python app `read_text` raised
/// `UnicodeDecodeError`, which `except OSError` does not catch, so it
/// reached the importer's per-album handler ([`import_track_tag_sidecars`]).
pub fn read_sidecar(album_dir: &Path) -> Result<BTreeMap<String, Vec<String>>, std::io::Error> {
    let mut out = BTreeMap::new();
    let p = musaic_fsops::sidecar::sidecar_path(album_dir);
    if !p.is_file() {
        return Ok(out);
    }
    let raw = match std::fs::read_to_string(&p) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => return Err(e),
        Err(e) => {
            musaic_log::warning(
                SIDECAR_LOG,
                &format!("Could not read tag sidecar {}: {e}", p.display()),
            );
            return Ok(out);
        }
    };
    if strip(&raw).is_empty() {
        return Ok(out);
    }
    let mut root = match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(serde_json::Value::Object(root)) => root,
        Ok(_) => {
            musaic_log::warning(
                SIDECAR_LOG,
                &format!(
                    "Tag sidecar {} has wrong root shape (expected object)",
                    p.display()
                ),
            );
            return Ok(out);
        }
        Err(e) => {
            musaic_log::warning(
                SIDECAR_LOG,
                &format!("Malformed tag sidecar {}: {e}", p.display()),
            );
            return Ok(out);
        }
    };
    if matches!(root.get("tracks"), Some(serde_json::Value::Object(_)))
        && let Some(serde_json::Value::Object(inner)) = root.remove("tracks")
    {
        root = inner;
    }
    for (filename, tags) in root {
        if filename.is_empty() {
            continue;
        }
        let serde_json::Value::Array(tags) = tags else {
            continue;
        };
        let mut seen = std::collections::HashSet::new();
        let mut unique = Vec::new();
        for t in tags {
            let serde_json::Value::String(t) = t else {
                continue;
            };
            let t = strip(&t);
            if t.is_empty() || !seen.insert(t.to_lowercase()) {
                continue;
            }
            unique.push(t.to_string());
        }
        if !unique.is_empty() {
            out.insert(filename, unique);
        }
    }
    Ok(out)
}

/// `replace_album_track_tags`: delete the album's rows and insert the map,
/// `created_at = now`, in one transaction.
pub fn replace_album_track_tags(
    conn: &mut Connection,
    album_id: &str,
    map: &BTreeMap<String, Vec<String>>,
    now: &str,
) -> rusqlite::Result<()> {
    let tx = musaic_db::immediate(conn)?;
    tx.execute("DELETE FROM track_tags WHERE album_id = ?1", [album_id])?;
    for (filename, tags) in map {
        if filename.is_empty() {
            continue;
        }
        for tag in tags {
            let tag = strip(tag);
            if tag.is_empty() {
                continue;
            }
            tx.execute(
                "INSERT OR IGNORE INTO track_tags (album_id, filename, tag, created_at) \
                 VALUES (?1, ?2, ?3, ?4)",
                [album_id, filename, tag, now],
            )?;
        }
    }
    tx.commit()
}

/// `import_track_tag_sidecars(snapshot)` over `(id, path)` pairs; returns how
/// many albums were imported. Per-album failures are skipped and logged as
/// the Python app's per-album handler logged them (cache.py:1534
/// `log.exception("Failed to import tag sidecar for album %s", album_id)`:
/// ERROR; the error text stands in for the traceback).
pub fn import_track_tag_sidecars<'a>(
    conn: &mut Connection,
    albums: impl IntoIterator<Item = (&'a str, &'a str)>,
    now: &str,
) -> usize {
    let mut imported = 0;
    for (album_id, album_path) in albums {
        if album_id.is_empty() || album_path.is_empty() {
            continue;
        }
        let failed = |e: &dyn std::fmt::Display| {
            musaic_log::error(
                CACHE_LOG,
                &format!("Failed to import tag sidecar for album {album_id}: {e}"),
            );
        };
        let has_rows = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM track_tags WHERE album_id = ?1)",
            [album_id],
            |r| r.get::<_, bool>(0),
        );
        match has_rows {
            Ok(false) => {}
            Ok(true) => continue,
            Err(e) => {
                failed(&e);
                continue;
            }
        }
        let map = match read_sidecar(Path::new(album_path)) {
            Ok(m) => m,
            Err(e) => {
                failed(&e);
                continue;
            }
        };
        if map.is_empty() {
            continue;
        }
        match replace_album_track_tags(conn, album_id, &map, now) {
            Ok(()) => imported += 1,
            Err(e) => failed(&e),
        }
    }
    imported
}
