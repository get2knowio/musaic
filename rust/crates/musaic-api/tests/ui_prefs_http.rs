//! The light/dark theme and the Library tile size are kept by the server (the
//! desktop window's address changes on every launch, so browser storage
//! forgets them): round trip, refusals, persistence across a relaunch.

mod common;

use std::sync::{Arc, Mutex};

use common::{Sandbox, call};
use musaic_api::router;
use musaic_events::{Event, Subscriber, Topic};
use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ui_preferences_round_trip_and_survive_a_relaunch() {
    let sb = Sandbox::new();
    let st = sb.state_no_lifespan();
    let seen: Arc<Mutex<Vec<Event>>> = Arc::default();
    let sink = seen.clone();
    st.bus().register(
        Topic::Event,
        Subscriber::from_fn(move |ev| {
            sink.lock().unwrap().push(ev.clone());
            Ok(())
        })
        .priority(400)
        .name("test_recorder"),
    );
    let app = router(st.clone());

    // Never chosen: both unset.
    let r = call(&app, "GET", "/api/ui-prefs", None).await;
    assert_eq!(
        (r.status, r.json()),
        (200, json!({"theme": null, "tile_columns": null}))
    );

    let r = call(
        &app,
        "PUT",
        "/api/ui-prefs",
        Some(&json!({"theme": "light", "tile_columns": 5})),
    )
    .await;
    assert_eq!(
        (r.status, r.json()),
        (200, json!({"theme": "light", "tile_columns": 5}))
    );
    // Names are announced, never values.
    {
        let seen = seen.lock().unwrap();
        let Some(Event::SettingsChanged { keys }) = seen.last() else {
            panic!("no SettingsChanged: {seen:?}");
        };
        let mut keys = keys.clone();
        keys.sort();
        assert_eq!(keys, ["ui_theme", "ui_tile_columns"]);
    }

    // A field left out is untouched; null forgets one.
    let r = call(
        &app,
        "PUT",
        "/api/ui-prefs",
        Some(&json!({"theme": "dark"})),
    )
    .await;
    assert_eq!(r.json(), json!({"theme": "dark", "tile_columns": 5}));
    let r = call(
        &app,
        "PUT",
        "/api/ui-prefs",
        Some(&json!({"tile_columns": null})),
    )
    .await;
    assert_eq!(r.json(), json!({"theme": "dark", "tile_columns": null}));
    let r = call(
        &app,
        "PUT",
        "/api/ui-prefs",
        Some(&json!({"tile_columns": 7})),
    )
    .await;
    assert_eq!(r.json(), json!({"theme": "dark", "tile_columns": 7}));

    // A new app object on the same config dir (a relaunch) still has them.
    let again = router(sb.state_no_lifespan());
    let r = call(&again, "GET", "/api/ui-prefs", None).await;
    assert_eq!(r.json(), json!({"theme": "dark", "tile_columns": 7}));

    // They are not part of the settings body.
    let r = call(&again, "GET", "/api/settings", None).await;
    let body = r.json();
    assert!(body.get("ui_theme").is_none() && body.get("ui_tile_columns").is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ui_preferences_refuse_bad_values_and_change_nothing() {
    let sb = Sandbox::new();
    let app = router(sb.state_no_lifespan());
    call(
        &app,
        "PUT",
        "/api/ui-prefs",
        Some(&json!({"theme": "light", "tile_columns": 4})),
    )
    .await;
    for bad in [
        json!({"theme": "blue"}),
        json!({"theme": 1}),
        json!({"tile_columns": 2}),
        json!({"tile_columns": 8}),
        json!({"tile_columns": 4.5}),
        json!({"tile_columns": "5"}),
        json!({"colour": "red"}),
        // One bad field refuses the whole request.
        json!({"theme": "dark", "tile_columns": 99}),
        json!([]),
    ] {
        let r = call(&app, "PUT", "/api/ui-prefs", Some(&bad)).await;
        assert_eq!(r.status, 400, "{bad}");
    }
    let r = call(&app, "GET", "/api/ui-prefs", None).await;
    assert_eq!(r.json(), json!({"theme": "light", "tile_columns": 4}));
}
