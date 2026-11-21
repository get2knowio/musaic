//! `GET /api/scan/follow` (issue #332): the scan stream for a reader that
//! only watches. Unlike `/api/scan/stream`, it never starts a scan: with no
//! scan since startup it answers one `idle` event and closes; after a scan
//! it replays that scan to its terminal event. Not a route of the Python app
//! (`EXTRA_ROUTES`), so nothing is recorded.

mod common;

use std::time::Duration;

use common::{Sandbox, call};
use musaic_api::router;
use serde_json::{Value, json};

async fn configure(app: &axum::Router, sb: &Sandbox) {
    let r = call(
        app,
        "POST",
        "/api/config",
        Some(&json!({"library_dir": Sandbox::s(&sb.lib)})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
}

async fn status(app: &axum::Router) -> Value {
    call(app, "GET", "/api/scan/status", None).await.json()
}

fn event_names(text: &str) -> Vec<&str> {
    text.lines()
        .filter_map(|l| l.strip_prefix("event: "))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn follow_with_no_scan_is_one_idle_event_and_starts_nothing() {
    let sb = Sandbox::new();
    let app = router(sb.state_no_lifespan());
    configure(&app, &sb).await;

    let got = call(&app, "GET", "/api/scan/follow", None).await;
    assert_eq!(got.status, 200, "{}", got.text());
    assert!(
        got.ctype().starts_with("text/event-stream"),
        "{}",
        got.ctype()
    );
    let text = got.text();
    assert_eq!(event_names(&text), ["idle"], "{text}");
    let data: Value = serde_json::from_str(
        text.lines()
            .find_map(|l| l.strip_prefix("data: "))
            .expect("a data line"),
    )
    .unwrap();
    assert_eq!(data["status"], "idle", "{data}");

    // Nothing was started by asking.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(status(&app).await["status"], "idle");
    let albums = call(&app, "GET", "/api/albums", None).await.json();
    assert_eq!(albums["count"], 0, "{albums}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn follow_after_a_scan_replays_it_to_its_end_and_starts_no_other() {
    let sb = Sandbox::new();
    let app = router(sb.state_no_lifespan());
    configure(&app, &sb).await;
    let scan = call(&app, "GET", "/api/scan/stream", None).await.text();
    assert!(scan.contains("event: done"), "{scan}");
    let mut ended = status(&app).await;
    for _ in 0..2000 {
        if ended["status"] == "done" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
        ended = status(&app).await;
    }
    assert_eq!(ended["status"], "done", "{ended}");

    let follow = call(&app, "GET", "/api/scan/follow", None).await;
    assert_eq!(follow.status, 200);
    let text = follow.text();
    assert_eq!(text, scan, "the same scan, replayed frame for frame");
    assert_eq!(event_names(&text).last(), Some(&"done"));

    tokio::time::sleep(Duration::from_millis(50)).await;
    let after = status(&app).await;
    assert_eq!(after["status"], "done");
    assert_eq!(after["started_at"], ended["started_at"], "no new scan");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn follow_answers_get_only() {
    let sb = Sandbox::new();
    let app = router(sb.state_no_lifespan());
    let got = call(&app, "POST", "/api/scan/follow", None).await;
    assert_eq!(got.status, 405, "{}", got.text());
}
