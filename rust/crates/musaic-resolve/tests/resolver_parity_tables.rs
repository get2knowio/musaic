//! Parity with the Python app on function-level tables exported by
//! the golden generator (Python app run in replay mode, plus constructed
//! responses stored in the golden). Every case here is PARITY unless a comment
//! names a deliberate difference.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use common::env::{ReplayMb, album_record, flac_track};
use musaic_resolve::content_hash;
use musaic_resolve::find_release::{FindOptions, MbMatch, MbTrack, find_release, parse_tracklist};
use musaic_resolve::resolution::{AlbumResolution, FieldFact, FingerprintMap, refine_from_anchor};
use musaic_resolve::source::{AlbumRecord, MusicBrainzSource};
use serde_json::{Value, json};

fn golden(name: &str) -> Value {
    musaic_testdata::golden(&format!("resolver/{name}"))
}

fn opt(v: &Value) -> Option<String> {
    v.as_str().map(str::to_owned)
}

fn track_json(t: &MbTrack) -> Value {
    json!({"position": t.position, "title": t.title, "length_ms": t.length_ms,
           "artist": t.artist, "recording_mbid": t.recording_mbid})
}

/// The Python app's request list as a set of (method, replay key). Rust keys the
/// same requests the same way (`Request::replay_key`); order and repeats are
/// not compared (pacing: replay matches by identity, not order or count).
fn key_set(reqs: &Value) -> BTreeSet<(String, String)> {
    reqs.as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["method"].as_str().unwrap().to_owned(),
                r["key"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn logged_set(mb: &ReplayMb) -> BTreeSet<(String, String)> {
    mb.take_log()
        .into_iter()
        .map(|l| (l.method, l.key))
        .collect()
}

fn find_album(a: &Value) -> AlbumRecord {
    let mut a = a.clone();
    a["id"] = json!("table");
    album_record(&a, None)
}

// The name search (query terms, blocklist, hint reranking, year fallback,
// genre, notes/source) and its failures (HTTP / parse / network failures as an
// error source, never a raise): every row of find_release.json.
#[test]
fn find_release_matches_the_python_app_table() {
    musaic_testdata::require_private!("replay");
    let g = golden("find_release.json");
    let cases = g["cases"].as_array().unwrap();
    assert!(cases.len() >= 35);
    let mut hinted_away_from_first = 0;
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let mb = ReplayMb::new(&case["overlay"]);
        let blocked: BTreeSet<String> = case["blocked"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b.as_str().unwrap().to_owned())
            .collect();
        let m = find_release(
            &find_album(&case["album"]),
            FindOptions {
                hint: case["hint"].as_str().unwrap(),
                blocked: &blocked,
                fetch_cover: false,
                fetch_tracklist: false,
            },
            &mb,
        );
        let want = &case["match"];
        assert_eq!(m.confident, want["confident"].as_bool().unwrap(), "{name}");
        assert_eq!(m.score, want["score"].as_i64().unwrap(), "{name}");
        assert_eq!(m.confidence, want["confidence"].as_str().unwrap(), "{name}");
        assert_eq!(m.mbid, opt(&want["mbid"]), "{name}");
        assert_eq!(m.release_mbid, opt(&want["release_mbid"]), "{name}");
        assert_eq!(m.artist, opt(&want["artist"]), "{name}");
        assert_eq!(m.title, opt(&want["title"]), "{name}");
        assert_eq!(m.year, opt(&want["year"]), "{name}");
        assert_eq!(m.primary_genre, opt(&want["primary_genre"]), "{name}");
        assert_eq!(m.cover_url, opt(&want["cover_url"]), "{name}");
        assert_eq!(m.source, want["source"].as_str().unwrap(), "{name}");
        let notes = want["notes"].as_str().unwrap();
        if m.source == "MusicBrainz (network error)" {
            // Deliberate (cosmetic): the Python app writes the Python exception
            // type and message ("ConnectionError: unobserved in replay set ...").
            assert!(notes.starts_with("ConnectionError: "), "{name}");
            assert!(m.notes.starts_with("NetworkError: "), "{name}: {}", m.notes);
        } else {
            assert_eq!(m.notes, notes, "{name}");
        }
        assert_eq!(
            logged_set(&mb),
            key_set(&case["requests"]),
            "{name}: requests"
        );
        if !case["hint"].as_str().unwrap().is_empty()
            && m.mbid.is_some()
            && case["overlay"].as_array().unwrap().is_empty()
            && m.mbid.as_deref() != Some(common::deltas::night_mix_rg())
        {
            hinted_away_from_first += 1;
        }
    }
    // Guard: the recorded Night Mix rows really exercise the hint bias.
    assert!(hinted_away_from_first >= 3);
}

// Tier 1 input: `_fetch_tracklist` on the six recorded release listings and
// on constructed ones (date order, unusable releases, multi-disc numbering,
// artist credits, lengths).
#[test]
fn tracklists_match_the_python() {
    musaic_testdata::require_private!("replay");
    let g = golden("tracklists.json");
    for case in g["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let mb = ReplayMb::new(&case["overlay"]);
        let got = mb
            .browse_releases(case["release_group"].as_str().unwrap())
            .and_then(|b| parse_tracklist(&b));
        let want = &case["result"];
        match got {
            Ok((rid, tracks)) => {
                assert_eq!(rid, want["release_mbid"].as_str().unwrap(), "{name}");
                let t: Vec<Value> = tracks.iter().map(track_json).collect();
                assert_eq!(Value::from(t), want["tracks"], "{name}");
            }
            // The Python app raises RuntimeError on a non-200 browse.
            Err(e) => assert_eq!(want["error"], "RuntimeError", "{name}: {e}"),
        }
        assert_eq!(logged_set(&mb), key_set(&case["requests"]), "{name}");
    }
}

fn mb_match(m: &Value) -> MbMatch {
    MbMatch {
        mbid: opt(&m["mbid"]),
        title: opt(&m["title"]),
        artist: opt(&m["artist"]),
        year: opt(&m["year"]),
        primary_genre: opt(&m["primary_genre"]),
        cover_url: opt(&m["cover_url"]),
        cover_url_verified: m["cover_url_verified"].as_bool().unwrap(),
        tracklist: m["tracklist"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| MbTrack {
                position: t["position"].as_i64().unwrap(),
                title: t["title"].as_str().unwrap().to_owned(),
                length_ms: t["length_ms"].as_i64(),
                artist: opt(&t["artist"]),
                recording_mbid: opt(&t["recording_mbid"]),
            })
            .collect(),
        ..MbMatch::default()
    }
}

/// The refine.json cases with no tracklist to
/// align against. The Python app lists no track and counts 0; this build lists
/// every FLAC with its fingerprint fields and counts each one unresolved.
const NO_TRACKLIST_CASES: [&str; 3] = ["empty tracklist", "anchor mismatch", "no mbid"];

/// The refine.json cases where the track counts
/// match but the 2 s pass leaves a track unaligned, so the positional
/// fallback is considered and declined. Alignment is the Python app's; only a
/// trace line is added (the Python app's `notes` is empty). Figures from the
/// case's own inputs (FLAC duration − MB length):
/// * "misaligned duration": track 2 is 225.0 − 195.0 = +30.0 s (> 10 s);
/// * "a used position": +5.0 and −9.5 s, both beyond 2 s, opposite signs;
/// * "zero / missing durations": track 1 has duration 0.0.
const POSITIONAL_REFUSALS: [(&str, &str); 3] = [
    (
        "misaligned duration unresolved (Python app test)",
        "Tier 1: not aligned by position: track 2 differs by +30.0 s, beyond 10 s",
    ),
    (
        "zero / missing durations skip the duration test",
        "Tier 1: not aligned by position: track 1 has no length to compare",
    ),
    (
        "a used position is not reused",
        "Tier 1: not aligned by position: lengths differ in both directions (-9.5 to +5.0 s)",
    ),
];

/// The case's own FLACs as bare, unresolved track entries.
fn bare_tracks(case: &Value) -> Value {
    let fp = case["fingerprint_map"].as_object().unwrap();
    Value::from(
        case["flac_tracks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| {
                let name = t["filename"].as_str().unwrap_or("");
                let row = fp.get(name);
                json!({
                    "filename": name,
                    "recording_mbid": row.map_or(Value::Null, |r| r[0].clone()),
                    "fingerprint_score": row.map_or(Value::Null, |r| r[1].clone()),
                    "fingerprinted": row.is_some(),
                    "canonical_title": null, "canonical_artist": null,
                    "canonical_tracknumber": null, "canonical_duration_s": null,
                })
            })
            .collect::<Vec<_>>(),
    )
}

// Tier 1 alignment (position + 2.0 s tolerance, unique-duration fallback,
// unresolved count) and the to_dict shape on the Python app's own
// `_refine_from_anchor`, with the no-tracklist delta on exactly the cases in NO_TRACKLIST_CASES
// (`tracks`: [] -> every FLAC bare; `unresolved_track_count`: 0 -> FLAC count)
// and the positional alignment delta on exactly the cases in POSITIONAL_REFUSALS (`notes`:
// [] -> the refusal line). No refine.json case aligns by position.
#[test]
fn tier_1_alignment_matches_the_python_app_table() {
    let g = golden("refine.json");
    let cases = g["cases"].as_array().unwrap();
    assert!(cases.len() >= 18);
    let mut changed = Vec::new();
    let mut refused = Vec::new();
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let mut res = AlbumResolution::new("album-1");
        res.anchor = Some(FieldFact::new(
            case["anchor"].as_str().unwrap(),
            "cache",
            0.9,
        ));
        res.anchor_strategy = "cache".into();
        let flac: Vec<_> = case["flac_tracks"]
            .as_array()
            .unwrap()
            .iter()
            .map(flac_track)
            .collect();
        let fp: FingerprintMap = case["fingerprint_map"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), (opt(&v[0]), v[1].as_f64())))
            .collect::<BTreeMap<_, _>>();
        refine_from_anchor(&mut res, &mb_match(&case["match"]), &flac, &fp);
        let mut want = case["resolution"].clone();
        if NO_TRACKLIST_CASES.contains(&name) {
            assert_eq!(want["tracks"], json!([]), "{name}: the Python app's record");
            assert_eq!(
                want["unresolved_track_count"], 0,
                "{name}: the Python app's record"
            );
            want["tracks"] = bare_tracks(case);
            want["unresolved_track_count"] = json!(flac.len());
            assert!(!flac.is_empty(), "{name}");
            changed.push(name);
        }
        if let Some((_, note)) = POSITIONAL_REFUSALS.iter().find(|(c, _)| *c == name) {
            assert_eq!(want["notes"], json!([]), "{name}: the Python app's record");
            want["notes"] = json!([note]);
            refused.push(name);
        }
        assert_eq!(res.to_dict(), want, "{name}");
    }
    assert_eq!(changed, NO_TRACKLIST_CASES);
    let mut listed: Vec<&str> = POSITIONAL_REFUSALS.iter().map(|(c, _)| *c).collect();
    listed.sort_unstable();
    refused.sort_unstable();
    assert_eq!(refused, listed);
}

// The memo key is the Python app's `_content_hash`, byte for byte
// (sorted filenames, Python float repr, ensure_ascii).
#[test]
fn content_hash_matches_the_python() {
    let g = golden("content_hash.json");
    let mut n = 0;
    for case in g["cases"].as_array().unwrap() {
        let mut a = case["album"].clone();
        a["id"] = json!("x");
        let files: Vec<String> = case["filenames"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f.as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            content_hash(&album_record(&a, None), &files),
            case["content_hash"].as_str().unwrap(),
            "{a}"
        );
        n += 1;
    }
    for case in golden("cascade.json")["cases"].as_array().unwrap() {
        if case["album"].is_null() {
            continue;
        }
        let files: Vec<String> = case["filenames"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f.as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            content_hash(&album_record(&case["album"], None), &files),
            case["content_hash"].as_str().unwrap(),
            "{}",
            case["name"]
        );
        n += 1;
    }
    // 19 of the cascade albums are in the private half of cascade.json.
    if musaic_testdata::json("golden/resolver/cascade.json").private {
        assert!(n >= 25);
    } else {
        assert!(n >= 18);
        musaic_testdata::skip_private_cases();
    }
}
