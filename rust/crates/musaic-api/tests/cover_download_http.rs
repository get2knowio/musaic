//! `POST /api/albums/{id}/apply/stream` with a
//! `cover_url` downloads under the cover preview's rule (https only,
//! public addresses only, checked on the URL and on the address actually
//! connected to). A refused URL ends the stream exactly as the Python app's
//! failed download does (musaic/services/apply.py:88-92): `cover_downloading
//! {url}` then `error {detail: "Failed to download cover from {url}"}`.
//!
//! The app is built with musaic-net's live transport pinned to a local
//! server that would answer any request with a JPEG; the Python app fetched the
//! loopback URL (and the Rust build before this change did too, through
//! the live transport's plain-HTTP loopback exemption). Nothing here leaves
//! 127.0.0.1.

mod common;

use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::{Sandbox, call};
use musaic_api::{AppState, Network, router};
use musaic_net::LiveTransport;
use serde_json::{Value, json};

const JPEG: &[u8] = &[
    0xff, 0xd8, 0xff, 0xe0, 0, 0x10, b'J', b'F', b'I', b'F', 0, 1,
];

/// Answers every connection with a 200 `image/jpeg`; counts connections.
fn jpeg_server() -> (SocketAddr, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let n = Arc::new(AtomicUsize::new(0));
    let n2 = n.clone();
    std::thread::spawn(move || {
        for mut c in l.incoming().flatten() {
            n2.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0u8; 4096];
            let _ = c.read(&mut buf);
            let mut out = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                JPEG.len()
            )
            .into_bytes();
            out.extend_from_slice(JPEG);
            let _ = c.write_all(&out);
        }
    });
    (addr, n)
}

/// (event, data) of every SSE frame.
fn frames(text: &str) -> Vec<(String, Value)> {
    text.split("\n\n")
        .filter_map(|f| {
            let ev = f.lines().find_map(|l| l.strip_prefix("event: "))?;
            let data = f.lines().find_map(|l| l.strip_prefix("data: "))?;
            Some((ev.to_owned(), serde_json::from_str(data).unwrap()))
        })
        .collect()
}

// A loopback cover URL (a request-forgery path in the Python app) and a public-looking name that
// resolves to a LAN address are refused; the stream reports the Python app's
// failed-download frames and the local server is never connected to.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_refuses_private_cover_urls_like_a_failed_download() {
    let (addr, conns) = jpeg_server();
    let lan: IpAddr = "192.168.1.20".parse().unwrap();
    let live = LiveTransport::with_declared_dns(addr, &[], &[("nas.example.org", lan)]);
    let sb = Sandbox::new();
    let st = AppState::builder(&sb.cfg)
        .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
        .network(Network::Live(Arc::new(live)))
        .fingerprinting(common::no_key_fingerprinting())
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
    assert_eq!(r.status, 200);
    let scan = call(&app, "GET", "/api/scan/stream", None).await;
    assert!(scan.text().contains("event: done"));
    let albums = call(&app, "GET", "/api/albums", None).await.json();
    let id = albums["albums"][0]["id"].as_str().unwrap().to_owned();
    for url in [
        format!("http://127.0.0.1:{}/api/health", addr.port()),
        format!("http://localhost:{}/cover.jpg", addr.port()),
        "https://nas.example.org/cover.jpg".to_owned(),
    ] {
        let got = call(
            &app,
            "POST",
            &format!("/api/albums/{id}/apply/stream"),
            Some(&json!({"suggestions": {}, "track_suggestions": {},
                         "cover_url": url, "dry_run": true})),
        )
        .await;
        assert_eq!(got.status, 200, "{url}");
        let f = frames(&got.text());
        assert_eq!(
            f,
            vec![
                ("cover_downloading".to_owned(), json!({"url": url})),
                (
                    "error".to_owned(),
                    json!({"detail": format!("Failed to download cover from {url}")})
                ),
            ],
            "{url}: {}",
            got.text()
        );
    }
    assert_eq!(conns.load(Ordering::SeqCst), 0, "nothing was connected to");
}
