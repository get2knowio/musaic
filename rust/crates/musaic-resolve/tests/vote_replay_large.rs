//! Replay the 273-album anchoring experiment (the measurement over a real
//! library that chose the anchoring rule) through the Rust cascade.
//!
//! Ignored by default: its inputs are private and local-only. Driven by a
//! script kept with the private test data, which
//!
//! 1. splits the experiment's raw lookups (`$MUSAIC_VOTE_LOOKUPS`) into
//!    its albums (the file has no album column) and writes
//!    `$MUSAIC_VOTE_REPLAY_INPUT`;
//! 2. runs this test with `--ignored`;
//! 3. compares `$MUSAIC_VOTE_REPLAY_OUT` with the experiment, writing the
//!    per-album table and the aggregates.
//!
//! Each album goes through the real [`Resolver::resolve`]: an album row as
//! the scanner writes it (directory fields only; no tags), no stored anchor,
//! no blocklist, a MusicBrainz source that is offline (so Tier 0b finds
//! nothing, as "the name search did not anchor" in the experiment's setting,
//! and Tier 1 has no listing), and a fingerprint source answering each audio
//! file with the raw AcoustID results of its `lookups.jsonl` line, parsed by
//! the crate's own `LookupResponse`. The anchor the cascade returns is
//! recorded, together with `vote::decide` on the same inputs for the
//! winner's numbers (a rejected winner is not in the resolution). The test
//! itself only checks that the two agree; agreement with the experiment is
//! judged by the script.
//!
//! Environment: `MUSAIC_VOTE_LOOKUPS`, `MUSAIC_VOTE_REPLAY_INPUT`,
//! `MUSAIC_VOTE_REPLAY_OUT` (the script sets all three). A missing input is
//! a skip with a message, not a failure.

mod common;

use std::collections::BTreeSet;
use std::path::PathBuf;

use common::env::{NOW_EPOCH_S, World};
use common::read_json;
use musaic_resolve::source::{
    AlbumRecord, FingerprintLookups, FingerprintSource, MusicBrainzSource, NetError, TrackLookup,
};
use musaic_resolve::{
    AlbumContext, Env, LookupResponse, LookupResult, Outcome, Resolver, Rule, decide,
    matched_tracks,
};
use serde_json::{Value, json};

/// Every MusicBrainz / Cover Art Archive request fails as unreachable.
struct OfflineMb;

impl MusicBrainzSource for OfflineMb {
    fn search_release_groups(&self, _a: &str, _t: &str) -> Result<Vec<u8>, NetError> {
        Err(NetError::Network("offline (vote replay)".into()))
    }
    fn browse_releases(&self, _rg: &str) -> Result<Vec<u8>, NetError> {
        Err(NetError::Network("offline (vote replay)".into()))
    }
    fn browse_release_dates(&self, _rg: &str) -> Result<Vec<u8>, NetError> {
        Err(NetError::Network("offline (vote replay)".into()))
    }
    fn cover_front_status(&self, _rg: &str) -> Result<u16, NetError> {
        Err(NetError::Network("offline (vote replay)".into()))
    }
}

/// One `TrackLookup` per audio file, from the album's lookup lines.
struct RecordedLookups {
    tracks: Vec<(String, Vec<LookupResult>)>,
}

impl FingerprintSource for RecordedLookups {
    fn lookups(&self, _album: &AlbumRecord) -> FingerprintLookups {
        FingerprintLookups::Tracks(
            self.tracks
                .iter()
                .map(|(f, r)| TrackLookup {
                    filename: f.clone(),
                    results: Ok(r.clone()),
                })
                .collect(),
        )
    }
}

/// `trim()` keeps a medium's tracks as bare positions; AcoustID sends
/// `{"position": n}` objects.
fn untrim_medium_tracks(line: &mut Value) {
    let Some(results) = line.get_mut("results").and_then(Value::as_array_mut) else {
        return;
    };
    for res in results {
        let Some(recs) = res.get_mut("recordings").and_then(Value::as_array_mut) else {
            continue;
        };
        for rec in recs {
            let Some(rgs) = rec.get_mut("releasegroups").and_then(Value::as_array_mut) else {
                continue;
            };
            for rg in rgs {
                let Some(rels) = rg.get_mut("releases").and_then(Value::as_array_mut) else {
                    continue;
                };
                for rel in rels {
                    let Some(meds) = rel.get_mut("mediums").and_then(Value::as_array_mut) else {
                        continue;
                    };
                    for m in meds {
                        if let Some(ts) = m.get_mut("tracks").and_then(Value::as_array_mut) {
                            for t in ts.iter_mut() {
                                if !t.is_object() {
                                    *t = json!({ "position": t.clone() });
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn env_path(var: &str) -> Option<PathBuf> {
    std::env::var_os(var).map(PathBuf::from)
}

// The vote rule, run through the
// cascade on the experiment's raw lookups.
#[test]
#[ignore = "private local inputs; run through the private replay script"]
fn replay_the_large_vote_experiment() {
    let Some(lookups_path) = env_path("MUSAIC_VOTE_LOOKUPS") else {
        eprintln!("SKIPPED: MUSAIC_VOTE_LOOKUPS not set (set by the private replay script)");
        return;
    };
    let Some(input_path) = env_path("MUSAIC_VOTE_REPLAY_INPUT") else {
        eprintln!("SKIPPED: MUSAIC_VOTE_REPLAY_INPUT not set (set by the private replay script)");
        return;
    };
    let Some(out_path) = env_path("MUSAIC_VOTE_REPLAY_OUT") else {
        eprintln!("SKIPPED: MUSAIC_VOTE_REPLAY_OUT not set");
        return;
    };
    for p in [&lookups_path, &input_path] {
        if !p.is_file() {
            eprintln!("SKIPPED: {} is absent (private, local-only)", p.display());
            return;
        }
    }

    // The lines, parsed by the crate's own AcoustID response type. They are
    // `vote_experiment.trim()`med responses, identical to AcoustID's shape
    // except that a medium's `tracks` is the list of positions (`[3]`)
    // instead of objects (`[{"position": 3}]`); that one field is put back
    // into AcoustID's shape first (the rule reads only the medium's
    // `track_count`, never its tracks).
    let lines: Vec<Vec<LookupResult>> = std::fs::read_to_string(&lookups_path)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let mut v: Value = serde_json::from_str(l).unwrap();
            untrim_medium_tracks(&mut v);
            LookupResponse::from_json(&serde_json::to_vec(&v).unwrap())
                .unwrap_or_else(|e| panic!("unparseable lookup line: {e}"))
                .results
        })
        .collect();
    let input = read_json(&input_path);
    assert_eq!(
        input["lookup_lines"].as_u64(),
        Some(lines.len() as u64),
        "the input was made from another lookups file"
    );

    let rule = Rule::default();
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for a in input["albums"].as_array().unwrap() {
        let index = a["index"].as_u64().unwrap();
        assert!(seen.insert(index));
        let tracks: Vec<(String, Vec<LookupResult>)> = a["lines"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(i, l)| {
                (
                    format!("track-{:03}.flac", i + 1),
                    lines[l.as_u64().unwrap() as usize].clone(),
                )
            })
            .collect();
        let album_id = format!("vote-replay-{index}");
        let case = json!({
            "album": {
                "id": album_id,
                "path": format!("/vote-replay/{index}"),
                "dir_name": a["dir_name"],
                "dir_artist": a["dir_artist"],
                "dir_album": a["dir_album"],
                "is_compilation": a["is_compilation"],
                "disc_number": a["disc_number"],
                "disc_total": a["disc_total"],
                "track_count": tracks.len(),
                "total_seconds": null,
                "title": null, "album_artist": null, "artist": null,
            },
            "filenames": tracks.iter().map(|(f, _)| f.clone()).collect::<Vec<_>>(),
            "tracks": tracks.iter().map(|(f, _)| json!({"filename": f, "tracknumber": null, "duration": null})).collect::<Vec<_>>(),
            "fingerprints": [],
            "stored_anchor": null,
            "blocked": [],
        });
        let world = World::from_case(&case);
        let fps = RecordedLookups { tracks };
        let env = Env {
            albums: &world,
            store: &world,
            mb: &OfflineMb,
            fingerprints: &fps,
            now_epoch_s: NOW_EPOCH_S,
        };
        let r = Resolver::new().resolve(&env, &album_id, "", false).unwrap();
        let body = r.resolution.to_dict();

        // The same inputs straight into the rule, for the winner's numbers.
        let record = musaic_resolve::source::AlbumSource::album(&world, &album_id).unwrap();
        let results: Vec<Vec<LookupResult>> = fps.tracks.iter().map(|(_, r)| r.clone()).collect();
        let title = record.own_title().to_owned();
        let d = decide(
            &results,
            &AlbumContext {
                track_count: results.len(),
                title: &title,
                multi_disc: record.is_disc_directory(),
            },
            &[],
            &rule,
        );
        let accepted = d.outcome.anchor().cloned();
        // The cascade anchors exactly what the rule accepts, at its score.
        match &accepted {
            Some(an) => {
                assert_eq!(body["anchor_strategy"], "fingerprint-vote", "album {index}");
                assert_eq!(
                    body["anchor"],
                    json!({"value": an.release_group_id, "source": "fingerprint+mb",
                           "confidence": f64::from(an.score) / 100.0}),
                    "album {index}"
                );
                assert_eq!(
                    world.take_writes(),
                    vec![
                        json!({"album_id": album_id, "release_group_mbid": an.release_group_id,
                                "release_mbid": null, "score": an.score})
                    ],
                    "album {index}"
                );
            }
            None => {
                assert_eq!(body["anchor"], Value::Null, "album {index}");
                assert!(world.take_writes().is_empty(), "album {index}");
            }
        }
        let w = d.winner.as_ref();
        out.push(json!({
            "index": index,
            "tracks": results.len(),
            "title_used": title,
            "multi_disc": record.is_disc_directory(),
            "matched": matched_tracks(&results, rule.min_result_score),
            "winner": w.map(|w| w.release_group_id.clone()),
            "winner_on_tracks": w.map(|w| w.on_tracks),
            "winner_count_gap": w.and_then(|w| w.count_gap),
            "tied": w.map(|w| w.tied_candidates),
            "accepted": accepted.is_some(),
            "anchor_score": accepted.as_ref().map(|a| a.score),
            "reason": d.outcome.reason_text(),
            "cascade_anchor": body["anchor"]["value"],
            "cascade_strategy": body["anchor_strategy"],
            "cascade_notes": body["notes"],
            "rejected": matches!(d.outcome, Outcome::Rejected(_)),
        }));
    }
    std::fs::write(
        &out_path,
        serde_json::to_string_pretty(&json!({"albums": out, "rule": {
            "coverage": rule.coverage, "min_tracks": rule.min_tracks,
            "max_count_gap": rule.max_count_gap, "min_result_score": rule.min_result_score}}))
        .unwrap(),
    )
    .unwrap();
    eprintln!("replayed {} albums -> {}", out.len(), out_path.display());
}
