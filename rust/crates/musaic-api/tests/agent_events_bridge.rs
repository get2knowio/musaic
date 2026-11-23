//! Events over the real bridge (spec 005, contracts/events.md test 3): pair,
//! poll for a cursor, create work, poll, restart the server on the same
//! database and resume with identical event ids; a revoked pairing is
//! refused. A real loopback server and a raw HTTP client; no CLI.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

use musaic_api::loopback::{LoopbackOptions, LoopbackServer, serve_loopback};
use musaic_api::{AppState, Network};
use musaic_commands::PermissionKind as K;
use musaic_commands::auth::{ExchangeRequest, client_proof, config_dir_id, hex_decode, random_hex};
use musaic_db::delegated_work::{self as work, NewWork};
use musaic_secrets::MemoryStore;
use serde_json::{Value, json};

fn state(config: &std::path::Path, store: Arc<MemoryStore>) -> AppState {
    AppState::builder(config)
        .secrets(Arc::new(MemoryStore::new()))
        .agent_secrets(store)
        .network(Network::Offline)
        .subscribers(false)
        .auto_snapshot(false)
        .build()
        .unwrap()
}

fn http(port: u16, method: &str, path: &str, token: Option<&str>, body: &Value) -> (u16, Value) {
    let body = body.to_string();
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let auth = token
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\n{auth}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut raw = String::new();
    s.read_to_string(&mut raw).unwrap();
    let (head, text) = raw.split_once("\r\n\r\n").expect("a response");
    let status: u16 = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    // The body may be chunked; take the JSON between the first '{' and last '}'.
    let start = text.find(['{', '[']);
    let json = match (start, text.rfind(['}', ']'])) {
        (Some(a), Some(b)) if a < b => serde_json::from_str(&text[a..=b]).unwrap_or(Value::Null),
        _ => Value::Null,
    };
    (status, json)
}

fn login(port: u16, config: &std::path::Path, store: &MemoryStore, pairing: &str) -> String {
    let hello = http(port, "POST", "/__musaic/agent/v1/hello", None, &json!({})).1;
    let instance = hello["instanceId"].as_str().unwrap().to_string();
    let nonce = hello["serverNonce"].as_str().unwrap().to_string();
    let resolved = musaic_core::config::resolve_path(config).unwrap();
    let cfg = config_dir_id(&resolved.to_string_lossy());
    let secret = hex_decode(
        &musaic_secrets::agent::get_secret(store, &cfg, pairing)
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    let client_nonce = random_hex(16);
    let req = ExchangeRequest {
        pairing_id: pairing.into(),
        instance_id: instance.clone(),
        server_nonce: nonce.clone(),
        client_nonce: client_nonce.clone(),
        client_proof: client_proof(&secret, pairing, &instance, &nonce, &client_nonce),
    };
    let (status, v) = http(
        port,
        "POST",
        "/__musaic/agent/v1/exchange",
        None,
        &serde_json::to_value(&req).unwrap(),
    );
    assert_eq!(status, 200, "{v}");
    v["token"].as_str().unwrap().to_string()
}

fn poll(port: u16, token: &str, cursor: Option<&str>) -> (u16, Value) {
    let mut body = json!({"name": "musaic.work.requested.v1"});
    if let Some(c) = cursor {
        body["cursor"] = json!(c);
    }
    http(
        port,
        "POST",
        "/__musaic/agent/v1/events/poll",
        Some(token),
        &body,
    )
}

async fn serve(state: &AppState) -> LoopbackServer {
    serve_loopback(state.clone(), LoopbackOptions::default(), |r, _| r)
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn events_survive_a_restart_and_a_revoked_pairing_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config");
    let store = Arc::new(MemoryStore::new());
    let st = state(&config, store.clone());
    let pairing = st
        .ops()
        .agent()
        .create_pairing("Events", &[K::LibraryRead, K::ProposalSubmit])
        .unwrap()
        .pairing
        .id;
    let server = serve(&st).await;
    let port = server.addr.port();
    let token = login(port, &config, &store, &pairing);

    let (status, list) = http(
        port,
        "POST",
        "/__musaic/agent/v1/events/list",
        Some(&token),
        &json!({}),
    );
    assert_eq!(status, 200);
    assert_eq!(
        list["result"]["events"].as_array().unwrap().len(),
        2,
        "{list}"
    );

    // Bootstrap: no events, a cursor.
    let (status, boot) = poll(port, &token, None);
    assert_eq!(status, 200, "{boot}");
    assert_eq!(boot["result"]["events"], json!([]));
    let cursor = boot["result"]["cursor"].as_str().unwrap().to_string();

    // New work, published as the app does after a commit.
    let conn = musaic_db::open_connection(st.db_path()).unwrap();
    work::enqueue(
        &conn,
        &[NewWork {
            id: "3ef3f326-6a82-406c-a473-601562c3b579",
            group_id: "g",
            kind: "metadata.identify",
            album_id: "0123456789abcdef",
            base_revision: "rev",
            input_json: "{}",
            input_digest: "d",
            max_attempts: 3,
        }],
        1_800_000_000_000,
    )
    .unwrap();
    st.ops().agent().sweep_delegation();
    let (_, first) = poll(port, &token, Some(&cursor));
    let events = first["result"]["events"].as_array().unwrap().clone();
    assert_eq!(events.len(), 1, "{first}");
    assert_eq!(events[0]["data"]["state"], "queued");
    let id = events[0]["eventId"].as_str().unwrap().to_string();

    // Restart on the same database: a new instance and port, same history.
    server.stop().await;
    drop(conn);
    drop(st);
    let st = state(&config, store.clone());
    let server = serve(&st).await;
    let port = server.addr.port();
    let token = login(port, &config, &store, &pairing);
    let (_, again) = poll(port, &token, Some(&cursor));
    assert_eq!(
        again["result"]["events"][0]["eventId"],
        json!(id),
        "{again}"
    );
    assert_eq!(again["result"]["events"], first["result"]["events"]);
    // The cursor survives too: nothing after the last one.
    let last = again["result"]["cursor"].as_str().unwrap().to_string();
    let (_, none) = poll(port, &token, Some(&last));
    assert_eq!(none["result"]["events"], json!([]));

    // Revoked: the next call is refused.
    st.ops().agent().revoke_pairing(&pairing).unwrap();
    let (status, refused) = poll(port, &token, Some(&last));
    assert!(
        status == 401 || refused["error"]["code"] == "EVENT_FORBIDDEN",
        "{status} {refused}"
    );
    assert_ne!(refused["ok"], json!(true));
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_host_observation_is_stored_and_shown_in_the_diagnostics() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config");
    let store = Arc::new(MemoryStore::new());
    let st = state(&config, store.clone());
    let pairing = st
        .ops()
        .agent()
        .create_pairing("Observed", &[K::LibraryRead])
        .unwrap()
        .pairing
        .id;
    let server = serve(&st).await;
    let port = server.addr.port();

    // Without a token the new routes are refused like every agent route.
    for (m, p) in [
        ("POST", "/__musaic/agent/v1/session/observe"),
        ("GET", "/__musaic/agent/v1/session"),
    ] {
        assert_eq!(http(port, m, p, None, &json!({})).0, 401, "{p}");
    }
    let (_, none) = http(port, "GET", "/api/agent/diagnostics", None, &json!({}));
    let first = none.as_array().unwrap_or_else(|| panic!("{none}"));
    assert!(
        first[0]["reachable"]["at"].is_null(),
        "never connected: {none}"
    );

    let token = login(port, &config, &store, &pairing);
    let body = json!({"clientName": "Harness", "clientVersion": "9", "profile": "2026-07-28",
                      "tasks": true, "skills": false, "events": true});
    let (status, ok) = http(
        port,
        "POST",
        "/__musaic/agent/v1/session/observe",
        Some(&token),
        &body,
    );
    assert_eq!((status, &ok["ok"]), (200, &json!(true)), "{ok}");
    // A malformed body never fails the session.
    let (status, _) = http(
        port,
        "POST",
        "/__musaic/agent/v1/session/observe",
        Some(&token),
        &json!([1]),
    );
    assert_eq!(status, 200);
    let (_, session) = http(
        port,
        "GET",
        "/__musaic/agent/v1/session",
        Some(&token),
        &json!({}),
    );
    assert_eq!(
        session["result"]["observation"]["clientName"], "Harness",
        "{session}"
    );
    assert_eq!(session["result"]["subscriptionActive"], false);
    assert!(
        session["result"]["skillVersion"]
            .as_str()
            .unwrap()
            .contains('.')
    );

    let (_, d) = http(port, "GET", "/api/agent/diagnostics", None, &json!({}));
    let row = &d[0];
    assert_eq!(row["pairingId"], json!(pairing));
    assert!(row["reachable"]["at"].is_string());
    assert_eq!(row["host"]["clientName"], "Harness");
    assert_eq!(row["subscription"]["active"], false);
    assert!(row["lastClaim"]["at"].is_null());
    let text = d.to_string().to_lowercase();
    assert!(!text.contains("ready"), "{text}");
    let (status, work) = http(port, "GET", "/api/research/work", None, &json!({}));
    assert_eq!((status, &work), (200, &json!([])));
    server.stop().await;
}
