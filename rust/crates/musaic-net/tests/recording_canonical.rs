//! `mb_lookup.get_recording_canonical` (mb_lookup.py:538-594) in this crate:
//! the request builder against two recordings of a live run and the Python
//! app's own prepared requests, and the client's answers against the Python
//! app's (testdata/golden/checks/recording_canonical.json). The recording
//! lookup backs the duplicate-tracks canonical route.

mod common;

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use base64::Engine as _;
use common::{replay_dir, replay_record};
use musaic_log::{Level, capture};
use musaic_net::{
    LookupFailure, MB_RECORDING_TIMEOUT, Method, MusicBrainzClient, RecordingCanonical,
    RecordingLookup, ReplayTransport, Request, Response, Transport, TransportError, USER_AGENT,
};
use serde_json::Value;

/// The recorded recording that exists (private).
fn found_mbid() -> &'static str {
    common::rec("recording_found")
}
const MISSING: &str = "00000000-0000-4000-8000-000000000000";

fn golden() -> Value {
    musaic_testdata::golden("checks/recording_canonical.json")
}

fn built(mbid: &str) -> Request {
    MusicBrainzClient::<ReplayTransport>::recording_canonical_request(mbid)
}

fn replay() -> ReplayTransport {
    ReplayTransport::new(replay_dir())
}

// Request shape: GET, fmt=json, inc=artist-credits, the
// MusicBrainz User-Agent, the Python app's timeout=10; the built request has
// the replay key of both recordings and of every request the Python app itself
// prepared for the golden's constructed answers.
#[test]
fn request_matches_the_recordings_and_the_python_app_requests() {
    musaic_testdata::require_private!("replay", "golden/checks/recording_canonical.json");
    for (mbid, status) in [(found_mbid(), 200), (MISSING, 404)] {
        let req = built(mbid);
        assert_eq!(req.method, Method::Get);
        assert_eq!(
            req.url(),
            format!("https://musicbrainz.org/ws/2/recording/{mbid}?fmt=json&inc=artist-credits")
        );
        assert!(
            req.headers
                .iter()
                .any(|(k, v)| k == "User-Agent" && v == USER_AGENT)
        );
        assert_eq!(req.timeout, Some(MB_RECORDING_TIMEOUT));
        assert_eq!(MB_RECORDING_TIMEOUT, Duration::from_secs(10));
        let rec = replay_record(&req.replay_key());
        assert_eq!(rec["url"], req.url());
        assert_eq!(rec["method"], "GET");
        assert_eq!(rec["status"], status);
    }
    let g = golden();
    let mut n = 0;
    for o in g["overlay"].as_array().unwrap() {
        let url = o["url"].as_str().unwrap();
        let mbid = url
            .trim_start_matches("https://musicbrainz.org/ws/2/recording/")
            .split('?')
            .next()
            .unwrap();
        assert_eq!(built(mbid).replay_key(), o["key"], "{url}");
        n += 1;
    }
    for c in g["function_cases"].as_array().unwrap() {
        assert_eq!(built(c["mbid"].as_str().unwrap()).replay_key(), c["key"]);
        n += 1;
    }
    assert!(n >= 30, "{n}");
}

// Found, 404 over the recordings: the Python app's answer for
// the found recording (title + joined artist), `None` for the 404 without a
// log line, one request each.
#[test]
fn recorded_found_and_404() {
    musaic_testdata::require_private!("replay", "golden/checks/recording_canonical.json");
    let g = golden();
    let found = g["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["label"] == "found")
        .unwrap();
    let r = replay();
    let client = MusicBrainzClient::new(&r);
    let (got, lines) = capture(|| client.recording_canonical(found_mbid()));
    let want = &found["response"]["json"];
    assert_eq!(
        got.unwrap(),
        Some(RecordingCanonical {
            title: want["title"].as_str().unwrap().into(),
            artist: want["artist"].as_str().unwrap().into(),
        })
    );
    assert!(lines.is_empty());
    let (got, lines) = capture(|| client.recording_canonical(MISSING));
    assert_eq!(got.unwrap(), None);
    assert!(lines.is_empty(), "{lines:?}");
    assert_eq!(r.hits().len(), 2);
    // `if not mbid: return None`: no request.
    assert_eq!(client.recording_canonical("").unwrap(), None);
    assert_eq!(r.hits().len(), 2);
}

/// One canned answer per request.
struct Canned(Mutex<HashMap<String, Result<Response, TransportError>>>);

impl Transport for Canned {
    fn send(&self, req: &Request) -> Result<Response, TransportError> {
        self.0
            .lock()
            .unwrap()
            .remove(&req.replay_key())
            .expect("one request per key")
    }
}

// mb_lookup.py:556 (a failed request): WARNING
// `MB recording lookup failed for {mbid}: {e}` and `None`. The text after
// the colon is this build's transport error (the Python app's is requests').
#[test]
fn failed_request_logs_and_answers_none() {
    let g = golden();
    let step = g["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["requests"][0]["served"] == "unobserved")
        .unwrap();
    let mbid = step["response"]["json"]["mbid"].as_str().unwrap();
    let t = Canned(Mutex::new(HashMap::from([(
        built(mbid).replay_key(),
        Err(TransportError::Dns {
            host: "musicbrainz.org".into(),
        }),
    )])));
    let (got, lines) = capture(|| MusicBrainzClient::new(&t).recording_canonical(mbid));
    assert_eq!(got.unwrap(), None);
    let want = &step["log"][0];
    let (head, _) = want["message"].as_str().unwrap().split_once(": ").unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].level, Level::Warning);
    assert_eq!(want["level"], "WARNING");
    assert_eq!(lines[0].target, want["logger"].as_str().unwrap());
    assert_eq!(
        lines[0].message,
        format!("{head}: could not resolve host musicbrainz.org")
    );
}

// mb_lookup.py:538-594 on 200 bodies of every shape its code branches on
// (golden `function_cases`, the Python app's function without its cache):
// `Some` with the Python app's title and artist, `None` where it returned None,
// `Err` where it raised (the route's 500).
#[test]
fn every_body_shape_answers_as_the_python() {
    let g = golden();
    let mut answers = HashMap::new();
    for c in g["function_cases"].as_array().unwrap() {
        answers.insert(
            c["key"].as_str().unwrap().to_owned(),
            Ok(Response {
                status: 200,
                headers: vec![],
                body: c["body_json"].as_str().unwrap().as_bytes().to_vec(),
            }),
        );
    }
    for o in g["overlay"].as_array().unwrap() {
        answers.insert(
            o["key"].as_str().unwrap().to_owned(),
            Ok(Response {
                status: o["status"].as_u64().unwrap() as u16,
                headers: vec![],
                body: base64::engine::general_purpose::STANDARD
                    .decode(o["content_b64"].as_str().unwrap())
                    .unwrap(),
            }),
        );
    }
    let t = Canned(Mutex::new(answers));
    let client = MusicBrainzClient::new(&t);
    let mut raised = 0;
    for c in g["function_cases"].as_array().unwrap() {
        let name = c["name"].as_str().unwrap();
        let got = client.recording_canonical(c["mbid"].as_str().unwrap());
        match (&got, c.get("raises")) {
            (Err(_), Some(_)) => raised += 1,
            (Ok(v), None) => {
                let want = &c["returns"];
                let v = v
                    .as_ref()
                    .map(|r| serde_json::json!({"title": r.title, "artist": r.artist}));
                assert_eq!(v.unwrap_or(Value::Null), *want, "{name}");
            }
            _ => panic!("{name}: {got:?} vs {c}"),
        }
    }
    assert!(raised >= 10, "{raised}");
    // The route steps' constructed answers through the client: 503 (logged),
    // not JSON, blank title, joinphrases, no credit, JSON array (raises).
    for s in g["steps"].as_array().unwrap() {
        let Some(req) = s["requests"].as_array().and_then(|r| r.first()) else {
            continue;
        };
        if req["served"] != "constructed" {
            continue;
        }
        let label = s["label"].as_str().unwrap();
        let mbid = s["set_detail_json"]
            .as_str()
            .map(|d| serde_json::from_str::<Value>(d).unwrap()["recording_mbid"].clone())
            .unwrap_or_else(|| s["response"]["json"]["mbid"].clone());
        let Some(mbid) = mbid.as_str() else {
            continue; // a repeat step: the answer was already taken
        };
        let (got, lines) = capture(|| client.recording_canonical(mbid));
        let resp = &s["response"];
        match got {
            Err(_) => assert_eq!(resp["status"], 500, "{label}"),
            Ok(Some(c)) => {
                assert_eq!(resp["json"]["title"], c.title.as_str(), "{label}");
                assert_eq!(resp["json"]["artist"], c.artist.as_str(), "{label}");
            }
            Ok(None) => assert_eq!(resp["json"]["title"], Value::Null, "{label}"),
        }
        let msgs: Vec<&str> = lines.iter().map(|l| l.message.as_str()).collect();
        let want: Vec<&str> = s["log"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["message"].as_str().unwrap())
            .collect();
        assert_eq!(msgs, want, "{label}");
    }
}

// The lookup's outcome for the
// per-lifetime cache. Done (remembered): the recorded found and 404
// answers, and every 200 body the Python app's function returned for (a blank
// or missing title included). Failed (asked again next time): a failed
// request, the 503, the 200 that is not JSON (logged as a body that is not
// JSON), and every body the Python app's function raised on, each of those
// logged as a malformed answer. The
// requests and the Python app's own log lines are the parity function's.
#[test]
fn lookup_outcomes_done_and_failed() {
    musaic_testdata::require_private!("replay", "golden/checks/recording_canonical.json");
    let g = golden();
    let r = replay();
    let client = MusicBrainzClient::new(&r);
    let found = g["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["label"] == "found")
        .unwrap();
    match capture(|| client.recording_canonical_lookup(found_mbid())) {
        (RecordingLookup::Done(Some(c)), lines) => {
            assert_eq!(found["response"]["json"]["title"], c.title.as_str());
            assert_eq!(found["response"]["json"]["artist"], c.artist.as_str());
            assert!(lines.is_empty());
        }
        other => panic!("{other:?}"),
    }
    let (got, lines) = capture(|| client.recording_canonical_lookup(MISSING));
    assert_eq!(got, RecordingLookup::Done(None));
    assert!(lines.is_empty());

    let failed = Canned(Mutex::new(HashMap::from([(
        built(found_mbid()).replay_key(),
        Err(TransportError::Dns {
            host: "musicbrainz.org".into(),
        }),
    )])));
    let (got, lines) =
        capture(|| MusicBrainzClient::new(&failed).recording_canonical_lookup(found_mbid()));
    assert_eq!(got, RecordingLookup::Failed(LookupFailure::Unreachable));
    assert_eq!(lines.len(), 1);
    assert!(
        lines[0]
            .message
            .starts_with("MB recording lookup failed for ")
    );

    let mut answers = HashMap::new();
    for c in g["function_cases"].as_array().unwrap() {
        answers.insert(
            c["key"].as_str().unwrap().to_owned(),
            Ok(Response {
                status: 200,
                headers: vec![],
                body: c["body_json"].as_str().unwrap().as_bytes().to_vec(),
            }),
        );
    }
    for o in g["overlay"].as_array().unwrap() {
        answers.insert(
            o["key"].as_str().unwrap().to_owned(),
            Ok(Response {
                status: o["status"].as_u64().unwrap() as u16,
                headers: vec![],
                body: base64::engine::general_purpose::STANDARD
                    .decode(o["content_b64"].as_str().unwrap())
                    .unwrap(),
            }),
        );
    }
    let t = Canned(Mutex::new(answers));
    let client = MusicBrainzClient::new(&t);
    let (mut done, mut malformed) = (0, 0);
    for c in g["function_cases"].as_array().unwrap() {
        let name = c["name"].as_str().unwrap();
        let mbid = c["mbid"].as_str().unwrap();
        let (got, lines) = capture(|| client.recording_canonical_lookup(mbid));
        if c.get("raises").is_some() {
            assert_eq!(
                got,
                RecordingLookup::Failed(LookupFailure::UnreadableAnswer),
                "{name}"
            );
            assert_eq!(lines.len(), 1, "{name}: {lines:?}");
            assert_eq!(lines[0].level, Level::Warning);
            assert_eq!(lines[0].target, "musaic.mb_lookup");
            let head = format!("MB recording lookup for {mbid} returned a malformed answer: ");
            assert!(
                lines[0].message.starts_with(&head),
                "{name}: {}",
                lines[0].message
            );
            malformed += 1;
        } else {
            let want = &c["returns"];
            let v = got
                .answer()
                .map(|r| serde_json::json!({"title": r.title, "artist": r.artist}));
            assert!(matches!(got, RecordingLookup::Done(_)), "{name}");
            assert_eq!(v.unwrap_or(Value::Null), *want, "{name}");
            assert!(lines.is_empty(), "{name}");
            done += 1;
        }
    }
    assert!(malformed >= 10 && done >= 10, "{malformed} {done}");
    let notes: HashMap<&str, RecordingLookup> = [
        (
            "CONSTRUCTED: service unavailable",
            RecordingLookup::Failed(LookupFailure::RateLimited),
        ),
        (
            "CONSTRUCTED: 200 whose body is not JSON",
            RecordingLookup::Failed(LookupFailure::UnreadableAnswer),
        ),
        ("CONSTRUCTED: blank title", RecordingLookup::Done(None)),
        (
            "CONSTRUCTED: a JSON array (the Python app's code raises)",
            RecordingLookup::Failed(LookupFailure::UnreadableAnswer),
        ),
    ]
    .into();
    let (mut seen, mut not_json) = (0, 0);
    for o in g["overlay"].as_array().unwrap() {
        let Some(want) = notes.get(o["note"].as_str().unwrap()) else {
            continue;
        };
        let url = o["url"].as_str().unwrap();
        let mbid = url
            .trim_start_matches("https://musicbrainz.org/ws/2/recording/")
            .split('?')
            .next()
            .unwrap();
        let (got, lines) = capture(|| client.recording_canonical_lookup(mbid));
        assert_eq!(&got, want, "{url}");
        if o["note"] == "CONSTRUCTED: 200 whose body is not JSON" {
            // Logged like the malformed answer (the Python app's
            // function returned None without a line; the parity function
            // still logs nothing, every_body_shape_answers_as_the_python).
            assert_eq!(lines.len(), 1, "{lines:?}");
            assert_eq!(lines[0].level, Level::Warning);
            assert_eq!(lines[0].target, "musaic.mb_lookup");
            let head = format!("MB recording lookup for {mbid} returned a body that is not JSON: ");
            assert!(
                lines[0].message.starts_with(&head) && lines[0].message.len() > head.len(),
                "{}",
                lines[0].message
            );
            not_json += 1;
        }
        seen += 1;
    }
    assert_eq!(seen, notes.len());
    assert_eq!(not_json, 1);
    // A long malformed answer is cut in the log line.
    let long = "f0000000-0000-4000-8000-0000000000ff";
    let t = Canned(Mutex::new(HashMap::from([(
        built(long).replay_key(),
        Ok(Response {
            status: 200,
            headers: vec![],
            body: format!("[\"{}\"]", "x".repeat(5000)).into_bytes(),
        }),
    )])));
    let (got, lines) = capture(|| MusicBrainzClient::new(&t).recording_canonical_lookup(long));
    assert_eq!(
        got,
        RecordingLookup::Failed(LookupFailure::UnreadableAnswer)
    );
    assert!(lines[0].message.len() < 400, "{}", lines[0].message.len());
    assert!(lines[0].message.ends_with("..."));
    // A long non-JSON body: the parse error names a position, never the
    // body, and the line stays short.
    let t = Canned(Mutex::new(HashMap::from([(
        built(long).replay_key(),
        Ok(Response {
            status: 200,
            headers: vec![],
            body: format!("<html>{}</html>", "x".repeat(5000)).into_bytes(),
        }),
    )])));
    let (got, lines) = capture(|| MusicBrainzClient::new(&t).recording_canonical_lookup(long));
    assert_eq!(
        got,
        RecordingLookup::Failed(LookupFailure::UnreadableAnswer)
    );
    assert_eq!(lines.len(), 1);
    assert!(lines[0].message.len() < 400, "{}", lines[0].message.len());
    assert!(!lines[0].message.contains("xxxx"), "{}", lines[0].message);
}
