//! The redirect limit of the live transport, and which requests go through
//! a proxy. Every server here is a listener the test starts on 127.0.0.1,
//! and every transport pins name resolution to it (or, for the one test that
//! reads the proxy environment, is only built after checking that the
//! environment names that local proxy), so no request can reach another
//! machine.

mod common;

use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::FaultServer;
use musaic_net::live::MAX_REDIRECTS;
use musaic_net::{LiveTransport, Method, Request, Transport, TransportError};
use serde_json::json;

fn get(url: &str) -> Request {
    Request::from_url(Method::Get, url)
        .unwrap()
        .timeout(Duration::from_secs(5))
}

/// A chain of redirects longer than the limit ends as "too many redirects"
/// after the limit's worth of hops (one request more than the limit),
/// naming the limit, and the limit is the small one the issue asks for
/// (cover downloads already stop at 5).
#[test]
fn redirects_stop_at_the_limit() {
    assert!(
        (3..=10).contains(&MAX_REDIRECTS),
        "limit is {MAX_REDIRECTS}"
    );
    let server = FaultServer::start(
        vec![json!({"status": 302, "headers": {"Location": "{origin}/again"}})],
        true,
    );
    let live = LiveTransport::with_static_dns(server.addr, &[]);
    match live.send(&get(&format!("{}/start", server.origin()))) {
        Err(TransportError::TooManyRedirects { limit, .. }) => assert_eq!(limit, MAX_REDIRECTS),
        other => panic!("{other:?}"),
    }
    assert_eq!(server.seen().len() as u32, MAX_REDIRECTS + 1);
}

/// A chain of exactly the limit is followed to its answer.
#[test]
fn a_chain_of_exactly_the_limit_is_followed() {
    let mut script: Vec<_> = (0..MAX_REDIRECTS)
        .map(|_| json!({"status": 301, "headers": {"Location": "{origin}/next"}}))
        .collect();
    script.push(json!({"status": 200, "body": {"text": "done"}}));
    let server = FaultServer::start(script, false);
    let live = LiveTransport::with_static_dns(server.addr, &[]);
    let resp = live
        .send(&get(&format!("{}/start", server.origin())))
        .unwrap();
    assert_eq!((resp.status, resp.body.as_slice()), (200, &b"done"[..]));
    assert_eq!(server.seen().len() as u32, MAX_REDIRECTS + 1);
}

/// The redirect limit does not replace the per-hop address rule: within the
/// limit, a hop to plain HTTP elsewhere is still refused at that hop.
#[test]
fn every_hop_is_still_checked_within_the_limit() {
    let server = FaultServer::start(
        vec![
            json!({"status": 302, "headers": {"Location": "{origin}/a"}}),
            json!({"status": 302, "headers": {"Location": "http://elsewhere.example.org/x"}}),
        ],
        false,
    );
    let live = LiveTransport::with_static_dns(server.addr, &[]);
    match live.send(&get(&format!("{}/start", server.origin()))) {
        Err(TransportError::InsecureScheme { url }) => {
            assert_eq!(url, "http://elsewhere.example.org/x")
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(server.seen().len(), 2);
}

/// What a listener received first on each connection.
type Firsts = Arc<Mutex<Vec<Vec<u8>>>>;

/// A listener that records the first bytes of every connection and answers
/// a proxy-style 502 (a TLS hello gets no reply and a closed socket).
fn recording_listener() -> (SocketAddr, Firsts) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let firsts: Firsts = Arc::default();
    let seen = Arc::clone(&firsts);
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { continue };
            let seen = Arc::clone(&seen);
            std::thread::spawn(move || {
                let _ = conn.set_read_timeout(Some(Duration::from_secs(5)));
                // A proxy request arrives in pieces: read to the end of its
                // first line. A TLS hello (first byte 0x16) has none.
                let mut got = Vec::new();
                let mut buf = [0u8; 4096];
                while !got.windows(2).any(|w| w == b"\r\n") && got.first() != Some(&0x16) {
                    match conn.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => got.extend_from_slice(&buf[..n]),
                    }
                }
                seen.lock().unwrap().push(got);
                let _ = conn.write_all(
                    b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            });
        }
    });
    (addr, firsts)
}

fn connect_lines(firsts: &Firsts) -> Vec<String> {
    firsts
        .lock()
        .unwrap()
        .iter()
        .filter_map(|b| {
            let text = String::from_utf8_lossy(b);
            text.starts_with("CONNECT ")
                .then(|| text.lines().next().unwrap_or("").to_string())
        })
        .collect()
}

/// With a proxy configured, an app request (MusicBrainz, AcoustID) goes to
/// the proxy as a CONNECT for the target name; the name itself is never
/// resolved here (a name that does not exist still reaches the proxy). A
/// `public_only` request (a cover preview or download) never uses the
/// proxy, because a proxy would resolve the name itself, out of reach of the
/// public-address check: it connects straight to the address it judged, so
/// the listener sees a TLS hello, not a CONNECT.
#[test]
fn a_configured_proxy_serves_app_requests_but_never_public_only_ones() {
    let (addr, firsts) = recording_listener();
    let public: IpAddr = "93.184.216.34".parse().unwrap();
    let live = LiveTransport::with_declared_dns_and_proxy(
        addr,
        &[],
        &[("cdn.example.org", public)],
        &format!("http://127.0.0.1:{}", addr.port()),
    );

    let e = live
        .send(&get("https://musicbrainz.example.invalid/ws/2/x"))
        .unwrap_err();
    assert!(!matches!(e, TransportError::Refused { .. }), "{e:?}");
    assert_eq!(
        connect_lines(&firsts),
        ["CONNECT musicbrainz.example.invalid:443 HTTP/1.1"]
    );

    let before = firsts.lock().unwrap().len();
    let e = live
        .send(
            &get("https://cdn.example.org/cover.jpg")
                .no_redirects()
                .public_only(),
        )
        .unwrap_err();
    assert!(!matches!(e, TransportError::Refused { .. }), "{e:?}");
    let after = firsts.lock().unwrap();
    assert_eq!(after.len(), before + 1, "it connected directly");
    assert_eq!(
        after[before].first(),
        Some(&0x16),
        "a TLS hello, not CONNECT"
    );
    drop(after);
    assert_eq!(connect_lines(&firsts).len(), 1);
}

/// A proxy that answers 502 is a connection failure of the app's request,
/// not a cert or protocol surprise and not a success.
#[test]
fn a_failing_proxy_is_a_failed_request() {
    let (addr, _firsts) = recording_listener();
    let live = LiveTransport::with_declared_dns_and_proxy(
        addr,
        &[],
        &[],
        &format!("http://127.0.0.1:{}", addr.port()),
    );
    let e = live
        .send(&get("https://musicbrainz.example.invalid/x"))
        .unwrap_err();
    assert!(!matches!(e, TransportError::Refused { .. }), "{e:?}");
}

/// The transports tests pin to a local server never use a proxy, whatever
/// the environment says (so the other tests here and elsewhere cannot be
/// redirected): the listener sees the request itself.
#[test]
fn pinned_transports_ignore_proxies() {
    let server = FaultServer::start(vec![json!({"status": 200})], true);
    let live = LiveTransport::with_static_dns(server.addr, &[]);
    live.send(&get(&format!("{}/x", server.origin()))).unwrap();
    assert_eq!(server.seen()[0].method, "GET");
}

const CHILD_MARK: &str = "MUSAIC_NET_PROXY_ENV_CHILD";

/// Today the system proxy variables are honoured for the app's own
/// requests (ureq's default; whether that is the wanted product behaviour
/// is for the owner to decide). Setting an environment variable in this
/// process is not possible without `unsafe`, so the test starts itself
/// again with `HTTPS_PROXY` pointing at a local listener and runs the
/// request there. The child refuses to send anything unless the proxy
/// settings it inherited are the ones it was given, so a build that stopped
/// honouring the environment would fail here without a request going
/// anywhere.
#[test]
fn environment_proxy_is_used_for_app_requests() {
    if std::env::var(CHILD_MARK).is_err() {
        let (addr, firsts) = recording_listener();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "environment_proxy_is_used_for_app_requests",
                "--nocapture",
            ])
            .env(CHILD_MARK, "1")
            .env("HTTPS_PROXY", format!("http://127.0.0.1:{}", addr.port()))
            .env("https_proxy", format!("http://127.0.0.1:{}", addr.port()))
            .env_remove("NO_PROXY")
            .env_remove("no_proxy")
            .env_remove("ALL_PROXY")
            .env_remove("all_proxy")
            .status()
            .unwrap();
        assert!(status.success(), "the child run failed: {status}");
        assert_eq!(
            connect_lines(&firsts),
            ["CONNECT proxy-env.example.invalid:443 HTTP/1.1"]
        );
        return;
    }
    // Child: only ever talks to the proxy named in the environment, and
    // only after checking that is what the client would use.
    let want = std::env::var("HTTPS_PROXY").unwrap();
    assert!(want.starts_with("http://127.0.0.1:"), "{want}");
    let proxy = ureq::Proxy::try_from_env().expect("the environment names a proxy");
    assert_eq!(
        proxy.port(),
        want.rsplit(':').next().unwrap().parse::<u16>().unwrap()
    );
    let live = LiveTransport::new();
    let e = live
        .send(&get("https://proxy-env.example.invalid/x"))
        .unwrap_err();
    assert!(!matches!(e, TransportError::Refused { .. }), "{e:?}");
}
