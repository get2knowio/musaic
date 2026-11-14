//! The Tier 0b acceptance check against RECORDED MusicBrainz responses for
//! the real-audio fixture set (captures/mb-name-search, private).
//!
//! What is real here: the search and release-browse responses, and each
//! album's own track count and total duration (from the DSP goldens).
//! What is not: the accept/reject verdicts are the NEW rule's, computed by the
//! Python reference rule. The Python app has no such check.

mod common;

use base64::Engine as _;
use common::deltas as d;
use common::read_json;
use musaic_resolve::musicbrainz::{parse_release_browse, parse_search};
use musaic_resolve::name_search::{AlbumShape, ReleaseShape, accept_name_search};
use serde_json::Value;

/// Body of the recorded GET whose URL contains every one of `needles`.
fn recorded(needles: &[&str]) -> Vec<u8> {
    let mut hits = Vec::new();
    for entry in std::fs::read_dir(musaic_testdata::private_required("replay")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let rec = read_json(&path);
        let url = rec["url"].as_str().unwrap_or("");
        if rec["method"] == "GET" && rec["status"] == 200 && needles.iter().all(|n| url.contains(n))
        {
            hits.push(
                base64::engine::general_purpose::STANDARD
                    .decode(rec["content_b64"].as_str().unwrap())
                    .unwrap(),
            );
        }
    }
    assert_eq!(
        hits.len(),
        1,
        "expected exactly one 200 recording matching {needles:?}"
    );
    hits.pop().unwrap()
}

fn browse(release_group: &str) -> Vec<(String, ReleaseShape)> {
    let body = recorded(&[
        "musicbrainz.org/ws/2/release?",
        &format!("release-group={release_group}"),
    ]);
    parse_release_browse(&body).unwrap()
}

/// For every release-group browsed for the five real albums, the
/// Rust check agrees with the reference verdict and picks the same release.
#[test]
fn recorded_release_listings_give_the_reference_verdicts() {
    musaic_testdata::require_private!("replay", "captures/mb-name-search/mb/name-search.json");
    let g = read_json(&musaic_testdata::private_required(
        "captures/mb-name-search/mb/name-search.json",
    ));
    let (mut accepted, mut rejected) = (0, 0);
    for a in g["albums"].as_array().unwrap() {
        let album = AlbumShape {
            track_count: a["album"]["track_count"].as_u64().unwrap(),
            total_duration_s: a["album"]["duration_s"].as_f64().unwrap(),
            multi_disc: a["album"]["multi_disc"].as_bool().unwrap(),
        };
        for b in a["browsed"].as_array().unwrap() {
            let rg = b["release_group"].as_str().unwrap();
            let releases = browse(rg);
            assert_eq!(
                releases.len() as u64,
                b["release_count"].as_u64().unwrap(),
                "{rg}"
            );
            // Shapes parsed in Rust equal the ones the tool derived.
            for ((id, shape), want) in releases.iter().zip(b["releases"].as_array().unwrap()) {
                assert_eq!(id, want["id"].as_str().unwrap());
                assert_eq!(
                    shape.track_count,
                    want["track_count"].as_u64().unwrap(),
                    "{id}"
                );
                match (shape.duration_s, want["duration_s"].as_f64()) {
                    (Some(h), Some(w)) => assert!((h - w).abs() < 0.001, "{id}: {h} vs {w}"),
                    (None, None) => {}
                    other => panic!("{id}: duration {other:?}"),
                }
            }
            // A release-group reached by fingerprints has no search score; pass a
            // passing one so this exercises the track-count and duration test alone.
            let score = b["search_score"].as_u64().unwrap_or(100) as u32;
            let shapes: Vec<ReleaseShape> = releases.iter().map(|(_, s)| s.clone()).collect();
            let have = accept_name_search(score, &album, &shapes);
            let want = &b["name_search_check"];
            assert_eq!(
                have.is_ok(),
                want["accept"].as_bool().unwrap(),
                "{} / {rg}: {have:?}",
                a["album"]["source_dir"]
            );
            if let Ok(agreed) = have {
                accepted += 1;
                // The agreeing unit really is within both tolerances.
                let unit = &shapes[agreed.release];
                let (count, dur) = match agreed.medium {
                    Some(m) => (unit.mediums[m].track_count, unit.mediums[m].duration_s),
                    None => (unit.track_count, unit.duration_s),
                };
                assert!(count.abs_diff(album.track_count) <= 2);
                assert!(
                    (dur.unwrap() - album.total_duration_s).abs() / album.total_duration_s <= 0.03
                );
            } else {
                rejected += 1;
            }
        }
    }
    // Conniff by name, real-album-5 and real-album-4 by their fingerprint-preferred
    // group accept; the best fingerprint candidates of rip-0002 and rip-0003 do not.
    assert_eq!((accepted, rejected), (3, 2));
}

/// The acceptance check, positive, end to end on recorded data: the one real
/// album whose name search finds anything ("Ray Conniff" and the album's
/// title, score 100)
/// is accepted, and only narrowly on duration (about 2.1% against a 3% limit).
#[test]
fn conniff_name_search_hit_is_accepted() {
    musaic_testdata::require_private!("replay", "captures/mb-name-search/mb/name-search.json");
    let g = read_json(&musaic_testdata::private_required(
        "captures/mb-name-search/mb/name-search.json",
    ));
    let a = g["albums"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["album"]["title"] == d::conniff_album_title())
        .unwrap();
    let search = parse_search(&recorded(&[
        "musicbrainz.org/ws/2/release-group/?",
        "Ray+Conniff",
    ]))
    .unwrap();
    assert_eq!(search.len(), 1);
    assert_eq!(
        (search[0].score, search[0].title.as_str()),
        (100, d::conniff_mb_title())
    );
    assert_eq!(
        Value::from(search[0].release_group_id.clone()),
        a["search_candidates"][0]["id"]
    );

    let album = AlbumShape {
        track_count: 6,
        total_duration_s: a["album"]["duration_s"].as_f64().unwrap(),
        multi_disc: false,
    };
    let shapes: Vec<ReleaseShape> = browse(&search[0].release_group_id)
        .into_iter()
        .map(|(_, s)| s)
        .collect();
    let agreed = accept_name_search(search[0].score, &album, &shapes).unwrap();
    let ratio = (shapes[agreed.release].duration_s.unwrap() - album.total_duration_s).abs()
        / album.total_duration_s;
    assert!(ratio > 0.02 && ratio < 0.03, "{ratio}");
}

/// The acceptance check, negative (the case that motivated it): the synthetic four-track,
/// 40-second "Night Mix" that the Python app anchored on name alone at score
/// 100 is rejected against that release-group's recorded releases.
#[test]
fn synthetic_night_mix_is_rejected() {
    musaic_testdata::require_private!("replay", "captures/mb-name-search/mb/name-search.json");
    let shapes: Vec<ReleaseShape> = browse(d::night_mix_rg())
        .into_iter()
        .map(|(_, s)| s)
        .collect();
    assert!(!shapes.is_empty());
    let album = AlbumShape {
        track_count: 4,
        total_duration_s: 40.0,
        multi_disc: false,
    };
    assert!(accept_name_search(100, &album, &shapes).is_err());
}
