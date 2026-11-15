//! The Tier 0b (directory-name search) acceptance check. A deliberate addition:
//! the Python app saved any directory-name search hit at score >= 70 as the
//! anchor, which let a ten-second synthetic album anchor to a real release.
//!
//! A name-search hit at score >= 70 is persisted only when some release in the
//! candidate release-group agrees with the album on BOTH track count (within 2)
//! and total duration (within 3%). Otherwise it is rejected with a reason the
//! resolver records in its trace, and the cascade continues to Tier 0c.
//!
//! **No recorded evidence yet.** There is no recorded MusicBrainz release browse
//! with per-release durations for the test albums, so this function is tested
//! only with constructed inputs that exercise each branch of the rule. The
//! tolerances are the decided ones (track gap 2, duration 3%, search score 70).
//!
//! Choices made here where the rule is silent:
//! * Duration tolerance is relative to the album's own total:
//!   `|release - album| <= 0.03 * album`.
//! * Both conditions must hold for the SAME unit (one release, or one disc).
//! * For a "(disc N of M)" directory each medium is a unit (its track count and
//!   its duration), and so is the whole release — mirroring the Tier 0c
//!   reference, which compares a disc directory against whole-release and
//!   per-disc counts alike. For any other directory only whole releases count.
//! * A unit with an unknown duration cannot satisfy the duration condition.

/// The minimum Tier 0b search score (the `mb_lookup` cut-off).
pub const MIN_SEARCH_SCORE: u32 = 70;
/// Maximum track-count gap.
pub const MAX_TRACK_GAP: u64 = 2;
/// Maximum relative total-duration difference.
pub const MAX_DURATION_FRACTION: f64 = 0.03;

/// The album on disk.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AlbumShape {
    pub track_count: u64,
    /// Sum of the album's audio durations, in seconds.
    pub total_duration_s: f64,
    /// The directory is a "(disc N of M)" directory.
    pub multi_disc: bool,
}

/// One disc of a candidate release.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MediumShape {
    pub track_count: u64,
    pub duration_s: Option<f64>,
}

/// One release of the candidate release-group.
#[derive(Debug, Clone, PartialEq)]
pub struct ReleaseShape {
    pub track_count: u64,
    pub duration_s: Option<f64>,
    pub mediums: Vec<MediumShape>,
}

/// Which unit agreed with the album.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Agreement {
    /// Index into the `releases` slice.
    pub release: usize,
    /// `Some(i)` when a disc (index into `mediums`) agreed rather than the whole release.
    pub medium: Option<usize>,
}

/// Why a name-search hit was not persisted.
#[derive(Debug, Clone, PartialEq)]
pub enum NameSearchRejection {
    /// The search score is below [`MIN_SEARCH_SCORE`].
    ScoreBelowThreshold { score: u32, min: u32 },
    /// The release-group has no releases to compare against.
    NoReleases,
    /// No release (or disc) within [`MAX_TRACK_GAP`] tracks. `closest_gap` is
    /// the smallest gap seen.
    TrackCountMismatch { closest_gap: u64, max: u64 },
    /// Some unit agrees on track count, but none of those is within
    /// [`MAX_DURATION_FRACTION`] of the album's duration. `closest_fraction`
    /// is the smallest relative difference among them (`None` when none of
    /// them has a known duration).
    DurationMismatch {
        closest_fraction: Option<f64>,
        max: f64,
    },
}

impl NameSearchRejection {
    /// A one-line reason for the resolution trace.
    pub fn message(&self) -> String {
        match self {
            Self::ScoreBelowThreshold { score, min } => {
                format!("search score {score} below {min}")
            }
            Self::NoReleases => "candidate release-group has no releases".to_owned(),
            Self::TrackCountMismatch { closest_gap, max } => {
                format!("no release within {max} tracks of the album (closest {closest_gap})")
            }
            Self::DurationMismatch {
                closest_fraction,
                max,
            } => match closest_fraction {
                Some(f) => format!(
                    "no release within {:.0}% of the album's total duration (closest {:.1}%)",
                    max * 100.0,
                    f * 100.0
                ),
                None => format!(
                    "no release within {:.0}% of the album's total duration (no duration known)",
                    max * 100.0
                ),
            },
        }
    }
}

/// Accept a Tier 0b hit only when a release agrees on track count and duration.
pub fn accept_name_search(
    search_score: u32,
    album: &AlbumShape,
    releases: &[ReleaseShape],
) -> Result<Agreement, NameSearchRejection> {
    if search_score < MIN_SEARCH_SCORE {
        return Err(NameSearchRejection::ScoreBelowThreshold {
            score: search_score,
            min: MIN_SEARCH_SCORE,
        });
    }
    if releases.is_empty() {
        return Err(NameSearchRejection::NoReleases);
    }
    let mut units: Vec<(Agreement, u64, Option<f64>)> = Vec::new();
    for (ri, r) in releases.iter().enumerate() {
        units.push((
            Agreement {
                release: ri,
                medium: None,
            },
            r.track_count,
            r.duration_s,
        ));
        if album.multi_disc {
            for (mi, m) in r.mediums.iter().enumerate() {
                units.push((
                    Agreement {
                        release: ri,
                        medium: Some(mi),
                    },
                    m.track_count,
                    m.duration_s,
                ));
            }
        }
    }
    let mut closest_gap = u64::MAX;
    let mut closest_fraction: Option<f64> = None;
    let mut any_count_ok = false;
    for (who, tc, dur) in &units {
        let gap = tc.abs_diff(album.track_count);
        closest_gap = closest_gap.min(gap);
        if gap > MAX_TRACK_GAP {
            continue;
        }
        any_count_ok = true;
        let Some(d) = dur else { continue };
        let fraction = if album.total_duration_s > 0.0 {
            (d - album.total_duration_s).abs() / album.total_duration_s
        } else {
            f64::INFINITY
        };
        if fraction <= MAX_DURATION_FRACTION {
            return Ok(*who);
        }
        closest_fraction = Some(closest_fraction.map_or(fraction, |c: f64| c.min(fraction)));
    }
    if !any_count_ok {
        return Err(NameSearchRejection::TrackCountMismatch {
            closest_gap,
            max: MAX_TRACK_GAP,
        });
    }
    Err(NameSearchRejection::DurationMismatch {
        closest_fraction,
        max: MAX_DURATION_FRACTION,
    })
}
