//! What the agent read tools show, built from the library snapshot and the
//! files without writing anything, taking a guard or touching the network
//! (FR-053). Outputs carry display fields and bare file or folder names, never
//! a path.
//!
//! The album's `revision` is the digest an approval is bound to (FR-051): it
//! changes when the stored tags, the cover, the set of files, or any file's
//! name, size or modification time changes.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use musaic_commands::auth::hex_encode;
use musaic_commands::tools::album::{AlbumDetail, AlbumGetOutput, Track};
use musaic_commands::tools::findings::{Finding, FindingsListOutput};
use musaic_commands::tools::library::{AlbumSummary, LibraryListOutput};
use musaic_commands::tools::resolution::{
    ResolutionGetOutput, ResolutionStatus, ResolutionTrack, ResolvedField,
};
use musaic_commands::{ToolError, ToolErrorCode};
use musaic_resolve::AlbumResolution;
use musaic_tags::apply::{FIELD_TO_VORBIS, TRACK_FIELD_TO_VORBIS};
use rusqlite::Connection;
use sha2::{Digest, Sha256};

use crate::albums::AlbumRow;
use crate::json::Json;
use crate::library_ops;

fn s<'a>(j: &'a Json, key: &str) -> Option<&'a str> {
    j.get(key).and_then(Json::as_str).filter(|v| !v.is_empty())
}

fn flag(j: &Json, key: &str) -> bool {
    match j.get(key) {
        Some(Json::Bool(b)) => *b,
        Some(Json::Int(n)) => *n != 0,
        _ => false,
    }
}

fn count(j: &Json, key: &str) -> u32 {
    match j.get(key) {
        Some(Json::Int(n)) => (*n).max(0) as u32,
        _ => 0,
    }
}

fn no_library() -> ToolError {
    ToolError::new(ToolErrorCode::NoLibrary, "no library folder is configured")
}

fn internal(e: impl std::fmt::Display) -> ToolError {
    musaic_log::error("musaic.agent", &format!("agent read failed: {e}"));
    ToolError::new(ToolErrorCode::Internal, "the library could not be read")
}

fn artist_of(j: &Json) -> String {
    musaic_core::album_fields::display_artist(
        s(j, "album_artist"),
        s(j, "artist"),
        s(j, "dir_artist"),
        true,
    )
}

fn summary(j: &Json) -> AlbumSummary {
    AlbumSummary {
        album_id: s(j, "id").unwrap_or_default().to_string(),
        artist: artist_of(j),
        title: s(j, "title")
            .or_else(|| s(j, "dir_album"))
            .or_else(|| s(j, "dir_name"))
            .unwrap_or_default()
            .to_string(),
        year: s(j, "year").map(str::to_string),
        track_count: count(j, "track_count"),
        missing: s(j, "missing_since").is_some(),
        has_cover: flag(j, "has_embedded_cover") || s(j, "cover_file").is_some(),
    }
}

// ----------------------------------------------------------------- paging

/// The opaque cursor: where to continue and which snapshot it belongs to.
pub fn encode_cursor(offset: usize, digest: &str) -> String {
    hex_encode(format!("{offset}:{digest}").as_bytes())
}

/// `Err(CURSOR_STALE)` when the cursor is not one of ours or the snapshot
/// changed since it was made.
pub fn decode_cursor(cursor: &str, digest: &str) -> Result<usize, ToolError> {
    let stale = || {
        ToolError::new(
            ToolErrorCode::CursorStale,
            "the list changed; start again without a cursor",
        )
    };
    let bytes = musaic_commands::auth::hex_decode(cursor).ok_or_else(stale)?;
    let text = String::from_utf8(bytes).map_err(|_| stale())?;
    let (offset, d) = text.split_once(':').ok_or_else(stale)?;
    if d != digest {
        return Err(stale());
    }
    offset.parse().map_err(|_| stale())
}

fn digest_of<'a>(parts: impl IntoIterator<Item = &'a str>) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update([0]);
    }
    hex_encode(&h.finalize()[..8])
}

// ------------------------------------------------------------- the library

/// A page of the library's albums, in the order the desktop lists them.
pub fn list_albums_page(
    conn: &mut Connection,
    library_dir: Option<&str>,
    cursor: Option<&str>,
    limit: u32,
    query: Option<&str>,
) -> Result<LibraryListOutput, ToolError> {
    let lib = library_dir
        .filter(|l| !l.is_empty())
        .ok_or_else(no_library)?;
    let listing = library_ops::list_albums(conn, lib, false).map_err(|e| internal(e.message()))?;
    let needle = query
        .map(|q| q.trim().to_lowercase())
        .filter(|q| !q.is_empty());
    let mut items: Vec<AlbumSummary> = listing.albums.iter().map(summary).collect();
    if let Some(n) = &needle {
        items.retain(|a| a.artist.to_lowercase().contains(n) || a.title.to_lowercase().contains(n));
    }
    let digest = digest_of(
        std::iter::once(listing.scanned_at.as_str())
            .chain(needle.as_deref())
            .chain(
                items
                    .iter()
                    .flat_map(|a| [a.album_id.as_str(), a.title.as_str(), a.artist.as_str()]),
            ),
    );
    let offset = match cursor {
        Some(c) => decode_cursor(c, &digest)?,
        None => 0,
    };
    let total = items.len();
    let page: Vec<AlbumSummary> = items
        .into_iter()
        .skip(offset)
        .take(limit as usize)
        .collect();
    let end = offset + page.len();
    Ok(LibraryListOutput {
        items: page,
        next_cursor: (end < total).then(|| encode_cursor(end, &digest)),
        total: total as u32,
    })
}

/// The album's cached row, which must belong to the configured library.
/// Never walks the disk.
pub fn cached_row(
    conn: &Connection,
    library_dir: Option<&str>,
    album_id: &str,
) -> Result<AlbumRow, ToolError> {
    if library_dir.is_none_or(str::is_empty) {
        return Err(no_library());
    }
    crate::sources::cached_album(conn, library_dir, album_id)
        .map_err(internal)?
        .ok_or_else(|| ToolError::new(ToolErrorCode::NotFound, "no such album"))
}

fn mtime_nanos(meta: &std::fs::Metadata) -> u128 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos())
}

/// The FLAC files of an album folder: listed name, size and modification
/// time in nanoseconds, sorted by name.
pub fn file_facts(album_dir: &str) -> Vec<(String, u64, u128)> {
    let mut out: Vec<(String, u64, u128)> = musaic_scan::list_flacs(Path::new(album_dir))
        .into_iter()
        .filter_map(|p| {
            let name = p.file_name()?.to_string_lossy().into_owned();
            let meta = std::fs::metadata(&p).ok()?;
            Some((name, meta.len(), mtime_nanos(&meta)))
        })
        .collect();
    out.sort();
    out
}

/// The revision (FR-051, research R7): the first 32 hex characters of SHA-256
/// over the album row's fields (without the scan time), the cover's presence
/// and the sidecar's hash, and each FLAC's `(name, size, mtime ns)`. Computed
/// on every read, never stored.
pub fn revision_of(row: &AlbumRow) -> String {
    let mut doc = row.json.clone();
    if let Json::Obj(pairs) = &mut doc {
        pairs.retain(|(k, _)| k != "scanned_at");
    }
    let mut h = Sha256::new();
    h.update(doc.dumps().as_bytes());
    h.update(b"\0cover\0");
    h.update([row.album.has_embedded_cover as u8]);
    let sidecar = row
        .album
        .cover_file
        .as_deref()
        .filter(|c| !c.is_empty())
        .and_then(|c| std::fs::read(Path::new(&row.album.path).join(c)).ok());
    match sidecar {
        Some(bytes) => h.update(Sha256::digest(&bytes)),
        None => h.update(b"none"),
    }
    h.update(b"\0files\0");
    for (name, size, mtime) in file_facts(&row.album.path) {
        h.update(format!("{name}\0{size}\0{mtime}\n").as_bytes());
    }
    hex_encode(&h.finalize()[..16])
}

fn int_of(raw: &str) -> Option<u32> {
    raw.split('/').next()?.trim().parse().ok()
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// One album with its tracks and revision (`musaic_album_get`).
pub fn album_get(
    conn: &Connection,
    library_dir: Option<&str>,
    album_id: &str,
) -> Result<AlbumGetOutput, ToolError> {
    let row = cached_row(conn, library_dir, album_id)?;
    let j = &row.json;
    let mut tags = BTreeMap::new();
    for (field, _) in FIELD_TO_VORBIS {
        if let Some(v) = s(j, field) {
            tags.insert(field.to_string(), v.to_string());
        }
    }
    let mut tracks = Vec::new();
    for p in musaic_scan::list_flacs(Path::new(&row.album.path)) {
        let file_name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Ok(meta) = musaic_scan::flacmeta::read(&p) else {
            tracks.push(Track {
                file_name,
                track_number: None,
                disc_number: None,
                title: None,
                artist: None,
                duration_seconds: None,
                tags: BTreeMap::new(),
            });
            continue;
        };
        let first = |k: &str| {
            meta.values(k)
                .next()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };
        let mut t = BTreeMap::new();
        for (field, vorbis) in TRACK_FIELD_TO_VORBIS {
            if let Some(v) = first(vorbis) {
                t.insert(field.to_string(), v);
            }
        }
        tracks.push(Track {
            file_name,
            track_number: first("tracknumber").as_deref().and_then(int_of),
            disc_number: first("discnumber").as_deref().and_then(int_of),
            title: first("title"),
            artist: first("artist"),
            duration_seconds: Some(round1(meta.info.length())),
            tags: t,
        });
    }
    let sum = summary(j);
    let disc_count = row
        .album
        .tag_disc_total
        .or(row.album.disc_total)
        .map_or(1, |n| n.max(1) as u32);
    Ok(AlbumGetOutput {
        album: AlbumDetail {
            album_id: sum.album_id,
            artist: sum.artist,
            title: sum.title,
            year: sum.year,
            track_count: sum.track_count,
            missing: sum.missing,
            has_cover: sum.has_cover,
            folder_name: row.album.dir_name.clone(),
            tags,
            disc_count,
        },
        tracks,
        revision: revision_of(&row),
    })
}

// ------------------------------------------------------------- resolution

fn fact(f: &Option<musaic_resolve::FieldFact>) -> Option<ResolvedField> {
    f.as_ref().map(|f| ResolvedField {
        value: f.value.clone(),
        source: f.source.clone(),
        confidence: f.confidence,
    })
}

/// The stored resolution of an album, or `not_resolved`. `stored` is what the
/// resolver's memo holds for the album's current content
/// ([`musaic_resolve::Resolver::memoized`]); nothing is resolved here.
pub fn resolution_output(stored: Option<Arc<AlbumResolution>>) -> ResolutionGetOutput {
    let Some(r) = stored.filter(|r| r.anchor.is_some() || r.canonical_title.is_some()) else {
        return ResolutionGetOutput {
            status: ResolutionStatus::NotResolved,
            anchor: None,
            strategy: None,
            fields: BTreeMap::new(),
            cover_candidate: false,
            tracks: Vec::new(),
            notes: vec![
                "No resolution is stored for this album yet. Open the album in Musaic to resolve it.".into(),
            ],
        };
    };
    let mut fields = BTreeMap::new();
    for (name, f) in [
        ("title", &r.canonical_title),
        ("album_artist", &r.canonical_album_artist),
        ("year", &r.canonical_year),
        ("album_artist_sort", &r.canonical_album_artist_sort),
        ("genre", &r.canonical_genre),
    ] {
        if let Some(f) = fact(f) {
            fields.insert(name.to_string(), f);
        }
    }
    ResolutionGetOutput {
        status: ResolutionStatus::Resolved,
        anchor: r.anchor.as_ref().map(|a| a.value.clone()),
        strategy: Some(r.anchor_strategy.clone()),
        fields,
        cover_candidate: r.cover_url.is_some(),
        tracks: r
            .tracks
            .iter()
            .map(|t| ResolutionTrack {
                file_name: t.filename.clone(),
                recording_mbid: t.recording_mbid.clone(),
            })
            .collect(),
        notes: r.notes.clone(),
    }
}

// --------------------------------------------------------------- findings

/// `text` with the folder paths it names replaced, so a finding's summary
/// never shows where the library lives.
fn without_paths(text: &str, paths: &[&str]) -> String {
    let mut out = text.to_string();
    for p in paths.iter().filter(|p| p.len() > 1) {
        out = out.replace(p, "<folder>");
    }
    out
}

/// A page of open findings, by reference. The check-specific detail is left
/// out because it can hold paths.
pub fn findings_page(
    conn: &Connection,
    library_dir: Option<&str>,
    album_id: Option<&str>,
    cursor: Option<&str>,
    limit: u32,
) -> Result<FindingsListOutput, ToolError> {
    let album_path = match album_id {
        Some(id) => Some(cached_row(conn, library_dir, id)?.album.path),
        None => None,
    };
    let rows = musaic_db::findings::list_findings(
        conn,
        &musaic_db::findings::FindingFilter {
            album_id,
            ..Default::default()
        },
    )
    .map_err(internal)?;
    let id_texts: Vec<String> = rows.iter().map(|r| r.id.to_string()).collect();
    let digest = digest_of(id_texts.iter().map(String::as_str));
    let offset = match cursor {
        Some(c) => decode_cursor(c, &digest)?,
        None => 0,
    };
    let lib = library_dir.unwrap_or_default();
    // Album paths are stored resolved (symlinks followed), so a summary names
    // the resolved library folder even when the setting names a link to it.
    let lib_resolved = (!lib.is_empty()).then(|| {
        musaic_scan::py_resolve(Path::new(lib))
            .to_string_lossy()
            .into_owned()
    });
    let mut hide: Vec<&str> = std::iter::once(lib)
        .chain(lib_resolved.as_deref())
        .chain(album_path.as_deref())
        .collect();
    // Longest first, so the album's folder is hidden whole before the
    // library folder it starts with.
    hide.sort_by_key(|p| std::cmp::Reverse(p.len()));
    let total = rows.len();
    let items: Vec<Finding> = rows
        .into_iter()
        .skip(offset)
        .take(limit as usize)
        .map(|r| Finding {
            finding_id: r.id.to_string(),
            check_id: r.check_id,
            album_id: r.album_id,
            severity: r.severity,
            title: without_paths(&r.summary, &hide),
            status: "open".into(),
            first_seen: r.first_seen_at,
            last_seen: r.last_seen_at,
        })
        .collect();
    let end = offset + items.len();
    Ok(FindingsListOutput {
        items,
        next_cursor: (end < total).then(|| encode_cursor(end, &digest)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_round_trip_and_go_stale() {
        let c = encode_cursor(50, "abc");
        assert_eq!(decode_cursor(&c, "abc").unwrap(), 50);
        assert_eq!(
            decode_cursor(&c, "abd").unwrap_err().code,
            ToolErrorCode::CursorStale
        );
        assert_eq!(
            decode_cursor("zz", "abc").unwrap_err().code,
            ToolErrorCode::CursorStale
        );
        assert_eq!(
            decode_cursor("", "abc").unwrap_err().code,
            ToolErrorCode::CursorStale
        );
    }

    #[test]
    fn paths_are_removed_from_summaries() {
        assert_eq!(
            without_paths(
                "Track gaps in /music/Artist - Album [FLAC]",
                &["/music/Artist - Album [FLAC]", "/music"]
            ),
            "Track gaps in <folder>"
        );
        assert_eq!(
            without_paths("AC/DC", &["/"]),
            "AC/DC",
            "a bare root is not hidden"
        );
    }

    #[test]
    fn an_unresolved_album_says_so() {
        let o = resolution_output(None);
        assert_eq!(o.status, ResolutionStatus::NotResolved);
        assert!(o.fields.is_empty() && !o.cover_candidate);
        let empty = Arc::new(AlbumResolution::new("a"));
        assert_eq!(
            resolution_output(Some(empty)).status,
            ResolutionStatus::NotResolved
        );
    }
}
