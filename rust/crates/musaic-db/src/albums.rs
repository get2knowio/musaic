//! Album rows: scans observe, users decide.
//!
//! A scan never deletes. An album that is in the cache but not in the new
//! snapshot gets `missing_since` set and keeps its row and everything hanging
//! off it (fingerprints, anchor, tags). Only [`delete_album`] (the user's removal)
//! and [`crate::merge::merge_album_rows`] (a user-confirmed merge, after moving
//! everything on the rows it drops) remove a row.
//!
//! Nothing here publishes bus events; each function returns the transitions
//! it made so the caller can.

use musaic_core::Album;
use musaic_core::pycompat::json_string;
use rusqlite::{Connection, OptionalExtension, params};

use crate::Result;

/// Columns a scan may overwrite on an existing row. Everything else on the
/// row (MusicBrainz anchor, AI review stamp, `missing_since`) belongs to
/// another subsystem and survives a rescan.
const SCANNER_OWNED_COLUMNS: [&str; 29] = [
    "library_path",
    "path",
    "dir_name",
    "title",
    "artist",
    "album_artist",
    "year",
    "genre",
    "track_count",
    "total_seconds",
    "has_embedded_cover",
    "cover_file",
    "sample_track",
    "dir_mtime",
    "dir_artist",
    "dir_album",
    "is_compilation",
    "disc_number",
    "disc_total",
    "sample_rate",
    "bits_per_sample",
    "channels",
    "track_numbers_json",
    "album_artist_sort",
    "artist_sort",
    "album_sort",
    "track_artists_json",
    "track_discs_json",
    "tag_disc_total",
];

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SnapshotResult {
    pub upserted_count: usize,
    /// Album ids that went Present -> Missing in this save, in id order.
    pub newly_missing: Vec<String>,
    /// Album ids that went Missing -> Present in this save, in id order.
    pub returned: Vec<String>,
    /// Album ids whose row did not exist before this save (inserted, not
    /// updated), in the order of `albums`. Only these may import a
    /// track-tag sidecar.
    pub inserted: Vec<String>,
    /// `(old id, new id)` of every row this save moved to a new id
    /// ([`save_snapshot_adopting`]), in the order given.
    pub adopted: Vec<(String, String)>,
}

/// A Present row of the library that the scan found again under another
/// path spelling, the same folder on disk: the save moves the row, and
/// everything that refers to it, to the new id and path instead of marking
/// it Missing and inserting a new row. The caller decided it is the same
/// folder; the save re-checks only what the database says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adoption {
    pub old_id: String,
    pub old_path: String,
    pub new_id: String,
    pub new_path: String,
    pub new_dir_name: String,
}

/// `json.dumps(track_numbers)`: `[1, 2, null]`.
fn track_numbers_json(a: &Album) -> String {
    int_list_json(&a.track_numbers)
}

fn int_list_json(list: &[Option<i64>]) -> String {
    let items: Vec<String> = list
        .iter()
        .map(|n| n.map_or_else(|| "null".to_string(), |n| n.to_string()))
        .collect();
    format!("[{}]", items.join(", "))
}

/// The per-file disc list in the shape of `track_numbers_json`; NULL while
/// the files have not been read for it, so a pre-v20 row that a scan reuses
/// unchanged keeps saying "not read yet".
fn track_discs_json(a: &Album) -> Option<String> {
    a.track_discs.as_deref().map(int_list_json)
}

/// `json.dumps(track_artists)`, or NULL when there are none.
fn track_artists_json(a: &Album) -> Option<String> {
    if a.track_artists.is_empty() {
        return None;
    }
    let items: Vec<String> = a
        .track_artists
        .iter()
        .map(|t| {
            format!(
                "{{\"filename\": {}, \"artist\": {}, \"artist_sort\": {}}}",
                json_string(&t.filename),
                json_string(&t.artist),
                json_string(&t.artist_sort)
            )
        })
        .collect();
    Some(format!("[{}]", items.join(", ")))
}

fn upsert_sql() -> String {
    let updates: Vec<String> = SCANNER_OWNED_COLUMNS
        .iter()
        .map(|c| format!("{c} = excluded.{c}"))
        .collect();
    format!(
        "INSERT INTO albums (id, {cols}) VALUES (?1, {marks}) ON CONFLICT(id) DO UPDATE SET {updates}",
        cols = SCANNER_OWNED_COLUMNS.join(", "),
        marks = (2..=SCANNER_OWNED_COLUMNS.len() + 1)
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(", "),
        updates = updates.join(", "),
    )
}

fn upsert_row(conn: &Connection, sql: &str, library_dir: &str, a: &Album) -> Result<()> {
    conn.execute(
        sql,
        params![
            a.id,
            library_dir,
            a.path,
            a.dir_name,
            a.title,
            a.artist,
            a.album_artist,
            a.year,
            a.genre,
            a.track_count as i64,
            a.total_seconds,
            a.has_embedded_cover as i64,
            a.cover_file,
            a.sample_track,
            a.dir_mtime,
            a.dir_artist,
            a.dir_album,
            a.is_compilation as i64,
            a.disc_number,
            a.disc_total,
            a.sample_rate,
            a.bits_per_sample,
            a.channels,
            track_numbers_json(a),
            a.album_artist_sort,
            a.artist_sort,
            a.album_sort,
            track_artists_json(a),
            track_discs_json(a),
            a.tag_disc_total,
        ],
    )?;
    Ok(())
}

/// Persist a scan result for `library_dir` (already resolved).
///
/// Inserts or updates every album in `albums`, clears `missing_since` on
/// albums that came back, and stamps `now` on cached albums of this library
/// that are absent and not already Missing (so the stamp records the FIRST
/// time an album was seen to be gone). All in one transaction.
pub fn save_snapshot(
    conn: &mut Connection,
    library_dir: &str,
    scanned_at: &str,
    now: &str,
    albums: &[Album],
) -> Result<SnapshotResult> {
    save_snapshot_adopting(conn, library_dir, scanned_at, now, albums, &[])
}

/// [`save_snapshot`] that first moves each [`Adoption`]'s row to its new
/// id and path (`renames::move_album_rows`), in the same transaction, so
/// an adopted row is updated by the upsert rather than inserted, and is
/// neither marked Missing nor counted as `inserted`.
///
/// Before moving a row it checks again that the row is still this
/// library's, Present, at `old_path`, that the new id and path are free
/// (`renames::album_key_in_use`) and that the new id is among `albums`.
/// If any of that no longer holds, nothing is written and
/// [`crate::Error::AdoptionStale`] is returned: a row is never merged into
/// one that appeared after the scan decided.
pub fn save_snapshot_adopting(
    conn: &mut Connection,
    library_dir: &str,
    scanned_at: &str,
    now: &str,
    albums: &[Album],
    adoptions: &[Adoption],
) -> Result<SnapshotResult> {
    let tx = crate::immediate(conn)?;
    tx.execute(
        "INSERT INTO libraries(path, scanned_at) VALUES (?1, ?2)
         ON CONFLICT(path) DO UPDATE SET scanned_at = excluded.scanned_at",
        [library_dir, scanned_at],
    )?;
    let sql = upsert_sql();
    let rows: Vec<&Album> = albums
        .iter()
        .filter(|a| !a.id.is_empty() && !a.path.is_empty())
        .collect();
    let mut adopted = Vec::with_capacity(adoptions.len());
    for ad in adoptions {
        let stale = || crate::Error::AdoptionStale {
            old_id: ad.old_id.clone(),
            new_id: ad.new_id.clone(),
        };
        let row: Option<(String, String, Option<String>)> = tx
            .query_row(
                "SELECT path, library_path, missing_since FROM albums WHERE id = ?1",
                [&ad.old_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let present_here = row.is_some_and(|(path, lib, missing)| {
            path == ad.old_path
                && lib == library_dir
                && missing.as_deref().is_none_or(str::is_empty)
        });
        let listed = rows
            .iter()
            .any(|a| a.id == ad.new_id && a.path == ad.new_path);
        if ad.old_id == ad.new_id
            || !present_here
            || !listed
            || crate::renames::album_key_in_use(&tx, &ad.new_id, &ad.new_path)?
        {
            return Err(stale());
        }
        crate::renames::move_album_rows(
            &tx,
            &ad.old_id,
            &ad.new_id,
            &ad.new_path,
            &ad.new_dir_name,
        )?;
        adopted.push((ad.old_id.clone(), ad.new_id.clone()));
    }
    let mut inserted = Vec::new();
    for a in &rows {
        let existed: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM albums WHERE id = ?1)",
            [&a.id],
            |r| r.get(0),
        )?;
        if !existed {
            inserted.push(a.id.clone());
        }
        upsert_row(&tx, &sql, library_dir, a)?;
    }
    let new_ids: std::collections::HashSet<&str> = rows.iter().map(|a| a.id.as_str()).collect();

    let cached: Vec<(String, bool)> = {
        let mut stmt = tx.prepare(
            "SELECT id, missing_since IS NOT NULL FROM albums WHERE library_path = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([library_dir], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    let mut result = SnapshotResult {
        upserted_count: rows.len(),
        inserted,
        adopted,
        ..Default::default()
    };
    for (id, was_missing) in cached {
        match (new_ids.contains(id.as_str()), was_missing) {
            (true, true) => {
                tx.execute(
                    "UPDATE albums SET missing_since = NULL WHERE id = ?1",
                    [&id],
                )?;
                result.returned.push(id);
            }
            (false, false) => {
                tx.execute(
                    "UPDATE albums SET missing_since = ?1 WHERE id = ?2",
                    [now, id.as_str()],
                )?;
                result.newly_missing.push(id);
            }
            _ => {}
        }
    }
    tx.commit()?;
    Ok(result)
}

/// Insert or refresh one album row after a single-album change, without
/// touching the library's `scanned_at` or any other album's presence.
pub fn upsert_album(
    conn: &mut Connection,
    library_dir: &str,
    now: &str,
    album: &Album,
) -> Result<()> {
    let tx = crate::immediate(conn)?;
    tx.execute(
        "INSERT INTO libraries(path, scanned_at) VALUES (?1, ?2) ON CONFLICT(path) DO NOTHING",
        [library_dir, now],
    )?;
    upsert_row(&tx, &upsert_sql(), library_dir, album)?;
    tx.commit()?;
    Ok(())
}

/// Every library path the database knows, in stored spelling.
pub fn library_paths(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT path FROM libraries ORDER BY path")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// `cache.drop_snapshot(library_dir)`: forget the cached snapshot WITHOUT
/// deleting anything. The library's `scanned_at` becomes `''` and every album
/// row of the library gets `dir_mtime = 0`, so the next scan re-reads every
/// album from disk while anchors, fingerprints, track tags and blocklists
/// survive. One transaction (the Python app's two UPDATEs). `library_dir` must
/// already be resolved the way the Python app does it
/// (`Path(library_dir).expanduser().resolve()`; `musaic_scan::py_resolve`).
pub fn drop_snapshot(conn: &mut Connection, library_dir: &str) -> Result<()> {
    let tx = crate::immediate(conn)?;
    tx.execute(
        "UPDATE libraries SET scanned_at = '' WHERE path = ?1",
        [library_dir],
    )?;
    tx.execute(
        "UPDATE albums SET dir_mtime = 0 WHERE library_path = ?1",
        [library_dir],
    )?;
    tx.commit()?;
    Ok(())
}

/// Remove an album row; fingerprints, track tags and blocklists cascade.
/// Reached only by an explicit user action; the other path that drops a row is
/// [`crate::merge::merge_album_rows`].
/// Returns whether a row existed.
pub fn delete_album(conn: &Connection, album_id: &str) -> Result<bool> {
    Ok(conn.execute("DELETE FROM albums WHERE id = ?1", [album_id])? > 0)
}

/// Present -> Missing. `true` only when the row actually changed.
pub fn mark_album_missing(conn: &Connection, album_id: &str, since: &str) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE albums SET missing_since = ?1 WHERE id = ?2 AND missing_since IS NULL",
        [since, album_id],
    )? > 0)
}

/// Missing -> Present. `true` only when the row actually changed.
pub fn clear_album_missing(conn: &Connection, album_id: &str) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE albums SET missing_since = NULL WHERE id = ?1 AND missing_since IS NOT NULL",
        [album_id],
    )? > 0)
}

/// `None` when no album row has this id; otherwise the row's
/// `missing_since` (`None` while the album is Present).
pub fn album_row_missing_since(
    conn: &Connection,
    album_id: &str,
) -> Result<Option<Option<String>>> {
    Ok(conn
        .query_row(
            "SELECT missing_since FROM albums WHERE id = ?1",
            [album_id],
            |r| r.get(0),
        )
        .optional()?)
}

pub fn missing_since(conn: &Connection, album_id: &str) -> Result<Option<String>> {
    Ok(album_row_missing_since(conn, album_id)?.flatten())
}

/// Why a scan refused to write its result.
#[derive(Debug, Clone, PartialEq)]
pub struct ScanAbort {
    pub vanish_ratio: f64,
    pub vanish_count: usize,
    pub cached_count: usize,
    pub threshold: f64,
    pub detail: String,
}

/// A scan that would mark more than 30% of a cache of 10 or more albums as
/// Missing is treated as a drive outage, and nothing is written.
///
/// Callers pass Present counts. `cached_count`
/// is the number of this library's cached albums that were Present before
/// the scan, and `vanish_count` how many of those are absent now; albums
/// already Missing count toward neither (the Python app counted every cached
/// row), so the ">= 10" minimum also applies to Present albums.
pub fn scan_sanity_check(
    cached_count: usize,
    vanish_count: usize,
    library_dir: &str,
) -> Option<ScanAbort> {
    const THRESHOLD: f64 = 0.30;
    const MIN_CACHED: usize = 10;
    let ratio = if cached_count > 0 {
        vanish_count as f64 / cached_count as f64
    } else {
        0.0
    };
    if cached_count < MIN_CACHED || ratio <= THRESHOLD {
        return None;
    }
    Some(ScanAbort {
        vanish_ratio: ratio,
        vanish_count,
        cached_count,
        threshold: THRESHOLD,
        detail: format!(
            "Scan aborted: {vanish_count} of {cached_count} cached album(s) ({pct:.0}%) appear to be missing \
             from disk. This looks like a drive outage or a mount problem rather than real deletions. No changes \
             were written. Check that {library_dir} is mounted and readable, then re-run the scan.",
            pct = ratio * 100.0
        ),
    })
}

/// `cache.record_mb_lookup_failure`: stamps `mb_last_checked_at` on an album
/// that has no release-group anchor ("tried, no match").
pub fn record_mb_lookup_failure(conn: &Connection, album_id: &str, now_iso: &str) -> Result<()> {
    conn.execute(
        "UPDATE albums SET mb_last_checked_at = ?1 WHERE id = ?2 AND mb_release_group_mbid IS NULL",
        params![now_iso, album_id],
    )?;
    Ok(())
}

/// `cache.mark_metadata_ai_acted_on`.
pub fn mark_metadata_ai_acted_on(conn: &Connection, album_id: &str, now_iso: &str) -> Result<()> {
    conn.execute(
        "UPDATE albums SET metadata_ai_acted_on_at = ?1 WHERE id = ?2",
        params![now_iso, album_id],
    )?;
    Ok(())
}
