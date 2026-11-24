//! FR-003: the old built-in provider's settings and keys are gone from the
//! settings body, and `PATCH /api/settings` ignores their names like any
//! unknown key (nothing stored, no `SettingsChanged`, the keys never reach
//! the keyring).

mod common;

use std::sync::{Arc, Mutex};

use common::{Sandbox, call};
use musaic_api::{AppState, Network, router};
use musaic_events::{Event, EventKind, Subscriber, Topic};
use serde_json::json;

const RETIRED_FIELDS: [&str; 6] = [
    "ai_provider_id",
    "ai_model_id",
    "ai_max_parallel",
    "ai_queue_paused",
    "ai_api_key_set",
    "firecrawl_api_key_set",
];

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_settings_body_has_no_retired_field_and_patch_ignores_them() {
    let sb = Sandbox::new();
    let store = Arc::new(musaic_secrets::MemoryStore::new());
    let st = AppState::builder(&sb.cfg)
        .secrets(store.clone())
        .network(Network::Offline)
        .fingerprinting(common::no_key_fingerprinting())
        .auto_snapshot(false)
        .build()
        .unwrap();
    let seen: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
    let s2 = Arc::clone(&seen);
    st.bus().register(
        Topic::Kind(EventKind::SettingsChanged),
        Subscriber::from_fn(move |e| {
            if let Event::SettingsChanged { keys } = e {
                s2.lock().unwrap().push(keys.clone());
            }
            Ok(())
        }),
    );
    let app = router(st.clone());

    let before = call(&app, "GET", "/api/settings", None).await;
    assert_eq!(before.status, 200);
    let before = before.json();
    for k in RETIRED_FIELDS {
        assert!(before.get(k).is_none(), "{k} in {before}");
    }
    assert_eq!(before["acoustid_api_key_set"], json!(false));

    let secret = "retired-key-value-0123456789";
    let r = call(
        &app,
        "PATCH",
        "/api/settings",
        Some(&json!({
            "ai_provider_id": "bedrock",
            "ai_model_id": "some-model",
            "ai_max_parallel": 99,
            "ai_queue_paused": "not a bool",
            "ai_api_key": secret,
            "firecrawl_api_key": secret,
        })),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(!r.text().contains(secret));
    let after = r.json();
    for k in RETIRED_FIELDS {
        assert!(after.get(k).is_none(), "{k} in {after}");
    }
    assert_eq!(after, before, "nothing changed");
    assert_eq!(
        call(&app, "GET", "/api/settings", None).await.json(),
        before
    );
    assert!(
        st.bus()
            .wait_background_idle(std::time::Duration::from_secs(30))
    );
    assert!(
        seen.lock().unwrap().is_empty(),
        "no SettingsChanged: {:?}",
        seen.lock().unwrap()
    );
    assert!(store.entries().is_empty(), "nothing reached the keyring");
    let conn = rusqlite::Connection::open(sb.cfg.join("library.db")).unwrap();
    let rows: i64 = conn
        .query_row(
            "SELECT count(*) FROM settings WHERE key IN \
             ('ai_provider_id','ai_model_id','ai_max_parallel','ai_queue_paused',\
              'ai_api_key','firecrawl_api_key')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rows, 0, "no retired row written");
}
