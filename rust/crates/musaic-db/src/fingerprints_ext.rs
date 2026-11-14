//! `track_fingerprints` rows as `fingerprint.fingerprint_album` and the
//! eager fingerprint pool read and write them (`musaic/cache.py`
//! `get_track_fingerprints` / `upsert_track_fingerprint`,
//! `musaic/services/fingerprint_pool.py::_candidates`), plus this build's
//! additions: the v17 `fingerprint_impl` column, the v18 AcoustID
//! lookup outcome columns and the v18
//! `fingerprint_rejects` table.
//!
//! Timestamps are passed in by the caller (the Python app's `cache._now_iso()`).

use std::collections::BTreeSet;

use rusqlite::{Connection, params};

use crate::Result;

/// One full `track_fingerprints` row.
#[derive(Debug, Clone, PartialEq)]
pub struct FingerprintRecord {
    pub filename: String,
    pub fingerprint: String,
    pub duration_s: f64,
    pub acoustid_recording_mbid: Option<String>,
    pub acoustid_score: Option<f64>,
    pub last_computed_at: String,
    /// `fpcalc` for rows the Python app wrote, `rust-chromaprint` for ours.
    pub fingerprint_impl: String,
    /// NULL (no lookup outcome yet), [`crate::LOOKUP_DONE`] or
    /// [`crate::LOOKUP_FAILED`].
    pub acoustid_lookup_state: Option<String>,
    /// Consecutive failed lookups of this row (0 after a completed one).
    pub acoustid_lookup_failures: i64,
    /// For a failed lookup: not asked again before this time (same ISO
    /// format as every timestamp here, so it compares as text).
    pub acoustid_retry_after: Option<String>,
}

impl FingerprintRecord {
    /// Whether this row's AcoustID lookup is due at `now`: none has
    /// completed (never asked, or asked without a key), or the last one
    /// failed and its retry time has passed. A completed lookup, match or
    /// not, is never due.
    pub fn lookup_due(&self, now: &str) -> bool {
        match self.acoustid_lookup_state.as_deref() {
            None => true,
            Some(s) if s == crate::LOOKUP_FAILED => self
                .acoustid_retry_after
                .as_deref()
                .is_none_or(|t| t <= now),
            Some(_) => false,
        }
    }
}

/// `cache.get_track_fingerprints(album_id)` with this build's columns,
/// ordered by filename.
pub fn get_album_fingerprints(conn: &Connection, album_id: &str) -> Result<Vec<FingerprintRecord>> {
    let mut stmt = conn.prepare(
        "SELECT filename, fingerprint, duration_s, acoustid_recording_mbid, acoustid_score, \
                last_computed_at, fingerprint_impl, acoustid_lookup_state, \
                acoustid_lookup_failures, acoustid_retry_after \
         FROM track_fingerprints WHERE album_id = ?1 ORDER BY filename",
    )?;
    let rows = stmt.query_map([album_id], |r| {
        Ok(FingerprintRecord {
            filename: r.get(0)?,
            fingerprint: r.get(1)?,
            duration_s: r.get(2)?,
            acoustid_recording_mbid: r.get(3)?,
            acoustid_score: r.get(4)?,
            last_computed_at: r.get(5)?,
            fingerprint_impl: r.get(6)?,
            acoustid_lookup_state: r.get(7)?,
            acoustid_lookup_failures: r.get(8)?,
            acoustid_retry_after: r.get(9)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// `cache.upsert_track_fingerprint(...)`: insert, or refresh every column of
/// the `(album_id, filename)` row. `rec.last_computed_at` is written as given.
pub fn upsert_track_fingerprint(
    conn: &Connection,
    album_id: &str,
    rec: &FingerprintRecord,
) -> Result<()> {
    conn.execute(
        "INSERT INTO track_fingerprints \
         (album_id, filename, fingerprint, duration_s, acoustid_recording_mbid, acoustid_score, \
          last_computed_at, fingerprint_impl, acoustid_lookup_state, acoustid_lookup_failures, \
          acoustid_retry_after) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
         ON CONFLICT(album_id, filename) DO UPDATE SET \
          fingerprint = excluded.fingerprint, \
          duration_s = excluded.duration_s, \
          acoustid_recording_mbid = excluded.acoustid_recording_mbid, \
          acoustid_score = excluded.acoustid_score, \
          last_computed_at = excluded.last_computed_at, \
          fingerprint_impl = excluded.fingerprint_impl, \
          acoustid_lookup_state = excluded.acoustid_lookup_state, \
          acoustid_lookup_failures = excluded.acoustid_lookup_failures, \
          acoustid_retry_after = excluded.acoustid_retry_after",
        params![
            album_id,
            rec.filename,
            rec.fingerprint,
            rec.duration_s,
            rec.acoustid_recording_mbid,
            rec.acoustid_score,
            rec.last_computed_at,
            rec.fingerprint_impl,
            rec.acoustid_lookup_state,
            rec.acoustid_lookup_failures,
            rec.acoustid_retry_after,
        ],
    )?;
    Ok(())
}

/// Delete the album's `track_fingerprints` and `fingerprint_rejects`
/// rows whose filename is not in `present` (files that no longer exist).
/// Returns the number of fingerprint rows deleted.
pub fn delete_vanished(conn: &Connection, album_id: &str, present: &[String]) -> Result<usize> {
    let keep: BTreeSet<&str> = present.iter().map(String::as_str).collect();
    let mut deleted = 0;
    for table in ["track_fingerprints", "fingerprint_rejects"] {
        let names: Vec<String> = conn
            .prepare(&format!("SELECT filename FROM {table} WHERE album_id = ?1"))?
            .query_map([album_id], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        for n in names.iter().filter(|n| !keep.contains(n.as_str())) {
            let k = conn.execute(
                &format!("DELETE FROM {table} WHERE album_id = ?1 AND filename = ?2"),
                params![album_id, n],
            )?;
            if table == "track_fingerprints" {
                deleted += k;
            }
        }
    }
    Ok(deleted)
}

/// One `fingerprint_rejects` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectRecord {
    pub filename: String,
    /// What the file looked like when it was rejected (size and times); a
    /// file whose signature differs is tried again.
    pub file_signature: String,
    /// The fingerprinter that rejected it; another one tries again.
    pub fingerprint_impl: String,
    pub rejected_at: String,
}

/// The album's rejected files, ordered by filename.
pub fn get_rejects(conn: &Connection, album_id: &str) -> Result<Vec<RejectRecord>> {
    let mut stmt = conn.prepare(
        "SELECT filename, file_signature, fingerprint_impl, rejected_at \
         FROM fingerprint_rejects WHERE album_id = ?1 ORDER BY filename",
    )?;
    let rows = stmt.query_map([album_id], |r| {
        Ok(RejectRecord {
            filename: r.get(0)?,
            file_signature: r.get(1)?,
            fingerprint_impl: r.get(2)?,
            rejected_at: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// Remember (or refresh) a rejected file.
pub fn upsert_reject(conn: &Connection, album_id: &str, rec: &RejectRecord) -> Result<()> {
    conn.execute(
        "INSERT INTO fingerprint_rejects \
         (album_id, filename, file_signature, fingerprint_impl, rejected_at) \
         VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT(album_id, filename) DO UPDATE SET \
          file_signature = excluded.file_signature, \
          fingerprint_impl = excluded.fingerprint_impl, \
          rejected_at = excluded.rejected_at",
        params![
            album_id,
            rec.filename,
            rec.file_signature,
            rec.fingerprint_impl,
            rec.rejected_at,
        ],
    )?;
    Ok(())
}

/// Remember a rejected file and drop the fingerprint row it may
/// still have from before it changed, in one transaction (the rows stay the
/// files that fingerprint).
pub fn record_reject(conn: &Connection, album_id: &str, rec: &RejectRecord) -> Result<()> {
    let tx = crate::immediate(conn)?;
    upsert_reject(&tx, album_id, rec)?;
    tx.execute(
        "DELETE FROM track_fingerprints WHERE album_id = ?1 AND filename = ?2",
        params![album_id, rec.filename],
    )?;
    tx.commit()?;
    Ok(())
}

/// Forget a rejected file (it fingerprinted after all).
pub fn delete_reject(conn: &Connection, album_id: &str, filename: &str) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM fingerprint_rejects WHERE album_id = ?1 AND filename = ?2",
        params![album_id, filename],
    )?)
}

/// `fingerprint_pool._candidates(library_dir)`: `(album_id, path)` of every
/// album of the library with tracks and fewer fingerprint rows than tracks,
/// in the order SQLite returns them (the Python app's query has no ORDER BY;
/// this is the same statement plus the conditions below, so the same plan
/// and order).
///
/// Deliberate changes:
///
/// * An album is also a candidate when any of its rows
///   was computed by another implementation than `current_impl` (the Python
///   app's fpcalc rows are recomputed, never mixed).
/// * Missing albums (`missing_since` set) are not candidates.
/// * With `lookups_due_at = Some(now)` (an AcoustID key
///   is set), an album is also a candidate when one of its rows has a lookup
///   due at `now` (see [`FingerprintRecord::lookup_due`]). `None` (no key)
///   leaves those albums alone, so a library fingerprinted without a key is
///   not walked on every pass.
pub fn pool_candidates(
    conn: &Connection,
    library_dir: &str,
    current_impl: &str,
    lookups_due_at: Option<&str>,
) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT a.id, a.path, a.track_count, \
                COALESCE(fp.fp_count, 0) AS fp_count \
         FROM albums a \
         LEFT JOIN ( \
             SELECT album_id, COUNT(*) AS fp_count \
             FROM track_fingerprints \
             GROUP BY album_id \
         ) fp ON a.id = fp.album_id \
         WHERE a.library_path = ?1 \
           AND a.track_count > 0 \
           AND a.missing_since IS NULL \
           AND (a.track_count > COALESCE(fp.fp_count, 0) \
                OR EXISTS (SELECT 1 FROM track_fingerprints o \
                           WHERE o.album_id = a.id AND o.fingerprint_impl != ?2) \
                OR (?3 IS NOT NULL AND EXISTS ( \
                       SELECT 1 FROM track_fingerprints l \
                       WHERE l.album_id = a.id \
                         AND (l.acoustid_lookup_state IS NULL \
                              OR (l.acoustid_lookup_state = 'failed' \
                                  AND (l.acoustid_retry_after IS NULL \
                                       OR l.acoustid_retry_after <= ?3))))))",
    )?;
    let rows = stmt.query_map(params![library_dir, current_impl, lookups_due_at], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}
