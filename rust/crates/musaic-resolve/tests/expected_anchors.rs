//! The fingerprint anchoring rule and its tie-break: reproduce
//! captures/expected-anchors/expected-anchors.json (private) for all 18
//! fixture albums from the recorded AcoustID lookups in the private replay set.
//!
//! That file is the reference for the deliberate change (the Python
//! reference rule, not the Python app, produced it).

mod common;

use common::{fixture_albums, read_json, title_and_multi_disc};
use musaic_resolve::{AlbumContext, Outcome, Rule, decide, matched_tracks};
use serde_json::Value;

const EXPECTED: &str = "captures/expected-anchors/expected-anchors.json";

fn round3(x: f64) -> f64 {
    format!("{x:.3}").parse().unwrap()
}

// Coverage, min tracks, count gap, score; the tie-break (winner + ties).
#[test]
fn reproduces_expected_anchors_for_every_fixture_album() {
    musaic_testdata::require_private!("replay", EXPECTED);
    let expected = read_json(&musaic_testdata::private_required(EXPECTED));
    let rule = Rule::default();
    // The rule recorded with the expected file is the rule we apply.
    let r = &expected["rule"];
    assert_eq!(r["coverage"].as_f64(), Some(rule.coverage));
    assert_eq!(r["min_tracks"].as_u64(), Some(rule.min_tracks as u64));
    assert_eq!(r["max_count_gap"].as_u64(), Some(rule.max_count_gap));
    assert_eq!(r["min_result_score"].as_f64(), Some(rule.min_result_score));
    assert_eq!(r["tiebreak"].as_str(), Some("v2"));

    let rows = expected["albums"].as_array().unwrap();
    let albums = fixture_albums();
    assert_eq!(albums.len(), 18);
    assert_eq!(rows.len(), albums.len());
    let mut anchored = 0;
    for (album, row) in albums.iter().zip(rows) {
        let ctx = format!("album {:?}", album.dir);
        assert_eq!(row["album"].as_str(), Some(album.dir.as_str()), "{ctx}");
        let (title, multi) = title_and_multi_disc(&album.dir);
        assert_eq!(
            row["title_used_for_ties"].as_str(),
            Some(title.as_str()),
            "{ctx}"
        );
        assert_eq!(row["multi_disc"].as_bool(), Some(multi), "{ctx}");
        let n = album.tracks.len();
        assert_eq!(row["tracks"].as_u64(), Some(n as u64), "{ctx}");
        assert_eq!(
            row["matched_tracks"].as_u64(),
            Some(matched_tracks(&album.tracks, rule.min_result_score) as u64),
            "{ctx}"
        );

        let d = decide(
            &album.tracks,
            &AlbumContext {
                track_count: n,
                title: &title,
                multi_disc: multi,
            },
            &[],
            &rule,
        );
        assert_eq!(
            row["reason"].as_str(),
            Some(d.outcome.reason_text().as_str()),
            "{ctx}"
        );
        match &d.winner {
            None => {
                assert!(row.get("winner").is_none(), "{ctx}: expected a winner");
                for k in [
                    "winner_on_tracks",
                    "coverage",
                    "count_gap",
                    "tied_candidates",
                ] {
                    assert!(row.get(k).is_none(), "{ctx}: unexpected {k}");
                }
            }
            Some(w) => {
                assert_eq!(
                    row["winner"].as_str(),
                    Some(w.release_group_id.as_str()),
                    "{ctx}"
                );
                assert_eq!(row["winner_title"].as_str(), w.title.as_deref(), "{ctx}");
                assert_eq!(
                    row["winner_on_tracks"].as_u64(),
                    Some(w.on_tracks as u64),
                    "{ctx}"
                );
                assert_eq!(row["coverage"].as_f64(), Some(round3(w.coverage)), "{ctx}");
                let gap = w.count_gap.map_or(Value::Null, Value::from);
                assert_eq!(row["count_gap"], gap, "{ctx}");
                assert_eq!(
                    row["tied_candidates"].as_u64(),
                    Some(w.tied_candidates as u64),
                    "{ctx}"
                );
            }
        }
        match &d.outcome {
            Outcome::Accepted(a) => {
                anchored += 1;
                assert_eq!(
                    row["anchor"].as_str(),
                    Some(a.release_group_id.as_str()),
                    "{ctx}"
                );
                assert_eq!(row["score"].as_u64(), Some(u64::from(a.score)), "{ctx}");
            }
            Outcome::Rejected(_) => {
                assert!(row["anchor"].is_null(), "{ctx}");
                assert!(row.get("score").is_none(), "{ctx}");
            }
        }
    }
    // Guard against a vacuous pass: the evidence has 3 anchored real albums.
    assert_eq!(anchored, 3);
}

// Every recorded release-group lookup is used, and the 94
// fixture files map onto the 93 recordings (two files of "Mono Harbor - Dupes"
// share a fingerprint, hence one recording).
#[test]
fn every_fixture_file_has_a_recorded_lookup() {
    musaic_testdata::require_private!("replay", EXPECTED);
    let lookups = common::recorded_lookups();
    let files: usize = fixture_albums().iter().map(|a| a.tracks.len()).sum();
    assert_eq!(files, 94);
    // 93 for the Python app's fpcalc fingerprints, plus the 52 recorded on
    // 2026-11-11 for this build's own fingerprints of the real-audio copy.
    assert_eq!(lookups.len(), 93 + 52);
}
