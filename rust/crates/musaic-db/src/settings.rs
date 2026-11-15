//! Key/value settings store.
//!
//! Values are strings in the `settings` table; a missing row falls back to
//! [`DEFAULTS`]. The three secret keys still read from the table here: moving
//! them to the OS keyring is musaic-secrets' job.

use rusqlite::{Connection, OptionalExtension};

use crate::Result;

/// Mirrors `DEFAULTS` in the Python app's `settings.py`; pinned by
/// `testdata/golden/settings-defaults.json`.
pub const DEFAULTS: &[(&str, &str)] = &[
    ("acoustid_api_key", ""),
    ("ai_max_parallel", "1"),
    ("ai_queue_paused", "0"),
    (
        "dir_template_compilation",
        "Various Artists - {album} [FLAC]",
    ),
    (
        "dir_template_single_artist",
        "{albumartist} - {album} [FLAC]",
    ),
    (
        "filename_template_compilation",
        "{tracknumber:02} - {artist} - {title}",
    ),
    (
        "filename_template_single_artist",
        "{tracknumber:02} - {title}",
    ),
    ("firecrawl_api_key", ""),
    ("snapshot_min_interval_hours", "24"),
    ("snapshot_retention_count", "10"),
];

pub fn default_for(key: &str) -> Option<&'static str> {
    DEFAULTS.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// The stored string, or `None` when there is no row (defaults NOT applied).
pub fn get_raw(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
            r.get(0)
        })
        .optional()?)
}

pub fn set_raw(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO settings(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [key, value],
    )?;
    Ok(())
}

/// Stored value parsed as an integer; `default` when missing or unparseable.
pub fn get_int(conn: &Connection, key: &str, default: i64) -> Result<i64> {
    Ok(get_raw(conn, key)?
        .and_then(|v| musaic_core::pycompat::int(&v))
        .unwrap_or(default))
}

/// Stored value as a boolean: "1", "true", "yes", "on" (trimmed, any case) are true,
/// anything else is false; `default` only when there is no row.
pub fn get_bool(conn: &Connection, key: &str, default: bool) -> Result<bool> {
    Ok(match get_raw(conn, key)? {
        None => default,
        Some(v) => matches!(
            v.trim().to_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
    })
}
