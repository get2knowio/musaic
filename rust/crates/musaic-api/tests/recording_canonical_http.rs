//! `GET /api/checks/duplicate-tracks/findings/{id}/canonical` through the
//! router (the last 501 stub is gone), against the
//! Python app's recorded answers in testdata/golden/checks/recording_canonical.json.
//!
//! Same database (a copy of testdata/db/v16-synthetic-offline.db as the
//! app's library.db), same order, one app (one server lifetime, so one
//! cache), the duplicate finding's detail set with the same SQL text,
//! MusicBrainz from the recorded replay set (private) plus the golden's CONSTRUCTED overlay.
//! Each step must answer with the Python app's status, content type and body
//! bytes, and make the Python app's outbound requests, with the deliberate changes
//! (failed lookups asked again, a JSON array is "no recording") applied (musaic-services
//! tests/deltas/recording_canonical.rs).

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use common::{Sandbox, call};
use musaic_api::{AppState, Network, router};
use musaic_net::{ReplayTransport, Request, Response, Transport, TransportError};
use serde_json::Value;

#[path = "../../musaic-services/tests/deltas/recording_canonical.rs"]
mod deltas;

fn golden() -> Value {
    musaic_testdata::golden("checks/recording_canonical.json")
}

struct Overlaid {
    overlay: HashMap<String, Response>,
    replay: ReplayTransport,
    log: Mutex<Vec<(String, String, String)>>,
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

fn overlaid(g: &Value) -> Arc<Overlaid> {
    Arc::new(Overlaid {
        overlay: g["overlay"]
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
            .collect(),
        replay: ReplayTransport::new(musaic_testdata::private_required("replay")),
        log: Mutex::default(),
    })
}

// Finding not found -> 404; finding of another check -> 404;
// non-integer id -> 422 (FastAPI's body); no recording MBID (null, "");
// found (title + joined artist); MusicBrainz 404; a failed request; 503;
// not JSON; blank title; joinphrases; no credit; a JSON array; and the
// once-per-lifetime cache: a repeated found or 404 MBID makes no request.
// A failed request, a 503 and a 200 that is not JSON are
// asked again on their repeat (the Python app answered from memory). Deliberate
// change: the JSON array answers 200 "no recording" (the Python app's 500
// plain text), asked again next time on both sides.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn canonical_route_answers_every_step_like_the_python_app() {
    musaic_testdata::require_private!("replay", "golden/checks/recording_canonical.json");
    let g = golden();
    let dup = g["provenance"]["duplicate_finding_id"].as_i64().unwrap();
    let sb = Sandbox::new();
    let db = sb.cfg.join("library.db");
    std::fs::copy(musaic_testdata::public("db/v16-synthetic-offline.db"), &db).unwrap();
    let t = overlaid(&g);
    let st = AppState::builder(&sb.cfg)
        .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
        .network(Network::Live(t.clone()))
        .fingerprinting(common::no_key_fingerprinting())
        .subscribers(false)
        .auto_snapshot(false)
        .build()
        .unwrap();
    let app = router(st.clone());
    for s in &deltas::expected_steps(&g) {
        let label = s["label"].as_str().unwrap();
        if let Some(d) = s["set_detail_json"].as_str() {
            rusqlite::Connection::open(&db)
                .unwrap()
                .execute(
                    "UPDATE check_findings SET detail_json=?1 WHERE id=?2",
                    rusqlite::params![d, dup],
                )
                .unwrap();
        }
        t.log.lock().unwrap().clear();
        let got = call(&app, "GET", s["path"].as_str().unwrap(), None).await;
        let want = &s["response"];
        assert_eq!(
            got.status as u64,
            want["status"].as_u64().unwrap(),
            "{label}: {}",
            got.text()
        );
        assert_eq!(
            got.ctype(),
            want["content_type"].as_str().unwrap(),
            "{label}"
        );
        match want["raw"].as_str() {
            // FastAPI's JSONResponse bytes (key order included), the shared
            // path validation's 422 (crate::lax) as well.
            Some(raw) => assert_eq!(got.text(), raw, "{label}"),
            None => assert_eq!(got.text(), want["text"].as_str().unwrap(), "{label}"),
        }
        assert_eq!(
            got.route.as_deref(),
            Some("/api/checks/duplicate-tracks/findings/{finding_id}/canonical"),
            "{label}"
        );
        let reqs = std::mem::take(&mut *t.log.lock().unwrap());
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
    }
}

// The cache is per server lifetime: a second app (a restart) asks again
// for an MBID the first one had cached.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_server_asks_again() {
    musaic_testdata::require_private!("replay", "golden/checks/recording_canonical.json");
    let g = golden();
    let dup = g["provenance"]["duplicate_finding_id"].as_i64().unwrap();
    let found = g["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["label"] == "found")
        .unwrap();
    let sb = Sandbox::new();
    let db = sb.cfg.join("library.db");
    std::fs::copy(musaic_testdata::public("db/v16-synthetic-offline.db"), &db).unwrap();
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute(
            "UPDATE check_findings SET detail_json=?1 WHERE id=?2",
            rusqlite::params![found["set_detail_json"].as_str().unwrap(), dup],
        )
        .unwrap();
    let t = overlaid(&g);
    for _ in 0..2 {
        let st = AppState::builder(&sb.cfg)
            .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
            .network(Network::Live(t.clone()))
            .fingerprinting(common::no_key_fingerprinting())
            .subscribers(false)
            .auto_snapshot(false)
            .build()
            .unwrap();
        let app = router(st);
        for _ in 0..2 {
            let got = call(&app, "GET", found["path"].as_str().unwrap(), None).await;
            assert_eq!(got.text(), found["response"]["raw"].as_str().unwrap());
        }
    }
    assert_eq!(t.log.lock().unwrap().len(), 2, "one request per server");
}
