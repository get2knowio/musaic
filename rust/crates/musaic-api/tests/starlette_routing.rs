//! Starlette's routing answers over the whole route inventory. Golden:
//! testdata/golden/api/route_order.json (recorded from the Python app): the Python app's
//! `app.routes` in order, and its status / `Allow` / body for every method
//! each route path does not serve, HEAD on every GET path, and
//! encoded-slash paths. Earlier builds answered `DELETE /api/health` ->
//! `Allow: GET,HEAD` (the Python app: `GET`) and
//! `POST /api/db/snapshots/..%2Fescape.db/restore` -> 400 (the Python app: 405).

mod common;

use axum::body::Body;
use axum::http::Request;
use common::{Sandbox, send};
use musaic_api::starlette_routing::PYTHON_ROUTES;
use serde_json::Value;

fn golden() -> Value {
    let p = musaic_testdata::public("golden/api/route_order.json");
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

/// The table the dispatcher uses is the Python app's route order.
#[test]
fn route_table_is_the_python_registration_order() {
    let g = golden();
    let python: Vec<(String, String)> = g["routes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["type"] == "APIRoute")
        .map(|r| {
            let m: Vec<&str> = r["methods"]
                .as_array()
                .unwrap()
                .iter()
                .map(|m| m.as_str().unwrap())
                .collect();
            (r["path"].as_str().unwrap().to_string(), m.join(", "))
        })
        .collect();
    let ours: Vec<(String, String)> = PYTHON_ROUTES
        .iter()
        .map(|(p, a)| ((*p).to_string(), (*a).to_string()))
        .collect();
    assert_eq!(ours, python);
}

/// Every recorded probe answers as
/// the Python app did — status, `Allow`, body.
#[tokio::test]
async fn status_405_allow_head_and_encoded_slash_match_python() {
    let g = golden();
    assert_eq!(g["_meta"]["frontend_dist"], true);
    let sb = Sandbox::new();
    let app = musaic_api::router(sb.state_no_lifespan());
    let mut bad = Vec::new();
    for p in g["probes"].as_array().unwrap() {
        let (m, path) = (p["method"].as_str().unwrap(), p["path"].as_str().unwrap());
        let req = common::with_host(
            Request::builder()
                .method(m)
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        );
        let got = send(&app, req).await;
        let allow = got.headers.get("allow").cloned();
        let want_allow = p["allow"].as_str().map(str::to_string);
        // Byte for byte: the Python app's JSONResponse bytes (the 422's
        // key order included).
        let body_ok = m == "HEAD" || got.text() == p["body"].as_str().unwrap();
        if u64::from(got.status) != p["status"].as_u64().unwrap() || allow != want_allow || !body_ok
        {
            bad.push(format!(
                "{m} {path}: ours {} {:?} {:?}; python {} {:?} {:?}",
                got.status,
                allow,
                got.text(),
                p["status"],
                want_allow,
                p["body"]
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{} mismatches:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

/// The SPA's refusal of path traversal (a deliberate change) still holds once routing decodes
/// the path: an encoded traversal never serves a file outside the
/// frontend dir.
#[tokio::test]
async fn encoded_traversal_still_refused_after_decoding() {
    let sb = Sandbox::new();
    let app = musaic_api::router(sb.state_no_lifespan());
    for path in [
        "/..%2Fsecret.txt",
        "/assets/..%2F..%2Fsecret.txt",
        "/%2E%2E/secret.txt",
    ] {
        let got = send(
            &app,
            common::with_host(Request::get(path).body(Body::empty()).unwrap()),
        )
        .await;
        assert!(
            !got.text().contains("outside the frontend dir"),
            "{path}: {}",
            got.text()
        );
    }
}
