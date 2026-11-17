//! A user-started check run publishes one `ChecksRerunFinished`, after the
//! run's findings are written, and the library-events relay carries it.
//! A run that never starts (unknown check) publishes nothing.

mod common;

use common::{Sandbox, call, no_key_fingerprinting};
use musaic_api::{AppState, Network, router};
use serde_json::{Value, json};

#[tokio::test]
async fn a_user_started_run_publishes_one_finished_event() {
    let sb = Sandbox::new();
    let st: AppState = AppState::builder(&sb.cfg)
        .secrets(std::sync::Arc::new(musaic_secrets::MemoryStore::new()))
        .network(Network::Offline)
        .fingerprinting(no_key_fingerprinting())
        .frontend_dir(None)
        .subscribers(false)
        .auto_snapshot(false)
        .build()
        .unwrap();
    let relay = st.relay().clone();
    let app = router(st);
    let r = call(
        &app,
        "POST",
        "/api/config",
        Some(&json!({"library_dir": Sandbox::s(&sb.lib)})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(
        call(&app, "GET", "/api/scan/stream", None)
            .await
            .text()
            .contains("event: done")
    );

    let conn = relay.connect();
    let r = call(&app, "POST", "/api/checks/nope/run", Some(&json!({}))).await;
    assert_eq!(r.status, 400, "{}", r.text());
    let r = call(
        &app,
        "POST",
        "/api/checks/track-gaps/run",
        Some(&json!({"scope": "all"})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());

    // The event is published before the response is, so it is queued
    // already; the run's own event is the only ChecksRerunFinished.
    let mut finished = Vec::new();
    while let Some(m) = conn.try_recv() {
        if &*m.name == "ChecksRerunFinished" {
            finished.push(serde_json::from_str::<Value>(&m.data).unwrap());
        }
    }
    assert_eq!(
        finished,
        [json!({"checks": ["track-gaps"], "failed": [], "album_ids": []})]
    );
}
