//! `duplicate-tracks` (port of `checks/duplicate_tracks.py`): clusters of
//! identical audio inside one album, by Chromaprint similarity (musaic-dsp's
//! rule). Also `derive_cluster` and the cluster-preview body, and
//! the trash step of the per-file delete.
//!
//! Deliberate change: fingerprints are only compared with
//! fingerprints of the same `fingerprint_impl`; with one implementation in
//! the database (always, after the pool recomputes) this is the Python app.

use std::path::{Path, PathBuf};

use musaic_core::pycompat::{decimal_digit_value, strip};
use musaic_db::findings::FindingRow;
use musaic_dsp::{ClusterTrack, cluster_duplicates};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Value, json};

use crate::base::{
    Check, CheckCtx, CheckError, CheckMeta, CheckScope, Cost, Emit, Finding, FixKind, Severity,
};
use crate::pyjson::{PyJson, dict};

pub struct DuplicateTracks;

pub static META: CheckMeta = CheckMeta {
    id: "duplicate-tracks",
    label: "Duplicate audio files",
    description: "Albums that hold the same recording more than once: files whose audio fingerprints match, whatever \
their names or tags say. This needs fingerprints, which Musaic computes in the background after a \
scan. Resolve lets you choose which copy to keep.",
    category: "duplicates",
    cost: Cost::Cheap,
    runs_during_scan: true,
    fix_kind: FixKind::Deterministic,
    bulk_action_label: None,
};

/// One fingerprinted track as the check reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct FpTrack {
    pub filename: String,
    pub duration_s: f64,
    pub fingerprint: String,
    pub mbid: Option<String>,
    pub fingerprint_impl: String,
}

/// The cluster shape stored in `Finding.detail` and returned by
/// `derive_cluster`.
#[derive(Debug, Clone, PartialEq)]
pub struct Cluster {
    pub recording_mbid: Option<String>,
    pub filenames: Vec<String>,
    pub durations_s: Vec<f64>,
    pub recommended_keep: Option<String>,
    pub recommended_reason: Option<&'static str>,
}

impl Cluster {
    fn from_tracks(tracks: &[&FpTrack]) -> Cluster {
        let filenames: Vec<String> = tracks.iter().map(|t| t.filename.clone()).collect();
        let (keep, reason) = recommended_keep(&filenames);
        Cluster {
            recording_mbid: most_common(tracks.iter().filter_map(|t| t.mbid.as_deref())),
            durations_s: tracks.iter().map(|t| t.duration_s).collect(),
            recommended_keep: keep,
            recommended_reason: reason,
            filenames,
        }
    }

    pub fn to_pyjson(&self) -> PyJson {
        dict([
            ("recording_mbid", self.recording_mbid.clone().into()),
            ("filenames", self.filenames.clone().into()),
            ("durations_s", self.durations_s.clone().into()),
            ("duplicate_count", PyJson::Int(self.filenames.len() as i64)),
            ("recommended_keep", self.recommended_keep.clone().into()),
            ("recommended_reason", self.recommended_reason.into()),
        ])
    }
}

/// `Counter(items).most_common(1)`: highest count, first seen on ties.
fn most_common<'a>(items: impl Iterator<Item = &'a str>) -> Option<String> {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for x in items {
        match counts.iter_mut().find(|(k, _)| *k == x) {
            Some((_, n)) => *n += 1,
            None => counts.push((x, 1)),
        }
    }
    let mut best: Option<(&str, usize)> = None;
    for (k, n) in counts {
        if best.is_none_or(|(_, b)| n > b) {
            best = Some((k, n));
        }
    }
    best.map(|(k, _)| k.to_string())
}

const WINDOWS_FORBIDDEN: &str = "<>:\"/\\|?*";

/// `_portability_score`: Windows-forbidden characters, plus one for a
/// trailing dot or space. Lower is better.
pub fn portability_score(filename: &str) -> usize {
    let n = filename
        .chars()
        .filter(|c| WINDOWS_FORBIDDEN.contains(*c))
        .count();
    n + usize::from(filename.ends_with([' ', '.']))
}

pub const KEEP_REASON: &str = "Other copies use characters that Windows filesystems and SMB shares reject \
(?, :, *, etc.). Keeping this one keeps the library portable to Windows backups, NAS shares, and external drives.";

/// `_recommended_keep`: `(filename, reason)`; both `None` when every
/// candidate is equally portable.
pub fn recommended_keep(filenames: &[String]) -> (Option<String>, Option<&'static str>) {
    match filenames {
        [] => (None, None),
        [one] => (Some(one.clone()), None),
        _ => {
            let mut scored: Vec<(usize, &String)> = filenames
                .iter()
                .map(|f| (portability_score(f), f))
                .collect();
            scored.sort();
            let (best, file) = scored[0];
            if best == scored[scored.len() - 1].0 {
                (None, None)
            } else {
                (Some(file.clone()), Some(KEEP_REASON))
            }
        }
    }
}

/// Python `str.isdigit()` for a stripped token (decimal digits).
fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| decimal_digit_value(c).is_some())
}

/// `_readable_label`: drop `.flac` and a leading `NN - ` / `NN. `.
pub fn readable_label(filename: &str) -> String {
    let chars: Vec<char> = filename.chars().collect();
    let mut name: String = if chars.len() >= 5
        && chars[chars.len() - 5..]
            .iter()
            .collect::<String>()
            .to_lowercase()
            == ".flac"
    {
        chars[..chars.len() - 5].iter().collect()
    } else {
        filename.to_string()
    };
    for sep in [" - ", ". "] {
        if let Some((head, tail)) = name.split_once(sep)
            && is_digits(strip(head))
        {
            name = tail.to_string();
        }
    }
    strip(&name).to_string()
}

/// Cluster one album's tracks; only tracks of the same fingerprint
/// implementation are compared. Clusters come out per
/// implementation in order of first appearance, then in musaic-dsp order.
pub fn cluster_album(tracks: &[FpTrack]) -> Vec<Vec<&FpTrack>> {
    let mut impls: Vec<&str> = Vec::new();
    for t in tracks {
        if !impls.contains(&t.fingerprint_impl.as_str()) {
            impls.push(&t.fingerprint_impl);
        }
    }
    let mut out = Vec::new();
    for imp in impls {
        let group: Vec<&FpTrack> = tracks
            .iter()
            .filter(|t| t.fingerprint_impl == imp)
            .collect();
        let input: Vec<ClusterTrack<'_>> = group
            .iter()
            .map(|t| ClusterTrack {
                filename: &t.filename,
                duration_s: t.duration_s,
                fingerprint: &t.fingerprint,
            })
            .collect();
        for c in cluster_duplicates(&input) {
            out.push(c.into_iter().map(|i| group[i]).collect());
        }
    }
    out
}

fn build_finding(album_id: &str, cluster: &[&FpTrack]) -> Finding {
    let label = cluster.first().map_or_else(
        || "Unknown track".to_string(),
        |t| readable_label(&t.filename),
    );
    Finding {
        album_id: Some(album_id.to_string()),
        summary: format!("Duplicate audio: \"{label}\" ({} copies)", cluster.len()),
        severity: Severity::Warn,
        detail: Some(Cluster::from_tracks(cluster).to_pyjson()),
    }
}

fn fp_track(r: &rusqlite::Row<'_>) -> rusqlite::Result<FpTrack> {
    Ok(FpTrack {
        filename: r.get("filename")?,
        duration_s: r.get::<_, Option<f64>>("duration_s")?.unwrap_or(0.0),
        fingerprint: r.get("fingerprint")?,
        mbid: r.get("acoustid_recording_mbid")?,
        fingerprint_impl: r.get("fingerprint_impl")?,
    })
}

impl Check for DuplicateTracks {
    fn meta(&self) -> &CheckMeta {
        &META
    }

    /// Present albums of the scope's library only (deliberate change:
    /// the Python app's query had no `library_path` condition and
    /// clustered other libraries' albums too). Missing albums are skipped.
    fn run(
        &self,
        ctx: &CheckCtx<'_>,
        scope: &CheckScope,
        emit: &mut Emit<'_>,
    ) -> Result<(), CheckError> {
        let rows: Vec<(String, FpTrack)> = {
            let mut stmt = ctx.conn.prepare(
                "SELECT tf.album_id, tf.filename, tf.duration_s, tf.fingerprint, \
                        tf.acoustid_recording_mbid, tf.fingerprint_impl \
                 FROM track_fingerprints tf JOIN albums a ON a.id = tf.album_id \
                 WHERE tf.fingerprint IS NOT NULL AND a.missing_since IS NULL \
                   AND a.library_path = ?1 \
                   AND (?2 IS NULL OR tf.album_id IN (SELECT value FROM json_each(?2))) \
                 ORDER BY tf.album_id, tf.filename",
            )?;
            // Duplicates are clustered within one album, so an album-scoped
            // run needs only its albums' fingerprints (the Python app loaded
            // and clustered every album's).
            let only = scope.album_ids_json();
            stmt.query_map(rusqlite::params![scope.library_path, only], |r| {
                Ok((r.get(0)?, fp_track(r)?))
            })?
            .collect::<rusqlite::Result<_>>()?
        };
        let mut albums: Vec<(String, Vec<FpTrack>)> = Vec::new();
        for (album_id, t) in rows {
            match albums.last_mut() {
                Some((a, ts)) if *a == album_id => ts.push(t),
                _ => albums.push((album_id, vec![t])),
            }
        }
        for (album_id, tracks) in &albums {
            ctx.checkpoint()?;
            if tracks.len() < 2 {
                continue;
            }
            for cluster in cluster_album(tracks) {
                emit(build_finding(album_id, &cluster))?;
            }
        }
        Ok(())
    }
}

/// `derive_cluster(album_id, original_filenames=...)`: re-cluster the album's
/// current fingerprints and return the cluster that overlaps the finding's
/// original filenames most (ties: the larger cluster, then the first).
/// `None` when no cluster remains, or when the original list was non-empty
/// and no current cluster shares a file with it.
pub fn derive_cluster(
    conn: &Connection,
    album_id: &str,
    original_filenames: &[String],
) -> rusqlite::Result<Option<Cluster>> {
    let tracks: Vec<FpTrack> = {
        let mut stmt = conn.prepare(
            "SELECT album_id, filename, duration_s, fingerprint, acoustid_recording_mbid, fingerprint_impl \
             FROM track_fingerprints WHERE album_id = ?1 AND fingerprint IS NOT NULL ORDER BY filename",
        )?;
        stmt.query_map([album_id], fp_track)?
            .collect::<rusqlite::Result<_>>()?
    };
    if tracks.is_empty() {
        return Ok(None);
    }
    let clusters = cluster_album(&tracks);
    let original: std::collections::HashSet<&str> =
        original_filenames.iter().map(String::as_str).collect();
    let mut best: Option<(&Vec<&FpTrack>, usize)> = None;
    for c in &clusters {
        let overlap = c
            .iter()
            .filter(|t| original.contains(t.filename.as_str()))
            .count();
        let better = match best {
            None => true,
            Some((b, bo)) => overlap > bo || (overlap == bo && c.len() > b.len()),
        };
        if better {
            best = Some((c, overlap));
        }
    }
    let Some((cluster, overlap)) = best else {
        return Ok(None);
    };
    if cluster.len() < 2 || (!original.is_empty() && overlap == 0) {
        return Ok(None);
    }
    Ok(Some(Cluster::from_tracks(cluster)))
}

/// An HTTP-shaped refusal from a duplicate-tracks endpoint body.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{status}: {detail}")]
pub struct HttpRefusal {
    pub status: u16,
    pub detail: String,
}

fn refuse(status: u16, detail: impl Into<String>) -> HttpRefusal {
    HttpRefusal {
        status,
        detail: detail.into(),
    }
}

fn stored_detail(f: &FindingRow) -> Value {
    f.detail_json
        .as_deref()
        .and_then(|d| serde_json::from_str::<Value>(d).ok())
        .filter(|v| !v.is_null())
        .unwrap_or_else(|| json!({}))
}

fn str_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The body of `GET /api/checks/duplicate-tracks/findings/{id}/cluster-preview`.
pub fn cluster_preview(
    conn: &Connection,
    finding: Option<&FindingRow>,
) -> Result<Value, HttpRefusal> {
    let f = finding
        .filter(|f| f.check_id == META.id)
        .ok_or_else(|| refuse(404, "Finding not found"))?;
    let album_id = f
        .album_id
        .as_deref()
        .filter(|a| !a.is_empty())
        .ok_or_else(|| refuse(400, "Finding has no album"))?;
    let detail = stored_detail(f);
    let original = str_list(detail.get("filenames"));
    let cluster =
        derive_cluster(conn, album_id, &original).map_err(|e| refuse(500, e.to_string()))?;
    Ok(match cluster {
        None => json!({
            "album_id": album_id,
            "is_empty": true,
            "recording_mbid": detail.get("recording_mbid").cloned().unwrap_or(Value::Null),
            "filenames": [],
            "durations_s": [],
            "duplicate_count": 0,
            "recommended_keep": null,
            "recommended_reason": null,
            "removed_filenames": original,
        }),
        Some(c) => {
            let removed: Vec<&String> = original
                .iter()
                .filter(|f| !c.filenames.contains(f))
                .collect();
            let mut body = c.to_pyjson().to_value();
            let o = body.as_object_mut().expect("cluster is an object");
            o.insert("album_id".into(), json!(album_id));
            o.insert("is_empty".into(), json!(false));
            o.insert("removed_filenames".into(), json!(removed));
            body
        }
    })
}

/// The precondition the delete-duplicate
/// endpoint must check before it touches anything (disk, fingerprint rows,
/// the album row, a rescan). `album_path` is the album row's `path`.
/// Refuses with 409 when the directory does not exist, is not a directory,
/// or cannot be listed (an unmounted drive, a permission flicker); returns
/// the canonical directory otherwise. The Python app took such an album for
/// "file already gone", dropped the fingerprint row, and its rescan then
/// deleted the whole album row (musaic/api/health.py:531-575).
pub fn require_reachable_album_dir(album_path: &Path) -> Result<PathBuf, HttpRefusal> {
    let unreachable = |why: &dyn std::fmt::Display| {
        refuse(
            409,
            format!(
                "Album directory {} cannot be reached ({why}); nothing was deleted. \
                 Reconnect the drive and try again.",
                musaic_fsops::py_repr(&album_path.to_string_lossy())
            ),
        )
    };
    let dir = album_path.canonicalize().map_err(|e| unreachable(&e))?;
    // Also fails for a path that is not a directory.
    std::fs::read_dir(&dir).map_err(|e| unreachable(&e))?;
    Ok(dir)
}

/// What [`trash_duplicate_file`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashedDuplicate {
    /// `None` when the file was already gone (the click finishes the cleanup).
    pub trashed_to: Option<PathBuf>,
    /// The `track_fingerprints` row was removed.
    pub fingerprint_row_deleted: bool,
    pub message: String,
}

/// The deterministic part of `POST .../delete-file`: validate that
/// `filename` is one of the finding's duplicates and lies inside the album
/// directory, move it to the trash and drop its fingerprint row.
///
/// The album must belong to `library_path` (the configured library, as
/// the Python app's `cached_album_dict` requires), Missing or not.
///
/// The album directory must be reachable ([`require_reachable_album_dir`],
/// otherwise 409, and nothing is moved or deleted (no fingerprint
/// row either). With the directory present, a file that is no longer there
/// is the Python app's idempotent cleanup: its fingerprint row is dropped so
/// the finding can clear (health.py:531-560). Any other failure to look the
/// file up is a 500, as in the Python app (only `FileNotFoundError` was caught).
///
/// Not done here (the API layer's job, see the crate docs): rescanning the
/// album, upserting or deleting its row, publishing the event.
pub fn trash_duplicate_file(
    conn: &Connection,
    library_path: &str,
    trash_root: &Path,
    finding: Option<&FindingRow>,
    filename: &str,
    stamp: impl FnOnce() -> String,
) -> Result<TrashedDuplicate, HttpRefusal> {
    let f = finding
        .filter(|f| f.check_id == META.id)
        .ok_or_else(|| refuse(404, "Finding not found"))?;
    let listed = str_list(stored_detail(f).get("filenames"));
    if !listed.iter().any(|x| x == filename) {
        return Err(refuse(
            400,
            format!(
                "Filename {} is not one of this finding's duplicates",
                musaic_fsops::py_repr(filename)
            ),
        ));
    }
    let album_id = f
        .album_id
        .as_deref()
        .filter(|a| !a.is_empty())
        .ok_or_else(|| refuse(400, "Finding has no album to act on"))?;
    let row: Option<(String, Option<String>)> = conn
        .query_row(
            "SELECT path, dir_name FROM albums WHERE id = ?1 AND library_path = ?2",
            [album_id, library_path],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| refuse(500, e.to_string()))?;
    let (path, dir_name) = row.ok_or_else(|| {
        refuse(
            404,
            format!("Album {} not in cache", musaic_fsops::py_repr(album_id)),
        )
    })?;
    let album_path = PathBuf::from(&path);
    let a = require_reachable_album_dir(&album_path)?;
    let target = album_path.join(filename);
    let trashed_to = match target.canonicalize() {
        Ok(t) => {
            if !t.ancestors().skip(1).any(|p| p == a) {
                return Err(refuse(
                    400,
                    "Refusing to delete file outside album directory",
                ));
            }
            let moved =
                musaic_fsops::trash::send_to_trash(trash_root, &t, dir_name.as_deref(), stamp)
                    .map_err(|e| refuse(500, e.to_string()))?;
            Some(moved)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(refuse(500, e.to_string())),
    };
    let deleted = conn
        .execute(
            "DELETE FROM track_fingerprints WHERE album_id = ?1 AND filename = ?2",
            [album_id, filename],
        )
        .map_err(|e| refuse(500, e.to_string()))?
        > 0;
    let message = match &trashed_to {
        Some(_) => format!("Moved {filename} to trash"),
        None => format!(
            "{filename} was already gone from disk — cleaned up the cache so this duplicate finding will clear."
        ),
    };
    Ok(TrashedDuplicate {
        trashed_to,
        fingerprint_row_deleted: deleted,
        message,
    })
}
