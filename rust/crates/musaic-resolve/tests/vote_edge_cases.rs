//! Edge cases of the anchoring rule on constructed AcoustID responses.
//!
//! These are not Python app comparisons: the inputs are built here and the
//! expected outcomes follow from the rule as written in the Python reference
//! (`pick` v2, `expect`): coverage, track count and the tie-break. The recorded-fixture comparison is
//! tests/expected_anchors.rs.

use musaic_resolve::vote::anchor_score;
use musaic_resolve::{
    AlbumContext, Anchor, LookupResponse, LookupResult, Outcome, RejectReason, Rule, decide,
    matched_tracks,
};
use serde_json::json;

/// A release-group on a recording: (id, title, releases as (whole count, per-disc counts)).
type Rg<'a> = (&'a str, &'a str, &'a [(u32, &'a [u32])]);

/// One AcoustID result list for a track: a single result with `score` whose
/// one recording sits on every listed release-group.
fn track(score: f64, rgs: &[Rg<'_>]) -> Vec<LookupResult> {
    let groups: Vec<_> = rgs
        .iter()
        .map(|(id, title, rels)| {
            json!({
                "id": id, "title": title, "type": "Album",
                "releases": rels.iter().map(|(whole, discs)| json!({
                    "id": format!("{id}-rel-{whole}"),
                    "track_count": whole,
                    "medium_count": discs.len(),
                    "mediums": discs.iter().enumerate().map(|(i, c)| json!({
                        "position": i + 1, "track_count": c, "tracks": [{"position": 1}],
                    })).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    let body = json!({"status": "ok", "results": [{
        "id": "fp", "score": score,
        "recordings": [{"id": "rec", "title": "t", "duration": 180.0, "releasegroups": groups}],
    }]});
    LookupResponse::from_json(body.to_string().as_bytes())
        .unwrap()
        .results
}

fn album(n: usize, title: &str, multi_disc: bool) -> AlbumContext<'_> {
    AlbumContext {
        track_count: n,
        title,
        multi_disc,
    }
}

fn winner_id(d: &musaic_resolve::Decision) -> &str {
    &d.winner.as_ref().unwrap().release_group_id
}

// No candidate at all -> no anchor, reason "no candidate release-group".
#[test]
fn no_candidates() {
    let empty: Vec<Vec<LookupResult>> = vec![vec![], vec![]];
    let d = decide(&empty, &album(2, "X", false), &[], &Rule::default());
    assert_eq!(d.winner, None);
    assert_eq!(
        d.outcome,
        Outcome::Rejected(vec![RejectReason::NoCandidate])
    );
    assert_eq!(d.outcome.reason_text(), "no candidate release-group");

    // Results below min_result_score (0.5) are ignored entirely.
    let low = vec![track(0.49, &[("a", "X", &[(2, &[2])])]); 2];
    assert_eq!(matched_tracks(&low, 0.5), 0);
    let d = decide(&low, &album(2, "X", false), &[], &Rule::default());
    assert_eq!(
        d.outcome,
        Outcome::Rejected(vec![RejectReason::NoCandidate])
    );
}

// A blocklisted release-group can never win; the next one is judged.
#[test]
fn blocklisted_winner_is_skipped() {
    let tracks = vec![
        track(
            0.9,
            &[("blocked", "X", &[(3, &[3])]), ("other", "X", &[(3, &[3])])],
        ),
        track(
            0.9,
            &[("blocked", "X", &[(3, &[3])]), ("other", "X", &[(3, &[3])])],
        ),
        track(0.9, &[("blocked", "X", &[(3, &[3])])]),
    ];
    let a = album(3, "X", false);
    let open = decide(&tracks, &a, &[], &Rule::default());
    assert_eq!(winner_id(&open), "blocked");

    let d = decide(&tracks, &a, &["blocked"], &Rule::default());
    let w = d.winner.as_ref().unwrap();
    assert_eq!(w.release_group_id, "other");
    assert_eq!(w.on_tracks, 2);
    assert_eq!(
        w.tied_candidates, 1,
        "blocklisted groups do not count as tied"
    );
    assert_eq!(
        d.outcome,
        Outcome::Accepted(Anchor {
            release_group_id: "other".into(),
            score: 88, // max(88, int(2/3*100) = 66)
        })
    );

    let all = decide(&tracks, &a, &["blocked", "other"], &Rule::default());
    assert_eq!(
        all.outcome,
        Outcome::Rejected(vec![RejectReason::NoCandidate])
    );
}

// tie-break key 1: equal track coverage -> closest whole-release track count wins,
// even though the other group was discovered first.
#[test]
fn tie_broken_by_track_count() {
    let rgs: &[Rg<'_>] = &[
        ("box", "Album", &[(40, &[10, 10, 10, 10])]),
        ("lp", "Other", &[(4, &[4])]),
    ];
    let tracks = vec![track(0.9, rgs); 4];
    let d = decide(&tracks, &album(4, "Album", false), &[], &Rule::default());
    assert_eq!(winner_id(&d), "lp");
    let w = d.winner.as_ref().unwrap();
    assert_eq!((w.count_gap, w.tied_candidates), (Some(0), 2));
}

// tie-break key 2: same coverage and count gap -> title more similar to the album's.
#[test]
fn tie_broken_by_title_similarity() {
    let rgs: &[Rg<'_>] = &[
        ("hits", "Golden Hits", &[(4, &[4])]),
        ("own", "Night Mix!", &[(4, &[4])]),
    ];
    let tracks = vec![track(0.9, rgs); 4];
    let d = decide(
        &tracks,
        &album(4, "Night Mix", false),
        &[],
        &Rule::default(),
    );
    assert_eq!(winner_id(&d), "own");
}

// tie-break key 3: same coverage, gap and title -> higher summed AcoustID score.
// A full tie keeps the first-discovered group (Python max()).
#[test]
fn tie_broken_by_summed_score() {
    let a: Rg<'_> = ("first", "Same", &[(2, &[2])]);
    let b: Rg<'_> = ("second", "Same", &[(2, &[2])]);
    // Track 0 reaches both at 0.6; track 1 reaches both, but "second" via a 0.95 result.
    let mut t1 = track(0.6, &[a, b]);
    t1.extend(track(0.95, &[b]));
    let tracks = vec![track(0.6, &[a, b]), t1];
    let d = decide(&tracks, &album(2, "Same", false), &[], &Rule::default());
    assert_eq!(winner_id(&d), "second");

    let even = vec![track(0.6, &[a, b]); 2];
    let d = decide(&even, &album(2, "Same", false), &[], &Rule::default());
    assert_eq!(winner_id(&d), "first");
}

// Per-disc counts are used only for a "(disc N of M)" album.
#[test]
fn multi_disc_uses_per_disc_counts() {
    let set: Rg<'_> = ("set", "Twin", &[(6, &[3, 3])]);
    let single: Rg<'_> = ("single", "Twin", &[(5, &[5])]);
    let tracks = vec![track(0.9, &[set, single]); 3];

    // Plain directory: whole counts only. "single" (gap 2) beats "set" (gap 3).
    let d = decide(&tracks, &album(3, "Twin", false), &[], &Rule::default());
    assert_eq!(winner_id(&d), "single");
    assert_eq!(d.winner.as_ref().unwrap().count_gap, Some(2));

    // Disc directory: the 3-track disc of "set" counts (gap 0) and wins.
    let d = decide(&tracks, &album(3, "Twin", true), &[], &Rule::default());
    assert_eq!(winner_id(&d), "set");
    assert_eq!(d.winner.as_ref().unwrap().count_gap, Some(0));
    assert!(matches!(d.outcome, Outcome::Accepted(_)));

    // Only the set on offer: rejected for a plain directory, accepted for a disc directory.
    let only_set = vec![track(0.9, &[set]); 3];
    let plain = decide(&only_set, &album(3, "Twin", false), &[], &Rule::default());
    assert_eq!(
        plain.outcome,
        Outcome::Rejected(vec![RejectReason::CountGapTooLarge {
            gap: Some(3),
            max: 2
        }])
    );
    assert_eq!(
        plain.outcome.reason_text(),
        "no release within 2 tracks of the album"
    );
    let disc = decide(&only_set, &album(3, "Twin", true), &[], &Rule::default());
    assert!(matches!(disc.outcome, Outcome::Accepted(_)));
}

// One matched track never anchors an album.
#[test]
fn single_matched_track_does_not_anchor() {
    // One of twelve tracks matched, on a 20-track compilation (cf. rip-0003).
    let mut tracks = vec![track(0.9, &[("comp", "20 Greatest", &[(20, &[20])])])];
    tracks.extend(std::iter::repeat_n(Vec::new(), 11));
    let d = decide(
        &tracks,
        &album(12, "Nighttide", false),
        &[],
        &Rule::default(),
    );
    assert_eq!(
        d.outcome.reason_text(),
        "fewer than 2 tracks on the winner; coverage below 0.6; no release within 2 tracks of the album"
    );

    // A one-track album, fully matched on a one-track release: coverage 1.0 and
    // gap 0, but still fewer than 2 tracks.
    let one = vec![track(0.9, &[("single", "S", &[(1, &[1])])])];
    let d = decide(&one, &album(1, "S", false), &[], &Rule::default());
    assert_eq!(
        d.outcome,
        Outcome::Rejected(vec![RejectReason::TooFewTracks {
            on_tracks: 1,
            min: 2
        }])
    );
}

// Persisted score is max(88, int(coverage * 100)).
#[test]
fn anchor_score_formula() {
    assert_eq!(anchor_score(2, 3), 88);
    assert_eq!(anchor_score(9, 10), 90);
    assert_eq!(anchor_score(11, 12), 91);
    assert_eq!(anchor_score(10, 10), 100);
}

// Absent or null fields in an AcoustID response read as empty / zero.
#[test]
fn tolerant_parsing() {
    let body = br#"{"status":"ok","results":[
        {"id":"a","recordings":null},
        {"id":"b","score":0.8,"recordings":[{"id":"r","releasegroups":[{"id":"g","releases":null}]}]}
    ]}"#;
    let r = LookupResponse::from_json(body).unwrap();
    assert!(r.is_ok());
    assert_eq!(r.results[0].score_or_zero(), 0.0);
    assert!(r.results[0].recordings.is_empty());
    let tracks = vec![r.results.clone(), r.results];
    let d = decide(&tracks, &album(2, "g", false), &[], &Rule::default());
    let w = d.winner.as_ref().unwrap();
    assert_eq!((w.release_group_id.as_str(), w.count_gap), ("g", None));
    assert_eq!(
        d.outcome,
        Outcome::Rejected(vec![RejectReason::CountGapTooLarge { gap: None, max: 2 }])
    );
}
