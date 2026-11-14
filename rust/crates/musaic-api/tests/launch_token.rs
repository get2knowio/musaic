//! The per-launch token (in musaic-api, off for the server binary, on for
//! the desktop app) together with the Host / Origin check:
//! every combination, through `router_with` and tower `oneshot`.
//!
//! Order pinned: Host / Origin first (a foreign Host gets the Host refusal
//! whatever cookie it carries), then the token, then the Python app routing
//! (HEAD 405, unknown `/api` 404) unchanged behind it. Without a gate
//! (`router`, the server binary) nothing about the Python app-pinned answers
//! changes: no cookie is needed.

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::Request;
use common::{Got, read};
use musaic_api::host_check::{HOST_REFUSED, ORIGIN_REFUSED};
use musaic_api::launch::{COOKIE, LAUNCH_PATH, NO_TOKEN};
use musaic_api::{AppState, LaunchGate, Network, RouterOptions, router, router_with};
use tower::ServiceExt;

const PORT: u16 = 41999;
const TOKEN: &str = "aa11bb22cc33dd44ee55ff6600112233aa11bb22cc33dd44ee55ff6600112233";

fn state(dir: &std::path::Path) -> AppState {
    AppState::builder(dir.to_path_buf())
        .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
        .network(Network::Offline)
        .subscribers(false)
        .auto_snapshot(false)
        .build()
        .unwrap()
}

fn gated(dir: &std::path::Path) -> axum::Router {
    router_with(
        state(dir),
        RouterOptions {
            launch: Some(Arc::new(LaunchGate::new(PORT, TOKEN.into()))),
        },
    )
}

#[derive(Clone, Copy)]
struct R<'a> {
    method: &'a str,
    path: &'a str,
    host: Option<&'a str>,
    origin: Option<&'a str>,
    cookie: bool,
}

const SELF_HOST: &str = "127.0.0.1:41999";

impl<'a> R<'a> {
    fn get(path: &'a str) -> Self {
        R {
            method: "GET",
            path,
            host: Some(SELF_HOST),
            origin: None,
            cookie: false,
        }
    }
    fn build(self) -> Request<Body> {
        let mut b = Request::builder().method(self.method).uri(self.path);
        if let Some(h) = self.host {
            b = b.header("host", h);
        }
        if let Some(o) = self.origin {
            b = b.header("origin", o);
        }
        if self.cookie {
            b = b.header("cookie", format!("other=1; {COOKIE}={TOKEN}"));
        }
        b.body(Body::empty()).unwrap()
    }
}

async fn send(app: &axum::Router, r: R<'_>) -> Got {
    read(app.clone().oneshot(r.build()).await.unwrap()).await
}

fn detail(g: &Got) -> String {
    g.json()["detail"].as_str().unwrap_or("").to_string()
}

// The full matrix of Host × Origin × cookie.
#[tokio::test]
async fn host_origin_then_token_matrix() {
    let dir = tempfile::tempdir().unwrap();
    let app = gated(dir.path());
    // (host, origin, cookie) -> (status, detail)
    type Case<'a> = (Option<&'a str>, Option<&'a str>, bool, u16, &'a str);
    let cases: &[Case] = &[
        (Some(SELF_HOST), None, true, 200, ""),
        (Some("localhost:41999"), None, true, 200, ""),
        (
            Some(SELF_HOST),
            Some("http://127.0.0.1:41999"),
            true,
            200,
            "",
        ),
        // Vite dev origin: allowed (any loopback port).
        (
            Some(SELF_HOST),
            Some("http://localhost:5173"),
            true,
            200,
            "",
        ),
        (Some(SELF_HOST), None, false, 403, NO_TOKEN),
        (
            Some(SELF_HOST),
            Some("http://127.0.0.1:41999"),
            false,
            403,
            NO_TOKEN,
        ),
        // Host first: a foreign Host is refused as such, cookie or not.
        (Some("evil.example"), None, true, 403, HOST_REFUSED),
        (Some("evil.example"), None, false, 403, HOST_REFUSED),
        (
            Some(SELF_HOST),
            Some("https://evil.example"),
            true,
            403,
            ORIGIN_REFUSED,
        ),
        (
            Some(SELF_HOST),
            Some("https://evil.example"),
            false,
            403,
            ORIGIN_REFUSED,
        ),
        (Some(SELF_HOST), Some("null"), true, 403, ORIGIN_REFUSED),
    ];
    for &(host, origin, cookie, status, want) in cases {
        let g = send(
            &app,
            R {
                host,
                origin,
                cookie,
                ..R::get("/api/health")
            },
        )
        .await;
        assert_eq!(g.status, status, "{host:?} {origin:?} cookie={cookie}");
        if status == 403 {
            assert_eq!(detail(&g), want, "{host:?} {origin:?} cookie={cookie}");
            assert_eq!(g.ctype(), "application/json");
        }
    }
}

// Every kind of route is behind the token (API, SPA, streams, the
// unmatched answers), and with the cookie the Python app routing is unchanged.
#[tokio::test]
async fn token_covers_every_route_and_keeps_starlette_routing() {
    let dir = tempfile::tempdir().unwrap();
    let app = gated(dir.path());
    for p in [
        "/api/health",
        "/",
        "/library",
        "/api/library-events/stream",
        "/api/nope",
    ] {
        let g = send(&app, R::get(p)).await;
        assert_eq!((g.status, detail(&g)), (403, NO_TOKEN.to_string()), "{p}");
    }
    let g = send(
        &app,
        R {
            method: "HEAD",
            ..R::get("/api/health")
        },
    )
    .await;
    assert_eq!(g.status, 403, "HEAD without the cookie");
    let with = |r: R<'static>| R { cookie: true, ..r };
    assert_eq!(send(&app, with(R::get("/api/nope"))).await.status, 404);
    assert_eq!(
        send(
            &app,
            with(R {
                method: "HEAD",
                ..R::get("/api/health")
            })
        )
        .await
        .status,
        405,
        "the HEAD answer is kept behind the token"
    );
    let g = send(&app, with(R::get("/api/config"))).await;
    assert_eq!(g.status, 200);
    assert!(g.json().get("library_dir").is_some());
}

// The launch URL: exact Host, right token, GET, once.
#[tokio::test]
async fn launch_url_is_one_shot_and_host_bound() {
    let dir = tempfile::tempdir().unwrap();
    let app = gated(dir.path());
    let good = format!("{LAUNCH_PATH}?t={TOKEN}");
    let leaked: &'static str = Box::leak(good.into_boxed_str());
    // Wrong token; wrong (but loopback) Host; foreign Host; POST.
    let g = send(&app, R::get("/__musaic/launch?t=nope")).await;
    assert_eq!((g.status, detail(&g).as_str()), (403, "Forbidden"));
    let g = send(
        &app,
        R {
            host: Some("localhost:41999"),
            ..R::get(leaked)
        },
    )
    .await;
    assert_eq!((g.status, detail(&g).as_str()), (403, "Forbidden"));
    let g = send(
        &app,
        R {
            host: Some("evil.example"),
            ..R::get(leaked)
        },
    )
    .await;
    assert_eq!((g.status, detail(&g)), (403, HOST_REFUSED.to_string()));
    let g = send(
        &app,
        R {
            method: "POST",
            ..R::get(leaked)
        },
    )
    .await;
    assert_eq!(g.status, 403);
    // The real launch.
    let g = send(&app, R::get(leaked)).await;
    assert_eq!(g.status, 200);
    let set = &g.headers["set-cookie"];
    assert!(set.starts_with(&format!("{COOKIE}={TOKEN}")), "{set}");
    assert!(set.contains("HttpOnly") && set.contains("SameSite=Strict") && set.contains("Path=/"));
    assert_eq!(g.headers["cache-control"], "no-store");
    assert!(
        g.text()
            .contains("http-equiv=\"refresh\" content=\"0;url=/\"")
    );
    assert!(!g.text().contains("<script"));
    // Once.
    assert_eq!(send(&app, R::get(leaked)).await.status, 410);
}

// The plain router (server binary) needs no cookie and knows no
// launch path (it is an ordinary unmatched GET there).
#[tokio::test]
async fn off_by_default_for_the_server_router() {
    let dir = tempfile::tempdir().unwrap();
    let app = router(state(dir.path()));
    assert_eq!(send(&app, R::get("/api/health")).await.status, 200);
    let g = send(&app, R::get("/__musaic/launch?t=x")).await;
    assert_ne!(g.status, 200);
    assert!(!g.headers.contains_key("set-cookie"));
    // The Host check is still there.
    let g = send(
        &app,
        R {
            host: Some("evil.example"),
            ..R::get("/api/health")
        },
    )
    .await;
    assert_eq!((g.status, detail(&g)), (403, HOST_REFUSED.to_string()));
}
