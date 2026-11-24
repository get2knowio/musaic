//! Remote cover previews go through the backend:
//! `GET /api/ai/jobs/{job_id}/cover-preview` and
//! `GET /api/albums/{album_id}/cover-preview`, routes the Python app does
//! not have (`musaic_api::EXTRA_ROUTES`). The client names a job or an
//! album; the server fetches the job's stored `result.cover.cover_url`, or
//! the Cover Art Archive front of the album's anchored release group,
//! through the app's transport with `musaic_net::preview`'s rules.
//! Scripted transports and 127.0.0.1 only.

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
use musaic_api::routes::preview::{
    ALBUM_NOT_FOUND, NO_ALBUM_COVER, NO_COVER_URL, PREVIEW_CACHE_CONTROL, PREVIEW_CSP,
};
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
    // An old job, as schema v24 folded it.
    let job = musaic_db::research_history::JobRow {
        album_path: "/lib/A - B [FLAC]".into(),
        album_title: "B".into(),
        result_json: result.map(|r| r.to_string()),
        completed_at: Some("2026-11-12T00:00:01".into()),
        ..musaic_db::research_history::JobRow::seed(
            id,
            "a1",
            "cover",
            "completed",
            "2026-11-12T00:00:00",
        )
    };
    musaic_db::research_history::import_legacy_job(&conn, &job).unwrap();
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

/// The jobs list shows a linked old job under its card, the group of the
/// requests the migration made for it; the preview finds the old result
/// through that id too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_linked_old_job_previews_by_its_card_id() {
    let sb = Sandbox::new();
    let t = Arc::new(Scripted::default().with(CAA, 200, &[("Content-Type", "image/jpeg")], JPEG));
    let st = state(&sb, t.clone());
    insert_job(&sb, "00000000000000a2", Some(cover_result(Some(CAA))));
    let group: String = rusqlite::Connection::open(sb.cfg.join("library.db"))
        .unwrap()
        .query_row(
            "SELECT group_id FROM delegated_work WHERE legacy_job_id = '00000000000000a2'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_ne!(group, "00000000000000a2");
    let app = router(st);
    let got = call(&app, "GET", &path(&group), None).await;
    assert_eq!(got.status, 200, "{}", got.text());
    assert_eq!(got.body, JPEG);
    assert_eq!(t.sent(), [CAA]);
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

// ---- GET /api/albums/{album_id}/cover-preview ----

/// An anchored release group and the URL the resolver derives for it.
const RG: &str = "0d9e7b1c-2f4a-4b6e-9c3d-1a2b3c4d5e6f";
const RG_FRONT: &str =
    "https://coverartarchive.org/release-group/0d9e7b1c-2f4a-4b6e-9c3d-1a2b3c4d5e6f/front";

fn album_path(id: &str) -> String {
    format!("/api/albums/{id}/cover-preview")
}

/// An album row, anchored to `anchor` when given, Missing when `missing`
/// is given. Only the database is touched: the route never reads the
/// album's files.
fn insert_album(sb: &Sandbox, id: &str, anchor: Option<&str>, missing: Option<&str>) {
    let conn = rusqlite::Connection::open(sb.cfg.join("library.db")).unwrap();
    let path = sb.lib.join(format!("Artist {id} - Album [FLAC]"));
    conn.execute(
        "INSERT OR IGNORE INTO libraries (path, scanned_at) VALUES (?1, '2026-11-15T00:00:00')",
        [Sandbox::s(&sb.lib)],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO albums (id, library_path, path, mb_release_group_mbid, mb_match_score, \
         missing_since) VALUES (?1, ?2, ?3, ?4, 95, ?5)",
        rusqlite::params![id, Sandbox::s(&sb.lib), Sandbox::s(&path), anchor, missing],
    )
    .unwrap();
}

fn block(sb: &Sandbox, id: &str, rg: &str) {
    let conn = rusqlite::Connection::open(sb.cfg.join("library.db")).unwrap();
    musaic_db::mb_anchor::add_blocked_mb_release_group(&conn, id, rg, "2026-11-15T00:00:00Z")
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn album_cover_is_derived_from_the_anchor_and_served_same_origin() {
    let sb = Sandbox::new();
    let t = Arc::new(
        Scripted::default()
            .with(RG_FRONT, 307, &[("Location", IA)], b"")
            .with(IA, 200, &[("Content-Type", "image/jpeg")], JPEG),
    );
    let st = state(&sb, t.clone());
    insert_album(&sb, "00000000000000a1", Some(RG), None);
    let app = router(st);
    let got = call(&app, "GET", &album_path("00000000000000a1"), None).await;
    assert_eq!(got.status, 200, "{}", got.text());
    assert_eq!(got.body, JPEG);
    assert_eq!(got.ctype(), "image/jpeg");
    assert_eq!(got.headers["cache-control"], PREVIEW_CACHE_CONTROL);
    assert_eq!(got.headers["x-content-type-options"], "nosniff");
    assert_eq!(got.headers["content-security-policy"], PREVIEW_CSP);
    assert_eq!(
        got.route.as_deref(),
        Some("/api/albums/{album_id}/cover-preview")
    );
    assert_eq!(t.sent(), [RG_FRONT, IA]);

    // Nothing in the request steers the fetch: a URL in the query string is
    // not read, and only the anchored group's front is asked for.
    let steer = format!(
        "{}?url=https%3A%2F%2Fcovers.example.org%2Fx.jpg&cover_url=https%3A%2F%2F192.168.1.10%2F",
        album_path("00000000000000a1")
    );
    let got = call(&app, "GET", &steer, None).await;
    assert_eq!(got.status, 200, "{}", got.text());
    assert_eq!(t.sent(), [RG_FRONT, IA, RG_FRONT, IA]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn album_nothing_to_preview_and_unknown() {
    let sb = Sandbox::new();
    let other_rg = "11111111-2222-4333-8444-555555555555";
    let other_front = format!("https://coverartarchive.org/release-group/{other_rg}/front");
    let t = Arc::new(Scripted::default().with(&other_front, 404, &[], b"Not Found"));
    let st = state(&sb, t.clone());
    // No anchor.
    insert_album(&sb, "00000000000000b1", None, None);
    // Anchor blocked with "Wrong match": Tier 0a would not use it.
    insert_album(&sb, "00000000000000b2", Some(RG), None);
    block(&sb, "00000000000000b2", RG);
    // A stored id that is not an MBID is never put into a URL.
    insert_album(
        &sb,
        "00000000000000b3",
        Some("x/../../@192.168.1.10/admin"),
        None,
    );
    insert_album(&sb, "00000000000000b4", Some(""), None);
    let app = router(st);
    for id in [
        "00000000000000b1",
        "00000000000000b2",
        "00000000000000b3",
        "00000000000000b4",
    ] {
        let got = call(&app, "GET", &album_path(id), None).await;
        assert_eq!(got.status, 404, "{id}: {}", got.text());
        assert_eq!(got.json(), json!({"detail": NO_ALBUM_COVER}), "{id}");
    }
    assert!(t.sent().is_empty(), "no anchor, nothing fetched");

    // Anchored, but the archive has no front cover for the group: the same
    // answer (the resolver's "Cover not in CAA").
    insert_album(&sb, "00000000000000b5", Some(other_rg), None);
    let got = call(&app, "GET", &album_path("00000000000000b5"), None).await;
    assert_eq!(got.status, 404, "{}", got.text());
    assert_eq!(got.json(), json!({"detail": NO_ALBUM_COVER}));
    assert_eq!(t.sent(), std::slice::from_ref(&other_front));

    // Unknown album.
    let got = call(&app, "GET", &album_path("ffffffffffffffff"), None).await;
    assert_eq!(got.status, 404);
    assert_eq!(got.json(), json!({"detail": ALBUM_NOT_FOUND}));
    // Only GET.
    let got = call(&app, "POST", &album_path("00000000000000b5"), None).await;
    assert_eq!(got.status, 405);
    assert_eq!(t.sent().len(), 1);
}

/// A Missing album's cover cannot be changed, so it is not previewed: the
/// apply pipeline's 409, and nothing fetched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn album_missing_is_refused_before_any_fetch() {
    let sb = Sandbox::new();
    let t =
        Arc::new(Scripted::default().with(RG_FRONT, 200, &[("Content-Type", "image/jpeg")], JPEG));
    let st = state(&sb, t.clone());
    insert_album(
        &sb,
        "00000000000000c1",
        Some(RG),
        Some("2026-11-14T00:00:00Z"),
    );
    let got = call(&router(st), "GET", &album_path("00000000000000c1"), None).await;
    assert_eq!(got.status, 409, "{}", got.text());
    assert_eq!(
        got.json()["detail"],
        musaic_services::apply::missing_album_detail("2026-11-14T00:00:00Z")
    );
    assert!(t.sent().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn album_refusals() {
    let groups = [
        ("00000000000000d1", "d1d1d1d1-0000-4000-8000-000000000001"),
        ("00000000000000d2", "d1d1d1d1-0000-4000-8000-000000000002"),
        ("00000000000000d3", "d1d1d1d1-0000-4000-8000-000000000003"),
        ("00000000000000d4", "d1d1d1d1-0000-4000-8000-000000000004"),
        ("00000000000000d5", "d1d1d1d1-0000-4000-8000-000000000005"),
    ];
    let front = |rg: &str| format!("https://coverartarchive.org/release-group/{rg}/front");
    let mut big = JPEG.to_vec();
    big.resize(musaic_net::preview::PREVIEW_MAX_BYTES as usize + 1, 0);
    let t = Arc::new(
        Scripted::default()
            // Non-image content type.
            .with(
                &front(groups[0].1),
                200,
                &[("Content-Type", "text/html")],
                b"<html></html>",
            )
            // Oversized.
            .with(
                &front(groups[1].1),
                200,
                &[("Content-Type", "image/jpeg")],
                &big,
            )
            // Redirect to a private address.
            .with(
                &front(groups[2].1),
                302,
                &[("Location", "https://192.168.1.10/admin.jpg")],
                b"",
            )
            // Redirect to plain HTTP.
            .with(
                &front(groups[3].1),
                302,
                &[("Location", "http://archive.org/x.jpg")],
                b"",
            )
            // An image type whose bytes are not that image.
            .with(
                &front(groups[4].1),
                200,
                &[("Content-Type", "image/png")],
                b"<svg/>",
            ),
    );
    let sb = Sandbox::new();
    let st = state(&sb, t.clone());
    for (id, rg) in groups {
        insert_album(&sb, id, Some(rg), None);
    }
    let app = router(st);
    let check = |got: &Got, detail_starts: &str, what: &str| {
        assert_eq!(got.status, 502, "{what}: {}", got.text());
        let d = got.json()["detail"].as_str().unwrap().to_string();
        assert!(d.starts_with(detail_starts), "{what}: {d}");
    };
    let got = call(&app, "GET", &album_path(groups[0].0), None).await;
    check(&got, "Cover preview unavailable: not an image", "html");
    let got = call(&app, "GET", &album_path(groups[1].0), None).await;
    check(&got, "Cover preview unavailable: image larger than", "huge");
    let got = call(&app, "GET", &album_path(groups[2].0), None).await;
    check(&got, "Cover preview unavailable: refused", "private");
    let got = call(&app, "GET", &album_path(groups[3].0), None).await;
    check(&got, "Cover preview unavailable: refused", "plain http");
    let got = call(&app, "GET", &album_path(groups[4].0), None).await;
    check(
        &got,
        "Cover preview unavailable: the body is not",
        "signature",
    );
    // The refused second hops were never requested.
    assert!(
        !t.sent()
            .iter()
            .any(|u| u.contains("192.168.") || u.starts_with("http:"))
    );
    assert_eq!(t.sent().len(), groups.len());
}

/// Offline mode: the transport refuses every request, so the preview is a
/// 502, as the job route's is.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn album_offline_is_unavailable() {
    let sb = Sandbox::new();
    let st = sb.state_no_lifespan();
    insert_album(&sb, "00000000000000e1", Some(RG), None);
    let got = call(&router(st), "GET", &album_path("00000000000000e1"), None).await;
    assert_eq!(got.status, 502, "{}", got.text());
    let d = got.json()["detail"].as_str().unwrap().to_string();
    assert!(d.starts_with("Cover preview unavailable: "), "{d}");
}

/// A read route like the job preview: a restore in progress does not
/// refuse it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn album_read_route_is_not_refused_during_a_restore() {
    let sb = Sandbox::new();
    let t =
        Arc::new(Scripted::default().with(RG_FRONT, 200, &[("Content-Type", "image/jpeg")], JPEG));
    let st = state(&sb, t);
    insert_album(&sb, "00000000000000f1", Some(RG), None);
    let app = router(st.clone());
    let restore = st.gate().begin_restore().unwrap();
    let got = call(&app, "GET", &album_path("00000000000000f1"), None).await;
    assert_eq!(got.status, 200, "{}", got.text());
    drop(restore);
}

/// The Host / Origin check and the desktop launch token guard it like every
/// route; a refused request fetches nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn album_host_check_and_launch_token_apply() {
    const PORT: u16 = 41997;
    const TOKEN: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";
    let sb = Sandbox::new();
    let t =
        Arc::new(Scripted::default().with(RG_FRONT, 200, &[("Content-Type", "image/jpeg")], JPEG));
    let st = state(&sb, t.clone());
    insert_album(&sb, "0000000000000a01", Some(RG), None);
    let app = router_with(
        st,
        RouterOptions {
            launch: Some(Arc::new(LaunchGate::new(PORT, TOKEN.into()))),
        },
    );
    let req = |host: &str, origin: Option<&str>, cookie: bool| {
        let mut b = HttpRequest::get(album_path("0000000000000a01")).header("host", host);
        if let Some(o) = origin {
            b = b.header("origin", o);
        }
        if cookie {
            b = b.header("cookie", format!("{COOKIE}={TOKEN}"));
        }
        b.body(Body::empty()).unwrap()
    };
    let self_host = format!("127.0.0.1:{PORT}");
    let got = read(
        app.clone()
            .oneshot(req(&self_host, None, false))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(got.status, 403);
    assert_eq!(got.json()["detail"], NO_TOKEN);
    let got = read(
        app.clone()
            .oneshot(req("evil.example", None, true))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(got.status, 403);
    let got = read(
        app.clone()
            .oneshot(req(&self_host, Some("https://evil.example"), true))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(got.status, 403);
    assert!(t.sent().is_empty(), "refused requests fetch nothing");
    let got = read(
        app.clone()
            .oneshot(req(&self_host, None, true))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(got.status, 200, "{}", got.text());
    assert_eq!(t.sent(), [RG_FRONT]);
}
