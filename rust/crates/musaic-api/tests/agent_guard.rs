//! The agent route family's guard (contracts/agent-http.md) and the proof that
//! the existing launch gate and Host / Origin check are unchanged (FR-027).
//! Requests go through `router_with` and tower `oneshot`, with the connection
//! info a real server supplies put on the request by hand.

mod common;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::connect_info::ConnectInfo;
use axum::http::Request;
use common::{Got, read};
use musaic_api::agent_guard::ConnInfo;
use musaic_api::launch::{COOKIE, NO_TOKEN};
use musaic_api::{AppState, LaunchGate, Network, RouterOptions, router_with};
use serde_json::{Value, json};
use tower::ServiceExt;

const PORT: u16 = 41999;
const TOKEN: &str = "aa11bb22cc33dd44ee55ff6600112233aa11bb22cc33dd44ee55ff6600112233";

struct World {
    _tmp: tempfile::TempDir,
    app: Router,
    state: AppState,
    log: Arc<Mutex<Vec<String>>>,
    agent_store: Arc<musaic_secrets::MemoryStore>,
}

fn world() -> World {
    let tmp = tempfile::tempdir().unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let l2 = log.clone();
    let agent_store = Arc::new(musaic_secrets::MemoryStore::new());
    let state = AppState::builder(tmp.path().join("cfg"))
        .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
        .agent_secrets(agent_store.clone())
        .network(Network::Offline)
        .subscribers(false)
        .auto_snapshot(false)
        .log(Arc::new(move |line: &str| {
            l2.lock().unwrap().push(line.to_string())
        }))
        .build()
        .unwrap();
    let app = router_with(
        state.clone(),
        RouterOptions {
            launch: Some(Arc::new(LaunchGate::new(PORT, TOKEN.into()))),
        },
    );
    World {
        _tmp: tmp,
        app,
        state,
        log,
        agent_store,
    }
}

fn conn(peer: &str) -> ConnectInfo<ConnInfo> {
    ConnectInfo(ConnInfo {
        peer: peer.parse::<SocketAddr>().unwrap(),
        local: SocketAddr::from(([127, 0, 0, 1], PORT)),
    })
}

struct Req {
    method: &'static str,
    path: String,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
    peer: &'static str,
}

impl Req {
    fn post(path: &str) -> Self {
        Req {
            method: "POST",
            path: path.into(),
            headers: vec![
                ("host", format!("127.0.0.1:{PORT}")),
                ("content-type", "application/json".into()),
            ],
            body: b"{}".to_vec(),
            peer: "127.0.0.1:50000",
        }
    }
    fn get(path: &str) -> Self {
        Req {
            method: "GET",
            body: Vec::new(),
            headers: vec![("host", format!("127.0.0.1:{PORT}"))],
            ..Req::post(path)
        }
    }
    fn header(mut self, k: &'static str, v: &str) -> Self {
        self.headers.push((k, v.into()));
        self
    }
    fn without(mut self, k: &str) -> Self {
        self.headers.retain(|(n, _)| *n != k);
        self
    }
    fn body(mut self, b: Vec<u8>) -> Self {
        self.body = b;
        self
    }
    async fn send(self, app: &Router) -> Got {
        let mut b = Request::builder().method(self.method).uri(&self.path);
        for (k, v) in &self.headers {
            b = b.header(*k, v);
        }
        let mut req = b.body(Body::from(self.body)).unwrap();
        req.extensions_mut().insert(conn(self.peer));
        read(app.clone().oneshot(req).await.unwrap()).await
    }
}

fn code(g: &Got) -> String {
    let v: Value = serde_json::from_slice(&g.body).unwrap_or(Value::Null);
    v["error"]["code"].as_str().unwrap_or("").to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hello_answers_a_well_formed_local_request() {
    let w = world();
    let g = Req::post("/__musaic/agent/v1/hello").send(&w.app).await;
    assert_eq!(g.status, 200, "{}", String::from_utf8_lossy(&g.body));
    let v: Value = serde_json::from_slice(&g.body).unwrap();
    assert!(v["instanceId"].as_str().unwrap().starts_with("i_"));
    assert_eq!(v["serverNonce"].as_str().unwrap().len(), 32);
    assert_eq!(g.route.as_deref(), Some("/__musaic/agent/v1/hello"));
    // An empty body is fine for hello.
    let g = Req::post("/__musaic/agent/v1/hello")
        .body(Vec::new())
        .send(&w.app)
        .await;
    assert_eq!(g.status, 200);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hostile_host_is_refused() {
    let w = world();
    for host in [
        "evil.com",
        &format!("localhost:{PORT}"),
        "127.0.0.1:1",
        "127.0.0.1",
        "[::1]:41999",
    ] {
        let g = Req::post("/__musaic/agent/v1/hello")
            .without("host")
            .header("host", host)
            .send(&w.app)
            .await;
        assert_eq!((g.status, code(&g).as_str()), (403, "FORBIDDEN"), "{host}");
    }
    // Two Host headers, even if one is right.
    let g = Req::post("/__musaic/agent/v1/hello")
        .header("host", "evil.com")
        .send(&w.app)
        .await;
    assert_eq!(g.status, 403);
    // No Host at all.
    let g = Req::post("/__musaic/agent/v1/hello")
        .without("host")
        .send(&w.app)
        .await;
    assert_eq!(g.status, 403);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_peer_that_is_not_loopback_is_refused() {
    let w = world();
    let mut r = Req::post("/__musaic/agent/v1/hello");
    r.peer = "10.1.2.3:5555";
    assert_eq!(r.send(&w.app).await.status, 403);
    // No connection info at all cannot be verified.
    let mut req = Request::builder()
        .method("POST")
        .uri("/__musaic/agent/v1/hello")
        .header("host", format!("127.0.0.1:{PORT}"))
        .body(Body::empty())
        .unwrap();
    req.extensions_mut().clear();
    assert_eq!(
        read(w.app.clone().oneshot(req).await.unwrap()).await.status,
        403
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn any_origin_and_a_cross_site_fetch_are_refused() {
    let w = world();
    for origin in [
        "http://evil.com",
        "null",
        &format!("http://127.0.0.1:{PORT}"),
        &format!("http://localhost:{PORT}"),
    ] {
        let g = Req::post("/__musaic/agent/v1/hello")
            .header("origin", origin)
            .send(&w.app)
            .await;
        assert_eq!(
            (g.status, code(&g).as_str()),
            (403, "FORBIDDEN"),
            "{origin}"
        );
    }
    for site in ["cross-site", "same-origin", "same-site"] {
        let g = Req::post("/__musaic/agent/v1/hello")
            .header("sec-fetch-site", site)
            .send(&w.app)
            .await;
        assert_eq!(g.status, 403, "{site}");
    }
    // `none` (a direct request) is fine.
    let g = Req::post("/__musaic/agent/v1/hello")
        .header("sec-fetch-site", "none")
        .send(&w.app)
        .await;
    assert_eq!(g.status, 200);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn size_and_content_type_limits() {
    let w = world();
    let big = vec![b' '; 300 * 1024];
    let g = Req::post("/__musaic/agent/v1/hello")
        .body(big)
        .send(&w.app)
        .await;
    assert_eq!((g.status, code(&g).as_str()), (413, "INVALID_INPUT"));
    let ok = vec![b' '; 256 * 1024];
    assert_eq!(
        Req::post("/__musaic/agent/v1/hello")
            .body(ok)
            .send(&w.app)
            .await
            .status,
        200
    );
    let g = Req::post("/__musaic/agent/v1/hello")
        .without("content-type")
        .header("content-type", "text/plain")
        .send(&w.app)
        .await;
    assert_eq!((g.status, code(&g).as_str()), (415, "INVALID_INPUT"));
    let g = Req::post("/__musaic/agent/v1/hello")
        .without("content-type")
        .send(&w.app)
        .await;
    assert_eq!(g.status, 415, "a body with no type");
    let g = Req::post("/__musaic/agent/v1/hello")
        .without("content-type")
        .header("content-type", "application/json; charset=utf-8")
        .send(&w.app)
        .await;
    assert_eq!(g.status, 200);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tools_need_a_bearer_token_and_a_launch_cookie_grants_nothing() {
    let w = world();
    for (path, method) in [
        ("/__musaic/agent/v1/tools", "GET"),
        ("/__musaic/agent/v1/tools/musaic_capabilities", "POST"),
    ] {
        let base = || {
            if method == "GET" {
                Req::get(path)
            } else {
                Req::post(path)
            }
        };
        let g = base().send(&w.app).await;
        assert_eq!(
            (g.status, code(&g).as_str()),
            (401, "NOT_PAIRED"),
            "{path} without a token"
        );
        let g = base()
            .header("authorization", "Bearer not-a-token")
            .send(&w.app)
            .await;
        assert_eq!(g.status, 401, "{path} with a wrong token");
        let g = base()
            .header("authorization", "Basic abc")
            .send(&w.app)
            .await;
        assert_eq!(g.status, 401);
        // The webview's launch cookie alone is not authority here.
        let g = base()
            .header("cookie", &format!("{COOKIE}={TOKEN}"))
            .send(&w.app)
            .await;
        assert_eq!(g.status, 401, "{path} with only the launch cookie");
    }
    // An unknown agent path is a JSON 404 from the agent router, not the SPA.
    let g = Req::get("/__musaic/agent/v1/nope").send(&w.app).await;
    assert_eq!(g.status, 404);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_paired_agent_reads_through_the_whole_stack_and_no_credential_reaches_the_log() {
    use musaic_commands::auth::{
        ExchangeRequest, client_proof, hex_decode, random_hex, verify_server_proof,
    };
    let w = world();
    let ops = w.state.ops();
    let created = ops
        .agent()
        .create_pairing(
            "test agent",
            &[musaic_commands::PermissionKind::LibraryRead],
        )
        .unwrap();
    let id = created.pairing.id;
    let cfg = {
        let resolved = musaic_core::config::resolve_path(w.state.config_dir()).unwrap();
        musaic_commands::auth::config_dir_id(&resolved.to_string_lossy())
    };
    let secret_hex = musaic_secrets::agent::get_secret(w.agent_store.as_ref(), &cfg, &id)
        .unwrap()
        .unwrap();
    let secret = hex_decode(&secret_hex).unwrap();

    let hello: Value = serde_json::from_slice(
        &Req::post("/__musaic/agent/v1/hello")
            .send(&w.app)
            .await
            .body,
    )
    .unwrap();
    let (instance, nonce) = (
        hello["instanceId"].as_str().unwrap(),
        hello["serverNonce"].as_str().unwrap(),
    );
    let cn = random_hex(16);
    let req = ExchangeRequest {
        pairing_id: id.clone(),
        instance_id: instance.into(),
        server_nonce: nonce.into(),
        client_nonce: cn.clone(),
        client_proof: client_proof(&secret, &id, instance, nonce, &cn),
    };
    let g = Req::post("/__musaic/agent/v1/exchange")
        .body(serde_json::to_vec(&req).unwrap())
        .send(&w.app)
        .await;
    assert_eq!(g.status, 200, "{}", String::from_utf8_lossy(&g.body));
    let ex: Value = serde_json::from_slice(&g.body).unwrap();
    let token = ex["token"].as_str().unwrap().to_string();
    assert!(verify_server_proof(
        &secret,
        ex["serverProof"].as_str().unwrap(),
        &id,
        instance,
        nonce,
        &cn
    ));
    assert_eq!(ex["scopes"], json!(["library.read"]));
    assert!(!String::from_utf8_lossy(&g.body).contains(&secret_hex));

    // A failed exchange with a made-up proof, and a refused call with a bad
    // token: both leave log lines that must not carry any credential.
    let mut bad = req.clone();
    bad.client_proof = "ab".repeat(32);
    let g = Req::post("/__musaic/agent/v1/exchange")
        .body(serde_json::to_vec(&bad).unwrap())
        .send(&w.app)
        .await;
    assert_eq!((g.status, code(&g).as_str()), (401, "NOT_PAIRED"));
    let _ = Req::get("/__musaic/agent/v1/tools")
        .header("authorization", "Bearer sk-ant-leakytoken1234567890")
        .send(&w.app)
        .await;

    // The token works: capabilities, then the catalog.
    let g = Req::post("/__musaic/agent/v1/tools/musaic_capabilities")
        .header("authorization", &format!("Bearer {token}"))
        .send(&w.app)
        .await;
    assert_eq!(g.status, 200, "{}", String::from_utf8_lossy(&g.body));
    let v: Value = serde_json::from_slice(&g.body).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["result"]["scopes"], json!(["library.read"]));
    let g = Req::get("/__musaic/agent/v1/tools")
        .header("authorization", &format!("Bearer {token}"))
        .send(&w.app)
        .await;
    let cat: Value = serde_json::from_slice(&g.body).unwrap();
    assert_eq!(cat["tools"].as_array().unwrap().len(), 12);
    assert_eq!(
        cat["registryDigest"],
        musaic_commands::catalog::registry_digest()
    );
    // A domain error is an answer: 200 with ok:false. Bad input is a 400.
    let g = Req::post("/__musaic/agent/v1/tools/musaic_album_get")
        .header("authorization", &format!("Bearer {token}"))
        .body(
            json!({"albumId": "0123456789abcdef"})
                .to_string()
                .into_bytes(),
        )
        .send(&w.app)
        .await;
    assert_eq!(g.status, 200);
    let v: Value = serde_json::from_slice(&g.body).unwrap();
    assert_eq!(v["ok"], false, "{v}");
    let g = Req::post("/__musaic/agent/v1/tools/musaic_album_get")
        .header("authorization", &format!("Bearer {token}"))
        .body(json!({"albumId": "nope"}).to_string().into_bytes())
        .send(&w.app)
        .await;
    assert_eq!((g.status, code(&g).as_str()), (400, "INVALID_INPUT"));
    let g = Req::post("/__musaic/agent/v1/tools/musaic_nope")
        .header("authorization", &format!("Bearer {token}"))
        .send(&w.app)
        .await;
    assert_eq!(g.status, 404);

    // FR-028: nothing the app logged holds a token, the secret or a header.
    let lines = w.log.lock().unwrap().join("\n");
    for needle in [token.as_str(), secret_hex.as_str(), "leakytoken", "Bearer "] {
        assert!(
            !lines.contains(needle),
            "the log holds {needle:?}:\n{lines}"
        );
    }
    // The scrubber itself redacts an authorization header in any spelling.
    for line in [
        "authorization: Bearer abc123token",
        "\"Authorization\": \"Bearer abc123token\"",
        "got Bearer abc123token here",
    ] {
        let scrubbed =
            musaic_app::logging::scrub_patterns(line).unwrap_or_else(|| line.to_string());
        assert!(!scrubbed.contains("abc123token"), "{line} -> {scrubbed}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn existing_routes_keep_the_launch_gate_and_the_host_origin_check() {
    let w = world();
    let health = |host: &str, cookie: bool, origin: Option<&str>| {
        let mut b = Request::builder()
            .method("GET")
            .uri("/api/health")
            .header("host", host);
        if cookie {
            b = b.header("cookie", format!("{COOKIE}={TOKEN}"));
        }
        if let Some(o) = origin {
            b = b.header("origin", o);
        }
        b.body(Body::empty()).unwrap()
    };
    let own = format!("127.0.0.1:{PORT}");
    // The cookie is still required (the launch gate), and still works.
    let g = read(
        w.app
            .clone()
            .oneshot(health(&own, false, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        (
            g.status,
            String::from_utf8_lossy(&g.body).contains(NO_TOKEN)
        ),
        (403, true)
    );
    let g = read(
        w.app
            .clone()
            .oneshot(health(&own, true, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(g.status, 200);
    // The Host / Origin check still refuses a foreign host and a foreign origin.
    let g = read(
        w.app
            .clone()
            .oneshot(health("evil.com", true, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(g.status, 403);
    let g = read(
        w.app
            .clone()
            .oneshot(health(&own, true, Some("http://evil.com")))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(g.status, 403);
    // And an agent bearer token does not open a webview route.
    let g = read(
        w.app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/health")
                    .header("host", &own)
                    .header("authorization", "Bearer anything")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(g.status, 403);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_webview_routes_manage_pairings_and_never_show_a_secret() {
    let w = world();
    let own = format!("127.0.0.1:{PORT}");
    let call = |method: &'static str, path: &str, body: Option<Value>| {
        let mut b = Request::builder()
            .method(method)
            .uri(path)
            .header("host", &own)
            .header("cookie", format!("{COOKIE}={TOKEN}"));
        let body = match body {
            Some(v) => {
                b = b.header("content-type", "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        b.body(body).unwrap()
    };
    let send = |req: Request<Body>| {
        let app = w.app.clone();
        async move { read(app.oneshot(req).await.unwrap()).await }
    };
    let g = send(call(
        "POST",
        "/api/agent/pairings",
        Some(json!({"name": "Claude", "scopes": ["library.read", "proposal.submit"]})),
    ))
    .await;
    assert_eq!(g.status, 200, "{}", String::from_utf8_lossy(&g.body));
    let created: Value = serde_json::from_slice(&g.body).unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["policy"]["library.read"], "allow");
    assert_eq!(created["policy"]["tags.write"], "ask");
    assert_eq!(created["connect"]["command"], "musaic-cli");
    let text = String::from_utf8_lossy(&g.body).to_string();
    assert!(
        !text.to_lowercase().contains("secret"),
        "no secret in {text}"
    );

    let g = send(call("GET", "/api/agent/pairings", None)).await;
    let list: Value = serde_json::from_slice(&g.body).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);

    let g = send(call(
        "PUT",
        &format!("/api/agent/pairings/{id}/policy"),
        Some(json!({"tags.write": "allow"})),
    ))
    .await;
    assert_eq!(g.status, 400, "outside the scope");
    let g = send(call(
        "PUT",
        &format!("/api/agent/pairings/{id}/policy"),
        Some(json!({"library.read": "refuse"})),
    ))
    .await;
    assert_eq!(g.status, 200);
    let g = send(call(
        "PUT",
        &format!("/api/agent/pairings/{id}/policy"),
        Some(json!({"library.read": "maybe"})),
    ))
    .await;
    assert_eq!(g.status, 400);
    let g = send(call(
        "POST",
        "/api/agent/pairings",
        Some(json!({"name": "x", "scopes": ["bogus"]})),
    ))
    .await;
    assert_eq!(g.status, 400);

    let g = send(call(
        "GET",
        &format!("/api/agent/pairings/{id}/activity"),
        None,
    ))
    .await;
    assert_eq!(g.status, 200);
    let g = send(call("GET", "/api/agent/approvals", None)).await;
    assert_eq!(serde_json::from_slice::<Value>(&g.body).unwrap(), json!([]));
    let g = send(call("POST", "/api/agent/approvals/ap_nope/approve", None)).await;
    assert_eq!(g.status, 404);
    let g = send(call("GET", "/api/agent/approvals?state=bogus", None)).await;
    assert_eq!(g.status, 400);

    let g = send(call("DELETE", &format!("/api/agent/pairings/{id}"), None)).await;
    assert_eq!(g.status, 200);
    let revoked: Value = serde_json::from_slice(&g.body).unwrap();
    assert!(revoked["revokedAt"].is_string());
    // The webview routes still need the cookie.
    let g = read(
        w.app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/agent/pairings")
                    .header("host", &own)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(g.status, 403);
    let _ = &w.log;
}
