//! Tier 0b name-search acceptance.
//!
//! There is no recorded MusicBrainz release browse with per-release durations
//! for the fixture albums yet, so these tests use constructed inputs only. They
//! pin the branches of the rule (track gap <= 2 AND total duration
//! within 3%, search score >= 70); they are not Python app comparisons and invent
//! no Python app values.

use musaic_resolve::name_search::{
    AlbumShape, MediumShape, NameSearchRejection, ReleaseShape, accept_name_search,
};

fn rel(track_count: u64, duration_s: Option<f64>, mediums: &[(u64, Option<f64>)]) -> ReleaseShape {
    ReleaseShape {
        track_count,
        duration_s,
        mediums: mediums
            .iter()
            .map(|&(track_count, duration_s)| MediumShape {
                track_count,
                duration_s,
            })
            .collect(),
    }
}

const ALBUM: AlbumShape = AlbumShape {
    track_count: 10,
    total_duration_s: 2000.0,
    multi_disc: false,
};

// Positive: a release within 2 tracks and 3% duration is accepted.
#[test]
fn accepts_release_within_both_tolerances() {
    let releases = [rel(20, Some(4000.0), &[]), rel(12, Some(2060.0), &[])];
    let ok = accept_name_search(100, &ALBUM, &releases).unwrap();
    assert_eq!((ok.release, ok.medium), (1, None));
    // Exactly at the limits (gap 2, 3.0%).
    assert!(accept_name_search(70, &ALBUM, &[rel(8, Some(1940.0), &[])]).is_ok());
}

// Negative: track count too far (cf. the 4-track synthetic Night Mix).
#[test]
fn rejects_on_track_count() {
    let err = accept_name_search(
        100,
        &ALBUM,
        &[rel(13, Some(2000.0), &[]), rel(30, None, &[])],
    )
    .unwrap_err();
    assert_eq!(
        err,
        NameSearchRejection::TrackCountMismatch {
            closest_gap: 3,
            max: 2
        }
    );
}

// Negative: track count agrees, duration does not.
#[test]
fn rejects_on_duration() {
    let err = accept_name_search(
        100,
        &ALBUM,
        &[rel(10, Some(2100.0), &[]), rel(11, None, &[])],
    )
    .unwrap_err();
    match err {
        NameSearchRejection::DurationMismatch {
            closest_fraction: Some(f),
            max,
        } => {
            assert!((f - 0.05).abs() < 1e-12);
            assert_eq!(max, 0.03);
        }
        other => panic!("unexpected {other:?}"),
    }
    let unknown = accept_name_search(100, &ALBUM, &[rel(10, None, &[])]).unwrap_err();
    assert!(matches!(
        unknown,
        NameSearchRejection::DurationMismatch {
            closest_fraction: None,
            ..
        }
    ));
}

// Both conditions must hold for the same release.
#[test]
fn conditions_must_hold_on_one_release() {
    let releases = [rel(10, Some(3000.0), &[]), rel(20, Some(2000.0), &[])];
    assert!(accept_name_search(100, &ALBUM, &releases).is_err());
}

// Per-disc counts and durations only for a "(disc N of M)" directory.
#[test]
fn multi_disc_compares_discs() {
    let releases = [rel(
        20,
        Some(4000.0),
        &[(10, Some(2010.0)), (10, Some(1990.0))],
    )];
    assert!(matches!(
        accept_name_search(100, &ALBUM, &releases),
        Err(NameSearchRejection::TrackCountMismatch { .. })
    ));
    let disc = AlbumShape {
        multi_disc: true,
        ..ALBUM
    };
    let ok = accept_name_search(100, &disc, &releases).unwrap();
    assert_eq!((ok.release, ok.medium), (0, Some(0)));
}

// A search hit below score 70 and an empty release-group are rejected.
#[test]
fn rejects_low_score_and_empty_group() {
    assert_eq!(
        accept_name_search(69, &ALBUM, &[rel(10, Some(2000.0), &[])]).unwrap_err(),
        NameSearchRejection::ScoreBelowThreshold { score: 69, min: 70 }
    );
    assert_eq!(
        accept_name_search(100, &ALBUM, &[]).unwrap_err(),
        NameSearchRejection::NoReleases
    );
}
