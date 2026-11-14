//! The per-album track-tag sidecar (`.media-manager-tags.json`), rewritten
//! after file renames (port of `track_tags_sidecar.write_sidecar`
//! and the apply.py step that calls it).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

pub const SIDECAR_FILENAME: &str = ".media-manager-tags.json";

pub fn sidecar_path(album_dir: &Path) -> PathBuf {
    album_dir.join(SIDECAR_FILENAME)
}

/// A JSON string as `json.dumps(..., ensure_ascii=False)` writes it.
fn json_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `json.dumps(map, indent=2, sort_keys=True, ensure_ascii=False)`.
pub fn render_sidecar(map: &BTreeMap<String, Vec<String>>) -> String {
    if map.is_empty() {
        return "{}".into();
    }
    let mut out = String::from("{\n");
    for (i, (k, tags)) in map.iter().enumerate() {
        out.push_str("  ");
        json_str(k, &mut out);
        out.push_str(": ");
        if tags.is_empty() {
            out.push_str("[]");
        } else {
            out.push_str("[\n");
            for (j, t) in tags.iter().enumerate() {
                out.push_str("    ");
                json_str(t, &mut out);
                out.push_str(if j + 1 < tags.len() { ",\n" } else { "\n" });
            }
            out.push_str("  ]");
        }
        out.push_str(if i + 1 < map.len() { ",\n" } else { "\n" });
    }
    out.push('}');
    out
}

/// `write_sidecar`: entries with an empty filename or no tags are dropped; an
/// empty result deletes the sidecar; otherwise write `<name>.tmp` and rename
/// over it. `false` on an I/O error, logged as the Python app logs it.
pub fn write_sidecar(album_dir: &Path, track_tags: &BTreeMap<String, Vec<String>>) -> bool {
    let p = sidecar_path(album_dir);
    let cleaned: BTreeMap<String, Vec<String>> = track_tags
        .iter()
        .filter(|(f, t)| !f.is_empty() && !t.is_empty())
        .map(|(f, t)| (f.clone(), t.clone()))
        .collect();
    let res = (|| -> std::io::Result<()> {
        if cleaned.is_empty() {
            if p.is_file() {
                std::fs::remove_file(&p)?;
            }
            return Ok(());
        }
        let tmp = album_dir.join(format!("{SIDECAR_FILENAME}.tmp"));
        std::fs::write(&tmp, render_sidecar(&cleaned))?;
        std::fs::rename(&tmp, &p)
    })();
    if let Err(e) = &res {
        // track_tags_sidecar.py:137; `e` is io::Error's text.
        musaic_log::warning(
            "musaic.track_tags_sidecar",
            &format!("Could not write tag sidecar {}: {e}", p.display()),
        );
    }
    res.is_ok()
}

/// apply.py step 5: after renames, rewrite the sidecar from the database when
/// the album has any track tags (an empty map leaves the file untouched).
/// Returns whether a write was attempted and its result.
pub fn resync_after_renames(
    conn: &Connection,
    album_id: &str,
    album_dir: &Path,
) -> musaic_db::Result<Option<bool>> {
    let map = musaic_db::renames::album_track_tags(conn, album_id)?;
    if map.is_empty() {
        return Ok(None);
    }
    Ok(Some(write_sidecar(album_dir, &map)))
}
