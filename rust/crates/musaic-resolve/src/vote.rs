//! The fingerprint anchoring rule that replaces the Python app's Tier 0c vote
//! (a deliberate change). The Python vote counted every release-group by
//! weighted AcoustID score and often split between compilations; this rule was
//! chosen by measuring candidate rules against a real library: a reference
//! implementation in Python (`album_candidates`, `pick` with tie-break "v2",
//! `expect`) whose expected anchors the private test tier replays.
//!
//! 1. [`candidates`]: for every release-group any recording of any track's
//!    AcoustID results (score >= `min_score`) sits on, collect which of the
//!    album's tracks reach it, the best result score per track, every release's
//!    whole track count, and every medium's track count.
//! 2. [`pick`]: the winner carries the most tracks; ties break by the closest
//!    track count (whole-release counts only, unless the album is a
//!    "(disc N of M)" directory, in which case whole and per-disc counts), then
//!    by title similarity rounded to two places, then by summed score. The first
//!    candidate in discovery order wins a full tie (Python `max`).
//!    Blocklisted release-groups are skipped.
//! 3. [`decide`]: accept the winner when it carries >= 2 tracks, >= 60% of the
//!    album's tracks, and a track count within 2 of the album's. The anchor
//!    score is `max(88, int(coverage * 100))`.

use std::collections::{BTreeMap, BTreeSet};

use crate::acoustid::LookupResult;
use crate::similarity::title_similarity_hundredths;

/// Python `closeness` default when a candidate has no usable track count.
const NO_COUNT_CLOSENESS: u64 = 999;

/// The thresholds of the rule. [`Rule::default`] is the decided rule
/// (`expected-anchors.json` → `rule`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rule {
    /// Minimum fraction of the album's tracks on the winner (0.6).
    pub coverage: f64,
    /// Minimum number of the album's tracks on the winner (2).
    pub min_tracks: usize,
    /// Maximum gap between the album's track count and the nearest release's (2).
    pub max_count_gap: u64,
    /// AcoustID results scoring below this are ignored (0.5).
    pub min_result_score: f64,
}

impl Default for Rule {
    fn default() -> Self {
        Self {
            coverage: 0.6,
            min_tracks: 2,
            max_count_gap: 2,
            min_result_score: 0.5,
        }
    }
}

/// What the rule needs to know about the album itself.
#[derive(Debug, Clone, Copy)]
pub struct AlbumContext<'a> {
    /// Number of audio files in the album directory.
    pub track_count: usize,
    /// The album's own title for the similarity tie-break (the app uses
    /// `dir_album`, else the ALBUM tag).
    pub title: &'a str,
    /// The directory is a "(disc N of M)" directory: compare per-disc counts too.
    pub multi_disc: bool,
}

/// One release-group reached by at least one of the album's tracks.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub release_group_id: String,
    pub title: Option<String>,
    pub primary_type: Option<String>,
    pub secondary_types: Vec<String>,
    /// Indices (into the album's track list) of tracks with a recording on this group.
    pub tracks: BTreeSet<usize>,
    /// Best AcoustID result score per track index.
    pub scores: BTreeMap<usize, f64>,
    /// Whole-release and per-medium track counts (Python `release_track_counts`).
    pub release_track_counts: BTreeSet<u64>,
    /// Whole-release track counts only (Python `whole_counts`).
    pub whole_counts: BTreeSet<u64>,
}

impl Candidate {
    /// Number of the album's tracks on this group.
    pub fn on_tracks(&self) -> usize {
        self.tracks.len()
    }

    /// Summed best score over the tracks, in ascending track order (Python
    /// sums the dict values in insertion order, which is ascending index).
    pub fn summed_score(&self) -> f64 {
        self.scores.values().fold(0.0, |acc, s| acc + s)
    }

    /// Smallest `|count - track_count|` over the counts the album type allows:
    /// whole-release counts, plus per-disc counts for a "(disc N of M)" album.
    /// `None` when the group has no usable count.
    pub fn count_gap(&self, track_count: usize, multi_disc: bool) -> Option<u64> {
        let counts = if multi_disc {
            &self.release_track_counts
        } else {
            &self.whole_counts
        };
        let n = track_count as u64;
        counts.iter().map(|&tc| tc.abs_diff(n)).min()
    }
}

/// `album_candidates(tracks, min_score)`: release-groups in discovery order.
///
/// `tracks[i]` is the AcoustID result list for the album's i-th track (in the
/// album's file order). Release-groups without an id are skipped (they cannot
/// become an anchor; the recorded fixtures have none).
pub fn candidates<R: AsRef<[LookupResult]>>(tracks: &[R], min_score: f64) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    let mut index: BTreeMap<String, usize> = BTreeMap::new();
    for (idx, results) in tracks.iter().enumerate() {
        for res in results.as_ref() {
            let score = res.score_or_zero();
            if score < min_score {
                continue;
            }
            for rec in &res.recordings {
                for rg in &rec.releasegroups {
                    let Some(id) = rg.id.as_deref() else { continue };
                    let pos = *index.entry(id.to_owned()).or_insert_with(|| {
                        out.push(Candidate {
                            release_group_id: id.to_owned(),
                            title: rg.title.clone(),
                            primary_type: rg.primary_type.clone(),
                            secondary_types: rg.secondarytypes.clone(),
                            tracks: BTreeSet::new(),
                            scores: BTreeMap::new(),
                            release_track_counts: BTreeSet::new(),
                            whole_counts: BTreeSet::new(),
                        });
                        out.len() - 1
                    });
                    let c = &mut out[pos];
                    c.tracks.insert(idx);
                    let best = c.scores.entry(idx).or_insert(0.0);
                    if score > *best {
                        *best = score;
                    }
                    for rel in &rg.releases {
                        if let Some(tc) = rel.track_count.filter(|&t| t > 0) {
                            c.release_track_counts.insert(u64::from(tc));
                            c.whole_counts.insert(u64::from(tc));
                        }
                        for m in &rel.mediums {
                            if let Some(tc) = m.track_count.filter(|&t| t > 0) {
                                c.release_track_counts.insert(u64::from(tc));
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

/// Tracks with at least one recording in a result scoring >= `min_score`
/// (`matched_tracks` in `expected-anchors.json`).
pub fn matched_tracks<R: AsRef<[LookupResult]>>(tracks: &[R], min_score: f64) -> usize {
    tracks
        .iter()
        .filter(|rs| {
            rs.as_ref()
                .iter()
                .any(|r| r.score_or_zero() >= min_score && !r.recordings.is_empty())
        })
        .count()
}

/// `pick(cand, n, title, multi, "v2")`, skipping blocklisted release-groups
/// Returns the index of the winner in `cands`.
pub fn pick(cands: &[Candidate], album: &AlbumContext<'_>, blocklist: &[&str]) -> Option<usize> {
    // Key compared lexicographically, larger wins:
    // (tracks, -closeness, round(similarity, 2), summed score).
    let key = |c: &Candidate| {
        let closeness = c
            .count_gap(album.track_count, album.multi_disc)
            .unwrap_or(NO_COUNT_CLOSENESS);
        let sim = title_similarity_hundredths(album.title, c.title.as_deref().unwrap_or(""));
        (
            c.on_tracks(),
            std::cmp::Reverse(closeness),
            sim,
            c.summed_score(),
        )
    };
    let mut best: Option<(usize, _)> = None;
    for (i, c) in cands.iter().enumerate() {
        if blocklist.contains(&c.release_group_id.as_str()) {
            continue;
        }
        let k = key(c);
        // Python max() keeps the first maximal element: replace only on strictly greater.
        let better = match &best {
            None => true,
            Some((_, bk)) => k.partial_cmp(bk) == Some(std::cmp::Ordering::Greater),
        };
        if better {
            best = Some((i, k));
        }
    }
    best.map(|(i, _)| i)
}

/// The winning release-group and the numbers the decision is made on.
#[derive(Debug, Clone, PartialEq)]
pub struct Winner {
    pub release_group_id: String,
    pub title: Option<String>,
    /// Number of the album's tracks on the winner.
    pub on_tracks: usize,
    /// `on_tracks / track_count` (unrounded).
    pub coverage: f64,
    /// See [`Candidate::count_gap`].
    pub count_gap: Option<u64>,
    /// Non-blocklisted candidates carrying as many tracks as the winner
    /// (the winner included).
    pub tied_candidates: usize,
}

/// A persisted anchor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anchor {
    pub release_group_id: String,
    /// `max(88, int(coverage * 100))`.
    pub score: u32,
}

/// Why no anchor was persisted. Several may apply at once; they are reported
/// in this order, like `record_acoustid.expect`.
#[derive(Debug, Clone, PartialEq)]
pub enum RejectReason {
    /// No (non-blocklisted) release-group was reached by any track.
    NoCandidate,
    /// The winner carries fewer than `min` of the album's tracks.
    TooFewTracks { on_tracks: usize, min: usize },
    /// The winner's coverage is below `min`.
    CoverageBelow { coverage: f64, min: f64 },
    /// No release (or disc, for a "(disc N of M)" album) within `max` tracks.
    /// `gap` is `None` when the group has no usable track count at all.
    CountGapTooLarge { gap: Option<u64>, max: u64 },
}

impl RejectReason {
    /// The message `record_acoustid.expect` writes for this reason.
    pub fn message(&self) -> String {
        match self {
            Self::NoCandidate => "no candidate release-group".to_owned(),
            Self::TooFewTracks { min, .. } => format!("fewer than {min} tracks on the winner"),
            Self::CoverageBelow { min, .. } => format!("coverage below {min}"),
            Self::CountGapTooLarge { max, .. } => {
                format!("no release within {max} tracks of the album")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Accepted(Anchor),
    Rejected(Vec<RejectReason>),
}

impl Outcome {
    /// The `reason` string of `expected-anchors.json`: `"accepted"` or the
    /// rejection messages joined by `"; "`.
    pub fn reason_text(&self) -> String {
        match self {
            Self::Accepted(_) => "accepted".to_owned(),
            Self::Rejected(rs) => rs
                .iter()
                .map(RejectReason::message)
                .collect::<Vec<_>>()
                .join("; "),
        }
    }

    pub fn anchor(&self) -> Option<&Anchor> {
        match self {
            Self::Accepted(a) => Some(a),
            Self::Rejected(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    /// `None` when there was no candidate.
    pub winner: Option<Winner>,
    pub outcome: Outcome,
}

/// The whole rule: candidates → pick → thresholds.
///
/// `tracks[i]` is the AcoustID result list for the album's i-th audio file;
/// `album.track_count` is normally `tracks.len()`.
pub fn decide<R: AsRef<[LookupResult]>>(
    tracks: &[R],
    album: &AlbumContext<'_>,
    blocklist: &[&str],
    rule: &Rule,
) -> Decision {
    let cands = candidates(tracks, rule.min_result_score);
    let rejected_none = || Decision {
        winner: None,
        outcome: Outcome::Rejected(vec![RejectReason::NoCandidate]),
    };
    if album.track_count == 0 {
        return rejected_none();
    }
    let Some(wi) = pick(&cands, album, blocklist) else {
        return rejected_none();
    };
    let c = &cands[wi];
    let k = c.on_tracks();
    let n = album.track_count;
    let coverage = k as f64 / n as f64;
    let gap = c.count_gap(n, album.multi_disc);
    let tied = cands
        .iter()
        .filter(|x| !blocklist.contains(&x.release_group_id.as_str()) && x.on_tracks() == k)
        .count();
    let winner = Winner {
        release_group_id: c.release_group_id.clone(),
        title: c.title.clone(),
        on_tracks: k,
        coverage,
        count_gap: gap,
        tied_candidates: tied,
    };
    let mut fails = Vec::new();
    if k < rule.min_tracks {
        fails.push(RejectReason::TooFewTracks {
            on_tracks: k,
            min: rule.min_tracks,
        });
    }
    if coverage < rule.coverage {
        fails.push(RejectReason::CoverageBelow {
            coverage,
            min: rule.coverage,
        });
    }
    if !gap.is_some_and(|g| g <= rule.max_count_gap) {
        fails.push(RejectReason::CountGapTooLarge {
            gap,
            max: rule.max_count_gap,
        });
    }
    let outcome = if fails.is_empty() {
        Outcome::Accepted(Anchor {
            release_group_id: c.release_group_id.clone(),
            score: anchor_score(k, n),
        })
    } else {
        Outcome::Rejected(fails)
    };
    Decision {
        winner: Some(winner),
        outcome,
    }
}

/// `max(88, int(k / n * 100))`, with the same float operations as Python.
pub fn anchor_score(on_tracks: usize, track_count: usize) -> u32 {
    let pct = (on_tracks as f64 / track_count as f64 * 100.0).trunc();
    // pct is in [0, 100] because on_tracks <= track_count; clamp defensively.
    let pct = if pct.is_finite() {
        pct.clamp(0.0, f64::from(u32::MAX)) as u32
    } else {
        0
    };
    pct.max(88)
}
