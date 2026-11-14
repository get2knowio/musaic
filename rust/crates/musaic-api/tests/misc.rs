//! Cases the Python app recordings do not cover directly.

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::Request;
use common::{Sandbox, call, send};
use musaic_api::{AppState, Network, router};
use serde_json::json;
use sha2::{Digest, Sha256};

fn tree_hashes(dir: &std::path::Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let h: String = Sha256::digest(std::fs::read(&p).unwrap())
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                out.push((p.to_string_lossy().into_owned(), h));
            }
        }
    }
    out.sort();
    out
}

async fn configured(sb: &Sandbox, st: AppState) -> axum::Router {
    let app = router(st);
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
    app
}

// The readiness probe answers without the database.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn health_does_not_touch_the_database() {
    let sb = Sandbox::new();
    let st = sb.state_no_lifespan();
    let app = router(st.clone());
    std::fs::write(st.db_path(), b"not a database").unwrap();
    let r = call(&app, "GET", "/api/health", None).await;
    assert_eq!((r.status, r.json()), (200, json!({"status": "ok"})));
    let r = call(&app, "GET", "/api/settings", None).await;
    assert_eq!(r.status, 500, "the settings route does read it");
}

// Unknown /api paths are JSON 404s even with a built SPA; other
// methods are 405; the bare /api is a client route (the Python app's rule).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_api_paths_never_fall_through_to_the_spa() {
    let sb = Sandbox::new();
    let app = router(sb.state_no_lifespan());
    for p in [
        "/api/nope",
        "/api/albums/x/y/z",
        "/api/",
        "/api/%2e%2e/index.html",
    ] {
        let r = call(&app, "GET", p, None).await;
        assert_eq!(r.status, 404, "{p}");
        assert_eq!(r.json(), json!({"detail": "Not Found"}), "{p}");
    }
    let r = call(&app, "DELETE", "/api/health", None).await;
    assert_eq!(
        (r.status, r.json()),
        (405, json!({"detail": "Method Not Allowed"}))
    );
    let r = call(&app, "GET", "/library/x", None).await;
    assert!(r.text().contains("<div id=\"root\">"));
}

// No file outside the frontend dir is served,
// whatever the encoding (more variants in tests/deliberate.rs).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spa_never_serves_outside_its_dir() {
    let sb = Sandbox::new();
    let app = router(sb.state_no_lifespan());
    for p in [
        "/..%2Fsecret.txt",
        "/%2e%2e/secret.txt",
        "/assets/..%2F..%2Fsecret.txt",
        "/%2e%2e%2f%2e%2e%2fapi-golden%2fsecret.txt",
    ] {
        let r = call(&app, "GET", p, None).await;
        assert!(!r.text().contains("outside the frontend dir"), "{p}");
    }
}

// No keyring -> keyring_available false, keys read as
// not set, a key write is refused (503) and nothing reaches SQLite.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_without_a_keyring() {
    let sb = Sandbox::new();
    let st = AppState::builder(&sb.cfg)
        .secrets(Arc::new(musaic_secrets::MemoryStore::unavailable()))
        .network(Network::Offline)
        .fingerprinting(common::no_key_fingerprinting())
        .auto_snapshot(false)
        .build()
        .unwrap();
    let app = router(st.clone());
    let r = call(&app, "GET", "/api/settings", None).await;
    assert_eq!(r.json()["keyring_available"], json!(false));
    let r = call(
        &app,
        "PATCH",
        "/api/settings",
        Some(&json!({"acoustid_api_key": "secret-value-1"})),
    )
    .await;
    assert_eq!(r.status, 503);
    assert!(!r.text().contains("secret-value-1"));
    let conn = rusqlite::Connection::open(st.db_path()).unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM settings WHERE value LIKE '%secret-value-1%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 0);
}

// A key set through PATCH is never echoed and never stored in
// SQLite.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_never_leak_a_secret() {
    let sb = Sandbox::new();
    let store = Arc::new(musaic_secrets::MemoryStore::new());
    let st = AppState::builder(&sb.cfg)
        .secrets(store.clone())
        .network(Network::Offline)
        .fingerprinting(common::no_key_fingerprinting())
        .auto_snapshot(false)
        .build()
        .unwrap();
    let app = router(st.clone());
    for key in ["ai_api_key", "acoustid_api_key", "firecrawl_api_key"] {
        let r = call(
            &app,
            "PATCH",
            "/api/settings",
            Some(&json!({key: " top-secret-42 "})),
        )
        .await;
        assert_eq!(r.status, 200);
        assert!(!r.text().contains("top-secret-42"), "{key}");
        assert_eq!(r.json()[format!("{key}_set")], json!(true));
        assert_eq!(store.entries()[key], "top-secret-42");
    }
    let r = call(&app, "GET", "/api/settings", None).await;
    assert!(!r.text().contains("top-secret-42"));
    let backup = call(&app, "GET", "/api/db/backup", None).await;
    assert!(
        !backup
            .body
            .windows(b"top-secret-42".len())
            .any(|w| w == b"top-secret-42")
    );
}

// A dry run over HTTP leaves files and album rows unchanged.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dry_run_apply_changes_nothing() {
    let sb = Sandbox::new();
    let st = sb.state_no_lifespan();
    let app = configured(&sb, st.clone()).await;
    let albums = call(&app, "GET", "/api/albums", None).await.json();
    let id = albums["albums"][0]["id"].as_str().unwrap().to_string();
    let before_files = tree_hashes(&sb.lib);
    let before_rows = call(&app, "GET", "/api/albums", None).await.text();
    let r = call(
        &app,
        "POST",
        &format!("/api/albums/{id}/apply/stream"),
        Some(&json!({"suggestions": {"genre": "Dry", "title": "Dry"}, "dry_run": true})),
    )
    .await;
    assert_eq!(r.status, 200);
    assert!(r.text().contains("event: done"), "{}", r.text());
    assert_eq!(tree_hashes(&sb.lib), before_files);
    assert_eq!(
        call(&app, "GET", "/api/albums", None).await.text(),
        before_rows
    );
}

// Cover upload validation (the Python app's order: type, empty, size, then
// the album).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cover_upload_validation() {
    let sb = Sandbox::new();
    let st = sb.state_no_lifespan();
    let app = configured(&sb, st).await;
    let up = |ct: &str, data: &[u8], album: &str| {
        let b = "bnd";
        let mut body = format!(
            "--{b}\r\nContent-Disposition: form-data; name=\"cover\"; filename=\"c\"\r\nContent-Type: {ct}\r\n\r\n"
        )
        .into_bytes();
        body.extend_from_slice(data);
        body.extend_from_slice(format!("\r\n--{b}--\r\n").as_bytes());
        Request::builder()
            .method("POST")
            .uri(format!("/api/albums/{album}/cover/upload"))
            .header("content-type", format!("multipart/form-data; boundary={b}"))
            .body(Body::from(body))
            .unwrap()
    };
    let r = send(&app, up("text/plain", b"x", "0000000000000000")).await;
    assert_eq!(r.status, 400);
    assert_eq!(
        r.json()["detail"],
        "Unsupported file type 'text/plain' — please upload an image"
    );
    let r = send(&app, up("image/png", b"", "0000000000000000")).await;
    assert_eq!(
        (r.status, r.json()["detail"].clone()),
        (400, json!("Empty file"))
    );
    let r = send(&app, up("image/png", b"png", "0000000000000000")).await;
    assert_eq!(r.status, 404);
    let r = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/albums/x/cover/upload")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, 422);
    assert_eq!(r.json()["detail"][0]["loc"], json!(["body", "cover"]));
}

// Apply and dry run on a Missing album are
// refused with musaic-services' 409; reads still answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_refuses_a_missing_album() {
    let sb = Sandbox::new();
    let st = sb.state_no_lifespan();
    let app = configured(&sb, st.clone()).await;
    let albums = call(&app, "GET", "/api/albums", None).await.json();
    let id = albums["albums"][0]["id"].as_str().unwrap().to_string();
    let conn = rusqlite::Connection::open(st.db_path()).unwrap();
    conn.execute(
        "UPDATE albums SET missing_since = '2026-01-01T00:00:00Z' WHERE id = ?1",
        [&id],
    )
    .unwrap();
    for dry in [true, false] {
        let r = call(
            &app,
            "POST",
            &format!("/api/albums/{id}/apply/stream"),
            Some(&json!({"suggestions": {"genre": "X"}, "dry_run": dry})),
        )
        .await;
        assert_eq!(r.status, 409, "dry_run={dry}: {}", r.text());
    }
    assert_eq!(
        call(&app, "GET", &format!("/api/albums/{id}/tracks"), None)
            .await
            .status,
        200
    );
}
