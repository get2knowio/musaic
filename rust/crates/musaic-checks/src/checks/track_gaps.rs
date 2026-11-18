//! `track-gaps` (port of `checks/track_gaps.py`): gaps and repeats in an
//! album's track numbers, from `scope.albums` (Missing albums never reach
//! it). Also the two deterministic renumber plans of the fix cascade, as
//! pure functions over a MusicBrainz tracklist the caller fetched.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

use musaic_core::pycompat::{is_py_space, strip};
use musaic_scan::{flacmeta, list_flacs, parse_tracknum};

use crate::base::{
    Check, CheckCtx, CheckError, CheckMeta, CheckScope, Cost, Emit, Finding, FixKind, Severity,
};
use crate::pyjson::{PyJson, dict};

pub struct TrackGaps;

pub static META: CheckMeta = CheckMeta {
    id: "track-gaps",
    label: "Track-number gaps & duplicates",
    description: "Albums whose track-number sequence has gaps (1, 2, 4) or repeats (1, 2, 2). Detected \
from tag inspection — fingerprints not required to detect, but Fix uses them when available. Cascade: MB \
title match → AcoustID fingerprint match → AI. Note: this is about the TAG; the duplicate-tracks check \
catches duplicate AUDIO regardless of tag.",
    category: "metadata",
    cost: Cost::Cheap,
    runs_during_scan: true,
    fix_kind: FixKind::AiJob,
    bulk_action_label: Some("Try MusicBrainz on all"),
};

fn join(v: &[i64]) -> String {
    v.iter().map(i64::to_string).collect::<Vec<_>>().join(", ")
}

fn ints(v: &[i64]) -> PyJson {
    PyJson::List(v.iter().map(|&i| PyJson::Int(i)).collect())
}

/// The findings for one album's track numbers (gap finding first).
pub fn findings_for(
    album_id: &str,
    track_numbers: &[Option<i64>],
    track_count: i64,
) -> Vec<Finding> {
    findings_on_disc(album_id, None, track_numbers, track_count)
}

/// [`findings_for`] for the tracks of one disc of a multi-disc album: the
/// disc is named in the summary (a finding's identity is its summary, so two
/// discs with the same problem stay two findings) and recorded in the detail.
fn findings_on_disc(
    album_id: &str,
    disc: Option<i64>,
    track_numbers: &[Option<i64>],
    track_count: i64,
) -> Vec<Finding> {
    let mut present: Vec<i64> = track_numbers.iter().flatten().copied().collect();
    present.sort_unstable();
    if present.len() < 2 {
        return Vec::new();
    }
    let on_disc = disc.map(|d| format!(" on disc {d}")).unwrap_or_default();
    let with_disc = |d: PyJson| {
        let PyJson::Dict(mut items) = d else {
            return Some(d);
        };
        if let Some(disc) = disc {
            items.push(("disc".to_string(), PyJson::Int(disc)));
        }
        Some(PyJson::Dict(items))
    };
    let unique: Vec<i64> = present
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut missing = Vec::new();
    for w in unique.windows(2) {
        if w[1] - w[0] > 1 {
            missing.extend(w[0] + 1..w[1]);
        }
    }
    let mut out = Vec::new();
    if !missing.is_empty() {
        out.push(Finding {
            album_id: Some(album_id.to_string()),
            summary: format!("Track-number gaps{on_disc}: missing {}", join(&missing)),
            severity: Severity::Warn,
            detail: with_disc(dict([
                ("present", ints(&unique)),
                ("missing", ints(&missing)),
                ("expected_total", PyJson::Int(track_count)),
            ])),
        });
    }
    let mut counts: BTreeMap<i64, usize> = BTreeMap::new();
    for &n in &present {
        *counts.entry(n).or_default() += 1;
    }
    let dups: Vec<i64> = counts
        .into_iter()
        .filter(|&(_, c)| c > 1)
        .map(|(n, _)| n)
        .collect();
    if !dups.is_empty() {
        out.push(Finding {
            album_id: Some(album_id.to_string()),
            summary: format!("Duplicate track numbers{on_disc}: {}", join(&dups)),
            severity: Severity::Error,
            detail: with_disc(dict([
                ("present", ints(&present)),
                ("duplicates", ints(&dups)),
            ])),
        });
    }
    out
}

/// Disc number of a file (see [`musaic_scan::flacmeta::file_disc`]).
fn disc_of(meta: &flacmeta::FlacMeta) -> Option<i64> {
    flacmeta::file_disc(meta.values("discnumber"), meta.values("tracknumber"))
}

/// Track numbers by disc number.
type DiscTracks = BTreeMap<i64, Vec<Option<i64>>>;

/// The track numbers of an album's files grouped by disc, read from the
/// files. `Ok(None)`: no file names a disc (judge the album as one, from
/// what the scan stored), or the folder lists no FLAC (nothing to read).
/// A file that names no disc in an album whose other files do counts as disc
/// 1, as the scanner's track order does. `Err`: a file could not be read.
fn discs_on_disk(album_dir: &Path) -> Result<Option<DiscTracks>, ()> {
    let mut files: Vec<(Option<i64>, Option<i64>)> = Vec::new();
    for flac in list_flacs(album_dir) {
        let meta = flacmeta::read(&flac).map_err(|_| ())?;
        let track = meta
            .values("tracknumber")
            .map(strip)
            .find(|v| !v.is_empty());
        files.push((disc_of(&meta), parse_tracknum(track)));
    }
    if files.iter().all(|(disc, _)| disc.is_none()) {
        return Ok(None);
    }
    let mut discs: BTreeMap<i64, Vec<Option<i64>>> = BTreeMap::new();
    for (disc, track) in files {
        discs.entry(disc.unwrap_or(1)).or_default().push(track);
    }
    Ok(Some(discs))
}

/// Whether the album's files name more than one disc. An unreadable folder
/// is "no".
pub fn spans_multiple_discs(album_dir: &Path) -> bool {
    matches!(discs_on_disk(album_dir), Ok(Some(d)) if d.len() > 1)
}

impl Check for TrackGaps {
    fn meta(&self) -> &CheckMeta {
        &META
    }

    fn run(
        &self,
        ctx: &CheckCtx<'_>,
        scope: &CheckScope,
        emit: &mut Emit<'_>,
    ) -> Result<(), CheckError> {
        for album in &scope.albums {
            let whole = findings_for(&album.id, &album.track_numbers, album.track_count);
            if whole.is_empty() {
                // Judging per disc turns repeats into nothing, so the files
                // are read only for albums that already have a finding: one
                // tag read per file of those albums rather than of the whole
                // library (this runs after every scan, often over a network
                // share). Numbers that interleave across discs (disc 1 has
                // 1 and 3, disc 2 has 2) are not looked at.
                continue;
            }
            match discs_on_disk(Path::new(&album.path)) {
                Ok(Some(discs)) if discs.len() > 1 => {
                    for (disc, numbers) in &discs {
                        let count = numbers.len() as i64;
                        for f in findings_on_disc(&album.id, Some(*disc), numbers, count) {
                            emit(f)?;
                        }
                    }
                }
                Ok(_) => {
                    for f in whole {
                        emit(f)?;
                    }
                }
                Err(()) => ctx.could_not_evaluate(&album.id),
            }
        }
        Ok(())
    }
}

/// One MusicBrainz tracklist entry, as the fix cascade reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MbTrack {
    pub title: String,
    pub position: i64,
    pub recording_mbid: Option<String>,
}

/// `_norm`: case-fold, collapse whitespace.
fn norm(s: &str) -> String {
    strip(s)
        .to_lowercase()
        .split(is_py_space)
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Tier 1, `_match_local_to_mb`: map every local FLAC (shared listing) to a
/// canonical position by exact normalised title. `None` on any ambiguity:
/// a blank or repeated MB title, an unreadable file, a title MB lacks, two
/// files on one position, or no files at all.
pub fn match_local_to_mb(album_dir: &Path, mb_tracks: &[MbTrack]) -> Option<BTreeMap<String, i64>> {
    if !album_dir.is_dir() {
        return None;
    }
    let mut by_title: HashMap<String, i64> = HashMap::new();
    let mut duplicate = false;
    for t in mb_tracks {
        let key = norm(&t.title);
        if key.is_empty() {
            return None;
        }
        if by_title.insert(key, t.position).is_some() {
            duplicate = true;
        }
    }
    if duplicate {
        return None;
    }
    let mut used = HashSet::new();
    let mut out = BTreeMap::new();
    for flac in list_flacs(album_dir) {
        let meta = flacmeta::read(&flac).ok()?;
        let key = norm(meta.values("title").next().unwrap_or(""));
        let position = *by_title.get(&key).filter(|_| !key.is_empty())?;
        if !used.insert(position) {
            return None;
        }
        let name = flac.file_name()?.to_string_lossy().into_owned();
        out.insert(name, position);
    }
    (!out.is_empty()).then_some(out)
}

/// Tier 2, `_fingerprint_renumbering` after fingerprinting: map each local
/// file's AcoustID recording to its MB position. `None` when MB repeats a
/// recording or has none, a file has no recording or one MB lacks, or two
/// files land on one position. `local` is `(filename, recording_mbid)` in the
/// fingerprinter's order.
pub fn fingerprint_renumbering(
    mb_tracks: &[MbTrack],
    local: &[(String, Option<String>)],
) -> Option<BTreeMap<String, i64>> {
    let mut by_recording: HashMap<&str, i64> = HashMap::new();
    for t in mb_tracks {
        let Some(r) = t.recording_mbid.as_deref() else {
            continue;
        };
        if by_recording.insert(r, t.position).is_some() {
            return None;
        }
    }
    if by_recording.is_empty() {
        return None;
    }
    let mut used = HashSet::new();
    let mut out = BTreeMap::new();
    for (filename, recording) in local {
        let position = *by_recording.get(recording.as_deref()?)?;
        if !used.insert(position) {
            return None;
        }
        out.insert(filename.clone(), position);
    }
    (!out.is_empty()).then_some(out)
}
