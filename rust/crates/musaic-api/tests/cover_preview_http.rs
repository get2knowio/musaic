//! Remote cover previews go through the backend:
//! `GET /api/ai/jobs/{job_id}/cover-preview`, a route the Python app does
//! not have (`musaic_api::EXTRA_ROUTES`). The client names a job; the server fetches the
//! job's stored `result.cover.cover_url` through the app's transport with
//! `musaic_net::preview`'s rules. Scripted transports and 127.0.0.1 only.

mod common;

use std::collections::HashMap;
use std::io::Write;
use std::net::{IpAddr, TcpListener};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::Request as HttpRequest;
use common::{Got, Sandbox, call, read};
use musaic_api::launch::{COOKIE, NO_TOKEN};
use musaic_api::routes::preview::{NO_COVER_URL, PREVIEW_CACHE_CONTROL, PREVIEW_CSP};
use musaic_api::{AppState, LaunchGate, Network, RouterOptions, router, router_with};
use musaic_net::{LiveTransport, Request, Response, Transport, TransportError};
use serde_json::json;
use tower::ServiceExt;

const JPEG: &[u8] = &[
    0xff, 0xd8, 0xff, 0xe0, 0, 0x10, b'J', b'F', b'I', b'F', 0, 1,
];
const CAA: &str = "https://coverartarchive.org/release-group/0d9e7b1c/front";
const IA: &str = "https://archive.org/download/mbid-x/mbid-x-1.jpg";

/// Answers by URL; records every URL it was asked for.
#[derive(Default)]
struct Scripted {
    answers: HashMap<String, Response>,
    sent: Mutex<Vec<String>>,
}

impl Scripted {
    fn with(mut self, url: &str, status: u16, headers: &[(&str, &str)], body: &[u8]) -> Self {
        self.answers.insert(
            url.into(),
            Response {
                status,
                headers: headers
                    .iter()
                    .map(|(k, v)| ((*k).into(), (*v).into()))
                    .collect(),
                body: body.to_vec(),
            },
        );
        self
    }
    fn sent(&self) -> Vec<String> {
        self.sent.lock().unwrap().clone()
    }
}

impl Transport for Scripted {
    fn send(&self, r: &Request) -> Result<Response, TransportError> {
        self.sent.lock().unwrap().push(r.url());
        if let Some(limit) = r.max_body
            && let Some(a) = self.answers.get(&r.url())
            && a.body.len() as u64 > limit
        {
            return Err(TransportError::BodyTooLarge {
                url: r.url(),
                limit,
            });
        }
        self.answers
            .get(&r.url())
            .cloned()
            .ok_or_else(|| TransportError::Unobserved {
                key: String::new(),
                method: "GET",
                url: r.url(),
            })
    }
}

fn state(sb: &Sandbox, t: Arc<dyn Transport + Send + Sync>) -> AppState {
    AppState::builder(&sb.cfg)
        .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
        .network(Network::Live(t))
        .fingerprinting(common::no_key_fingerprinting())
        .subscribers(false)
        .auto_snapshot(false)
        .build()
        .unwrap()
}

/// A job row with `result_json` as given (the shape the worker stores).
fn insert_job(sb: &Sandbox, id: &str, result: Option<serde_json::Value>) {
    let conn = rusqlite::Connection::open(sb.cfg.join("library.db")).unwrap();
    conn.execute(
        "INSERT INTO ai_jobs (id, album_id, album_path, album_title, job_type, status, \
         result_json, created_at, completed_at) \
         VALUES (?1, 'a1', '/lib/A - B [FLAC]', 'B', 'cover', 'completed', ?2, \
         '2026-11-12T00:00:00', '2026-11-12T00:00:01')",
        rusqlite::params![id, result.map(|r| r.to_string())],
    )
    .unwrap();
}

fn cover_result(url: Option<&str>) -> serde_json::Value {
    json!({
        "metadata": null,
        "cover": {"cover_url": url, "source": "MusicBrainz", "notes": "", "confidence": "high"},
    })
}

fn path(id: &str) -> String {
    format!("/api/ai/jobs/{id}/cover-preview")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn found_image_is_served_same_origin() {
    let sb = Sandbox::new();
    let t = Arc::new(
        Scripted::default()
            .with(CAA, 307, &[("Location", IA)], b"")
            .with(IA, 200, &[("Content-Type", "image/jpeg")], JPEG),
    );
    let st = state(&sb, t.clone());
    insert_job(&sb, "00000000000000a1", Some(cover_result(Some(CAA))));
    let app = router(st);
    let got = call(&app, "GET", &path("00000000000000a1"), None).await;
    assert_eq!(got.status, 200, "{}", got.text());
    assert_eq!(got.body, JPEG);
    assert_eq!(got.ctype(), "image/jpeg");
    assert_eq!(got.headers["cache-control"], PREVIEW_CACHE_CONTROL);
    assert_eq!(got.headers["x-content-type-options"], "nosniff");
    assert_eq!(got.headers["content-security-policy"], PREVIEW_CSP);
    assert_eq!(
        got.route.as_deref(),
        Some("/api/ai/jobs/{job_id}/cover-preview")
    );
    assert_eq!(t.sent(), [CAA, IA]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refusals() {
    let sb = Sandbox::new();
    let mut big = JPEG.to_vec();
    big.resize(musaic_net::preview::PREVIEW_MAX_BYTES as usize + 1, 0);
    let html = "https://covers.example.org/page";
    let huge = "https://covers.example.org/huge.jpg";
    let to_private = "https://covers.example.org/redirect";
    let t = Arc::new(
        Scripted::default()
            .with(
                html,
                200,
                &[("Content-Type", "text/html")],
                b"<html></html>",
            )
            .with(huge, 200, &[("Content-Type", "image/jpeg")], &big)
            .with(
                to_private,
                302,
                &[("Location", "https://192.168.1.10/admin.jpg")],
                b"",
            ),
    );
    let st = state(&sb, t.clone());
    insert_job(&sb, "00000000000000b1", Some(cover_result(Some(html))));
    insert_job(&sb, "00000000000000b2", Some(cover_result(Some(huge))));
    insert_job(
        &sb,
        "00000000000000b3",
        Some(cover_result(Some(to_private))),
    );
    insert_job(&sb, "00000000000000b4", Some(cover_result(None)));
    insert_job(&sb, "00000000000000b5", None);
    insert_job(
        &sb,
        "00000000000000b6",
        Some(cover_result(Some("http://127.0.0.1:8765/api/health"))),
    );
    let app = router(st);
    let check = |got: &Got, status: u16, detail_starts: &str, what: &str| {
        assert_eq!(got.status, status, "{what}: {}", got.text());
        let d = got.json()["detail"].as_str().unwrap().to_string();
        assert!(d.starts_with(detail_starts), "{what}: {d}");
    };
    // Non-image content type.
    let got = call(&app, "GET", &path("00000000000000b1"), None).await;
    check(&got, 502, "Cover preview unavailable: not an image", "html");
    // Oversized.
    let got = call(&app, "GET", &path("00000000000000b2"), None).await;
    check(
        &got,
        502,
        "Cover preview unavailable: image larger than",
        "huge",
    );
    // Redirect to a private address: the second hop is never requested.
    let got = call(&app, "GET", &path("00000000000000b3"), None).await;
    check(&got, 502, "Cover preview unavailable: refused", "private");
    assert!(!t.sent().iter().any(|u| u.contains("192.168.")));
    // A stored loopback / plain-HTTP URL: refused before any request.
    let before = t.sent().len();
    let got = call(&app, "GET", &path("00000000000000b6"), None).await;
    check(&got, 502, "Cover preview unavailable: refused", "loopback");
    assert_eq!(t.sent().len(), before);
    // No cover URL stored (cover searched, nothing found; no result yet).
    for id in ["00000000000000b4", "00000000000000b5"] {
        let got = call(&app, "GET", &path(id), None).await;
        assert_eq!(got.status, 404, "{id}");
        assert_eq!(got.json(), json!({"detail": NO_COVER_URL}), "{id}");
    }
    // Unknown id.
    let got = call(&app, "GET", &path("ffffffffffffffff"), None).await;
    assert_eq!(got.status, 404);
    assert_eq!(got.json(), json!({"detail": "Job not found"}));
    // Only GET.
    let got = call(&app, "POST", &path("00000000000000b1"), None).await;
    assert_eq!(got.status, 405);
    assert_eq!(t.sent().len(), before);
}

/// Through the real live transport: a stored URL whose host *resolves* to
/// a private address is refused at resolution, nothing is connected to.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn name_resolving_to_a_private_address_is_refused_by_the_live_transport() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let conns = Arc::new(AtomicUsize::new(0));
    let c2 = conns.clone();
    std::thread::spawn(move || {
        for mut c in l.incoming().flatten() {
            c2.fetch_add(1, Ordering::SeqCst);
            let _ = c.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n");
        }
    });
    let private: IpAddr = "10.20.30.40".parse().unwrap();
    let live = LiveTransport::with_declared_dns(addr, &[], &[("nas.example.org", private)]);
    let sb = Sandbox::new();
    let st = state(&sb, Arc::new(live));
    insert_job(
        &sb,
        "00000000000000c1",
        Some(cover_result(Some("https://nas.example.org/cover.jpg"))),
    );
    let got = call(&router(st), "GET", &path("00000000000000c1"), None).await;
    assert_eq!(got.status, 502, "{}", got.text());
    let d = got.json()["detail"].as_str().unwrap().to_string();
    assert!(d.contains("not a public address"), "{d}");
    assert_eq!(conns.load(Ordering::SeqCst), 0);
}

/// A read route; a restore in progress does not refuse
/// it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_route_is_not_refused_during_a_restore() {
    let sb = Sandbox::new();
    let t = Arc::new(Scripted::default().with(IA, 200, &[("Content-Type", "image/jpeg")], JPEG));
    let st = state(&sb, t);
    insert_job(&sb, "00000000000000d1", Some(cover_result(Some(IA))));
    let app = router(st.clone());
    let restore = st.gate().begin_restore().unwrap();
    let got = call(&app, "GET", &path("00000000000000d1"), None).await;
    assert_eq!(got.status, 200, "{}", got.text());
    drop(restore);
}

/// The Host / Origin check and the desktop launch token
/// guard it like every route; a refused request fetches nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_check_and_launch_token_apply() {
    const PORT: u16 = 41998;
    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let sb = Sandbox::new();
    let t = Arc::new(Scripted::default().with(IA, 200, &[("Content-Type", "image/jpeg")], JPEG));
    let st = state(&sb, t.clone());
    insert_job(&sb, "00000000000000e1", Some(cover_result(Some(IA))));
    let app = router_with(
        st,
        RouterOptions {
            launch: Some(Arc::new(LaunchGate::new(PORT, TOKEN.into()))),
        },
    );
    let req = |host: &str, cookie: bool| {
        let mut b = HttpRequest::get(path("00000000000000e1")).header("host", host);
        if cookie {
            b = b.header("cookie", format!("{COOKIE}={TOKEN}"));
        }
        b.body(Body::empty()).unwrap()
    };
    let self_host = format!("127.0.0.1:{PORT}");
    let got = read(app.clone().oneshot(req(&self_host, false)).await.unwrap()).await;
    assert_eq!(got.status, 403);
    assert_eq!(got.json()["detail"], NO_TOKEN);
    let got = read(
        app.clone()
            .oneshot(req("evil.example", true))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(got.status, 403);
    assert!(t.sent().is_empty(), "refused requests fetch nothing");
    let got = read(app.clone().oneshot(req(&self_host, true)).await.unwrap()).await;
    assert_eq!(got.status, 200, "{}", got.text());
    assert_eq!(t.sent(), [IA]);
}
