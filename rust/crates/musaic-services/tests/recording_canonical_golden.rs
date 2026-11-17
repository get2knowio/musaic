//! `GET /api/checks/duplicate-tracks/findings/{id}/canonical` as the
//! services operation, against the Python app's answers in
//! testdata/golden/checks/recording_canonical.json
//! (recorded from the Python app's
//! route over a copy of testdata/db/v16-synthetic-offline.db, MusicBrainz
//! from the recorded replay set (private) plus the golden's CONSTRUCTED overlay).
//!
//! Every step runs here in the same order on a copy of the same database
//! with ONE cache (one server lifetime), the finding's detail set with the
//! same SQL text, and must give the Python app's body (byte for byte as the
//! route renders it), the same outbound requests (method, replay key, how
//! served), the same number of cached answers, and the Python app's
//! `musaic.mb_lookup` log lines (for a failed request, the part before the
//! transport's own error text), with the deliberate changes (only
//! found / 404 are remembered; a malformed 200 is "no answer"
//! and a warning) applied by tests/deltas/recording_canonical.rs.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use base64::Engine as _;
use musaic_net::{
    MusicBrainzClient, ReplayTransport, Request, Response, Transport, TransportError,
};
use musaic_services::Json;
use musaic_services::recording_canonical::{
    CACHE_MAXSIZE, RecordingCanonicalCache, duplicate_finding_canonical,
};
use serde_json::Value;

#[path = "deltas/recording_canonical.rs"]
mod deltas;

fn golden() -> Value {
    musaic_testdata::golden("checks/recording_canonical.json")
}

/// The golden's overlay first, then the recorded replay set (private); every request logged
/// as (method, key, served).
struct Overlaid {
    overlay: HashMap<String, Response>,
    replay: ReplayTransport,
    log: Mutex<Vec<(String, String, String)>>,
}

impl Overlaid {
    fn new(g: &Value) -> Self {
        let overlay = g["overlay"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| {
                (
                    o["key"].as_str().unwrap().to_owned(),
                    Response {
                        status: o["status"].as_u64().unwrap() as u16,
                        headers: vec![],
                        body: base64::engine::general_purpose::STANDARD
                            .decode(o["content_b64"].as_str().unwrap())
                            .unwrap(),
                    },
                )
            })
            .collect();
        Overlaid {
            overlay,
            replay: ReplayTransport::new(musaic_testdata::private_required("replay")),
            log: Mutex::default(),
        }
    }

    fn take(&self) -> Vec<(String, String, String)> {
        std::mem::take(&mut *self.log.lock().unwrap())
    }
}

impl Transport for Overlaid {
    fn send(&self, req: &Request) -> Result<Response, TransportError> {
        let key = req.replay_key();
        let (r, served) = match self.overlay.get(&key) {
            Some(r) => (Ok(r.clone()), "constructed"),
            None => match self.replay.send(req) {
                Ok(r) => (Ok(r), "recorded"),
                Err(e) => (Err(e), "unobserved"),
            },
        };
        self.log
            .lock()
            .unwrap()
            .push((req.method.as_str().to_owned(), key, served.to_owned()));
        r
    }
}

fn db_copy(dir: &Path) -> rusqlite::Connection {
    let p = dir.join("library.db");
    std::fs::copy(musaic_testdata::public("db/v16-synthetic-offline.db"), &p).unwrap();
    rusqlite::Connection::open(p).unwrap()
}

// The canonical route (failures end in an answer, not an error): every
// step of the Python app's sequence. Covers
// finding not found / of another check (404), no MBID (null and ""),
// found (title + joined artist), MusicBrainz 404, a failed request, 503,
// a body that is not JSON, a blank title, joinphrases, no credit, a JSON
// array, and the once-per-lifetime cache (a repeated found or 404 MBID
// makes no request). a failed request, a 503
// and a body that is not JSON are asked again and not counted in the
// cache; the JSON array answers "no recording" with a warning, not 500.
#[test]
fn every_recorded_step_is_reproduced() {
    musaic_testdata::require_private!("replay", "golden/checks/recording_canonical.json");
    let g = golden();
    let dup = g["provenance"]["duplicate_finding_id"].as_i64().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let conn = db_copy(tmp.path());
    let t = Overlaid::new(&g);
    let mb = MusicBrainzClient::new(&t);
    let cache = RecordingCanonicalCache::new();
    let mut steps = 0;
    let want_steps = deltas::expected_steps(&g);
    for s in &want_steps {
        let label = s["label"].as_str().unwrap();
        let path = s["path"].as_str().unwrap();
        let raw_id = path
            .trim_start_matches("/api/checks/duplicate-tracks/findings/")
            .trim_end_matches("/canonical");
        let Ok(id) = raw_id.parse::<i64>() else {
            // Path validation (422) is the route's, not the operation's.
            assert_eq!(s["response"]["status"], 422, "{label}");
            continue;
        };
        if let Some(d) = s["set_detail_json"].as_str() {
            conn.execute(
                "UPDATE check_findings SET detail_json=?1 WHERE id=?2",
                rusqlite::params![d, dup],
            )
            .unwrap();
        }
        let (got, lines) =
            musaic_log::capture(|| duplicate_finding_canonical(&conn, id, &mb, &cache));
        let want = &s["response"];
        match got {
            Ok(body) => {
                assert_eq!(want["status"], 200, "{label}");
                assert_eq!(body.to_value(), want["json"], "{label}");
                // Key order as the Python app's dict (the route's raw body).
                let Json::Obj(pairs) = &body else {
                    panic!("{label}: not an object")
                };
                let keys: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
                let raw = want["raw"].as_str().unwrap();
                let mut at: Vec<usize> = keys
                    .iter()
                    .map(|k| raw.find(&format!("\"{k}\":")).unwrap())
                    .collect();
                assert!(at.is_sorted(), "{label}: {keys:?} vs {raw}");
                at.dedup();
                assert_eq!(at.len(), 5, "{label}");
            }
            Err(e) => {
                assert_eq!(
                    want["status"].as_u64(),
                    Some(e.status as u64),
                    "{label}: {e:?}"
                );
                if e.status != 500 {
                    assert_eq!(want["json"]["detail"], e.detail.as_str(), "{label}");
                }
            }
        }
        let reqs = t.take();
        let want_reqs: Vec<(String, String, String)> = s["requests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                (
                    r["method"].as_str().unwrap().to_owned(),
                    r["key"].as_str().unwrap().to_owned(),
                    r["served"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        assert_eq!(reqs, want_reqs, "{label}: requests");
        assert_eq!(
            cache.len() as u64,
            s["cache_size"].as_u64().unwrap(),
            "{label}: cache"
        );
        let want_log = s["log"].as_array().unwrap();
        assert_eq!(lines.len(), want_log.len(), "{label}: {lines:?}");
        for (l, w) in lines.iter().zip(want_log) {
            assert_eq!(l.level.as_str(), w["level"].as_str().unwrap(), "{label}");
            assert_eq!(l.target, w["logger"].as_str().unwrap(), "{label}");
            let wm = w["message"].as_str().unwrap();
            if s["requests"][0]["served"] == "unobserved" {
                // The Python app's text after ": " is requests' exception; this
                // build's is its own transport error.
                let (head, _) = wm.split_once(": ").unwrap();
                assert!(
                    l.message.starts_with(&format!("{head}: ")),
                    "{label}: {}",
                    l.message
                );
                assert!(l.message.len() > head.len() + 2, "{label}");
            } else {
                assert_eq!(l.message, wm, "{label}");
            }
        }
        steps += 1;
    }
    assert_eq!(steps, g["steps"].as_array().unwrap().len() - 1);
}

/// Answers 404 to everything and counts requests.
#[derive(Default)]
struct Counting(Mutex<Vec<String>>);

impl Transport for Counting {
    fn send(&self, req: &Request) -> Result<Response, TransportError> {
        self.0.lock().unwrap().push(req.url());
        Ok(Response {
            status: 404,
            headers: vec![],
            body: vec![],
        })
    }
}

// `lru_cache(maxsize=2048)` (mb_lookup.py:537): the 2049th distinct MBID
// evicts the least recently used one, not the oldest inserted: a hit
// refreshes an entry.
#[test]
fn cache_holds_2048_and_evicts_the_least_recently_used() {
    let t = Counting::default();
    let mb = MusicBrainzClient::new(&t);
    let cache = RecordingCanonicalCache::new();
    let id = |i: usize| format!("00000000-0000-4000-8000-{i:012}");
    for i in 0..CACHE_MAXSIZE {
        let _ = cache.get_or_lookup(&mb, &id(i));
    }
    assert_eq!(cache.len(), CACHE_MAXSIZE);
    let _ = cache.get_or_lookup(&mb, &id(0)); // hit: 0 becomes the most recent
    assert_eq!(t.0.lock().unwrap().len(), CACHE_MAXSIZE);
    let _ = cache.get_or_lookup(&mb, &id(CACHE_MAXSIZE)); // evicts 1
    assert_eq!(cache.len(), CACHE_MAXSIZE);
    let before = t.0.lock().unwrap().len();
    let _ = cache.get_or_lookup(&mb, &id(0));
    assert_eq!(t.0.lock().unwrap().len(), before, "0 is still cached");
    let _ = cache.get_or_lookup(&mb, &id(1));
    assert_eq!(t.0.lock().unwrap().len(), before + 1, "1 was evicted");
}

/// Each MBID's answers in order, one per request; every request logged.
struct Sequence {
    answers: Mutex<HashMap<String, Vec<Result<Response, TransportError>>>>,
    asked: Mutex<Vec<String>>,
}

impl Sequence {
    fn new(answers: Vec<(&str, Vec<Result<Response, TransportError>>)>) -> Self {
        Sequence {
            answers: Mutex::new(
                answers
                    .into_iter()
                    .map(|(m, a)| {
                        (
                            MusicBrainzClient::<ReplayTransport>::recording_canonical_request(m)
                                .url(),
                            a,
                        )
                    })
                    .collect(),
            ),
            asked: Mutex::default(),
        }
    }

    fn asked(&self) -> Vec<String> {
        std::mem::take(&mut *self.asked.lock().unwrap())
    }
}

impl Transport for Sequence {
    fn send(&self, req: &Request) -> Result<Response, TransportError> {
        let url = req.url();
        self.asked.lock().unwrap().push(url.clone());
        let mut a = self.answers.lock().unwrap();
        let queue = a.get_mut(&url).expect("a scripted MBID");
        assert!(!queue.is_empty(), "{url}: asked more often than scripted");
        queue.remove(0)
    }
}

fn answer(status: u16, body: &str) -> Result<Response, TransportError> {
    Ok(Response {
        status,
        headers: vec![],
        body: body.as_bytes().to_vec(),
    })
}

// Only definitive answers are remembered (the same rule as AcoustID's
// `done` / `failed`), and a malformed 200 is "no answer" and a warning,
// through the route's
// operation on one cache: an MBID whose lookups fail (a network error,
// a 503, a 429, a 200 that is not JSON, a malformed 200) is asked again on
// every request, answering "no recording" each time, until MusicBrainz
// answers; that answer is then remembered and served without a request. A
// 404 is remembered at once. The Python app's lru_cache answered the second
// request from memory after the first failure (golden steps "request
// failed before: the None is cached", "HTTP 503 again ...") and answered
// 500 to the malformed 200.
#[test]
fn failures_are_asked_again_definitive_answers_are_kept() {
    musaic_testdata::require_private!("replay", "golden/checks/recording_canonical.json");
    let g = golden();
    let dup = g["provenance"]["duplicate_finding_id"].as_i64().unwrap();
    let found = g["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["label"] == "found")
        .unwrap();
    let recorded = ReplayTransport::new(musaic_testdata::private_required("replay"))
        .send(
            &MusicBrainzClient::<ReplayTransport>::recording_canonical_request(
                found["response"]["json"]["mbid"].as_str().unwrap(),
            ),
        )
        .unwrap();
    let flaky = "aaaaaaaa-0000-4000-8000-000000000001";
    let gone = "aaaaaaaa-0000-4000-8000-000000000002";
    let t = Sequence::new(vec![
        (
            flaky,
            vec![
                Err(TransportError::Dns {
                    host: "musicbrainz.org".into(),
                }),
                answer(503, "{}"),
                answer(429, "{}"),
                answer(200, "<html>busy</html>"),
                answer(200, "[1, 2]"),
                answer(200, "{\"title\": 7}"),
                Ok(recorded.clone()),
            ],
        ),
        (gone, vec![answer(404, "")]),
    ]);
    let mb = MusicBrainzClient::new(&t);
    let cache = RecordingCanonicalCache::new();
    let tmp = tempfile::tempdir().unwrap();
    let conn = db_copy(tmp.path());
    let ask = |mbid: &str| {
        conn.execute(
            "UPDATE check_findings SET detail_json=?1 WHERE id=?2",
            rusqlite::params![serde_json::json!({"recording_mbid": mbid}).to_string(), dup],
        )
        .unwrap();
        musaic_log::capture(|| {
            duplicate_finding_canonical(&conn, dup, &mb, &cache)
                .map(|b| b.to_value())
                .map_err(|e| e.status)
        })
    };
    let no_recording = |mbid: &str| {
        serde_json::json!({"title": null, "artist": null, "mbid": mbid,
                           "reason": "MusicBrainz didn't return a recording for that MBID",
                           "reason_code": "not_found"})
    };
    let failed = |mbid: &str, code: &str, text: &str| {
        serde_json::json!({"title": null, "artist": null, "mbid": mbid,
                           "reason": text, "reason_code": code})
    };
    let want_failures = [
        ("unreachable", deltas::UNREACHABLE),
        ("rate_limited", deltas::RATE_LIMITED),
        ("rate_limited", deltas::RATE_LIMITED),
        ("unreadable_answer", deltas::UNREADABLE),
        ("unreadable_answer", deltas::UNREADABLE),
        ("unreadable_answer", deltas::UNREADABLE),
    ];
    // Six failures in a row: each one a request, each "no recording", none kept.
    let want_lines: [Option<&str>; 6] = [
        Some("MB recording lookup failed for "),
        Some("MB recording lookup HTTP 503 for "),
        Some("MB recording lookup HTTP 429 for "),
        // The 200 whose body is not JSON is logged too.
        Some(
            "MB recording lookup for aaaaaaaa-0000-4000-8000-000000000001 returned a body that is not JSON: ",
        ),
        Some(
            "MB recording lookup for aaaaaaaa-0000-4000-8000-000000000001 returned a malformed answer: not an object: [1,2]",
        ),
        Some(
            "MB recording lookup for aaaaaaaa-0000-4000-8000-000000000001 returned a malformed answer: title is 7",
        ),
    ];
    for (i, line) in want_lines.iter().enumerate() {
        let (got, lines) = ask(flaky);
        let (code, text) = want_failures[i];
        assert_eq!(got, Ok(failed(flaky, code, text)), "failure {i}");
        assert_eq!(t.asked().len(), 1, "failure {i}: asked again");
        assert_eq!(cache.len(), 0, "failure {i}: not kept");
        match line {
            Some(l) => {
                assert_eq!(lines.len(), 1, "failure {i}: {lines:?}");
                assert_eq!(lines[0].level, musaic_log::Level::Warning);
                assert_eq!(lines[0].target, "musaic.mb_lookup");
                assert!(lines[0].message.starts_with(l), "{}", lines[0].message);
            }
            None => assert!(lines.is_empty(), "failure {i}: {lines:?}"),
        }
    }
    // MusicBrainz answers: kept, and the next request is served from memory.
    let want = found["response"]["json"].clone();
    let mut want = want;
    want["mbid"] = serde_json::json!(flaky);
    want["reason_code"] = serde_json::Value::Null;
    assert_eq!(ask(flaky).0, Ok(want.clone()));
    assert_eq!(t.asked().len(), 1);
    assert_eq!(ask(flaky).0, Ok(want));
    assert!(t.asked().is_empty(), "the found answer is remembered");
    // A 404 is definitive: remembered at once.
    assert_eq!(ask(gone).0, Ok(no_recording(gone)));
    assert_eq!(ask(gone).0, Ok(no_recording(gone)));
    assert_eq!(t.asked().len(), 1, "the 404 is remembered");
    assert_eq!(cache.len(), 2);
}

// Every way a lookup can end has its own reason code and sentence, so the
// duplicate-tracks dialog can say which case happened. Public data only: the
// answers are scripted.
#[test]
fn each_lookup_outcome_has_its_own_reason() {
    let g = golden();
    let dup = g["provenance"]["duplicate_finding_id"].as_i64().unwrap();
    let ids: Vec<String> = (0..7)
        .map(|i| format!("bbbbbbbb-0000-4000-8000-00000000000{i}"))
        .collect();
    let t = Sequence::new(vec![
        (
            &ids[0],
            vec![Err(TransportError::Dns {
                host: "musicbrainz.org".into(),
            })],
        ),
        (&ids[1], vec![answer(429, "{}")]),
        (&ids[2], vec![answer(503, "{}")]),
        (&ids[3], vec![answer(500, "{}")]),
        (&ids[4], vec![answer(200, "<html></html>")]),
        (&ids[5], vec![answer(404, "")]),
        (&ids[6], vec![answer(200, "{\"title\": \"T\"}")]),
    ]);
    let mb = MusicBrainzClient::new(&t);
    let cache = RecordingCanonicalCache::new();
    let tmp = tempfile::tempdir().unwrap();
    let conn = db_copy(tmp.path());
    let ask = |mbid: &str| {
        conn.execute(
            "UPDATE check_findings SET detail_json=?1 WHERE id=?2",
            rusqlite::params![serde_json::json!({"recording_mbid": mbid}).to_string(), dup],
        )
        .unwrap();
        let body = duplicate_finding_canonical(&conn, dup, &mb, &cache).unwrap();
        let v = body.to_value();
        (
            v["reason_code"].as_str().map(str::to_owned),
            v["reason"].as_str().map(str::to_owned),
        )
    };
    let want: [(&str, &str); 6] = [
        ("unreachable", deltas::UNREACHABLE),
        ("rate_limited", deltas::RATE_LIMITED),
        ("rate_limited", deltas::RATE_LIMITED),
        (
            "service_error",
            "MusicBrainz reported an error (HTTP 500). Try again later.",
        ),
        ("unreadable_answer", deltas::UNREADABLE),
        (
            "not_found",
            "MusicBrainz didn't return a recording for that MBID",
        ),
    ];
    for (i, (code, text)) in want.iter().enumerate() {
        assert_eq!(
            ask(&ids[i]),
            (Some((*code).to_owned()), Some((*text).to_owned())),
            "{i}"
        );
    }
    assert_eq!(
        ask(&ids[6]),
        (None, None),
        "a found recording has no reason"
    );
    // No MBID at all.
    conn.execute(
        "UPDATE check_findings SET detail_json='{}' WHERE id=?1",
        rusqlite::params![dup],
    )
    .unwrap();
    let v = duplicate_finding_canonical(&conn, dup, &mb, &cache)
        .unwrap()
        .to_value();
    assert_eq!(v["reason_code"], "no_mbid");
    assert_eq!(v["reason"], "no recording MBID for this cluster");
}
