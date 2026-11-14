//! The cascade on the five real albums with Tier 0c ON, i.e. under the
//! DECIDED rules. AcoustID answers come from the
//! recorded lookups of the DSP-golden fingerprints through
//! `musaic_net::AcoustIdClient` over the replay transport; MusicBrainz from
//! the recorded replay set (private).
//!
//! Two references:
//! * captures/expected-anchors/expected-anchors.json (private) — which
//!   album anchors to which release-group at which score (deliberate change).
//! * testdata/golden/resolver/cascade.json "real 0a <album>" — the Python app's
//!   Tier 1 output for the same anchor (parity: tracks, cover, canonical
//!   fields, store writes).
//! * captures/real-resolutions/api (private) — the Python app's own bodies under its
//!   old vote (what changes, field by field).

mod common;

use std::collections::BTreeSet;

use common::deltas as d;
use common::env::{NOW_EPOCH_S, ReplayFingerprints, ReplayMb, World};
use common::read_json;
use musaic_resolve::{Env, Resolver};
use serde_json::{Value, json};

const EXPECTED: &str = "captures/expected-anchors/expected-anchors.json";
const REAL_CAPTURE: &str = "captures/real-resolutions/api";

fn golden_case(name: &str) -> Value {
    musaic_testdata::golden("resolver/cascade.json")["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("{name}"))
        .clone()
}

fn expected(dir: &str) -> Value {
    read_json(&musaic_testdata::private_required(EXPECTED))["albums"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["album"] == dir)
        .unwrap()
        .clone()
}

fn capture_body(slug: &str) -> Value {
    let dir = musaic_testdata::private_required(REAL_CAPTURE);
    let entry = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| {
            let n = p.file_name().unwrap().to_str().unwrap();
            n.contains("-resolution-") && n.contains(slug)
        })
        .unwrap();
    read_json(&entry)["response"]["json"].clone()
}

struct Run {
    body: Value,
    events: Vec<(String, String)>,
    writes: Value,
    world: World,
    fps: ReplayFingerprints,
}

fn run(case: &Value, dir: &str, blocked: &[&str]) -> Run {
    let mut case = case.clone();
    case["stored_anchor"] = Value::Null; // start unanchored: the cascade must find it
    case["blocked"] = json!(blocked);
    let world = World::from_case(&case);
    let mb = ReplayMb::new(&json!([]));
    let fps = ReplayFingerprints::new(dir);
    let env = Env {
        albums: &world,
        store: &world,
        mb: &mb,
        fingerprints: &fps,
        now_epoch_s: NOW_EPOCH_S,
    };
    let r = Resolver::new()
        .resolve(&env, case["album"]["id"].as_str().unwrap(), "", false)
        .unwrap();
    let writes = Value::from(world.take_writes());
    Run {
        body: r.resolution.to_dict(),
        events: r
            .events
            .iter()
            .map(|e| (e.album_id.clone(), e.source.clone()))
            .collect(),
        writes,
        world,
        fps,
    }
}

const TIER1_KEYS: [&str; 9] = [
    "canonical_title",
    "canonical_album_artist",
    "canonical_year",
    "canonical_album_artist_sort",
    "canonical_genre",
    "cover_url",
    "tracks",
    "unresolved_track_count",
    "album_id",
];

// Deliberate change in Tier 0c; Tier 1 is the Python app's plus the
// enumerated deltas of common::deltas::real_album_deltas (album fields from
// the recorded release-group lookup, the best-fit release on the compilation,
// positional alignment on rip-0001): the three real albums the new rule
// anchors get exactly the expected anchor and score, are persisted, publish the Python app's event sequence for a
// Tier 0c anchor, and their Tier 1 output equals the Python app's for that anchor
// with those deltas applied.
#[test]
fn real_albums_anchor_by_coverage_and_refine_like_the_python() {
    musaic_testdata::require_private!("replay", EXPECTED, REAL_CAPTURE);
    let deltas = d::real_album_deltas();
    for (label, dir) in [
        ("rip-0001", "rip-0001"),
        ("real-album-5", d::compilation_dir()),
        ("real-album-4", d::disc_album_dir()),
    ] {
        let python = golden_case(&format!("real 0a {label}"));
        let want = expected(dir);
        let r = run(&python, dir, &[]);
        let rg = want["anchor"].as_str().unwrap();
        let score = want["score"].as_i64().unwrap();
        assert_eq!(
            r.body["anchor"],
            json!({"value": rg, "source": "fingerprint+mb", "confidence": score as f64 / 100.0}),
            "{label}"
        );
        assert_eq!(r.body["anchor_strategy"], "fingerprint-vote", "{label}");
        let notes = r.body["notes"].as_array().unwrap();
        // Rip-0001 (0 of 6 at 2 s) aligns by position; the trace
        // says so. The other two already align fully.
        let positional = label == "rip-0001";
        // The release-group lookup is recorded for all three, so
        // it succeeds and adds no trace line (the album fields it gives are
        // checked through the deltas below).
        assert!(d::recorded_group_lookups().contains(&rg), "{label}");
        assert_eq!(notes.len(), 1 + positional as usize, "{label}: {notes:?}");
        if positional {
            assert_eq!(notes[1], d::conniff_positional_note(&python), "{label}");
        }
        let note = notes[0].as_str().unwrap();
        assert!(
            note.starts_with(&format!(
                "Tier 0c: {}/{} tracks on \"{}\" ({rg})",
                want["winner_on_tracks"],
                want["tracks"],
                want["winner_title"].as_str().unwrap()
            )),
            "{note}"
        );
        assert!(
            note.ends_with(&format!("{} tied", want["tied_candidates"])),
            "{note}"
        );
        // Tier 1 parity with the Python app's resolve of the same anchor, plus
        // the enumerated deltas.
        let o = &python["results"][0];
        let mut want = json!({
            "body": o["body"], "events": o["events"], "store_writes": o["store_writes"],
            "store_after": o["store_after"],
        });
        let mut reqs = BTreeSet::new();
        let scenario = format!("real 0a {label}");
        let listed = deltas
            .iter()
            .filter(|x| x.scenario == scenario && x.steps.contains(&0))
            .count();
        // Every listed delta applies (at least the four album fields and
        // the lookup request).
        assert!(listed >= 5, "{label}: {listed}");
        assert_eq!(
            d::apply(&deltas, &scenario, 0, &mut want, &mut reqs),
            listed,
            "{label}"
        );
        let o = &want;
        for k in TIER1_KEYS {
            assert_eq!(r.body[k], o["body"][k], "{label}: {k}");
        }
        // Writes: the Tier 0c anchor (no release), then the Python app's Tier 1
        // release backfill with the same score.
        let backfill = &o["store_writes"][0];
        assert_eq!(
            r.writes,
            json!([
                {"album_id": backfill["album_id"], "release_group_mbid": rg, "release_mbid": null, "score": score},
                {"album_id": backfill["album_id"], "release_group_mbid": rg,
                 "release_mbid": backfill["release_mbid"], "score": score},
            ]),
            "{label}"
        );
        let id = backfill["album_id"].as_str().unwrap().to_owned();
        assert_eq!(
            r.events,
            vec![
                (id.clone(), "musicbrainz".into()),
                (id, "fingerprint+mb".into())
            ],
            "{label}"
        );
        assert_eq!(*r.fps.calls.borrow(), 1);
        drop(r.world);
    }
}

// Fingerprint anchor, negative: rip-0002 and rip-0003 stay unanchored and
// nothing is persisted. rip-0002 was unanchored in the Python app too: its body
// equals the Python app capture except for one added trace line (the coverage vote puts
// the rule's verdict in the trace). rip-0003 is the deliberate reversal
// of the Python app's one-track anchor.
#[test]
fn rejected_real_albums_stay_unanchored() {
    musaic_testdata::require_private!("replay", EXPECTED, REAL_CAPTURE);
    for (label, slug) in [("rip-0002", "rip-0002"), ("rip-0003", "rip-0003")] {
        let python = golden_case(&format!("real 0a {label}"));
        let want = expected(label);
        let r = run(&python, label, &[]);
        assert_eq!(r.body["anchor"], Value::Null, "{label}");
        assert!(r.writes.as_array().unwrap().is_empty(), "{label}");
        assert!(r.events.is_empty(), "{label}");
        let notes: Vec<&str> = r.body["notes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n.as_str().unwrap())
            .collect();
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(
            notes[0].starts_with(&format!(
                "Tier 0c: no anchor: {}/{} tracks on \"{}\" ({})",
                want["winner_on_tracks"],
                want["tracks"],
                want["winner_title"].as_str().unwrap(),
                want["winner"].as_str().unwrap()
            )),
            "{notes:?}"
        );
        assert!(
            notes[0].ends_with(want["reason"].as_str().unwrap()),
            "{notes:?}"
        );
        // Compare with the Python app's own body (capture, album id placeholder).
        let mut cap = capture_body(slug);
        let mut body = r.body.clone();
        body["album_id"] = cap["album_id"].clone();
        body["notes"] = json!([notes[1]]);
        if label == "rip-0003" {
            // Deliberate: the Python app anchored a release-group on one track
            // and aligned 4 of 12 tracks against it.
            assert_eq!(cap["anchor_strategy"], "fingerprint-vote");
            assert_eq!(cap["unresolved_track_count"], 8);
            for k in [
                "anchor",
                "anchor_strategy",
                "notes",
                "unresolved_track_count",
            ] {
                cap[k] = body[k].clone();
            }
            for t in cap["tracks"].as_array_mut().unwrap() {
                for k in [
                    "canonical_title",
                    "canonical_artist",
                    "canonical_tracknumber",
                    "canonical_duration_s",
                ] {
                    t[k] = Value::Null;
                }
            }
        }
        assert_eq!(body, cap, "{label}");
    }
}

// Tier order: with Tier 0c available, a directory name the search finds still
// anchors at Tier 0b (cheapest first, first match wins) and the fingerprint
// lookups are never made. Equal to the Python app's 0b scenario (the Tier 0b
// acceptance check passes on the recorded listing; the cover check is
// answered by the recorded CAA HEAD on both sides) plus two deltas: the album
// fields come from the search hit, and every track aligns by position.
#[test]
fn name_search_wins_before_fingerprints_are_asked() {
    musaic_testdata::require_private!("replay", EXPECTED, REAL_CAPTURE);
    let python = golden_case("conniff 0b by directory name");
    let world = World::from_case(&python);
    let mb = ReplayMb::new(&json!([]));
    let fps = ReplayFingerprints::new("rip-0001");
    let env = Env {
        albums: &world,
        store: &world,
        mb: &mb,
        fingerprints: &fps,
        now_epoch_s: NOW_EPOCH_S,
    };
    let r = Resolver::new()
        .resolve(&env, python["album"]["id"].as_str().unwrap(), "", false)
        .unwrap();
    let mut want = json!({"body": python["results"][0]["body"]});
    let s = "conniff 0b by directory name";
    let mut deltas = d::conniff_search_hit_deltas(s);
    // 6 of 6 aligned by position, the trace line after the 0b one.
    let old_notes: Vec<String> = python["results"][0]["body"]["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_str().unwrap().to_owned())
        .collect();
    deltas.extend(d::conniff_positional_deltas(
        s,
        &[0],
        &python,
        &old_notes,
        1,
    ));
    let n = d::apply(&deltas, s, 0, &mut want, &mut BTreeSet::new());
    assert_eq!(n, deltas.len());
    assert_eq!(r.resolution.to_dict(), want["body"]);
    assert_eq!(*fps.calls.borrow(), 0);
}

// A blocklisted release-group can neither be the Tier 0c
// anchor nor be persisted, even though every track points at it.
#[test]
fn blocklisted_group_is_never_the_fingerprint_anchor() {
    musaic_testdata::require_private!("replay", EXPECTED, REAL_CAPTURE);
    let blocked = d::conniff_rg();
    let python = golden_case("real 0a rip-0001");
    let r = run(&python, "rip-0001", &[blocked]);
    assert_ne!(r.body["anchor"]["value"], blocked);
    for w in r.writes.as_array().unwrap() {
        assert_ne!(w["release_group_mbid"], blocked);
    }
    let notes = r.body["notes"].as_array().unwrap();
    assert_eq!(
        notes[0],
        "Blocklist active (1 release-group(s) rejected via Wrong match)"
    );
    assert!(!notes[1].as_str().unwrap().contains(blocked), "{notes:?}");
}

// Tier 0b acceptance check and fingerprint anchor (deliberate change): the synthetic "Night Mix" that the
// Python app anchored by name is rejected at Tier 0b, and with fingerprints
// available Tier 0c finds no candidate either (expected-anchors: "no
// candidate release-group"), so it stays unanchored with both verdicts traced.
#[test]
fn night_mix_stays_unanchored_with_fingerprints_available() {
    musaic_testdata::require_private!("replay", EXPECTED, REAL_CAPTURE);
    let case = golden_case("night mix 0b by name (negative: no anchor without a duration check)");
    let want = expected("Various Artists - Night Mix [FLAC]");
    let r = run(&case, "Various Artists - Night Mix [FLAC]", &[]);
    assert_eq!(r.body["anchor"], Value::Null);
    assert!(r.writes.as_array().unwrap().is_empty());
    let notes: Vec<&str> = r.body["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_str().unwrap())
        .collect();
    assert_eq!(notes.len(), 3, "{notes:?}");
    assert!(notes[0].starts_with("Tier 0b: rejected "), "{notes:?}");
    assert_eq!(
        notes[1],
        format!("Tier 0c: no anchor: {}", want["reason"].as_str().unwrap())
    );
    assert_eq!(*r.fps.calls.borrow(), 1);
}
