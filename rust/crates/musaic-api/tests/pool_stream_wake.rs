//! One wake-up per
//! `GET /api/fingerprint-pool/stream` connection.
//!
//! The Python app's connections share one `asyncio.Event`
//! (musaic/api/library.py:161-193): a connection opened while the event is
//! still set sends the state twice, and a connection busy sending when
//! another one clears the event misses that change. Recorded in
//! testdata/golden/fingerprint/pool.json `stream_on_connect_after_change`.
//!
//! Deltas pinned here (against that golden and the pool state the test
//! sets):
//! * after a change nobody consumed, a new connection sends the state once
//!   (the golden's first frame, byte for byte) and then only the keepalive;
//! * a connection that is not reading while changes happen (its frames back
//!   up) still receives the newest state once it reads again, whatever the
//!   other connections did meanwhile;
//! * every waiting connection is woken by one change and sends it once.

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use common::{Sandbox, call};
use http_body_util::BodyExt;
use musaic_api::{AppState, Network, router};
use musaic_services::fingerprint_pool::{KEEPALIVE_FRAME, PoolState, PoolStatus};
use regex::Regex;
use serde_json::{Value, json};
use tower::ServiceExt;

fn pool_golden() -> Value {
    let p = musaic_testdata::public("golden/fingerprint/pool.json");
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn state(sb: &Sandbox, ping: Duration) -> AppState {
    AppState::builder(&sb.cfg)
        .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
        .network(Network::Offline)
        .fingerprinting(common::no_key_fingerprinting())
        .frontend_dir(None)
        .subscribers(false)
        .auto_snapshot(false)
        .ping_interval(ping)
        .build()
        .unwrap()
}

/// An open SSE response, read frame by frame.
struct Stream {
    body: Body,
    buf: String,
}

impl Stream {
    async fn open(app: &axum::Router) -> Stream {
        let resp = app
            .clone()
            .oneshot(common::get("/api/fingerprint-pool/stream"))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        Stream {
            body: resp.into_body(),
            buf: String::new(),
        }
    }

    /// The next `\n\n`-terminated frame, or `None` after `wait`.
    async fn next(&mut self, wait: Duration) -> Option<String> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            if let Some(i) = self.buf.find("\n\n") {
                let f = self.buf[..i + 2].to_string();
                self.buf.drain(..i + 2);
                return Some(f);
            }
            let left = deadline.checked_duration_since(tokio::time::Instant::now())?;
            match tokio::time::timeout(left, self.body.frame()).await {
                Ok(Some(Ok(f))) => {
                    if let Ok(d) = f.into_data() {
                        self.buf.push_str(&String::from_utf8_lossy(&d));
                    }
                }
                _ => return None,
            }
        }
    }
}

fn norm_frame(f: &str, lib: &str) -> String {
    let iso = Regex::new(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ").unwrap();
    iso.replace_all(&musaic_testdata::replace_root(f, lib, "<LIB>"), "<NOW>")
        .into_owned()
}

/// A pool change as the pool makes one: new state, then the change event.
fn change(st: &AppState, completed: usize) -> String {
    let s = PoolState {
        status: PoolStatus::Running,
        library_dir: Some("/lib".into()),
        total: 100,
        completed,
        ..PoolState::default()
    };
    let frame = musaic_api::render::system::pool_frame(&s);
    st.pool().set_state(s);
    st.pool().changed().set();
    frame
}

// Deliberate change (one wake-up per connection), a delta on pool.json
// `stream_on_connect_after_change`:
// the Python app sent the state twice (frames[0], frames[1]); here once, then
// the keepalive. The pool's event is left set (no stream clears it).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unconsumed_change_is_sent_once_then_keepalive() {
    let g = pool_golden();
    let want = &g["stream_on_connect_after_change"];
    let sb = Sandbox::new();
    let st = state(&sb, Duration::from_millis(300));
    let app = router(st.clone());
    let r = call(
        &app,
        "POST",
        "/api/config",
        Some(&json!({"library_dir": Sandbox::s(&sb.lib)})),
    )
    .await;
    assert_eq!(r.status, 200);
    let scan = call(&app, "GET", "/api/scan/stream", None).await;
    assert!(scan.text().contains("event: done"));
    assert_eq!(st.pool().state().status, PoolStatus::Unavailable);
    assert_eq!(
        json!(st.pool().changed().is_set()),
        want["event_set_before_connect"]
    );
    let lib = Sandbox::s(&sb.lib);
    let golden_first = norm_frame(
        &want["frames"][0]
            .as_str()
            .unwrap()
            .replace("/some/library", &lib),
        &lib,
    );
    let mut s = Stream::open(&app).await;
    let first = s.next(Duration::from_secs(5)).await.unwrap();
    assert_eq!(norm_frame(&first, &lib), golden_first);
    // The Python app's second frame (the same state again) is not sent.
    let second = s.next(Duration::from_secs(5)).await.unwrap();
    assert_eq!(second, KEEPALIVE_FRAME, "no second state frame");
    assert_eq!(json!(KEEPALIVE_FRAME), g["stream"]["keepalive"]);
}

// A connection that does not read while the pool
// changes (its frames back up behind the response body) is not starved by
// another connection consuming the changes: once it reads again it gets the
// newest state, before any keepalive. In the Python app the reading connection
// cleared the shared event and the other one waited for the next change.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_busy_connection_does_not_miss_a_change() {
    let sb = Sandbox::new();
    let st = state(&sb, Duration::from_secs(30));
    let app = router(st.clone());
    let mut busy = Stream::open(&app).await;
    let mut reader = Stream::open(&app).await;
    assert!(busy.next(Duration::from_secs(5)).await.is_some());
    assert!(reader.next(Duration::from_secs(5)).await.is_some());
    let mut last = String::new();
    for n in 1..=8 {
        last = change(&st, n);
        // The reader takes each change as it comes.
        let f = reader.next(Duration::from_secs(5)).await.expect("woken");
        assert_eq!(f, last, "reader frame {n}");
    }
    // The busy connection: whatever backed up, ending with the newest state.
    let mut got = Vec::new();
    while let Some(f) = busy.next(Duration::from_secs(2)).await {
        assert_ne!(f, KEEPALIVE_FRAME);
        let done = f == last;
        got.push(f);
        if done {
            break;
        }
    }
    assert_eq!(got.last(), Some(&last), "busy connection frames: {got:?}");
    // Then it is idle: no change is pending for it.
    assert_eq!(busy.next(Duration::from_millis(500)).await, None);
}

// One change wakes every waiting connection exactly once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_change_wakes_each_connection_once() {
    let sb = Sandbox::new();
    let st = state(&sb, Duration::from_secs(30));
    let app = router(st.clone());
    let mut streams = Vec::new();
    for _ in 0..3 {
        let mut s = Stream::open(&app).await;
        assert!(s.next(Duration::from_secs(5)).await.is_some());
        streams.push(s);
    }
    let frame = change(&st, 1);
    for s in &mut streams {
        assert_eq!(
            s.next(Duration::from_secs(5)).await.as_deref(),
            Some(frame.as_str())
        );
        assert_eq!(s.next(Duration::from_millis(300)).await, None, "sent once");
    }
}
