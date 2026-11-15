//! MusicBrainz anchor columns, the per-album "Wrong match" blocklist, and the
//! fingerprint rows the resolver reads.
//!
//! Same SQL shapes as the Python app's `musaic/cache.py`
//! (`get_album_mb_anchor`, `set_album_mb_anchor`, `add_blocked_mb_release_group`,
//! `get_blocked_mb_release_groups`, `clear_album_mb_anchor`,
//! `get_track_fingerprints`). Timestamps are passed in by the caller (the Python app
//! writes `now()` as `YYYY-MM-DDTHH:MM:SSZ`), so these functions stay pure.
//! Nothing here publishes events; the caller does.

use std::collections::BTreeSet;

use rusqlite::{Connection, OptionalExtension, params};

use crate::Result;

/// `get_album_mb_anchor`: the four anchor columns of an album row.
#[derive(Debug, Clone, PartialEq)]
pub struct MbAnchorRow {
    pub release_group_mbid: String,
    pub release_mbid: Option<String>,
    pub score: Option<i64>,
    pub last_checked_at: Option<String>,
}

/// `None` when the album does not exist or has no release-group anchor.
pub fn get_album_mb_anchor(conn: &Connection, album_id: &str) -> Result<Option<MbAnchorRow>> {
    let row = conn
        .query_row(
            "SELECT mb_release_group_mbid, mb_release_mbid, mb_match_score, mb_last_checked_at \
             FROM albums WHERE id = ?1",
            [album_id],
            |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .optional()?;
    Ok(row.and_then(|(rg, rel, score, checked)| {
        rg.map(|release_group_mbid| MbAnchorRow {
            release_group_mbid,
            release_mbid: rel,
            score,
            last_checked_at: checked,
        })
    }))
}

/// `set_album_mb_anchor`: writes all four columns, `last_checked_at = now_iso`.
/// Returns whether a row was updated (the Python app ignores a missing album).
pub fn set_album_mb_anchor(
    conn: &Connection,
    album_id: &str,
    release_group_mbid: &str,
    release_mbid: Option<&str>,
    score: i64,
    now_iso: &str,
) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE albums SET mb_release_group_mbid = ?1, mb_release_mbid = ?2, \
         mb_match_score = ?3, mb_last_checked_at = ?4 WHERE id = ?5",
        params![release_group_mbid, release_mbid, score, now_iso, album_id],
    )? > 0)
}

/// `clear_album_mb_anchor`: resets every anchor column.
pub fn clear_album_mb_anchor(conn: &Connection, album_id: &str) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE albums SET mb_release_group_mbid = NULL, mb_release_mbid = NULL, \
         mb_match_score = NULL, mb_last_checked_at = NULL WHERE id = ?1",
        [album_id],
    )? > 0)
}

/// `add_blocked_mb_release_group`: `INSERT OR IGNORE`; an empty id is a no-op.
pub fn add_blocked_mb_release_group(
    conn: &Connection,
    album_id: &str,
    release_group_mbid: &str,
    now_iso: &str,
) -> Result<bool> {
    if release_group_mbid.is_empty() {
        return Ok(false);
    }
    Ok(conn.execute(
        "INSERT OR IGNORE INTO album_blocked_mb_release_groups \
         (album_id, release_group_mbid, blocked_at) VALUES (?1, ?2, ?3)",
        params![album_id, release_group_mbid, now_iso],
    )? > 0)
}

/// `get_blocked_mb_release_groups`.
pub fn get_blocked_mb_release_groups(
    conn: &Connection,
    album_id: &str,
) -> Result<BTreeSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT release_group_mbid FROM album_blocked_mb_release_groups WHERE album_id = ?1",
    )?;
    let rows = stmt.query_map([album_id], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// The resolver's view of one `track_fingerprints` row.
#[derive(Debug, Clone, PartialEq)]
pub struct FingerprintRow {
    pub filename: String,
    pub acoustid_recording_mbid: Option<String>,
    pub acoustid_score: Option<f64>,
}

/// `get_track_fingerprints` (only the columns the resolver reads), ordered by filename.
pub fn get_track_fingerprints(conn: &Connection, album_id: &str) -> Result<Vec<FingerprintRow>> {
    let mut stmt = conn.prepare(
        "SELECT filename, acoustid_recording_mbid, acoustid_score \
         FROM track_fingerprints WHERE album_id = ?1 ORDER BY filename",
    )?;
    let rows = stmt.query_map([album_id], |r| {
        Ok(FingerprintRow {
            filename: r.get(0)?,
            acoustid_recording_mbid: r.get(1)?,
            acoustid_score: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}
