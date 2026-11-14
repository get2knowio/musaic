//! `--verification-memory-secrets=unavailable|fail-set:<key>`
//! on the real binary (127.0.0.1, ephemeral port, temp config dir, offline
//! network; never the OS keychain). Reproduces without a keychain:
//!
//! * `keyring_available: false` in `GET /api/settings`;
//! * a secret write the keyring refuses answers 503 and changes
//!   nothing.
//!
//! And the refusals that keep the seam out of a normal launch: an unknown
//! mode, a key that is not a secret, the space-separated form, and a config
//! dir outside the temp dir / a `work` dir all exit 2 before anything is
//! created. Also, for the server binary: SIGINT with an open SSE
//! stream ends the stream cleanly and exits 0 without waiting for the
//! client.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_musaic-api");

fn base(cfg: &Path) -> Command {
    let mut c = Command::new(BIN);
    for (k, _) in std::env::vars() {
        if k.starts_with("MUSAIC_") || k.starts_with("MEDIA_MANAGER_") {
            c.env_remove(k);
        }
    }
    c.arg("--config-dir")
        .arg(cfg)
        .args(["--host", "127.0.0.1", "--port", "0", "--network", "offline"])
        .stdin(Stdio::null());
    c
}

struct Server {
    child: Child,
    port: u16,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start(cfg: &Path, flag: &str) -> Server {
    let child = base(cfg)
        .arg(flag)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // Owned by the guard at once: a failed start below still reaps it.
    let mut srv = Server { child, port: 0 };
    let mut out = BufReader::new(srv.child.stdout.take().unwrap());
    let t0 = Instant::now();
    loop {
        let mut l = String::new();
        assert!(
            out.read_line(&mut l).unwrap() > 0 && t0.elapsed() < Duration::from_secs(60),
            "server did not start"
        );
        if let Some(rest) = l
            .trim()
            .strip_prefix("[musaic] listening on http://127.0.0.1:")
        {
            srv.port = rest.parse().unwrap();
            // Keep draining stdout so the child never blocks on a full pipe.
            std::thread::spawn(move || {
                let mut sink = String::new();
                let _ = out.read_to_string(&mut sink);
            });
            return srv;
        }
    }
}

/// One request, `Connection: close`; (status, body).
fn http(port: u16, method: &str, path: &str, body: Option<&str>) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let b = body.unwrap_or("");
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}",
        b.len()
    )
    .unwrap();
    let mut raw = String::new();
    s.read_to_string(&mut raw).unwrap();
    let status = raw[9..12].parse().unwrap();
    let body = raw
        .split_once("\r\n\r\n")
        .map(|x| x.1)
        .unwrap_or("")
        .to_string();
    (status, body)
}

fn settings(port: u16) -> serde_json::Value {
    let (s, b) = http(port, "GET", "/api/settings", None);
    assert_eq!(s, 200, "{b}");
    serde_json::from_str(&b).unwrap()
}

// `=unavailable` reproduces the missing
// keyring: the flag is false and a key write is refused with 503.
#[test]
fn unavailable_mode_reports_no_keyring_and_503s_key_writes() {
    let td = tempfile::tempdir().unwrap();
    let cfg = td.path().join("cfg");
    let srv = start(&cfg, "--verification-memory-secrets=unavailable");
    assert_eq!(settings(srv.port)["keyring_available"], false);
    let (s, b) = http(
        srv.port,
        "PATCH",
        "/api/settings",
        Some(r#"{"acoustid_api_key": "seam-value-1"}"#),
    );
    assert_eq!(s, 503, "{b}");
    assert!(
        b.contains("acoustid_api_key") && b.contains("keyring"),
        "{b}"
    );
    assert!(!b.contains("seam-value-1"), "the value never appears: {b}");
    let log = std::fs::read_to_string(cfg.join("server.log")).unwrap();
    assert!(
        log.contains("[WARNING] musaic.main: verification mode: secrets in memory, keyring unavailable (verification)"),
        "{log}"
    );
}

// `=fail-set:<key>` refuses that key only.
#[test]
fn fail_set_mode_refuses_one_key() {
    let td = tempfile::tempdir().unwrap();
    let cfg = td.path().join("cfg");
    let srv = start(
        &cfg,
        "--verification-memory-secrets=fail-set:acoustid_api_key",
    );
    assert_eq!(settings(srv.port)["keyring_available"], true);
    let (s, b) = http(
        srv.port,
        "PATCH",
        "/api/settings",
        Some(r#"{"acoustid_api_key": "seam-value-2"}"#),
    );
    assert_eq!(s, 503, "{b}");
    assert_eq!(settings(srv.port)["acoustid_api_key_set"], false);
    let (s, b) = http(
        srv.port,
        "PATCH",
        "/api/settings",
        Some(r#"{"firecrawl_api_key": "seam-value-3"}"#),
    );
    assert_eq!(s, 200, "{b}");
}

// The seam cannot be switched on by accident: every misuse is a
// usage error (exit 2) and leaves no config dir behind.
#[test]
fn seam_refusals_exit_2_before_creating_anything() {
    let td = tempfile::tempdir().unwrap();
    let cfg = td.path().join("cfg");
    for args in [
        vec!["--verification-memory-secrets=probe"],
        vec!["--verification-memory-secrets=fail-set:not_a_key"],
        vec!["--verification-memory-secrets=fail-set:"],
        // The space form is the bare switch plus a stray argument.
        vec!["--verification-memory-secrets", "unavailable"],
    ] {
        let o = base(&cfg).args(&args).output().unwrap();
        assert_eq!(o.status.code(), Some(2), "{args:?}: {o:?}");
        assert!(!cfg.exists(), "{args:?} created the config dir");
    }
    // A real-looking config dir (not under the temp dir or a `work` dir).
    let home_like = Path::new("/Users/nobody-musaic-seam-test/.musaic");
    let o = base(home_like)
        .arg("--verification-memory-secrets=unavailable")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2), "{o:?}");
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("not under the temp dir"), "{err}");
    assert!(!home_like.exists());
}

// Server binary: Ctrl-C with an open SSE stream: the stream ends
// with the chunked terminator (a clean end, not a reset) and the process
// exits 0 at once, instead of the graceful drain waiting for the client.
#[cfg(unix)]
#[test]
fn sigint_ends_open_sse_streams_cleanly() {
    let td = tempfile::tempdir().unwrap();
    let cfg = td.path().join("cfg");
    let mut srv = start(&cfg, "--verification-memory-secrets");
    let mut s = TcpStream::connect(("127.0.0.1", srv.port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    write!(
        s,
        "GET /api/library-events/stream HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAccept: text/event-stream\r\n\r\n",
        srv.port
    )
    .unwrap();
    let mut seen = Vec::new();
    let mut buf = [0u8; 4096];
    while !String::from_utf8_lossy(&seen).contains("event: ready") {
        let n = s.read(&mut buf).unwrap();
        assert!(n > 0);
        seen.extend_from_slice(&buf[..n]);
    }
    let t0 = Instant::now();
    assert!(
        Command::new("kill")
            .args(["-INT", &srv.child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    loop {
        let n = s.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        seen.extend_from_slice(&buf[..n]);
    }
    let text = String::from_utf8_lossy(&seen);
    assert!(text.ends_with("0\r\n\r\n"), "clean chunked end: {text:?}");
    let status = srv.child.wait().unwrap();
    assert_eq!(status.code(), Some(0));
    assert!(t0.elapsed() < Duration::from_secs(10), "{:?}", t0.elapsed());
}
