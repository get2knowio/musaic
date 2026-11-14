//! Positional alignment (a deliberate change from the Python app): Tier 1's positional
//! fallback. When the (tracknumber, 2 s) pass and the unique-duration
//! fallback leave a FLAC unaligned, the track counts match and EVERY position
//! is within `POSITIONAL_MAX_DIFF_S` with the differences beyond 2 s all in
//! one direction, every track aligns by position.
//!
//! Inputs are the Python app golden's album sides and the recorded release
//! browses (the private recorded replay set). The wrong pairings below are the
//! closest ones measured over the real albums and their recorded listings.
//! Values marked CONSTRUCTED are built
//! from recorded lengths plus a stated offset. Scenario-level deltas against
//! the Python app's record live in cascade_python_parity.rs / common::deltas.

mod common;

use common::deltas::{self as d};
use common::env::{NOW_EPOCH_S, NoFingerprints, ReplayMb, World, flac_track};
use musaic_resolve::find_release::{
    MbMatch, MbTrack, best_fit_release, parse_listing, release_medium_sizes,
};
use musaic_resolve::resolution::{
    AlbumResolution, FieldFact, FingerprintMap, POSITIONAL_CONFIDENCE, POSITIONAL_MAX_DIFF_S,
    PositionalRefusal, PositionalScope, positional_alignment, refine_from_anchor_scoped,
};
use musaic_resolve::source::{FlacTrack, MusicBrainzSource};
use musaic_resolve::{Env, Resolver};
use serde_json::{Value, json};

const ALBUM_ONE: &str = "cached anchor without score";

fn browse(rg: &str) -> Vec<u8> {
    ReplayMb::new(&json!([])).browse_releases(rg).unwrap()
}

/// The recorded tracks of one release (linear positions).
fn release_tracks(rg: &str, rel: &str) -> Vec<MbTrack> {
    parse_listing(&browse(rg))
        .unwrap()
        .usable
        .into_iter()
        .find(|r| r.id == rel)
        .unwrap_or_else(|| panic!("{rel}"))
        .tracks
}

fn flac_of(case: &Value) -> Vec<FlacTrack> {
    case["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(flac_track)
        .collect()
}

fn diffs(flac: &[FlacTrack], mb: &[MbTrack]) -> Vec<f64> {
    flac.iter()
        .zip(mb)
        .map(|(f, t)| f.duration.unwrap() - t.length_s().unwrap())
        .collect()
}

/// `refine_from_anchor_scoped` on an anchored resolution; returns the body.
fn refine(flac: &[FlacTrack], tracks: &[MbTrack], scope: &PositionalScope) -> Value {
    let mut res = AlbumResolution::new("album-1");
    res.anchor = Some(FieldFact::new("rg", "cache", 1.0));
    let m = MbMatch {
        mbid: Some("rg".into()),
        tracklist: tracks.to_vec(),
        ..MbMatch::default()
    };
    refine_from_anchor_scoped(&mut res, &m, flac, &FingerprintMap::new(), scope);
    res.to_dict()
}

fn aligned(body: &Value) -> usize {
    body["tracks"].as_array().unwrap().len()
        - body["unresolved_track_count"].as_u64().unwrap() as usize
}

fn by_position(body: &Value) -> usize {
    body["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["canonical_title"]["confidence"] == json!(POSITIONAL_CONFIDENCE))
        .count()
}

fn resolve(case: &Value) -> Value {
    let world = World::from_case(case);
    let mb = ReplayMb::new(&case["overlay"]);
    let env = Env {
        albums: &world,
        store: &world,
        mb: &mb,
        fingerprints: &NoFingerprints,
        now_epoch_s: NOW_EPOCH_S,
    };
    Resolver::new()
        .resolve(&env, case["album"]["id"].as_str().unwrap(), "", false)
        .unwrap()
        .resolution
        .to_dict()
}

// Positional alignment: aligned-of-total, Python app (golden) -> this build, through
// the whole cascade, on the five real albums. Only rip-0001 changes (0 -> 6,
// all by position); none gets worse; no album that already aligned is touched.
#[test]
fn real_albums_before_and_after() {
    musaic_testdata::require_private!("replay");
    let mut table = Vec::new();
    for (scenario, _, _) in d::real_albums() {
        let case = d::cascade_case(scenario);
        let n = case["tracks"].as_array().unwrap().len();
        let before = n - case["results"][0]["body"]["unresolved_track_count"]
            .as_u64()
            .unwrap() as usize;
        let body = resolve(&case);
        let after = aligned(&body);
        assert!(after >= before, "{scenario}: {before} -> {after}");
        table.push((scenario, n, before, after, by_position(&body)));
    }
    eprintln!("{table:#?}");
    let figures: Vec<_> = table.iter().map(|t| (t.1, t.2, t.3, t.4)).collect();
    assert_eq!(
        figures,
        vec![
            (6, 0, 6, 6),    // rip-0001 Conniff: the motivating album
            (12, 5, 5, 0),   // rip-0002: release lists 22 tracks
            (12, 4, 4, 0),   // rip-0003: release lists 20
            (12, 12, 12, 0), // real-album-4 (disc 1): already aligned
            (10, 10, 10, 0), // real-album-5: already aligned
        ]
    );
}

// The positional result on rip-0001 agrees with the fingerprints:
// every FLAC whose AcoustID recording appears on the chosen release sits at
// that position (2 of 6; the other four FLACs match other recordings of the
// same songs), and none contradicts it.
#[test]
fn conniff_positions_agree_with_fingerprints() {
    musaic_testdata::require_private!("replay");
    let case = d::cascade_case("real 0a rip-0001");
    let flac = flac_of(&case);
    let tracks = release_tracks(d::conniff_rg(), d::conniff_rel());
    let p = positional_alignment(&flac, &tracks, &PositionalScope::WholeRelease).unwrap();
    let mut confirmed = 0;
    for (i, f) in flac.iter().enumerate() {
        let fp = case["fingerprints"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["filename"].as_str() == Some(&f.filename))
            .and_then(|r| r["acoustid_recording_mbid"].as_str());
        let Some(rec) = fp else { continue };
        if let Some(at) = tracks
            .iter()
            .position(|t| t.recording_mbid.as_deref() == Some(rec))
        {
            assert_eq!(at, i, "{}", f.filename);
            assert!(std::ptr::eq(p.tracks[i], &tracks[at]));
            confirmed += 1;
        }
    }
    assert_eq!(confirmed, 2);
    // The measured shape: uniformly longer, +2.7 to +8.1 s.
    assert!(p.min_diff_s > 2.0 && p.max_diff_s <= POSITIONAL_MAX_DIFF_S);
}

// Positional alignment on the synthetic albums. "Album One" (golden's CONSTRUCTED
// one-release listing) already aligns 3/3 and is untouched. Night Mix is
// unanchored in this build (the Tier 0b acceptance check), so Tier 1 never runs; had it run, no
// release of its recorded group has 4 tracks: the fallback declines.
#[test]
fn synthetic_albums() {
    musaic_testdata::require_private!("replay");
    let case = d::cascade_case(ALBUM_ONE);
    let body = resolve(&case);
    assert_eq!((aligned(&body), by_position(&body)), (3, 0));
    assert_eq!(case["results"][0]["body"]["unresolved_track_count"], 0);

    let case =
        d::cascade_case("night mix 0b by name (negative: no anchor without a duration check)");
    let flac = flac_of(&case);
    let listing = parse_listing(&browse(d::night_mix_rg())).unwrap();
    let best = best_fit_release(&listing, &flac).unwrap();
    assert!(matches!(
        positional_alignment(&flac, &best.tracks, &PositionalScope::WholeRelease),
        Err(PositionalRefusal::CountMismatch { flacs: 4, .. })
    ));
    assert_eq!(aligned(&resolve(&case)), 0);
}

// Positional alignment, negative, measured wrong pairings with the same count: rip-0002
// and rip-0003, 12 FLACs each, against real-album-4's
// 12-track medium 1 (a different album). Neither aligns by position.
#[test]
fn same_count_different_album_does_not_align() {
    musaic_testdata::require_private!("replay");
    let tracks = release_tracks(d::disc_album_rg(), d::disc_album_rel());
    let sizes = release_medium_sizes(&browse(d::disc_album_rg()), d::disc_album_rel());
    assert_eq!(sizes, Some(vec![12, 13]));
    let scope = PositionalScope::Disc {
        disc: 1,
        medium_sizes: sizes,
    };
    for s in ["real 0a rip-0002", "real 0a rip-0003"] {
        let flac = flac_of(&d::cascade_case(s));
        assert_eq!(flac.len(), 12);
        assert!(
            matches!(
                positional_alignment(&flac, &tracks, &scope),
                Err(PositionalRefusal::BeyondBound { .. })
            ),
            "{s}"
        );
        assert_eq!(by_position(&refine(&flac, &tracks, &scope)), 0, "{s}");
    }
}

// Positional alignment, negative: the closest measured wrong pairing whose differences all
// lean one way — "Album One" against positions 14-16 of real-album-4's release
// (17.2 s at most, every difference beyond 2 s positive). Only the bound
// refuses it; a bound of 2 x 10 s would accept it.
#[test]
fn closest_same_direction_wrong_pairing_is_beyond_the_bound() {
    musaic_testdata::require_private!("replay");
    let flac = flac_of(&d::cascade_case(ALBUM_ONE));
    let tracks = release_tracks(d::disc_album_rg(), d::disc_album_rel())[13..16].to_vec();
    let dd = diffs(&flac, &tracks);
    assert!(
        dd.iter().filter(|x| x.abs() > 2.0).all(|x| *x > 0.0),
        "{dd:?}"
    );
    let worst = dd.iter().fold(0.0f64, |a, x| a.max(x.abs()));
    assert!(worst > POSITIONAL_MAX_DIFF_S && worst <= 2.0 * POSITIONAL_MAX_DIFF_S);
    assert!(matches!(
        positional_alignment(&flac, &tracks, &PositionalScope::WholeRelease),
        Err(PositionalRefusal::BeyondBound { .. })
    ));
    assert_eq!(
        by_position(&refine(&flac, &tracks, &PositionalScope::WholeRelease)),
        0
    );
}

// Positional alignment, negative: the closest measured wrong pairing overall — "Album One"
// against positions 1-3 of a Night Mix release (8.0 s at most, inside the
// bound, but the differences point both ways). Only the direction guard
// refuses it; an absolute bound alone would not separate it from rip-0001's
// right pairing at 8.1 s.
#[test]
fn closest_wrong_pairing_is_refused_by_direction() {
    musaic_testdata::require_private!("replay");
    let flac = flac_of(&d::cascade_case(ALBUM_ONE));
    let tracks = release_tracks(d::night_mix_rg(), d::night_mix_rel())[0..3].to_vec();
    let dd = diffs(&flac, &tracks);
    assert!(
        dd.iter().all(|x| x.abs() <= POSITIONAL_MAX_DIFF_S),
        "{dd:?}"
    );
    assert!(matches!(
        positional_alignment(&flac, &tracks, &PositionalScope::WholeRelease),
        Err(PositionalRefusal::MixedDirections { .. })
    ));
    let body = refine(&flac, &tracks, &PositionalScope::WholeRelease);
    assert_eq!(by_position(&body), 0);
    assert!(
        body["notes"][0]
            .as_str()
            .unwrap()
            .starts_with("Tier 1: not aligned by position: lengths differ in both directions")
    );
}

// Positional alignment, negative: re-orderings of rip-0001's own release (every rotation
// and the reversal) never align by position.
#[test]
fn shuffled_release_does_not_align() {
    musaic_testdata::require_private!("replay");
    let flac = flac_of(&d::cascade_case("real 0a rip-0001"));
    let tracks = release_tracks(d::conniff_rg(), d::conniff_rel());
    let mut orders: Vec<Vec<MbTrack>> = (1..tracks.len())
        .map(|k| {
            let mut t = tracks.clone();
            t.rotate_left(k);
            t
        })
        .collect();
    orders.push(tracks.iter().rev().cloned().collect());
    for t in &orders {
        assert!(positional_alignment(&flac, t, &PositionalScope::WholeRelease).is_err());
        assert_eq!(
            by_position(&refine(&flac, t, &PositionalScope::WholeRelease)),
            0
        );
    }
}

// Positional alignment, negative: a count mismatch never aligns by position, even when
// every comparable position is within the bound — rip-0001 without its last
// FLAC against its own 6-track release, and real-album-4 (disc 1) (12 FLACs)
// against its whole 25-track release. No trace line for a count mismatch.
#[test]
fn count_mismatch_does_not_align() {
    musaic_testdata::require_private!("replay");
    let mut flac = flac_of(&d::cascade_case("real 0a rip-0001"));
    flac.pop();
    let tracks = release_tracks(d::conniff_rg(), d::conniff_rel());
    assert_eq!(
        positional_alignment(&flac, &tracks, &PositionalScope::WholeRelease).unwrap_err(),
        PositionalRefusal::CountMismatch {
            flacs: 5,
            tracks: 6
        }
    );
    let body = refine(&flac, &tracks, &PositionalScope::WholeRelease);
    assert_eq!((aligned(&body), body["notes"].clone()), (0, json!([])));

    let flac = flac_of(&d::cascade_case(d::DISC_ALBUM_CASE));
    let tracks = release_tracks(d::disc_album_rg(), d::disc_album_rel());
    assert!(matches!(
        positional_alignment(&flac, &tracks, &PositionalScope::WholeRelease),
        Err(PositionalRefusal::CountMismatch {
            flacs: 12,
            tracks: 25
        })
    ));
}

// Positional alignment, negative: one outlier beyond the bound and NO track aligns by
// position. rip-0001's six FLACs with track 5 moved to its recorded length
// + 10.1 s (CONSTRUCTED; the measured +8.1 s is inside). Exactly + 10.0 s is
// still inside (the bound is inclusive, like the 2 s tolerance).
#[test]
fn one_outlier_beyond_the_bound_blocks_every_track() {
    musaic_testdata::require_private!("replay");
    let tracks = release_tracks(d::conniff_rg(), d::conniff_rel());
    let mut flac = flac_of(&d::cascade_case("real 0a rip-0001"));
    let len5 = tracks[4].length_s().unwrap();
    flac[4].duration = Some(len5 + 10.1);
    let body = refine(&flac, &tracks, &PositionalScope::WholeRelease);
    assert_eq!((aligned(&body), by_position(&body)), (0, 0));
    assert_eq!(
        body["notes"],
        json!(["Tier 1: not aligned by position: track 5 differs by +10.1 s, beyond 10 s"])
    );
    flac[4].duration = Some(len5 + POSITIONAL_MAX_DIFF_S);
    let body = refine(&flac, &tracks, &PositionalScope::WholeRelease);
    assert_eq!((aligned(&body), by_position(&body)), (6, 6));
}

// Positional matches are marked — confidence 0.8 (0.95 for the 2 s
// pass) on every canonical fact, a trace line — and the JSON shape is the
// Python app's (same keys on every track).
#[test]
fn positional_matches_are_marked_without_changing_the_shape() {
    musaic_testdata::require_private!("replay");
    let case = d::cascade_case("real 0a rip-0001");
    let body = resolve(&case);
    let python = &case["results"][0]["body"];
    for (t, o) in body["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .zip(python["tracks"].as_array().unwrap())
    {
        let keys = |v: &Value| v.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
        assert_eq!(keys(t), keys(o));
        for k in [
            "canonical_title",
            "canonical_artist",
            "canonical_tracknumber",
        ] {
            assert_eq!(t[k]["confidence"], json!(POSITIONAL_CONFIDENCE), "{k}");
            assert_eq!(t[k]["source"], "musicbrainz", "{k}");
        }
    }
    assert!(
        body["notes"]
            .as_array()
            .unwrap()
            .contains(&json!(d::conniff_positional_note(&case)))
    );
    // real-album-5 aligns by duration: 0.95, no positional note.
    let case = d::cascade_case(d::COMPILATION_CASE);
    let body = resolve(&case);
    assert_eq!(
        body["tracks"][0]["canonical_title"]["confidence"],
        json!(0.95)
    );
    assert!(
        !body["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n.as_str().unwrap().contains("aligned by position"))
    );
}

// Positional alignment for "(disc N of M)" folders: compared with medium N of the chosen
// release (listed order), never the whole release; track numbers are
// positions within the medium. Recorded listing of real-album-4 (media 12 + 13).
#[test]
fn disc_folder_uses_its_medium() {
    musaic_testdata::require_private!("replay");
    let tracks = release_tracks(d::disc_album_rg(), d::disc_album_rel());
    let sizes = release_medium_sizes(&browse(d::disc_album_rg()), d::disc_album_rel());
    let flac = flac_of(&d::cascade_case(d::DISC_ALBUM_CASE));
    let disc = |n: i64, s: Option<Vec<usize>>| PositionalScope::Disc {
        disc: n,
        medium_sizes: s,
    };
    let p = positional_alignment(&flac, &tracks, &disc(1, sizes.clone())).unwrap();
    assert_eq!(p.numbers, (1..=12).collect::<Vec<i64>>());
    assert_eq!(p.medium, Some(1));
    assert_eq!(
        positional_alignment(&flac, &tracks, &disc(2, sizes.clone())).unwrap_err(),
        PositionalRefusal::CountMismatch {
            flacs: 12,
            tracks: 13
        }
    );
    for (n, s) in [
        (3, sizes.clone()),
        (0, sizes.clone()),
        (1, None),
        (1, Some(vec![12])),
    ] {
        assert!(
            matches!(
                positional_alignment(&flac, &tracks, &disc(n, s.clone())),
                Err(PositionalRefusal::NoSuchMedium { .. })
            ),
            "disc {n} {s:?}"
        );
    }
}

// Positional alignment through the cascade for a disc-2 folder: a CONSTRUCTED "(disc 2 of
// 2)" sibling of real-album-4 whose 13 FLACs run uniformly 3.0 s
// longer than the recorded medium 2 (rounded to 0.1 s as the scanner does),
// numbered 1-13. The 2 s pass compares TRACKNUMBER with linear positions
// (disc 1's tracks) and the uniform offset defeats the duration fallback;
// the positional fallback aligns all 13 against medium 2, as track numbers
// 1-13 with medium 2's titles.
#[test]
fn disc_two_folder_aligns_against_medium_two() {
    musaic_testdata::require_private!("replay");
    let tracks = release_tracks(d::disc_album_rg(), d::disc_album_rel());
    let medium2 = &tracks[12..];
    let mut case = d::cascade_case(d::DISC_ALBUM_CASE);
    let dir = case["album"]["dir_name"]
        .as_str()
        .unwrap()
        .replace("(disc 1 of 2)", "(disc 2 of 2)");
    case["album"]["dir_name"] = json!(dir);
    case["album"]["disc_number"] = json!(2);
    let flacs: Vec<Value> = medium2
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let d = ((t.length_s().unwrap() + 3.0) * 10.0).round() / 10.0;
            json!({"filename": format!("{:02}.flac", i + 1), "tracknumber": (i + 1).to_string(),
                   "duration": d, "title": "x", "artist": ""})
        })
        .collect();
    case["filenames"] = json!(
        flacs
            .iter()
            .map(|f| f["filename"].clone())
            .collect::<Vec<_>>()
    );
    case["tracks"] = json!(flacs);
    case["fingerprints"] = json!([]);
    let body = resolve(&case);
    assert_eq!((aligned(&body), by_position(&body)), (13, 13));
    for (i, (t, m)) in body["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .zip(medium2)
        .enumerate()
    {
        assert_eq!(t["canonical_title"]["value"], m.title.as_str());
        assert_eq!(t["canonical_tracknumber"]["value"], (i + 1).to_string());
    }
    let note = body["notes"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|n| n.as_str().filter(|n| n.contains("aligned by position")))
        .unwrap()
        .to_owned();
    assert!(
        note.starts_with("Tier 1: 13 of 13 tracks aligned by position against medium 2"),
        "{note}"
    );
}
