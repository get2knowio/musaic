//! First-run answers: the one-time AcoustID prompt's dismissal is kept by the
//! server, and a refused library folder leaves the configured one in place.

mod common;

use common::{Sandbox, call};
use musaic_api::router;
use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_prompt_is_not_dismissed_until_dismissed_and_stays_so() {
    let sb = Sandbox::new();
    let st = sb.state_no_lifespan();
    let app = router(st.clone());
    let r = call(&app, "GET", "/api/prompts/acoustid", None).await;
    assert_eq!((r.status, r.json()), (200, json!({"dismissed": false})));

    let r = call(&app, "POST", "/api/prompts/acoustid/dismiss", None).await;
    assert_eq!((r.status, r.json()), (200, json!({"dismissed": true})));
    // Twice is fine.
    let r = call(&app, "POST", "/api/prompts/acoustid/dismiss", None).await;
    assert_eq!(r.json(), json!({"dismissed": true}));

    // A new app object on the same config dir (a relaunch) still has it.
    let again = router(sb.state_no_lifespan());
    let r = call(&again, "GET", "/api/prompts/acoustid", None).await;
    assert_eq!(r.json(), json!({"dismissed": true}));

    // It is not part of the settings body.
    let r = call(&again, "GET", "/api/settings", None).await;
    assert!(r.json().get("acoustid_prompt_dismissed").is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_folder_leaves_the_configured_one_and_says_why() {
    let sb = Sandbox::new();
    let app = router(sb.state_no_lifespan());
    let r = call(
        &app,
        "POST",
        "/api/config",
        Some(&json!({"library_dir": Sandbox::s(&sb.lib)})),
    )
    .await;
    assert_eq!(r.status, 200);
    let stored = r.json()["library_dir"].clone();

    let missing = format!("{}/no-such-folder", Sandbox::s(&sb.lib));
    let r = call(
        &app,
        "POST",
        "/api/config",
        Some(&json!({ "library_dir": missing })),
    )
    .await;
    assert_eq!(r.status, 400);
    assert!(r.json()["detail"].as_str().unwrap().contains(&missing));

    let r = call(&app, "GET", "/api/config", None).await;
    assert_eq!(r.json()["library_dir"], stored);
    assert_eq!(r.json()["exists"], json!(true));
}
