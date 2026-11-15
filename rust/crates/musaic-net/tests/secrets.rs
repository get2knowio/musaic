//! No secret in any message: the AcoustID key, a request
//! parameter, must not appear in any `Display` or `Debug` output of any
//! error variant, request or client, whichever way it reached them.

mod common;

use std::collections::BTreeSet;

use common::{FaultServer, closed_port, replay_dir};
use musaic_net::{
    AcoustIdClient, ClientError, CoverError, LiveTransport, MusicBrainzClient, ReplayTransport,
    Request, Transport, TransportError,
};
use serde_json::json;

const KEY: &str = "test-key-not-real";
/// Also try a key that changes under URL encoding.
const KEY2: &str = "s3cr3t+key/with=chars";

fn variant(e: &TransportError) -> &'static str {
    // Exhaustive on purpose: a new variant must be added to this test.
    match e {
        TransportError::Unobserved { .. } => "Unobserved",
        TransportError::InsecureScheme { .. } => "InsecureScheme",
        TransportError::Dns { .. } => "Dns",
        TransportError::Connect { .. } => "Connect",
        TransportError::Timeout { .. } => "Timeout",
        TransportError::Tls { .. } => "Tls",
        TransportError::Protocol { .. } => "Protocol",
        TransportError::TooManyRedirects { .. } => "TooManyRedirects",
        TransportError::BodyTooLarge { .. } => "BodyTooLarge",
        TransportError::Refused { .. } => "Refused",
        TransportError::Stopped => "Stopped",
        TransportError::Other(_) => "Other",
    }
}

fn client_variant(e: &ClientError) -> &'static str {
    match e {
        ClientError::Transport(t) => variant(t),
        ClientError::Status { .. } => "Status",
        ClientError::Parse { .. } => "Parse",
        ClientError::AcoustId(_) => "AcoustId",
    }
}

/// A request carrying the key both as a query parameter and in a form body,
/// so every path that could echo either is exercised.
fn keyed(origin: &str, path: &str, key: &str) -> Request {
    let mut r = Request::post_form(origin, path, vec![("client".into(), key.into())])
        .query("client", key)
        .timeout(std::time::Duration::from_millis(500));
    r.method = musaic_net::Method::Get;
    r
}

fn assert_clean(what: &str, text: &str, key: &str) {
    let mut enc = String::new();
    for b in key.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => {
                enc.push(b as char)
            }
            _ => enc.push_str(&format!("%{b:02X}")),
        }
    }
    assert!(
        !text.contains(key) && !text.contains(&enc),
        "{what} leaks the key: {text}"
    );
}

/// Produce every transport error variant through the real
/// code paths with the key on the request, plus every client error variant
/// from AcoustID lookups, and format each with Display and Debug.
#[test]
fn no_error_message_contains_the_key() {
    musaic_testdata::require_private!("replay");
    for key in [KEY, KEY2] {
        let mut errors: Vec<ClientError> = Vec::new();
        let mut transport_errors: Vec<TransportError> = Vec::new();

        let slow = FaultServer::start(
            vec![json!({"status": 200, "delay_s": 1.5, "body": {"text": "{}"}})],
            true,
        );
        let redirect = FaultServer::start(
            vec![json!({"status": 302, "headers": {"Location": "{origin}/again"}})],
            true,
        );
        let big = FaultServer::start(
            vec![json!({"status": 200, "body": {"fill": "x", "len": 64}})],
            true,
        );
        let short = FaultServer::start(
            vec![json!({"status": 200, "body": {"text": "{}"}, "action": "short_body"})],
            true,
        );
        let reset = FaultServer::start(vec![json!({"action": "reset"})], true);
        let to_http = FaultServer::start(
            vec![
                json!({"status": 302, "headers": {"Location": format!("http://elsewhere.example/?client={key}")}}),
            ],
            true,
        );
        let refused = closed_port();
        let refused_addr = format!("127.0.0.1:{refused}").parse().unwrap();

        let via = |server: &FaultServer, mut req: Request| {
            req.origin = server.origin();
            LiveTransport::with_static_dns(server.addr, &[]).send(&req)
        };
        let base = keyed("http://127.0.0.1:1", "/x", key);
        transport_errors.push(via(&slow, base.clone()).unwrap_err());
        transport_errors.push(via(&redirect, base.clone()).unwrap_err());
        transport_errors.push(via(&big, base.clone().max_body(10)).unwrap_err());
        transport_errors.push(via(&short, base.clone()).unwrap_err());
        transport_errors.push(via(&reset, base.clone()).unwrap_err());
        transport_errors.push(via(&to_http, base.clone()).unwrap_err());
        let mut r = base.clone();
        r.origin = format!("http://127.0.0.1:{refused}");
        transport_errors.push(
            LiveTransport::with_static_dns(refused_addr, &[])
                .send(&r)
                .unwrap_err(),
        );
        let mut r = base.clone();
        r.origin = "https://unresolvable.example".into();
        transport_errors.push(
            LiveTransport::with_static_dns(refused_addr, &["unresolvable.example"])
                .send(&r)
                .unwrap_err(),
        );
        // TLS: an HTTPS client talking to a server that answers in plain text.
        let plain = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let plain_addr = plain.local_addr().unwrap();
        std::thread::spawn(move || {
            use std::io::Write;
            for mut c in plain.incoming().flatten() {
                let _ = c.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n");
            }
        });
        let mut r = base.clone();
        r.origin = "https://tls.example".into();
        transport_errors.push(
            LiveTransport::with_static_dns(plain_addr, &[])
                .send(&r)
                .unwrap_err(),
        );
        let mut r = base.clone();
        r.origin = "http://plain.example".into();
        transport_errors.push(LiveTransport::new().send(&r).unwrap_err());
        transport_errors.push(ReplayTransport::new(replay_dir()).send(&base).unwrap_err());
        // Refused: a public-only request to a loopback URL (before any
        // I/O), and to a name that resolves to a loopback address.
        transport_errors.push(
            LiveTransport::with_static_dns(refused_addr, &[])
                .send(&base.clone().public_only())
                .unwrap_err(),
        );
        let mut r = base.clone().public_only();
        r.origin = "https://loopback.example".into();
        transport_errors.push(
            LiveTransport::with_static_dns(refused_addr, &[])
                .send(&r)
                .unwrap_err(),
        );
        // `Other` is built from text the transport scrubbed (live.rs).
        transport_errors.push(TransportError::Other(base.scrub(&format!("echo {key}"))));
        // `Stopped`: a background request whose stop flag is already set.
        transport_errors.push(
            musaic_net::BackgroundPaced::new(
                LiveTransport::with_static_dns(refused_addr, &[]),
                std::sync::Arc::new(musaic_net::Limiter::new(
                    musaic_net::SystemClock,
                    musaic_net::PacingPolicy::default(),
                )),
                musaic_net::RetryPolicy::default(),
                || false,
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            )
            .send(&base)
            .unwrap_err(),
        );

        // Client-level errors from AcoustID lookups with this key.
        for body in [
            json!({"status": 500, "body": {"text": "no"}}),
            json!({"status": 200, "body": {"text": format!("{{\"status\": \"error\", \"error\": {{\"message\": \"bad key {key}\"}}}}")}}),
            json!({"status": 200, "body": {"text": format!("{{\"status\": \"ok\", \"results\": [\"{key}")}}),
        ] {
            let s = FaultServer::start(vec![body], true);
            let live = LiveTransport::with_static_dns(s.addr, &[]);
            let c = AcoustIdClient::with_origin(&live, key, &s.origin());
            errors.push(c.lookup("AQAA", 10).unwrap_err());
            assert_clean("AcoustIdClient Debug", &format!("{c:?}"), key);
        }
        errors.extend(transport_errors.into_iter().map(ClientError::Transport));

        let covered: BTreeSet<&str> = errors.iter().map(client_variant).collect();
        let all: BTreeSet<&str> = [
            "Unobserved",
            "InsecureScheme",
            "Dns",
            "Connect",
            "Timeout",
            "Tls",
            "Protocol",
            "TooManyRedirects",
            "BodyTooLarge",
            "Refused",
            "Stopped",
            "Other",
            "Status",
            "Parse",
            "AcoustId",
        ]
        .into();
        assert_eq!(covered, all, "every variant is produced");
        for e in &errors {
            assert_clean(client_variant(e), &format!("{e}"), key);
            assert_clean(client_variant(e), &format!("{e:?}"), key);
            assert_clean("class", &format!("{:?}", e.class()), key);
        }
        for e in [
            CoverError::Transport(TransportError::Other(base.scrub(key))),
            CoverError::BadUrl("not a url".into()),
        ] {
            assert_clean("CoverError", &format!("{e} {e:?}"), key);
        }

        // Requests and clients.
        let lookup = AcoustIdClient::<ReplayTransport>::lookup_request(key, "AQAA", 10);
        assert_clean("Request Debug", &format!("{lookup:?}"), key);
        assert_clean("Request Debug", &format!("{base:?}"), key);
        assert_clean("redacted_url", &base.redacted_url(), key);
        assert_clean(
            "MusicBrainzClient Debug",
            &format!("{:?}", MusicBrainzClient::new(LiveTransport::new())),
            key,
        );
        // The key does go on the wire, where the service needs it.
        assert!(lookup.form_body().unwrap().contains("client="));
    }
}
