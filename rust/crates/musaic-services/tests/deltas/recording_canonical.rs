//! Enumerated deliberate changes against
//! testdata/golden/checks/recording_canonical.json (the Python app's answers,
//! recorded from the Python app), shared by the
//! service-level replay (musaic-services tests/recording_canonical_golden.rs)
//! and the HTTP replay (musaic-api tests/recording_canonical_http.rs).
//!
//! * only definitive answers (found, 404) are
//!   remembered. The Python app's `lru_cache` also kept the `None` of a failed
//!   request, a 503 and a 200 whose body is not JSON, and answered their
//!   repeats from memory; this build asks MusicBrainz again (the same
//!   request, the same log line), and its cache holds fewer entries from
//!   the first of those steps on.
//! * a malformed 200 (the JSON array) is "no
//!   answer": the route's no-recording body (200) where the Python app answered
//!   500, and a WARNING on `musaic.mb_lookup`. Neither side keeps it. A 200
//!   whose body is not JSON (the Python app's silent `None`) also logs a
//!   WARNING, `returned a body that is not JSON: {parse error}`.
//!
//! Every other step, field and byte is the Python app's. Each replaced part
//! differs from the Python app's (checked), so a delta that changes nothing
//! fails here.
#![allow(dead_code)]

use std::collections::HashMap;

use base64::Engine as _;
use serde_json::{Value, json};

/// The first lookup of each MBID whose answer was not definitive.
/// The Python app cached its `None` (its cache grew by one); this build does not.
pub const UNCACHED_FIRST: [&str; 3] = [
    "request fails (unrecorded)",
    "HTTP 503",
    "200 whose body is not JSON",
];

/// (repeat, first). The Python app answered the repeat from memory
/// with no request; this build makes the first step's request again.
pub const UNCACHED_REPEATS: [(&str, &str); 3] = [
    (
        "request failed before: the None is cached",
        "request fails (unrecorded)",
    ),
    ("HTTP 503 again: the None is cached", "HTTP 503"),
    (
        "200 not JSON again: the None is cached",
        "200 whose body is not JSON",
    ),
];

/// The malformed-200 steps (the Python app's 500).
pub const MALFORMED_STEPS: [&str; 2] = [
    "JSON array: the Python app raises",
    "JSON array again: nothing was cached",
];

/// The steps that ask for the 200 whose body is not JSON (the
/// first, and its repeat, asked again because a failure is not kept): the Python app logged nothing; this build
/// logs a WARNING naming the parse error.
pub const NOT_JSON_STEPS: [&str; 2] = [
    "200 whose body is not JSON",
    "200 not JSON again: the None is cached",
];

/// The step the no-recording body is taken from (a 200 with that reason).
const NO_RECORDING_TEMPLATE: &str = "HTTP 503";

fn mbid_of(step: &Value) -> String {
    let url = step["requests"][0]["url"].as_str().unwrap();
    url.trim_start_matches("https://musicbrainz.org/ws/2/recording/")
        .split('?')
        .next()
        .unwrap()
        .to_owned()
}

fn replace(want: &mut Value, label: &str, path: &str, new: Value) {
    let slot = want.pointer_mut(path).unwrap();
    assert_ne!(*slot, new, "{label} {path}: delta changes nothing");
    *slot = new;
}

/// The golden's steps with the two deliberate changes applied (only
/// definitive answers are kept; a malformed 200 is "no answer").
pub fn expected_steps(g: &Value) -> Vec<Value> {
    let steps = g["steps"].as_array().unwrap();
    let by_label: HashMap<&str, &Value> = steps
        .iter()
        .map(|s| (s["label"].as_str().unwrap(), s))
        .collect();
    let overlay: HashMap<&str, &Value> = g["overlay"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| (o["key"].as_str().unwrap(), o))
        .collect();
    for l in UNCACHED_FIRST
        .iter()
        .chain(UNCACHED_REPEATS.iter().map(|(r, _)| r))
        .chain(MALFORMED_STEPS.iter())
    {
        assert!(by_label.contains_key(l), "golden has no step {l:?}");
    }
    let template = by_label[NO_RECORDING_TEMPLATE];
    let mut uncached = 0_u64;
    let mut prev_size = 0_u64;
    let mut out = Vec::with_capacity(steps.len());
    for s in steps {
        let label = s["label"].as_str().unwrap();
        let python_size = s["cache_size"].as_u64().unwrap();
        let mut want = s.clone();
        if UNCACHED_FIRST.contains(&label) {
            assert_eq!(
                python_size,
                prev_size + 1,
                "{label}: the Python app cached it"
            );
            assert_eq!(s["response"]["status"], 200, "{label}");
            assert_eq!(s["requests"].as_array().unwrap().len(), 1, "{label}");
            uncached += 1;
        }
        if let Some((_, first)) = UNCACHED_REPEATS.iter().find(|(r, _)| *r == label) {
            let first = by_label[first];
            assert_eq!(
                s["requests"],
                json!([]),
                "{label}: the Python app asked nobody"
            );
            assert_eq!(s["response"], first["response"], "{label}");
            replace(&mut want, label, "/requests", first["requests"].clone());
            // The same log line as the first time (none for the 200 that
            // is not JSON, as in the Python app).
            *want.pointer_mut("/log").unwrap() = first["log"].clone();
        }
        if NOT_JSON_STEPS.contains(&label) {
            let key = want["requests"][0]["key"].as_str().unwrap();
            let raw = base64::engine::general_purpose::STANDARD
                .decode(overlay[key]["content_b64"].as_str().unwrap())
                .unwrap();
            let why = serde_json::from_slice::<Value>(&raw).unwrap_err();
            let mbid = mbid_of(&want);
            replace(
                &mut want,
                label,
                "/log",
                json!([{
                    "level": "WARNING",
                    "logger": "musaic.mb_lookup",
                    "message": format!(
                        "MB recording lookup for {mbid} returned a body that is not JSON: {why}"
                    ),
                }]),
            );
        }
        if MALFORMED_STEPS.contains(&label) {
            assert_eq!(s["response"]["status"], 500, "{label}");
            let mbid = mbid_of(s);
            let key = s["requests"][0]["key"].as_str().unwrap();
            let body: Value = serde_json::from_slice(
                &base64::engine::general_purpose::STANDARD
                    .decode(overlay[key]["content_b64"].as_str().unwrap())
                    .unwrap(),
            )
            .unwrap();
            assert!(!body.is_object(), "{label}: the malformed shape");
            let old = mbid_of(template);
            let mut resp = template["response"].clone();
            resp["json"]["mbid"] = json!(mbid);
            let raw = resp["raw"].as_str().unwrap().replace(&old, &mbid);
            resp["raw"] = json!(raw);
            replace(&mut want, label, "/response", resp);
            replace(
                &mut want,
                label,
                "/log",
                json!([{
                    "level": "WARNING",
                    "logger": "musaic.mb_lookup",
                    "message": format!(
                        "MB recording lookup for {mbid} returned a malformed answer: not an object: {body}"
                    ),
                }]),
            );
        }
        if uncached > 0 {
            replace(
                &mut want,
                label,
                "/cache_size",
                json!(python_size - uncached),
            );
        }
        prev_size = python_size;
        out.push(want);
    }
    assert_eq!(uncached, UNCACHED_FIRST.len() as u64);
    out
}
