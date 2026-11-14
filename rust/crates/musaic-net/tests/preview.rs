//! The cover
//! preview fetch (`musaic_net::preview`). Not an Python app behaviour (the
//! Python app's browser loaded remote covers itself); these tests pin the
//! security rules of the new path. Scripted transports and 127.0.0.1
//! servers only.

mod common;

use std::collections::HashMap;
use std::io::Write;
use std::net::{IpAddr, TcpListener};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use musaic_net::preview::{
    PREVIEW_MAX_BYTES, PREVIEW_MAX_REDIRECTS, PREVIEW_TIMEOUT, PreviewError, fetch_preview,
};
use musaic_net::{LiveTransport, Request, Response, Transport, TransportError};

const JPEG: &[u8] = &[0xff, 0xd8, 0xff, 0xe0, 0, 0x10, b'J', b'F', b'I', b'F'];

/// Answers by URL; records every request it was sent.
#[derive(Default)]
struct Scripted {
    answers: HashMap<String, Result<Response, ()>>,
    sent: Mutex<Vec<Request>>,
}

impl Scripted {
    fn ok(mut self, url: &str, status: u16, headers: &[(&str, &str)], body: &[u8]) -> Self {
        self.answers.insert(
            url.into(),
            Ok(Response {
                status,
                headers: headers
                    .iter()
                    .map(|(k, v)| ((*k).into(), (*v).into()))
                    .collect(),
                body: body.to_vec(),
            }),
        );
        self
    }
    fn too_large(mut self, url: &str) -> Self {
        self.answers.insert(url.into(), Err(()));
        self
    }
    fn urls(&self) -> Vec<String> {
        self.sent.lock().unwrap().iter().map(Request::url).collect()
    }
}

impl Transport for Scripted {
    fn send(&self, r: &Request) -> Result<Response, TransportError> {
        self.sent.lock().unwrap().push(r.clone());
        match self.answers.get(&r.url()) {
            Some(Ok(resp)) => Ok(resp.clone()),
            Some(Err(())) => Err(TransportError::BodyTooLarge {
                url: r.url(),
                limit: r.max_body.unwrap(),
            }),
            None => Err(TransportError::Unobserved {
                key: String::new(),
                method: "GET",
                url: r.url(),
            }),
        }
    }
}

const A: &str = "https://coverartarchive.org/release-group/rg/front";
const B: &str = "https://archive.org/download/x/front.jpg";

#[test]
fn found_image_follows_checked_redirects() {
    let t = Scripted::default().ok(A, 307, &[("Location", B)], b"").ok(
        B,
        200,
        &[("Content-Type", "image/jpeg; charset=binary")],
        JPEG,
    );
    let p = fetch_preview(&t, A).unwrap();
    assert_eq!(p.bytes, JPEG);
    assert_eq!(p.mime, "image/jpeg");
    assert_eq!(t.urls(), [A, B]);
    for r in t.sent.lock().unwrap().iter() {
        assert!(!r.follow_redirects, "the transport must not follow alone");
        assert!(r.public_only);
        assert_eq!(r.timeout, Some(PREVIEW_TIMEOUT));
        assert_eq!(r.max_body, Some(PREVIEW_MAX_BYTES + 1));
        assert_eq!(r.method, musaic_net::Method::Get);
    }
}

#[test]
fn non_image_content_types_refused() {
    for ct in [
        Some("text/html"),
        Some("image/svg+xml"),
        Some("application/octet-stream"),
        None,
    ] {
        let h: Vec<(&str, &str)> = ct.map(|c| ("Content-Type", c)).into_iter().collect();
        let t = Scripted::default().ok(A, 200, &h, JPEG);
        assert!(
            matches!(fetch_preview(&t, A), Err(PreviewError::ContentType(_))),
            "{ct:?}"
        );
    }
    // Declared an image, but the bytes are not one.
    let t = Scripted::default().ok(A, 200, &[("Content-Type", "image/png")], b"<html>");
    assert!(matches!(
        fetch_preview(&t, A),
        Err(PreviewError::NotAnImage(_))
    ));
}

#[test]
fn oversized_refused() {
    let t = Scripted::default().too_large(A);
    assert!(matches!(
        fetch_preview(&t, A),
        Err(PreviewError::TooLarge(_))
    ));
    // A transport that ignores max_body: the length is checked again.
    let mut big = JPEG.to_vec();
    big.resize(PREVIEW_MAX_BYTES as usize + 1, 0);
    let t = Scripted::default().ok(A, 200, &[("Content-Type", "image/jpeg")], &big);
    assert!(matches!(
        fetch_preview(&t, A),
        Err(PreviewError::TooLarge(_))
    ));
}

#[test]
fn redirect_to_a_private_address_refused_before_it_is_sent() {
    for target in [
        "https://10.0.0.7/x.jpg",
        "https://169.254.169.254/latest/meta-data/",
        "https://[::1]/x.jpg",
        "https://localhost/x.jpg",
        "http://127.0.0.1:8765/api/health",
        "http://archive.org/x.jpg",
        "https://127.1/x.jpg",
    ] {
        let t = Scripted::default().ok(A, 302, &[("Location", target)], b"");
        let got = fetch_preview(&t, A);
        assert!(
            matches!(got, Err(PreviewError::Refused(_))),
            "{target}: {got:?}"
        );
        assert_eq!(t.urls(), [A], "{target}: the private hop was never sent");
    }
    // The first URL itself.
    let t = Scripted::default();
    assert!(matches!(
        fetch_preview(&t, "https://192.168.1.1/x.jpg"),
        Err(PreviewError::Refused(_))
    ));
    assert!(t.urls().is_empty());
}

#[test]
fn redirect_limit() {
    let mut t = Scripted::default();
    let hops = PREVIEW_MAX_REDIRECTS as usize + 1;
    for i in 0..=hops {
        t = t.ok(
            &format!("https://a{i}.example.org/x"),
            302,
            &[("Location", &format!("https://a{}.example.org/x", i + 1))],
            b"",
        );
    }
    assert!(matches!(
        fetch_preview(&t, "https://a0.example.org/x"),
        Err(PreviewError::TooManyRedirects(_))
    ));
    assert_eq!(t.urls().len(), hops);
}

#[test]
fn http_errors_are_reported() {
    let t = Scripted::default().ok(A, 404, &[], b"");
    assert!(matches!(
        fetch_preview(&t, A),
        Err(PreviewError::Status(404))
    ));
    let t = Scripted::default().ok(A, 302, &[], b"");
    assert!(matches!(
        fetch_preview(&t, A),
        Err(PreviewError::Status(302))
    ));
}

/// A listener that answers every connection in plain text (an HTTPS client
/// fails its handshake), counting connections.
fn plain_listener() -> (std::net::SocketAddr, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let n = Arc::new(AtomicUsize::new(0));
    let n2 = n.clone();
    std::thread::spawn(move || {
        for mut c in l.incoming().flatten() {
            n2.fetch_add(1, Ordering::SeqCst);
            let _ = c.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n");
        }
    });
    (addr, n)
}

fn public_get(url: &str) -> Request {
    Request::from_url(musaic_net::Method::Get, url)
        .unwrap()
        .timeout(Duration::from_millis(500))
        .no_redirects()
        .public_only()
}

/// The live transport checks the *resolved* address of a public-only
/// request: a name that resolves to a private address is refused without a
/// connection; a public one is connected to (and here fails TLS against
/// the plain-text listener, which proves the check let it through).
#[test]
fn live_transport_refuses_names_resolving_to_private_addresses() {
    let (addr, conns) = plain_listener();
    let private: IpAddr = "10.0.0.5".parse().unwrap();
    let public: IpAddr = "93.184.216.34".parse().unwrap();
    let t = LiveTransport::with_declared_dns(
        addr,
        &[],
        &[
            ("internal.example.org", private),
            ("cdn.example.org", public),
        ],
    );
    for url in [
        "https://internal.example.org/x.jpg",
        // Not declared: judged as the pinned 127.0.0.1.
        "https://undeclared.example.org/x.jpg",
        // URL-level refusals, before resolution.
        "http://127.0.0.1:1/x.jpg",
        "https://localhost/x.jpg",
    ] {
        let e = t.send(&public_get(url)).unwrap_err();
        assert!(matches!(e, TransportError::Refused { .. }), "{url}: {e:?}");
    }
    assert_eq!(conns.load(Ordering::SeqCst), 0, "nothing was connected to");
    let e = t
        .send(&public_get("https://cdn.example.org/x.jpg"))
        .unwrap_err();
    assert!(!matches!(e, TransportError::Refused { .. }), "{e:?}");
    assert_eq!(conns.load(Ordering::SeqCst), 1);
    // Without public_only the same transport still connects anywhere the
    // resolver points (the resolver's existing rule, unchanged).
    let mut r = public_get("https://internal.example.org/x.jpg");
    r.public_only = false;
    let e = t.send(&r).unwrap_err();
    assert!(!matches!(e, TransportError::Refused { .. }), "{e:?}");
}

/// `no_redirects`: the live transport returns the 3xx itself.
#[test]
fn live_transport_returns_redirects_when_asked() {
    let s = common::FaultServer::start(
        vec![serde_json::json!({"status": 302, "headers": {"Location": "{origin}/again"}})],
        true,
    );
    let t = LiveTransport::with_static_dns(s.addr, &[]);
    let r = Request::from_url(musaic_net::Method::Get, &format!("{}/x", s.origin()))
        .unwrap()
        .no_redirects();
    let resp = t.send(&r).unwrap();
    assert_eq!(resp.status, 302);
    assert_eq!(s.seen().len(), 1);
}
