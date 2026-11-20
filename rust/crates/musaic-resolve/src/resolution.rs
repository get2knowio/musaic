//! `AlbumResolution` and its `to_dict()`, plus Tier 1 alignment
//! (`_refine_from_anchor`).
//!
//! The serialised form is the body of `GET /api/albums/{id}/resolution`:
//! same keys, same nesting, floats where the Python app has floats. Golden:
//! `testdata/golden/resolver/refine.json` and `cascade.json`.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::Serialize;

use crate::find_release::{MbMatch, MbTrack};
use crate::source::FlacTrack;

/// Where a fact came from (`resolver.Source`).
pub mod sources {
    pub const CACHE: &str = "cache";
    pub const MUSICBRAINZ: &str = "musicbrainz";
    pub const FINGERPRINT: &str = "fingerprint";
    pub const FINGERPRINT_MB: &str = "fingerprint+mb";
}

/// Tier 1 alignment tolerance in seconds.
pub const ALIGN_TOLERANCE_S: f64 = 2.0;

/// Deliberate change from the Python app: the largest |FLAC duration −
/// MusicBrainz length| allowed at EVERY position for the positional
/// fallback ([`positional_alignment`]).
///
/// Chosen by measuring real albums against recorded MusicBrainz listings: the
/// largest difference on a right, count-matched pairing is 8.1 s (a Ray
/// Conniff album, both releases; 8.07 s on exact lengths); the closest wrong pairing whose differences all
/// lean the same way ([`POSITIONAL_SIGN_DEADBAND_S`]) is 17.2 s; the closest
/// wrong pairing from real albums against recorded lengths is 18.2 s. 10 s
/// sits above the first with ~2 s to spare and well below the others. In the
/// per-track supplement (4518 fingerprint-matched tracks, recording lengths),
/// 0.27 % of right tracks differ by more than 10 s.
///
/// Confirmed on 271 albums against 293 release listings (album lengths in
/// whole seconds): with
/// the direction guard the same 3 of 864,949 wrong pairings are accepted at
/// every bound from 2 s to 20 s, and 83 of 390 right pairings are rejected
/// at 10 s (75 at 20 s, 128 at 2 s).
pub const POSITIONAL_MAX_DIFF_S: f64 = 10.0;

/// Decided with [`POSITIONAL_MAX_DIFF_S`]: every
/// position whose difference exceeds this many seconds must differ in the same
/// direction (all FLACs longer, or all shorter) — a pressing that runs
/// uniformly long or short, as on the measured Ray Conniff album (+2.7 to +8.1 s). It is
/// the per-track tolerance [`ALIGN_TOLERANCE_S`]; differences inside it are
/// agreement and carry no direction. Without this guard an absolute bound does
/// not separate the measured data: a wrong pairing at 8.0 s (mixed directions)
/// lies below the right one at 8.1 s. In the larger measurement, wrong
/// pairings accepted without the guard grow from 4 at 10 s to 37 at 20 s.
pub const POSITIONAL_SIGN_DEADBAND_S: f64 = ALIGN_TOLERANCE_S;

/// The `confidence` of the per-track canonical facts of a positional match
/// against 0.95 for a (tracknumber, 2 s) or unique-duration match.
/// It is a marker, not a measured probability: the Python app's per-track shape
/// has no "how was this aligned" field, and no consumer reads per-track
/// confidence, so this carries the distinction without changing the JSON shape.
pub const POSITIONAL_CONFIDENCE: f64 = 0.8;

/// `FieldFact`. Every value the resolver produces is a string.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FieldFact {
    pub value: String,
    pub source: String,
    pub confidence: f64,
}

impl FieldFact {
    pub fn new(value: impl Into<String>, source: &str, confidence: f64) -> Self {
        Self {
            value: value.into(),
            source: source.to_owned(),
            confidence,
        }
    }
}

/// `TrackResolution`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TrackResolution {
    pub filename: String,
    pub recording_mbid: Option<String>,
    pub fingerprint_score: Option<f64>,
    pub fingerprinted: bool,
    pub canonical_title: Option<FieldFact>,
    pub canonical_artist: Option<FieldFact>,
    pub canonical_tracknumber: Option<FieldFact>,
    pub canonical_duration_s: Option<f64>,
}

impl TrackResolution {
    fn bare(filename: &str, fp: &FingerprintMap) -> Self {
        let (recording_mbid, fingerprint_score) = fp.get(filename).cloned().unwrap_or((None, None));
        Self {
            filename: filename.to_owned(),
            recording_mbid,
            fingerprint_score,
            fingerprinted: fp.contains_key(filename),
            canonical_title: None,
            canonical_artist: None,
            canonical_tracknumber: None,
            canonical_duration_s: None,
        }
    }
}

/// `AlbumResolution`; `serde_json::to_value` of it is `to_dict()`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AlbumResolution {
    pub album_id: String,
    pub anchor: Option<FieldFact>,
    /// `cache | dir-search | fingerprint-vote | ai | user | none`.
    pub anchor_strategy: String,
    pub canonical_title: Option<FieldFact>,
    pub canonical_album_artist: Option<FieldFact>,
    pub canonical_year: Option<FieldFact>,
    /// The sort-name of the first credited artist. The Python app never sets it;
    /// this build fills it whenever MusicBrainz supplies one.
    pub canonical_album_artist_sort: Option<FieldFact>,
    pub canonical_genre: Option<FieldFact>,
    pub cover_url: Option<FieldFact>,
    pub tracks: Vec<TrackResolution>,
    pub unresolved_track_count: usize,
    pub notes: Vec<String>,
}

impl AlbumResolution {
    pub fn new(album_id: &str) -> Self {
        Self {
            album_id: album_id.to_owned(),
            anchor: None,
            anchor_strategy: "none".to_owned(),
            canonical_title: None,
            canonical_album_artist: None,
            canonical_year: None,
            canonical_album_artist_sort: None,
            canonical_genre: None,
            cover_url: None,
            tracks: Vec::new(),
            unresolved_track_count: 0,
            notes: Vec::new(),
        }
    }

    /// `to_dict()`.
    pub fn to_dict(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("plain data serialises")
    }

    /// The unanchored ending of `_resolve_uncached`: per-track fingerprint
    /// info only, every track unresolved.
    pub fn attach_unanchored_tracks(&mut self, flac: &[FlacTrack], fp: &FingerprintMap) {
        self.attach_unresolved_tracks(flac, fp);
        self.notes.push(UNANCHORED_NOTE.to_owned());
    }
}

/// The Python app's trace note for an album no tier anchored. The resolver
/// always records this text (results are memoized and also fed to the AI
/// prompt); [`AlbumResolution::reword_notes`] rewords it where it is shown.
pub const UNANCHORED_NOTE: &str = "No anchor found via Tiers 0a-0c; AI escalation may be required";

/// [`UNANCHORED_NOTE`] when no AI provider is ready.
pub const UNANCHORED_NOTE_NEUTRAL: &str = "No anchor found via Tiers 0a-0c";

impl AlbumResolution {
    /// The `notes` as shown to the user (`GET /api/albums/{id}/resolution`,
    /// the badge tooltip). With [`AiWording::NamesAi`] nothing changes; with
    /// [`AiWording::Neutral`] [`UNANCHORED_NOTE`] becomes
    /// [`UNANCHORED_NOTE_NEUTRAL`]. Call it on a copy: the memoized resolution
    /// itself is never changed, so a change of readiness only changes what the
    /// next read shows.
    ///
    /// [`AiWording::NamesAi`]: musaic_core::AiWording::NamesAi
    /// [`AiWording::Neutral`]: musaic_core::AiWording::Neutral
    pub fn reword_notes(&mut self, wording: musaic_core::AiWording) {
        if wording.names_ai() {
            return;
        }
        for n in self.notes.iter_mut() {
            if n == UNANCHORED_NOTE {
                *n = UNANCHORED_NOTE_NEUTRAL.to_string();
            }
        }
    }

    /// Every FLAC with its fingerprint fields only, all unresolved (for an
    /// anchored album without a tracklist, a deliberate change; the Python app's unanchored ending).
    pub fn attach_unresolved_tracks(&mut self, flac: &[FlacTrack], fp: &FingerprintMap) {
        for ft in flac {
            self.tracks.push(TrackResolution::bare(&ft.filename, fp));
        }
        self.unresolved_track_count = flac.len();
    }
}

/// filename -> (recording_mbid, score); presence means a fingerprint row exists.
pub type FingerprintMap = BTreeMap<String, (Option<String>, Option<f64>)>;

/// Python truthiness of an optional float.
fn nonzero(x: Option<f64>) -> Option<f64> {
    x.filter(|v| *v != 0.0)
}

/// `_refine_from_anchor` given the match Tier 1 built, for a folder that
/// names no disc ([`PositionalScope::WholeRelease`]); see
/// [`refine_from_anchor_scoped`].
pub fn refine_from_anchor(
    resolution: &mut AlbumResolution,
    m: &MbMatch,
    flac: &[FlacTrack],
    fp: &FingerprintMap,
) {
    refine_from_anchor_scoped(resolution, m, flac, fp, &PositionalScope::WholeRelease);
}

/// `_refine_from_anchor` given the match Tier 1 built. Mutates `resolution`
/// in place as the Python app does, with three deliberate changes:
///
/// * When the (tracknumber, 2 s) pass and the unique-duration
///   fallback leave a track unaligned, [`positional_alignment`] may align
///   every track by position instead; such tracks carry
///   [`POSITIONAL_CONFIDENCE`] and the trace says so.
/// * `m.artist_sort` fills `canonical_album_artist_sort`.
/// * When there is no tracklist to align against (the match is for
///   another group or has no MBID, or the tracklist is empty because the
///   fetch failed or listed nothing usable) every FLAC is listed with its
///   fingerprint fields and counted unresolved, so `unresolved_track_count`
///   equals the number of tracks. The Python app lists none and counts 0, which
///   the AI worker's skip rule reads as "fully resolved".
pub fn refine_from_anchor_scoped(
    resolution: &mut AlbumResolution,
    m: &MbMatch,
    flac: &[FlacTrack],
    fp: &FingerprintMap,
    scope: &PositionalScope,
) {
    let Some(anchor) = resolution.anchor.as_ref().map(|a| a.value.clone()) else {
        return;
    };
    if m.mbid.as_deref() != Some(anchor.as_str()) {
        resolution
            .notes
            .push("Tier 1: anchor present but tracklist fetch missed".to_owned());
        resolution.attach_unresolved_tracks(flac, fp);
        return;
    }
    let fact = |v: &Option<String>, c: f64| {
        v.as_ref()
            .filter(|s| !s.is_empty())
            .map(|s| FieldFact::new(s.clone(), sources::MUSICBRAINZ, c))
    };
    if let Some(f) = fact(&m.title, 0.95) {
        resolution.canonical_title = Some(f);
    }
    if let Some(f) = fact(&m.artist, 0.95) {
        resolution.canonical_album_artist = Some(f);
    }
    if let Some(f) = fact(&m.artist_sort, 0.95) {
        resolution.canonical_album_artist_sort = Some(f);
    }
    if let Some(f) = fact(&m.year, 0.95) {
        resolution.canonical_year = Some(f);
    }
    if let Some(f) = fact(&m.primary_genre, 0.55) {
        resolution.canonical_genre = Some(f);
    }
    if m.cover_url_verified
        && let Some(f) = fact(&m.cover_url, 0.95)
    {
        resolution.cover_url = Some(f);
    }
    if m.tracklist.is_empty() {
        resolution
            .notes
            .push("Tier 1: anchored but MB returned no tracklist".to_owned());
        resolution.attach_unresolved_tracks(flac, fp);
        return;
    }
    let aligned = align(flac, &m.tracklist);
    // (MbTrack, reported track number, confidence) per FLAC.
    let mut hits: Vec<Option<(&MbTrack, i64, f64)>> = aligned
        .iter()
        .map(|h| h.map(|t| (t, t.position, 0.95)))
        .collect();
    if aligned.iter().any(Option::is_none) {
        match positional_alignment(flac, &m.tracklist, scope) {
            Ok(p) => {
                let replaced = aligned
                    .iter()
                    .zip(&p.tracks)
                    .filter(|(a, b)| a.is_some_and(|a| !std::ptr::eq(a, **b)))
                    .count();
                let kept = aligned.iter().filter(|a| a.is_some()).count() - replaced;
                resolution.notes.push(p.note(kept, replaced));
                hits = p
                    .tracks
                    .iter()
                    .zip(&p.numbers)
                    .map(|(t, n)| Some((*t, *n, POSITIONAL_CONFIDENCE)))
                    .collect();
            }
            Err(r) => {
                if let Some(note) = r.note() {
                    resolution.notes.push(note);
                }
            }
        }
    }
    for (ft, hit) in flac.iter().zip(hits) {
        let mut track = TrackResolution::bare(&ft.filename, fp);
        match hit {
            Some((t, number, confidence)) => {
                track.canonical_title = Some(FieldFact::new(
                    t.title.clone(),
                    sources::MUSICBRAINZ,
                    confidence,
                ));
                track.canonical_artist = t
                    .artist
                    .as_ref()
                    .filter(|a| !a.is_empty())
                    .map(|a| FieldFact::new(a.clone(), sources::MUSICBRAINZ, confidence));
                track.canonical_tracknumber = Some(FieldFact::new(
                    number.to_string(),
                    sources::MUSICBRAINZ,
                    confidence,
                ));
                track.canonical_duration_s = t.length_s();
            }
            None => resolution.unresolved_track_count += 1,
        }
        resolution.tracks.push(track);
    }
}

/// Which part of the chosen release the positional fallback compares the
/// album's FLACs with.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum PositionalScope {
    /// The folder names no disc: the whole release, every medium in listed
    /// order (MusicBrainz's linear positions). A multi-disc album kept in one
    /// folder is compared with all of its media together.
    #[default]
    WholeRelease,
    /// The folder names "(disc N of M)": only medium `disc` (1-based, in the
    /// order the release lists its media) of the chosen release, whose track
    /// count per medium is `medium_sizes` (`None` when unknown, which refuses
    /// the fallback). Reported track numbers are positions within that medium.
    Disc {
        disc: i64,
        medium_sizes: Option<Vec<usize>>,
    },
}

/// Why [`positional_alignment`] declined.
#[derive(Debug, Clone, PartialEq)]
pub enum PositionalRefusal {
    /// The compared tracklist (whole release or the folder's medium) has a
    /// different number of tracks than the album has FLACs.
    CountMismatch { flacs: usize, tracks: usize },
    /// The folder names a disc the chosen release does not have (or the
    /// release's media are unknown).
    NoSuchMedium { disc: i64, media: Option<usize> },
    /// FLAC `track` (1-based, scanner order) or its partner has no duration
    /// or length, so "every position within the bound" cannot be checked.
    MissingLength { track: usize },
    /// FLAC `track` differs from its partner by `diff_s` (FLAC − MB), beyond
    /// [`POSITIONAL_MAX_DIFF_S`].
    BeyondBound { track: usize, diff_s: f64 },
    /// Differences beyond [`POSITIONAL_SIGN_DEADBAND_S`] point both ways.
    MixedDirections { min_diff_s: f64, max_diff_s: f64 },
}

impl PositionalRefusal {
    /// The trace line, only for refusals on a count-matched tracklist (a
    /// count mismatch is the ordinary case and says nothing new).
    pub fn note(&self) -> Option<String> {
        let head = "Tier 1: not aligned by position";
        match self {
            Self::CountMismatch { .. } | Self::NoSuchMedium { .. } => None,
            Self::MissingLength { track } => {
                Some(format!("{head}: track {track} has no length to compare"))
            }
            Self::BeyondBound { track, diff_s } => Some(format!(
                "{head}: track {track} differs by {diff_s:+.1} s, beyond {POSITIONAL_MAX_DIFF_S} s"
            )),
            Self::MixedDirections {
                min_diff_s,
                max_diff_s,
            } => Some(format!(
                "{head}: lengths differ in both directions ({min_diff_s:+.1} to {max_diff_s:+.1} s)"
            )),
        }
    }
}

/// A positional alignment: every FLAC, in order, with its MusicBrainz track.
#[derive(Debug, Clone, PartialEq)]
pub struct PositionalAlignment<'a> {
    pub tracks: Vec<&'a MbTrack>,
    /// The track number each FLAC is given: the linear position for the
    /// whole release, the position within the medium for a disc folder.
    pub numbers: Vec<i64>,
    /// `Some(disc)` when compared with one medium.
    pub medium: Option<i64>,
    /// Smallest and largest FLAC − MB difference, seconds.
    pub min_diff_s: f64,
    pub max_diff_s: f64,
}

impl PositionalAlignment<'_> {
    /// The trace line. `kept` / `replaced`: FLACs the first pass had aligned
    /// to the same / another MusicBrainz track.
    pub fn note(&self, kept: usize, replaced: usize) -> String {
        let n = self.tracks.len();
        let against = match self.medium {
            Some(d) => format!(" against medium {d}"),
            None => String::new(),
        };
        format!(
            "Tier 1: {n} of {n} tracks aligned by position{against}: track counts match and every length is within {POSITIONAL_MAX_DIFF_S} s ({:+.1} to {:+.1} s); {kept} agreed with the duration pass, {replaced} replaced",
            self.min_diff_s, self.max_diff_s
        )
    }
}

/// Deliberate change from the Python app: align every FLAC by position, or
/// refuse.
///
/// `flac` is in scanner order (disc, then track number), which the
/// caller passes unchanged. The compared tracklist is `mb` for
/// [`PositionalScope::WholeRelease`], or the folder's medium for
/// [`PositionalScope::Disc`]. Accepted only when ALL of:
///
/// 1. the compared tracklist has exactly as many tracks as there are FLACs;
/// 2. every FLAC has a non-zero duration and every partner a non-zero length;
/// 3. EVERY position's |FLAC − MB| is at most [`POSITIONAL_MAX_DIFF_S`];
/// 4. every difference beyond [`POSITIONAL_SIGN_DEADBAND_S`] has the same sign.
///
/// The caller ([`refine_from_anchor_scoped`]) asks only when [`align`] left at
/// least one FLAC unaligned; on success the positional result replaces the
/// first pass for every track.
pub fn positional_alignment<'a>(
    flac: &[FlacTrack],
    mb: &'a [MbTrack],
    scope: &PositionalScope,
) -> Result<PositionalAlignment<'a>, PositionalRefusal> {
    let (target, medium, first_number) = match scope {
        PositionalScope::WholeRelease => (mb, None, None),
        PositionalScope::Disc { disc, medium_sizes } => {
            let refuse = || PositionalRefusal::NoSuchMedium {
                disc: *disc,
                media: medium_sizes.as_ref().map(Vec::len),
            };
            let sizes = medium_sizes
                .as_ref()
                .filter(|s| s.iter().sum::<usize>() == mb.len())
                .ok_or_else(refuse)?;
            let idx = usize::try_from(*disc)
                .ok()
                .and_then(|d| d.checked_sub(1))
                .filter(|i| *i < sizes.len())
                .ok_or_else(refuse)?;
            let start: usize = sizes[..idx].iter().sum();
            (&mb[start..start + sizes[idx]], Some(*disc), Some(1))
        }
    };
    if target.len() != flac.len() || flac.is_empty() {
        return Err(PositionalRefusal::CountMismatch {
            flacs: flac.len(),
            tracks: target.len(),
        });
    }
    let mut diffs = Vec::with_capacity(flac.len());
    for (i, (ft, t)) in flac.iter().zip(target).enumerate() {
        let (Some(d), Some(l)) = (nonzero(ft.duration), nonzero(t.length_s())) else {
            return Err(PositionalRefusal::MissingLength { track: i + 1 });
        };
        diffs.push(d - l);
    }
    for (i, d) in diffs.iter().enumerate() {
        if d.abs() > POSITIONAL_MAX_DIFF_S {
            return Err(PositionalRefusal::BeyondBound {
                track: i + 1,
                diff_s: *d,
            });
        }
    }
    let min_diff_s = diffs.iter().copied().fold(f64::INFINITY, f64::min);
    let max_diff_s = diffs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let beyond = |d: &&f64| d.abs() > POSITIONAL_SIGN_DEADBAND_S;
    let longer = diffs.iter().filter(beyond).any(|d| *d > 0.0);
    let shorter = diffs.iter().filter(beyond).any(|d| *d < 0.0);
    if longer && shorter {
        return Err(PositionalRefusal::MixedDirections {
            min_diff_s,
            max_diff_s,
        });
    }
    let numbers = target
        .iter()
        .enumerate()
        .map(|(i, t)| first_number.map_or(t.position, |f: i64| f + i as i64))
        .collect();
    Ok(PositionalAlignment {
        tracks: target.iter().collect(),
        numbers,
        medium,
        min_diff_s,
        max_diff_s,
    })
}

/// For each FLAC in order, the MusicBrainz track it aligns to.
///
/// 1. The track at its TRACKNUMBER position, unless that position is taken or
///    both durations are known and differ by more than 2.0 s.
/// 2. Otherwise the only untaken MusicBrainz track (with a known length)
///    within 2.0 s of the FLAC's duration.
/// 3. Otherwise unresolved.
pub fn align<'a>(flac: &[FlacTrack], mb: &'a [MbTrack]) -> Vec<Option<&'a MbTrack>> {
    // `{t.position: t for t in mb_tracks}`: a later duplicate position wins.
    let mut by_position: HashMap<i64, &MbTrack> = HashMap::new();
    for t in mb {
        by_position.insert(t.position, t);
    }
    let mut used: HashSet<i64> = HashSet::new();
    let mut out = Vec::with_capacity(flac.len());
    for ft in flac {
        let dur = nonzero(ft.duration);
        let by_pos = ft.tracknumber.as_position().and_then(|tn| {
            let cand = *by_position.get(&tn)?;
            if used.contains(&tn) {
                return None;
            }
            if let (Some(d), Some(l)) = (dur, nonzero(cand.length_s()))
                && (d - l).abs() > ALIGN_TOLERANCE_S
            {
                return None;
            }
            Some(cand)
        });
        let hit = by_pos.or_else(|| {
            let d = dur?;
            let mut cands = mb.iter().filter(|t| {
                !used.contains(&t.position)
                    && nonzero(t.length_s()).is_some_and(|l| (d - l).abs() <= ALIGN_TOLERANCE_S)
            });
            let first = cands.next()?;
            cands.next().is_none().then_some(first)
        });
        if let Some(t) = hit {
            used.insert(t.position);
        }
        out.push(hit);
    }
    out
}
