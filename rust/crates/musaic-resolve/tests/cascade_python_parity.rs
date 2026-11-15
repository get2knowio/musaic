//! The whole cascade against the Python app's `resolve_album`, scenario by
//! scenario (`testdata/golden/resolver/cascade.json`, Python app run in replay
//! mode with the album-side inputs stubbed and fingerprinting unavailable, as
//! tests/test_resolver.py does).
//!
//! For each resolve step the Rust result must equal the Python app's record —
//! `to_dict()` body, `AlbumMbAnchorSet` events in order, anchor-store writes,
//! the stored anchor afterwards, whether the memo served it, and the set of
//! requests made — after applying the deliberate deltas enumerated in
//! [`deltas`] (scenario, step, field, Python app value -> new value, why).
//! Any other difference fails, and so does a listed delta that is absent.

mod common;

use std::collections::BTreeSet;

use common::deltas::{self as d, Delta, add_req, bare_tracks, fact, remove_req, set};
use common::env::{NOW_EPOCH_S, NoFingerprints, ReplayMb, World};
use musaic_resolve::find_release::search_terms;
use musaic_resolve::source::MusicBrainzSource;
use musaic_resolve::{Env, Resolver};
use serde_json::{Value, json};

fn cases() -> Vec<Value> {
    musaic_testdata::golden("resolver/cascade.json")["cases"]
        .as_array()
        .unwrap()
        .clone()
}

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

/// The Python app's recorded notes of a scenario's first step.
fn notes_of(name: &str) -> Vec<String> {
    case_named(name)["results"][0]["body"]["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_str().unwrap().to_owned())
        .collect()
}

fn case_named(name: &str) -> Value {
    cases()
        .into_iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("{name}"))
}

const MEDIUM_RG: &str = "55555555-5555-5555-5555-555555555555";
const OTHER_RG: &str = "66666666-6666-6666-6666-666666666666";
const UNRECORDED_RG: &str = "77777777-7777-7777-7777-777777777777";

/// The trace line for a failed Tier 0b search (network failures are traced); the error is the one
/// the replay source gives for that scenario's search.
fn search_failed_note(case: &Value) -> String {
    let album = common::env::album_record(&case["album"], None);
    let (artist, title) = search_terms(&album);
    let e = ReplayMb::new(&case["overlay"])
        .search_release_groups(&artist, &title)
        .unwrap_err();
    format!("Tier 0b: MusicBrainz search failed ({e})")
}

/// A synthetic cached anchor whose Tier 1 has no tracklist: the three FLACs listed bare and counted unresolved.
fn unresolved_deltas(scenario: &str, case: &Value) -> Vec<Delta> {
    vec![
        set(
            scenario,
            &[0],
            "no tracklist",
            "/body/tracks",
            json!([]),
            bare_tracks(case),
        ),
        set(
            scenario,
            &[0],
            "no tracklist",
            "/body/unresolved_track_count",
            json!(0),
            json!(3),
        ),
    ]
}

/// Every deliberate difference from the Python app's record in cascade.json.
fn deltas() -> Vec<Delta> {
    let mut out = d::real_album_deltas();

    // Tier 0b anchors: canonical album fields from the search hit.
    // No new request: the hit already carries them.
    for s in ["conniff 0b by directory name", "conniff 0b with hint"] {
        out.extend(d::conniff_search_hit_deltas(s));
    }

    // Fresh cached anchor, release already known: album fields from the recorded
    // release-group lookup.
    out.extend(d::group_field_deltas(
        "conniff cached anchor with release already known",
        &[0],
        &[0],
        d::conniff_rg(),
    ));
    // Memo: same body on every resolve (step 1 is the memo hit and asks nothing).
    out.extend(d::group_field_deltas(
        "memo: second call served from memo; force re-runs; invalidate re-runs",
        &[0, 1, 2, 4, 6],
        &[0, 2, 4, 6],
        d::conniff_rg(),
    ));

    // Stale anchor refreshed by id. No name search; the same group
    // is written back with its own score (92), not the search's 100; the
    // title / artist / year the Python app took from the search are the same
    // values as the recorded lookup gives (exact equality below checks it),
    // so only the sort-name is new; the genre stays null (the
    // group's only tag has count 1).
    let s = "conniff stale cached anchor: Tier 1 searches again";
    out.extend([
        remove_req(
            s,
            &[0],
            "anchor refreshed by id",
            d::search_key("Ray Conniff", d::conniff_album_title()),
        ),
        add_req(s, &[0], "album fields", d::lookup_key(d::conniff_rg())),
        set(
            s,
            &[0],
            "anchor refreshed by id",
            "/store_writes/0/score",
            json!(100),
            json!(92),
        ),
        set(
            s,
            &[0],
            "anchor refreshed by id",
            "/store_after/score",
            json!(100),
            json!(92),
        ),
        set(
            s,
            &[0],
            "anchor refreshed by id",
            "/body/notes",
            json!(["Tier 0a: anchored from cache (score 92)"]),
            json!([
                "Tier 0a: anchored from cache (score 92)",
                format!(
                    "Tier 1: stale anchor refreshed by release-group id ({})",
                    d::conniff_rg()
                ),
            ]),
        ),
        set(
            s,
            &[0],
            "album fields",
            "/body/canonical_album_artist_sort",
            Value::Null,
            d::group_fields(d::conniff_rg())
                .into_iter()
                .find(|(k, _)| *k == "canonical_album_artist_sort")
                .unwrap()
                .1,
        ),
    ]);

    // Stale anchor, negative case: the Python app's re-search found ANOTHER group (5555) and
    // overwrote the stored anchor with it. Now the stored 6666 is refetched by
    // id; nothing is recorded for 6666 (the Python app never asked), so the
    // refresh fails: anchor kept, still stale, nothing written, no event, and
    // every track unresolved.
    let s = "stale anchor, fresh search finds another group";
    let case = case_named(s);
    let browse_err = ReplayMb::new(&case["overlay"])
        .browse_releases(OTHER_RG)
        .unwrap_err();
    out.extend([
        remove_req(s, &[0], "anchor refreshed by id", d::search_key("Ana Artist", "Album One")),
        remove_req(s, &[0], "anchor refreshed by id", d::cover_key(MEDIUM_RG)),
        remove_req(s, &[0], "anchor refreshed by id", d::browse_key(MEDIUM_RG)),
        add_req(s, &[0], "anchor refreshed by id", d::cover_key(OTHER_RG)),
        add_req(s, &[0], "anchor refreshed by id", d::browse_key(OTHER_RG)),
        add_req(s, &[0], "album fields", d::lookup_key(OTHER_RG)),
        set(
            s,
            &[0],
            "anchor refreshed by id",
            "/store_writes",
            json!([{"album_id": "medium-1", "release_group_mbid": MEDIUM_RG, "release_mbid": "rel-1", "score": 95}]),
            json!([]),
        ),
        set(
            s,
            &[0],
            "anchor refreshed by id",
            "/store_after",
            json!({"release_group_mbid": MEDIUM_RG, "release_mbid": "rel-1", "score": 95,
                   "last_checked_at": common::env::FRESH}),
            case["stored_anchor"].clone(),
        ),
        set(
            s,
            &[0],
            "anchor refreshed by id",
            "/events",
            json!([{"event": "AlbumMbAnchorSet", "album_id": "medium-1", "source": "musicbrainz"}]),
            json!([]),
        ),
        set(
            s,
            &[0],
            "anchor refreshed by id",
            "/body/notes",
            json!([
                "Tier 0a: anchored from cache (score 90)",
                "Tier 1: anchor present but tracklist fetch missed"
            ]),
            json!([
                "Tier 0a: anchored from cache (score 90)",
                d::lookup_failed_note(OTHER_RG, false),
                format!("Tier 1: stale anchor kept, not refreshed: release listing unavailable ({browse_err})"),
                "Tier 1: anchored but MB returned no tracklist",
            ]),
        ),
    ]);
    out.extend(unresolved_deltas(s, &case));

    // Fresh synthetic cached anchors whose Tier 1 has no tracklist
    // and whose release-group lookup is unrecorded.
    for (s, rg, score) in [
        ("cached anchor, empty release listing", MEDIUM_RG, 95),
        ("cached anchor, CAA 500 and browse 503", MEDIUM_RG, 95),
        (
            "network: cached anchor, nothing recorded for Tier 1",
            UNRECORDED_RG,
            95,
        ),
    ] {
        let case = case_named(s);
        out.extend(unresolved_deltas(s, &case));
        out.extend([
            add_req(s, &[0], "album fields", d::lookup_key(rg)),
            set(
                s,
                &[0],
                "album fields",
                "/body/notes",
                json!([
                    format!("Tier 0a: anchored from cache (score {score})"),
                    "Tier 1: anchored but MB returned no tracklist"
                ]),
                json!([
                    format!("Tier 0a: anchored from cache (score {score})"),
                    d::lookup_failed_note(rg, false),
                    "Tier 1: anchored but MB returned no tracklist"
                ]),
            ),
        ]);
    }

    // Fresh synthetic anchor with a usable listing: the golden's constructed
    // listing has a dated release ("1979-03") but no title or credit, so only
    // the year comes back.
    let s = "cached anchor without score";
    out.extend([
        add_req(s, &[0], "album fields", d::lookup_key(MEDIUM_RG)),
        set(
            s,
            &[0],
            "album fields",
            "/body/canonical_year",
            Value::Null,
            fact("1979"),
        ),
        set(
            s,
            &[0],
            "album fields",
            "/body/notes",
            json!(["Tier 0a: anchored from cache (score 0)"]),
            json!([
                "Tier 0a: anchored from cache (score 0)",
                d::lookup_failed_note(MEDIUM_RG, true)
            ]),
        ),
    ]);

    // Failed Tier 0b searches are traced.
    for s in [
        "network: search unrecorded",
        "network: recorded 503 on search",
    ] {
        let case = case_named(s);
        out.push(set(
            s,
            &[0],
            "network failures",
            "/body/notes",
            json!(["No anchor found via Tiers 0a-0c; AI escalation may be required"]),
            json!([
                search_failed_note(&case),
                "No anchor found via Tiers 0a-0c; AI escalation may be required"
            ]),
        ));
    }

    // The hinted Night Mix cascade (and its un-hinted control),
    // album shaped like the release the Python app aligns against. Step 0 is the
    // (hinted) Tier 0b resolve: the album fields come from the hit the hint
    // picked, as for any Tier 0b anchor. Step 2, after the memo is
    // dropped, reads the stored hinted anchor (Tier 0a): the album fields come
    // from the recorded release-group lookup, one request more.
    // Everything else, including per-track alignment, cover check, store
    // writes and events, is the Python app's record unchanged (all tracks align
    // in the 2 s pass, and the best-fitting release is the Python app's earliest,
    // so neither positional alignment nor best-fit release applies).
    for (hint, rg) in hinted_night_mix() {
        let s = hinted_scenario(&hint);
        out.extend(d::search_hit_deltas(
            &s,
            &[0],
            "Various Artists",
            "Night Mix",
            rg,
        ));
        out.extend(d::group_field_deltas(&s, &[2], &[2], rg));
    }

    // Applied after every other delta of the same scenario.
    // Every Conniff scenario that reaches Tier 1 now aligns 6 of 6
    // by position (Python app: 0 of 6); the trace line goes after Tier 1's own
    // notes and before the hint note.
    let tier0a = "Tier 0a: anchored from cache (score 100)".to_owned();
    for (s, steps, old, at) in [
        (
            "conniff 0b by directory name",
            &[0][..],
            notes_of("conniff 0b by directory name"),
            1,
        ),
        (
            "conniff 0b with hint",
            &[0][..],
            notes_of("conniff 0b with hint"),
            1,
        ),
        (
            "conniff cached anchor with release already known",
            &[0][..],
            vec![tier0a.clone()],
            1,
        ),
        (
            "memo: second call served from memo; force re-runs; invalidate re-runs",
            &[0, 1, 2, 4, 6][..],
            vec![tier0a.clone()],
            1,
        ),
        (
            "conniff stale cached anchor: Tier 1 searches again",
            &[0][..],
            vec![
                "Tier 0a: anchored from cache (score 92)".to_owned(),
                format!(
                    "Tier 1: stale anchor refreshed by release-group id ({})",
                    d::conniff_rg()
                ),
            ],
            2,
        ),
    ] {
        out.extend(d::conniff_positional_deltas(
            s,
            steps,
            &case_named(s),
            &old,
            at,
        ));
    }
    out
}

/// The hinted Night Mix scenarios of cascade.json: `(hint,
/// release-group the recorded search picks for it)`, read from the Python app's
/// own find_release table so the pairing is not typed in here. `''` is the
/// un-hinted control.
fn hinted_night_mix() -> Vec<(String, &'static str)> {
    let table = musaic_testdata::golden("resolver/find_release.json");
    std::iter::once("")
        .chain(d::night_mix_hints())
        .map(|hint| {
            let name = format!("recorded night mix, hint {}", py_repr(hint));
            let case = table["cases"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["name"] == name.as_str())
                .unwrap_or_else(|| panic!("{name}"));
            let rg = case["match"]["mbid"].as_str().unwrap();
            let rg = d::recorded_group_lookups()
                .into_iter()
                .find(|r| *r == rg)
                .unwrap_or_else(|| panic!("{rg}: lookup not recorded"));
            (hint.to_owned(), rg)
        })
        .collect()
}

/// Python's `repr()` of the plain-ASCII hints used here.
fn py_repr(s: &str) -> String {
    assert!(!s.contains(['\'', '\\']) && s.is_ascii());
    format!("'{s}'")
}

fn hinted_scenario(hint: &str) -> String {
    let prefix = format!(
        "hinted night mix {}: album shaped like release ",
        py_repr(hint)
    );
    let names: Vec<String> = cases()
        .into_iter()
        .filter_map(|c| c["name"].as_str().map(str::to_owned))
        .filter(|n| n.starts_with(&prefix))
        .collect();
    assert_eq!(names.len(), 1, "{prefix}");
    names[0].clone()
}

/// Runs one scenario; returns `(step, deltas applied)` per resolve step.
fn run_case(case: &Value, deltas: &[Delta]) -> Vec<(usize, usize)> {
    let name = case["name"].as_str().unwrap().to_owned();
    let world = World::from_case(case);
    let mb = ReplayMb::new(&case["overlay"]);
    let env = Env {
        albums: &world,
        store: &world,
        mb: &mb,
        fingerprints: &NoFingerprints,
        now_epoch_s: NOW_EPOCH_S,
    };
    let resolver = Resolver::new();
    let album_id = case["album"]["id"].as_str().unwrap_or("missing-album");
    let mut out = Vec::new();
    let mut prev = None;
    for (i, step) in case["results"].as_array().unwrap().iter().enumerate() {
        let spec = &case["steps"][i];
        let ctx = format!("{name} / step {i}");
        match step["op"].as_str().unwrap() {
            "invalidate" => resolver.invalidate(Some(album_id)),
            "rename_file" => {
                world.rename_file(spec["from"].as_str().unwrap(), spec["to"].as_str().unwrap())
            }
            "resolve" => {
                let r = resolver
                    .resolve(
                        &env,
                        album_id,
                        spec["hint"].as_str().unwrap_or(""),
                        spec["force"].as_bool().unwrap_or(false),
                    )
                    .unwrap();
                let events: Vec<Value> = r
                    .events
                    .iter()
                    .map(|e| json!({"event": "AlbumMbAnchorSet", "album_id": e.album_id, "source": e.source}))
                    .collect();
                let same = prev
                    .as_ref()
                    .is_some_and(|p| std::sync::Arc::ptr_eq(p, &r.resolution));
                let got = json!({
                    "body": r.resolution.to_dict(),
                    "events": events,
                    "store_writes": world.take_writes(),
                    "store_after": world.store_json(),
                    "same_object_as_previous_resolve": same,
                });
                let got_requests: BTreeSet<_> = mb
                    .take_log()
                    .into_iter()
                    .map(|l| (l.method, l.key))
                    .collect();
                let mut want = json!({
                    "body": step["body"],
                    "events": step["events"],
                    "store_writes": step["store_writes"],
                    "store_after": step["store_after"],
                    "same_object_as_previous_resolve": step["same_object_as_previous_resolve"],
                });
                let mut want_requests = key_set(&step["requests"]);
                let n = d::apply(deltas, &name, i, &mut want, &mut want_requests);
                for k in [
                    "body",
                    "events",
                    "store_writes",
                    "store_after",
                    "same_object_as_previous_resolve",
                ] {
                    assert_eq!(got[k], want[k], "{ctx}: {k}");
                }
                assert_eq!(got_requests, want_requests, "{ctx}: requests");
                prev = Some(r.resolution);
                out.push((i, n));
            }
            other => panic!("{other}"),
        }
    }
    out
}

// Tier order, 0a short-circuit, blocklisted cache anchor, 0b at >= 70 /
// persisted at >= 88, event emission, Tier 1 on five real release listings,
// resolution bodies, failures that end in a trace: every parity scenario equals the Python app's record except for the deltas
// enumerated in `deltas()`.
// Every delta must apply (no stale entries), and the scenarios that should
// be untouched are listed so that a new difference there cannot hide.
#[test]
fn every_parity_scenario_matches_the_python_app_plus_enumerated_deltas() {
    musaic_testdata::require_private!("replay", "golden/resolver/cascade.json");
    let deltas = deltas();
    let mut steps = 0;
    let mut applied = 0;
    let mut untouched = Vec::new();
    for case in cases() {
        if case["kind"] != "parity" {
            continue;
        }
        let name = case["name"].as_str().unwrap().to_owned();
        let per_step = run_case(&case, &deltas);
        steps += per_step.len();
        let n: usize = per_step.iter().map(|(_, n)| n).sum();
        applied += n;
        if n == 0 {
            untouched.push(name);
        }
    }
    assert!(steps >= 25, "{steps}");
    let expected_total: usize = deltas.iter().map(|d| d.steps.len()).sum();
    assert_eq!(
        applied, expected_total,
        "some delta names a scenario or step that never ran"
    );
    assert_eq!(
        untouched,
        vec![
            "conniff cached anchor blocklisted",
            "album not in cache",
            "medium score 0b: not persisted, Tier 1 re-searches",
            "medium score 0b, no first-release-date (year browse)",
            "score 69: Tier 0b declines",
        ],
        "scenarios with no deliberate delta"
    );
}

// The Python app anchors the synthetic
// four-track "Night Mix" by name at score 100 (golden), this build rejects it
// against the recorded release listing and leaves it unanchored, persisting
// nothing. Tracks keep the Python app's per-track fingerprint fields.
#[test]
fn night_mix_name_hit_is_rejected_where_the_python_app_anchored() {
    musaic_testdata::require_private!("replay", "golden/resolver/cascade.json");
    let case = cases()
        .into_iter()
        .find(|c| c["kind"] == "deliberate")
        .unwrap();
    let python = &case["results"][0];
    assert_eq!(python["body"]["anchor_strategy"], "dir-search");
    assert_eq!(python["body"]["anchor"]["value"], d::night_mix_rg());
    let world = World::from_case(&case);
    let mb = ReplayMb::new(&case["overlay"]);
    let env = Env {
        albums: &world,
        store: &world,
        mb: &mb,
        fingerprints: &NoFingerprints,
        now_epoch_s: NOW_EPOCH_S,
    };
    let r = Resolver::new()
        .resolve(&env, case["album"]["id"].as_str().unwrap(), "", false)
        .unwrap();
    let body = r.resolution.to_dict();
    assert_eq!(body["anchor"], Value::Null);
    assert_eq!(body["anchor_strategy"], "none");
    assert!(r.events.is_empty());
    assert!(world.take_writes().is_empty());
    let notes: Vec<&str> = body["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_str().unwrap())
        .collect();
    assert_eq!(notes.len(), 2, "{notes:?}");
    assert!(
        notes[0].starts_with(&format!(
            "Tier 0b: rejected \"Various Artists \u{2014} {}\" (score 100/100, MBID {}): no release within 2 tracks of the album",
            d::rec_night_mix_title(),
            d::night_mix_rg()
        )),
        "{notes:?}"
    );
    assert_eq!(
        notes[1],
        "No anchor found via Tiers 0a-0c; AI escalation may be required"
    );
    for (t, o) in body["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .zip(python["body"]["tracks"].as_array().unwrap())
    {
        for k in [
            "filename",
            "recording_mbid",
            "fingerprint_score",
            "fingerprinted",
        ] {
            assert_eq!(t[k], o[k]);
        }
    }
    assert_eq!(body["unresolved_track_count"], 4);
}

// Tier 0b acceptance check (deliberate change): the same hints on the
// synthetic four-track "Night Mix" album. The Python app anchors whatever group
// the hint picks (golden) and runs Tier 1 against it; this build rejects each
// against the hinted group's recorded release listing and leaves the album
// unanchored. Requests: the Python app's, minus the cover check (Tier 1 never
// runs); nothing persisted, nothing published; tracks keep the Python app's
// per-track fingerprint fields.
#[test]
fn hinted_night_mix_hits_are_rejected_on_the_synthetic_album() {
    musaic_testdata::require_private!("replay", "golden/resolver/cascade.json");
    let hinted: Vec<Value> = cases()
        .into_iter()
        .filter(|c| c["kind"] == "deliberate" && c["steps"][0]["hint"].is_string())
        .collect();
    assert_eq!(hinted.len(), 3);
    for case in hinted {
        let name = case["name"].as_str().unwrap();
        let hint = case["steps"][0]["hint"].as_str().unwrap();
        let python = &case["results"][0];
        let rg = python["body"]["anchor"]["value"].as_str().unwrap();
        assert_eq!(python["body"]["anchor_strategy"], "dir-search", "{name}");
        assert!(
            hinted_night_mix()
                .iter()
                .any(|(h, r)| h == hint && *r == rg),
            "{name}: the Python app anchored another group than its search picks"
        );
        let world = World::from_case(&case);
        let mb = ReplayMb::new(&case["overlay"]);
        let env = Env {
            albums: &world,
            store: &world,
            mb: &mb,
            fingerprints: &NoFingerprints,
            now_epoch_s: NOW_EPOCH_S,
        };
        let r = Resolver::new()
            .resolve(&env, case["album"]["id"].as_str().unwrap(), hint, false)
            .unwrap();
        let body = r.resolution.to_dict();
        assert_eq!(body["anchor"], Value::Null, "{name}");
        assert_eq!(body["anchor_strategy"], "none", "{name}");
        assert!(r.events.is_empty(), "{name}");
        assert!(world.take_writes().is_empty(), "{name}");
        assert_eq!(world.store_json(), Value::Null, "{name}");
        let python_notes = python["body"]["notes"].as_array().unwrap();
        let matched = python_notes[0]
            .as_str()
            .unwrap()
            .strip_prefix("Tier 0b: Matched ")
            .unwrap();
        let notes: Vec<&str> = body["notes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n.as_str().unwrap())
            .collect();
        assert_eq!(notes.len(), 3, "{name}: {notes:?}");
        assert!(
            notes[0].starts_with(&format!(
                "Tier 0b: rejected {matched}: no release within 2 tracks of the album"
            )),
            "{name}: {notes:?}"
        );
        assert_eq!(
            notes[1], "No anchor found via Tiers 0a-0c; AI escalation may be required",
            "{name}"
        );
        // The hint line is the Python app's own (its last note).
        assert_eq!(
            notes[2],
            python_notes.last().unwrap().as_str().unwrap(),
            "{name}"
        );
        let got: BTreeSet<_> = mb
            .take_log()
            .into_iter()
            .map(|l| (l.method, l.key))
            .collect();
        let mut want = key_set(&python["requests"]);
        assert!(want.remove(&d::cover_key(rg)), "{name}");
        assert_eq!(got, want, "{name}: requests");
        for (t, o) in body["tracks"]
            .as_array()
            .unwrap()
            .iter()
            .zip(python["body"]["tracks"].as_array().unwrap())
        {
            for k in [
                "filename",
                "recording_mbid",
                "fingerprint_score",
                "fingerprinted",
            ] {
                assert_eq!(t[k], o[k], "{name}");
            }
        }
        assert_eq!(body["unresolved_track_count"], 4, "{name}");
    }
}
