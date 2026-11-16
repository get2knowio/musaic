//! Per-check detection tables replayed from the Python app
//! (testdata/golden/checks/cases.json).
//!
//! Detection of all seven checks, incl. the cases of the Python app's
//! tests/test_checks.py, tests/test_missing_state.py and the
//! ARTIST-only clustering), with the run row and upserted rows. Every finding
//! row is compared field by field, `detail_json` byte for byte, timestamps
//! included (the generator and this test use the same counter clock).
//!
//! Allowed differences: only the enumerated deliberate-change deltas in
//! [`DELTAS`]. No case mixes fingerprint implementations
//! or has a normalisation-only name difference.

mod common;

use std::sync::Arc;

use common::{RecordedResolver, Sandbox, env, findings_json, registry, run_json, setup, str_ids};
use musaic_checks::{ScopeKind, run_check};
use serde_json::{Value as J, json};

fn run_case(case: &J) -> (J, J) {
    let (sb, db) = Sandbox::new();
    let conn = db.connection();
    setup(&sb, conn, case);
    let log = Default::default();
    let env = env(
        registry(None, &log),
        Arc::new(RecordedResolver::from_case(case)),
    );
    let ids = str_ids(&case["album_ids"]);
    let kind = ScopeKind::parse(case["scope"].as_str().unwrap_or("all")).unwrap();
    let result = match run_check(
        conn,
        &env,
        case["check"].as_str().unwrap(),
        &sb.lib_s,
        kind,
        ids.as_deref(),
    ) {
        Ok(run) => json!({ "run": run_json(&sb, &run) }),
        Err(e) => json!({ "error": e.to_string() }),
    };
    (result, findings_json(&sb, conn))
}

/// A deliberate change applied to the Python app's record of one case:
/// (case id, what the change is, what changes, the edit). The edit asserts the old
/// value before writing the new one, so a delta that is no longer in the
/// record fails instead of passing vacuously.
type CaseDelta = (&'static str, &'static str, &'static str, fn(&mut J, &mut J));

/// Drop the Python app's findings on `album`, and the clock tick each upsert
/// took: `issue_count` and `completed_at` (counter clock, 1 s per tick)
/// move back by the number dropped.
fn drop_other_library_findings(result: &mut J, findings: &mut J, album: &str) {
    let rows = findings.as_array_mut().unwrap();
    let before = rows.len();
    rows.retain(|f| f["album_id"] != album);
    let n = (before - rows.len()) as i64;
    assert!(n > 0, "the Python app recorded no finding on {album}");
    let run = &mut result["run"];
    run["issue_count"] = json!(run["issue_count"].as_i64().unwrap() - n);
    let done = run["completed_at"].as_str().unwrap();
    let secs: i64 = done[17..19].parse().unwrap();
    assert!(done.starts_with("2026-01-01T00:00:") && secs >= n, "{done}");
    run["completed_at"] = json!(format!("2026-01-01T00:00:{:02}Z", secs - n));
}

const DELTAS: &[CaseDelta] = &[
    (
        "toos-resolver-raises-or-unanchored",
        "a resolver error leaves an album unevaluated instead of reading as \"no resolution\"",
        "findings: a2 (its resolver raises) 'missing: year' -> absent, since the album is not \
         evaluated and emits nothing; run.issue_count 2 -> 1; run.completed_at 00:00:04Z -> 00:00:03Z",
        |r, f| {
            assert_eq!(r["run"]["issue_count"], 2);
            drop_other_library_findings(r, f, "a2");
        },
    ),
    (
        "dupes-filters",
        "duplicate-tracks looks only at the library being checked",
        "findings: a2 (library <LIB>-other) 'Duplicate audio: \"a\" (2 copies)' -> absent; \
         run.issue_count 1 -> 0; run.completed_at 00:00:03Z -> 00:00:02Z",
        |r, f| {
            assert_eq!(r["run"]["issue_count"], 1);
            drop_other_library_findings(r, f, "a2");
        },
    ),
    (
        "dupes-two-libraries",
        "duplicate-tracks looks only at the library being checked",
        "findings: a2 (library <LIB>-other) -> absent, a1 (this library) kept; \
         run.issue_count 2 -> 1; run.completed_at 00:00:04Z -> 00:00:03Z",
        |r, f| {
            assert_eq!(r["run"]["issue_count"], 2);
            assert!(f.as_array().unwrap().iter().any(|x| x["album_id"] == "a1"));
            drop_other_library_findings(r, f, "a2");
        },
    ),
    (
        "ias-artist-only-album",
        "an album with ARTIST but no ALBUMARTIST joins its cluster at album level \
         (credited with its ARTIST, the shared display-artist cascade)",
        "the finding gains x1 (ARTIST 'Harold Mansell', no ALBUMARTIST) as its first \
         member, so it is stored on x1 (smallest id) instead of x2 and reads \
         '3 credit instances' instead of '2'",
        |_r, f| {
            let row = &mut f[0];
            assert_eq!(row["album_id"], "x2");
            row["album_id"] = json!("x1");
            let old = "2 credit instances for 'Harold Mansell' / 'Harold Mansell & His Orchestra'";
            assert_eq!(row["summary"], old);
            row["summary"] = json!(old.replacen('2', "3", 1));
            let detail = row["detail_json"].as_str().unwrap().to_string();
            let first = "\"members\": [{\"kind\": \"album\", \"album_id\": \"x2\"";
            assert!(detail.contains(first), "{detail}");
            row["detail_json"] = json!(detail.replace(
                "\"members\": [",
                "\"members\": [{\"kind\": \"album\", \"album_id\": \"x1\", \"dir_name\": \
                 \"Harold Mansell - A\", \"album_artist\": \"Harold Mansell\", \"sort\": \"\"}, ",
            ));
        },
    ),
];

/// Each case's run row and findings equal the Python app's,
/// with the enumerated deltas applied; every delta is used.
#[test]
fn detection_cases_match_the_python() {
    let g = common::golden("cases.json");
    let cases = g["cases"].as_array().unwrap();
    assert!(cases.len() >= 30);
    let mut checks_seen = std::collections::BTreeSet::new();
    let mut used = Vec::new();
    for case in cases {
        let id = case["id"].as_str().unwrap();
        let (result, findings) = run_case(case);
        let (mut want_result, mut want_findings) =
            (case["result"].clone(), case["findings"].clone());
        for (cid, unk, what, edit) in DELTAS {
            if *cid == id {
                edit(&mut want_result, &mut want_findings);
                assert_ne!(
                    (&want_result, &want_findings),
                    (&case["result"], &case["findings"]),
                    "{unk} delta on {id} changed nothing: {what}"
                );
                used.push(*cid);
            }
        }
        assert_eq!(result, want_result, "case {id}: run row");
        assert_eq!(findings, want_findings, "case {id}: findings");
        checks_seen.insert(case["check"].as_str().unwrap().to_string());
    }
    // Every registered check has detection cases.
    assert_eq!(checks_seen.len(), 7, "{checks_seen:?}");
    let all: Vec<&str> = DELTAS.iter().map(|d| d.0).collect();
    assert_eq!(
        used, all,
        "every enumerated delta applies to a recorded case"
    );
}

/// Malformed: the track-numbers table includes malformed JSON, a
/// non-list, strings, floats, booleans and NULL; spot-check that the Python app
/// recorded the tolerant parse (and the test above replays it).
#[test]
fn malformed_track_numbers_are_in_the_golden() {
    let g = common::golden("cases.json");
    let case = g["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "gaps-table")
        .unwrap();
    let ids: Vec<&str> = case["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["album_id"].as_str().unwrap())
        .collect();
    for absent in [
        "t-malformed",
        "t-object",
        "t-empty",
        "t-null-col",
        "t-one",
        "t-missing",
    ] {
        assert!(!ids.contains(&absent), "{absent}");
    }
    for present in ["t-floats", "t-bools", "t-strings", "t-nulls"] {
        assert!(ids.contains(&present), "{present}");
    }
}
