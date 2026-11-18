//! The scopes a check run may be asked for over HTTP: `all` and `albums`.
//! `incremental` is refused with a plain 400 (the background sweep does that
//! work), and an unknown scope gets the same message.

mod common;

use common::{Sandbox, call, no_key_fingerprinting};
use musaic_api::{AppState, Network, router};
use serde_json::json;

const MSG: &str = "scope must be 'all' or 'albums'";

#[tokio::test]
async fn only_all_and_albums_are_accepted() {
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

    for scope in ["incremental", "bogus"] {
        let body = json!({ "scope": scope });
        let r = call(&app, "POST", "/api/checks/track-gaps/run", Some(&body)).await;
        assert_eq!(r.status, 400, "{scope}: {}", r.text());
        assert_eq!(r.json()["detail"], MSG, "{scope}");
    }

    let body = json!({"scope": "all"});
    let r = call(&app, "POST", "/api/checks/track-gaps/run", Some(&body)).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["scope"], "all");

    let albums = call(&app, "GET", "/api/albums", None).await.json();
    let id = albums["albums"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|a| a["id"].as_str())
        .expect("an album")
        .to_string();
    let body = json!({"scope": "albums", "album_ids": [id]});
    let r = call(&app, "POST", "/api/checks/track-gaps/run", Some(&body)).await;
    assert_eq!(r.status, 200, "{}", r.text());

    // The 400 for an albums scope without ids is unchanged.
    let body = json!({"scope": "albums"});
    let r = call(&app, "POST", "/api/checks/track-gaps/run", Some(&body)).await;
    assert_eq!(r.status, 400, "{}", r.text());
}
