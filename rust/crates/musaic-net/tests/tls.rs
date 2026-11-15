//! Certificate verification against a local TLS server on 127.0.0.1 whose
//! certificate is made at test time and trusted by nobody: the live
//! transport refuses it with [`TransportError::UntrustedCertificate`],
//! which names the host only and is not an "offline" error.
//!
//! The accepting path needs a root the operating system trusts. On macOS and
//! Windows that means changing the system trust store, which these tests
//! never do. On Linux the system verifier reads the CA bundle that
//! `SSL_CERT_FILE` names, so a child test process pointed at a bundle holding
//! the test's own authority proves that the system's trust, not the roots
//! built into the app, decides.

mod common;

use common::{pki, serve_tls as serve};
use musaic_net::live::{Trust, trust};
use musaic_net::{
    ClientError, LiveTransport, MusicBrainzClient, Request, Transport, TransportError,
};
#[cfg(target_os = "linux")]
use rustls::pki_types::CertificateDer;

const KEY: &str = "test-key-not-real";

fn keyed(origin: &str) -> Request {
    Request::get(origin, "/ws/2/release-group")
        .query("client", KEY)
        .query("fmt", "json")
        .timeout(std::time::Duration::from_secs(10))
}

/// The certificate is issued by an authority the system does not trust:
/// refused, as its own error, naming the host and nothing else.
#[test]
fn an_untrusted_certificate_is_refused() {
    let pki = pki(&["untrusted.example"]);
    let addr = serve(&pki);
    let transport = LiveTransport::with_static_dns(addr, &[]);
    let err = transport
        .send(&keyed("https://untrusted.example"))
        .unwrap_err();
    match &err {
        TransportError::UntrustedCertificate { host, reason } => {
            assert_eq!(host, "untrusted.example");
            assert!(!reason.is_empty());
        }
        other => panic!("expected UntrustedCertificate, got {other:?}"),
    }
    assert!(!err.is_offline(), "{err:?}");
    let text = err.to_string();
    assert!(
        text.starts_with(
            "refused the connection to untrusted.example: the server's certificate is not trusted ("
        ),
        "{text}"
    );
    for leak in [KEY, "/ws/2", "client=", "fmt=json"] {
        assert!(!text.contains(leak), "{leak:?} in {text}");
        assert!(!format!("{err:?}").contains(leak), "{leak:?} in {err:?}");
    }
}

/// Through a client, the refusal reaches callers (the resolver's "network
/// error" text, the log, an API error detail) as the same plain message.
#[test]
fn a_client_reports_the_untrusted_certificate() {
    let pki = pki(&["musicbrainz.example"]);
    let addr = serve(&pki);
    let transport = LiveTransport::with_static_dns(addr, &[]);
    let mb = MusicBrainzClient::with_origin(&transport, "https://musicbrainz.example");
    match mb.search_release_groups("Artist", "Album") {
        Err(ClientError::Transport(TransportError::UntrustedCertificate { host, .. })) => {
            assert_eq!(host, "musicbrainz.example");
        }
        other => panic!("expected UntrustedCertificate, got {other:?}"),
    }
}

/// A public-only request (cover downloads) is refused the same way once its
/// address passes the public-address rule.
#[test]
fn a_public_only_request_reports_the_untrusted_certificate() {
    let pki = pki(&["covers.example"]);
    let addr = serve(&pki);
    let transport = LiveTransport::with_declared_dns(
        addr,
        &[],
        &[("covers.example", "93.184.216.34".parse().unwrap())],
    );
    let req = Request::from_url(musaic_net::Method::Get, "https://covers.example/front.jpg")
        .unwrap()
        .public_only();
    match transport.send(&req) {
        Err(TransportError::UntrustedCertificate { host, .. }) => {
            assert_eq!(host, "covers.example");
        }
        other => panic!("expected UntrustedCertificate, got {other:?}"),
    }
}

/// A certificate for another name than the one asked for is refused too,
/// even when nothing else about it is checked first.
#[test]
fn a_certificate_for_another_name_is_refused() {
    let pki = pki(&["other.example"]);
    let addr = serve(&pki);
    let err = LiveTransport::with_static_dns(addr, &[])
        .send(&keyed("https://asked-for.example"))
        .unwrap_err();
    match &err {
        TransportError::UntrustedCertificate { host, .. } => {
            assert_eq!(host, "asked-for.example");
        }
        other => panic!("expected UntrustedCertificate, got {other:?}"),
    }
}

/// Every supported desktop system has a trust store, so the system
/// verifier is the one in use (the bundled roots are a fallback for a
/// system without one).
#[test]
fn the_system_verifier_is_in_use() {
    assert_eq!(*trust(), Trust::System);
}

/// Linux only: the system verifier trusts what the system's CA bundle holds.
/// A child test process is pointed (`SSL_CERT_FILE`) at a bundle with the
/// test authority only; the request that the parent saw refused succeeds.
/// The roots built into the app could not accept this certificate.
#[cfg(target_os = "linux")]
#[test]
fn linux_trusts_the_system_ca_bundle() {
    const CHILD: &str = "MUSAIC_TLS_TEST_CHILD";
    if let Ok(dir) = std::env::var(CHILD) {
        // The child: the bundle and the server's certificate were made by the
        // parent; serve them and connect.
        let read = |name: &str| std::fs::read(std::path::Path::new(&dir).join(name)).unwrap();
        let pki = common::TestPki {
            ca_der: CertificateDer::from(read("ca.der")),
            leaf_der: CertificateDer::from(read("leaf.der")),
            leaf_key: read("leaf.key"),
        };
        let addr = serve(&pki);
        let resp = LiveTransport::with_static_dns(addr, &[])
            .send(&keyed("https://trusted.example"))
            .expect("the system bundle's authority is trusted");
        assert_eq!(resp.status, 200);
        return;
    }
    use base64::Engine as _;
    let pki = pki(&["trusted.example"]);
    let dir = std::env::temp_dir().join(format!("musaic-tls-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("ca.der"), pki.ca_der.as_ref()).unwrap();
    std::fs::write(dir.join("leaf.der"), pki.leaf_der.as_ref()).unwrap();
    std::fs::write(dir.join("leaf.key"), &pki.leaf_key).unwrap();
    let b64 = base64::engine::general_purpose::STANDARD.encode(pki.ca_der.as_ref());
    let mut pem = String::from("-----BEGIN CERTIFICATE-----\n");
    for line in b64.as_bytes().chunks(64) {
        pem.push_str(std::str::from_utf8(line).unwrap());
        pem.push('\n');
    }
    pem.push_str("-----END CERTIFICATE-----\n");
    let bundle = dir.join("bundle.pem");
    std::fs::write(&bundle, pem).unwrap();

    // Without the bundle (this process): refused.
    let addr = serve(&pki);
    let err = LiveTransport::with_static_dns(addr, &[])
        .send(&keyed("https://trusted.example"))
        .unwrap_err();
    assert!(
        matches!(err, TransportError::UntrustedCertificate { .. }),
        "{err:?}"
    );

    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "linux_trusts_the_system_ca_bundle",
            "--nocapture",
        ])
        .env(CHILD, &dir)
        .env("SSL_CERT_FILE", &bundle)
        .env_remove("SSL_CERT_DIR")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("1 passed"),
        "child failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
