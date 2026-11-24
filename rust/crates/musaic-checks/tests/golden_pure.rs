//! Pure helpers compared with the Python app's own functions
//! (testdata/golden/checks/pure.json): check metadata, inconsistent-artist-sort
//! token / classification / member normalisation / bulk-apply plan,
//! tags-out-of-sync article rotation and summaries, unidentified-album
//! summaries, duplicate labels and keep recommendation, track-gaps renumber
//! plans: detection building blocks and the bulk apply.

mod common;

use std::sync::{Arc, Mutex};

use common::{Sandbox, flac_bytes, golden, tags_of};
use musaic_checks::Registry;
use musaic_checks::checks::{duplicate_tracks as dt, inconsistent_artist_sort as ias};
use musaic_checks::checks::{tags_out_of_sync as toos, track_gaps as tg, unidentified_album as ua};
use musaic_checks::runner::meta_to_json;
use musaic_checks::wording;
use musaic_events::{Bus, Subscriber, Topic};
use serde_json::{Value as J, json};

fn pure() -> J {
    golden("pure.json")
}

fn s(v: &J) -> Option<&str> {
    v.as_str()
}

/// CheckMeta of every registered check, in registry order: the Python app's
/// except the descriptions of the three checks whose text names AI, each
/// replaced by its entry in `wording::NEUTRAL_DESCRIPTIONS` (the enumerated
/// delta). Each replaced description named AI in the Python app's record and
/// no longer does.
#[test]
fn check_meta_matches_the_python() {
    let reg = Registry::default();
    let got: Vec<J> = reg.all_meta().into_iter().map(meta_to_json).collect();
    let mut want = pure()["meta"].clone();
    let mut replaced = 0;
    for m in want.as_array_mut().unwrap() {
        let id = m["id"].as_str().unwrap().to_string();
        if let Some((_, d)) = wording::NEUTRAL_DESCRIPTIONS.iter().find(|(i, _)| *i == id) {
            assert!(ai_words(m["description"].as_str().unwrap()), "{id}");
            m["description"] = json!(d);
            replaced += 1;
        }
        assert!(!ai_words(m["description"].as_str().unwrap()), "{id}");
    }
    assert_eq!(replaced, 3);
    assert_eq!(J::Array(got), want);
}

/// The tags-out-of-sync summaries of the Python app's record, as shown.
/// Stored summaries are built exactly as recorded (above); an unanchored
/// summary is shown ending with `NO_ANCHOR_SUFFIX_NEUTRAL` instead of the
/// Python app's `NO_ANCHOR_SUFFIX` and nothing else changes.
#[test]
fn summaries_as_shown() {
    let p = pure();
    let mut changed = 0;
    for c in p["build_summary"].as_array().unwrap() {
        let out = c["out"].as_str().unwrap();
        let id = toos::META.id;
        let shown = wording::finding_summary(id, out);
        assert!(!ai_words(&shown), "{shown}");
        match out.strip_suffix(toos::NO_ANCHOR_SUFFIX) {
            Some(head) => {
                assert!(!c["has_anchor"].as_bool().unwrap());
                assert_eq!(shown, format!("{head}{}", toos::NO_ANCHOR_SUFFIX_NEUTRAL));
                changed += 1;
            }
            None => assert_eq!(shown, out),
        }
    }
    assert!(changed > 0);
}

/// `\bAI\b|Claude`, the frontend's AI_WORDS (src/test/aiVocabulary.test.tsx).
fn ai_words(s: &str) -> bool {
    regex::Regex::new(r"\bAI\b|Claude").unwrap().is_match(s)
}

/// Clustering token and never-normalise set.
#[test]
fn primary_token_and_never_normalize() {
    let p = pure();
    for c in p["primary_token"].as_array().unwrap() {
        assert_eq!(
            ias::primary_token(s(&c["in"])),
            c["out"].as_str().unwrap(),
            "{}",
            c["in"]
        );
    }
    for c in p["never_normalize"].as_array().unwrap() {
        assert_eq!(
            ias::is_never_normalize(s(&c["in"])),
            c["out"].as_bool().unwrap(),
            "{}",
            c["in"]
        );
    }
}

/// Cluster classification (tests/test_checks.py cases + edges),
/// canonical display, track_artists_json parsing.
#[test]
fn classify_display_parse() {
    let p = pure();
    for c in p["classify_cluster"].as_array().unwrap() {
        let members = c["members"].as_array().unwrap();
        let (state, value) =
            ias::classify_cluster(members.iter().map(|m| m.get("sort").and_then(J::as_str)));
        assert_eq!(json!([state.as_str(), value]), c["out"], "{}", c["members"]);
    }
    for c in p["canonical_display"].as_array().unwrap() {
        let credits: Vec<String> = c["in"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap().into())
            .collect();
        assert_eq!(ias::canonical_display(&credits), c["out"].as_str().unwrap());
    }
    for c in p["parse_track_artists_json"].as_array().unwrap() {
        let got: Vec<J> = ias::parse_track_artists_json(s(&c["in"]))
            .into_iter()
            .map(|t| json!({"filename": t.filename, "artist": t.artist, "artist_sort": t.artist_sort}))
            .collect();
        assert_eq!(J::Array(got), c["out"], "{}", c["in"]);
    }
}

/// Tags-out-of-sync building blocks.
#[test]
fn articles_normalize_summaries() {
    let p = pure();
    for c in p["rotate_article"].as_array().unwrap() {
        let v = c["in"].as_str().unwrap();
        assert_eq!(
            toos::starts_with_article(Some(v)),
            c["starts"].as_bool().unwrap(),
            "{v:?}"
        );
        assert_eq!(toos::rotate_article(v), c["out"].as_str().unwrap(), "{v:?}");
    }
    for c in p["normalize"].as_array().unwrap() {
        assert_eq!(
            toos::normalize(c["in"].as_str().unwrap()),
            c["out"].as_str().unwrap()
        );
    }
    for c in p["build_summary"].as_array().unwrap() {
        let mc: Vec<&str> = c["missing_core"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        let got = toos::build_summary(
            &mc,
            c["n_album"].as_u64().unwrap() as usize,
            c["n_track"].as_u64().unwrap() as usize,
            c["n_sort"].as_u64().unwrap() as usize,
            c["has_anchor"].as_bool().unwrap(),
        );
        assert_eq!(got, c["out"].as_str().unwrap());
    }
    for c in p["unidentified_summary"].as_array().unwrap() {
        let prev = c["previously_attempted"].as_bool().unwrap();
        let got = ua::summarize(
            c["needs_fp"].as_bool().unwrap(),
            c["needs_anchor"].as_bool().unwrap(),
            prev,
            prev.then_some("2020-01-01T00:00:00Z"),
        );
        assert_eq!(got, c["out"].as_str().unwrap());
    }
}

/// Duplicate-tracks summary label and keep recommendation.
#[test]
fn duplicate_label_and_keep() {
    let p = pure();
    for c in p["readable_label"].as_array().unwrap() {
        assert_eq!(
            dt::readable_label(c["in"].as_str().unwrap()),
            c["out"].as_str().unwrap(),
            "{}",
            c["in"]
        );
    }
    for c in p["recommended_keep"].as_array().unwrap() {
        let files: Vec<String> = c["in"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap().into())
            .collect();
        let (f, r) = dt::recommended_keep(&files);
        assert_eq!(json!({"filename": f, "reason": r}), c["out"], "{}", c["in"]);
    }
}

/// Member normalisation of the apply-sort endpoint.
#[test]
fn normalize_cluster_member() {
    for c in pure()["normalize_cluster_member"].as_array().unwrap() {
        assert_eq!(
            ias::normalize_cluster_member(&c["in"]).unwrap_or(J::Null),
            c["out"],
            "{}",
            c["in"]
        );
    }
}

/// The bulk-apply plan and its driver. The Python app's apply_direct
/// calls (recorded with in_bulk_run) equal the plan; every write runs with
/// in_bulk_run set; one BulkBatchCompleted iff a write succeeded.
/// Deliberate change: no direct sweep before the
/// publish (the Python app's was skipped in the golden: no library configured).
#[test]
fn apply_cluster_sort_plan_and_driver() {
    for c in pure()["apply_cluster_sort"].as_array().unwrap() {
        let members = c["members"].as_array().unwrap();
        let plan = ias::plan_cluster_sort(members, c["sort_value"].as_str().unwrap());
        let bus = Bus::new();
        let published = Arc::new(Mutex::new(Vec::<String>::new()));
        let p2 = published.clone();
        bus.register(
            Topic::Event,
            Subscriber::from_fn(move |e| {
                p2.lock().unwrap().push(e.name().to_string());
                Ok(())
            }),
        );
        let mut calls = Vec::new();
        let n = ias::apply_cluster_sort(&bus, &plan, |w| {
            let (album, tracks) = w.suggestions();
            calls.push(json!({
                "album_id": w.album_id,
                "in_bulk_run": musaic_events::in_bulk_run(),
                "suggestions": if album.is_empty() { J::Null } else {
                    J::Object(album.into_iter().map(|(k, v)| (k.to_string(), json!(v))).collect())
                },
                "track_suggestions": if tracks.is_empty() { J::Null } else {
                    J::Object(tracks.into_iter().map(|(f, k, v)| (f, json!({k: v}))).collect())
                },
            }));
            w.album_id != "fail"
        });
        assert_eq!(J::Array(calls), c["calls"]);
        assert_eq!(n as u64, c["succeeded"].as_u64().unwrap());
        assert_eq!(json!(*published.lock().unwrap()), c["published"]);
        assert!(!musaic_events::in_bulk_run());
    }
}

/// "also on error": a write that panics after another
/// succeeded still gets its one BulkBatchCompleted, the bulk-run flag is
/// reset, and the panic reaches the caller. Input: the golden's first plan.
#[test]
fn apply_cluster_sort_publishes_when_a_write_panics() {
    let golden = pure();
    let c = &golden["apply_cluster_sort"].as_array().unwrap()[0];
    let plan = ias::plan_cluster_sort(
        c["members"].as_array().unwrap(),
        c["sort_value"].as_str().unwrap(),
    );
    assert!(plan.len() >= 2, "needs two writes");
    let bus = Bus::new();
    let published = Arc::new(Mutex::new(Vec::<String>::new()));
    let p2 = published.clone();
    bus.register(
        Topic::Event,
        Subscriber::from_fn(move |e| {
            p2.lock().unwrap().push(e.name().to_string());
            Ok(())
        }),
    );
    let mut n = 0;
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        ias::apply_cluster_sort(&bus, &plan, |_| {
            n += 1;
            assert!(n < 2, "injected: second write");
            true
        })
    }));
    assert!(r.is_err(), "the panic reaches the caller");
    assert_eq!(*published.lock().unwrap(), ["BulkBatchCompleted"]);
    assert!(!musaic_events::in_bulk_run());
}

/// `try_quick_fix`'s propagate gate and message.
#[test]
fn propagate_quick_fix() {
    for c in pure()["quick_fix_propagate"].as_array().unwrap() {
        let detail = (!c["detail"].is_null()).then_some(&c["detail"]);
        match ias::propagate_plan(detail) {
            Ok((canonical, plan)) => {
                assert!(c["ok"].as_bool().unwrap());
                assert_eq!(plan.len(), c["calls"].as_array().unwrap().len());
                assert_eq!(
                    ias::propagate_message(&canonical, plan.len()),
                    c["message"].as_str().unwrap()
                );
            }
            Err(msg) => {
                assert!(!c["ok"].as_bool().unwrap());
                assert_eq!(msg, c["message"].as_str().unwrap());
                assert!(c["calls"].as_array().unwrap().is_empty());
            }
        }
    }
}

fn mb(v: &J) -> Vec<tg::MbTrack> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|t| tg::MbTrack {
            title: t["title"].as_str().unwrap().into(),
            position: t["position"].as_i64().unwrap(),
            recording_mbid: t["recording_mbid"].as_str().map(Into::into),
        })
        .collect()
}

/// Track-gaps fix tier 1 (title match over FLACs on disk) and tier 2
/// (fingerprint recordings) renumber plans.
#[test]
fn track_gaps_renumber_plans() {
    let p = pure();
    let (sb, _db) = Sandbox::new();
    for c in p["match_local_to_mb"].as_array().unwrap() {
        let dir = sb.root.join("tg").join(c["id"].as_str().unwrap());
        if let Some(files) = c["files"].as_array() {
            std::fs::create_dir_all(&dir).unwrap();
            for f in files {
                std::fs::write(
                    dir.join(f["name"].as_str().unwrap()),
                    flac_bytes(&tags_of(&f["tags"]), 3),
                )
                .unwrap();
            }
        }
        let got = tg::match_local_to_mb(&dir, &mb(&c["mb"]));
        assert_eq!(json!(got), c["out"], "{}", c["id"]);
    }
    for c in p["fingerprint_renumbering"].as_array().unwrap() {
        let local: Vec<(String, Option<String>)> = c["local"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| {
                (
                    l["filename"].as_str().unwrap().into(),
                    l["recording_mbid"].as_str().map(Into::into),
                )
            })
            .collect();
        assert_eq!(
            json!(tg::fingerprint_renumbering(&mb(&c["mb"]), &local)),
            c["out"],
            "{}",
            c["id"]
        );
    }
}
