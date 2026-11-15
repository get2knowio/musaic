//! `serve_loopback` (the desktop shell's entry point) over real sockets.
//!
//! * binds 127.0.0.1 on an ephemeral port, answers, and
//!   `stop()` releases the port and runs the lifespan teardown.
//! * On stop, open SSE streams end cleanly
//!   (chunked terminator, then EOF) so the drain completes at once instead
//!   of waiting out the timeout and cutting them; a stream opened after
//!   shutdown began ends immediately. "Work running" is read from the gate
//!   and the scan service: a held scan is named in the quit prompt.
//! * with `launch_token: true` the launch URL / cookie work over
//!   TCP; without it nothing needs a cookie.

mod common;

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use common::Sandbox;
use musaic_api::{AppState, LoopbackOptions, Network, WorkKind, serve_loopback};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn state_at(cfg: &std::path::Path) -> AppState {
    AppState::builder(cfg)
        .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
        .network(Network::Offline)
        .fingerprinting(common::no_key_fingerprinting())
        .auto_snapshot(false)
        .build()
        .unwrap()
}

async fn open(addr: std::net::SocketAddr, path: &str, extra: &str) -> TcpStream {
    let mut s = TcpStream::connect(addr).await.unwrap();
    let req =
        format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nAccept: text/event-stream\r\n{extra}\r\n");
    s.write_all(req.as_bytes()).await.unwrap();
    s
}

async fn read_until(s: &mut TcpStream, seen: &mut String, needle: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut buf = [0u8; 4096];
    while !seen.contains(needle) {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "timed out for {needle:?}: {seen}");
        if let Ok(r) = tokio::time::timeout(left, s.read(&mut buf)).await {
            let n = r.unwrap();
            assert!(n > 0, "closed before {needle:?}: {seen}");
            seen.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    }
}

async fn read_to_eof(s: &mut TcpStream, seen: &mut String) {
    let mut buf = [0u8; 4096];
    loop {
        let n = tokio::time::timeout(Duration::from_secs(10), s.read(&mut buf))
            .await
            .expect("EOF in time")
            .unwrap();
        if n == 0 {
            return;
        }
        seen.push_str(&String::from_utf8_lossy(&buf[..n]));
    }
}

// Stop ends every SSE stream cleanly, drains at once,
// releases the port and runs the teardown.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_ends_sse_streams_cleanly_and_releases_the_port() {
    let dir = tempfile::tempdir().unwrap();
    let st = state_at(dir.path());
    let server = serve_loopback(
        st.clone(),
        LoopbackOptions {
            drain: Duration::from_secs(30),
            ..Default::default()
        },
        |r, _| r,
    )
    .await
    .unwrap();
    let addr = server.addr;
    assert!(addr.ip().is_loopback() && addr.port() != 0);
    assert!(server.launch.is_none());

    let mut streams = Vec::new();
    for (path, first) in [
        ("/api/library-events/stream", "event: ready"),
        ("/api/fingerprint-pool/stream", "event: "),
        ("/api/ai/jobs/stream", "event: "),
        ("/api/checks/stream", "\r\n\r\n"),
    ] {
        let mut s = open(addr, path, "").await;
        let mut seen = String::new();
        read_until(&mut s, &mut seen, first).await;
        assert!(seen.starts_with("HTTP/1.1 200"), "{path}: {seen}");
        streams.push((path, s, seen));
    }
    let t0 = Instant::now();
    let stop = tokio::spawn(server.stop());
    for (path, mut s, mut seen) in streams {
        read_to_eof(&mut s, &mut seen).await;
        assert!(
            seen.ends_with("0\r\n\r\n"),
            "{path} ended cleanly: {seen:?}"
        );
    }
    let report = stop.await.unwrap();
    assert!(report.drained, "drained without the timeout");
    assert!(report.state_shutdown);
    assert!(t0.elapsed() < Duration::from_secs(10), "{:?}", t0.elapsed());
    assert!(st.is_shutting_down());
    // The port is free again.
    std::net::TcpListener::bind(addr).expect("port released");
}

// A stream that starts while shutting down ends at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_opened_after_shutdown_began_ends_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let st = state_at(dir.path());
    let server = serve_loopback(st.clone(), LoopbackOptions::default(), |r, _| r)
        .await
        .unwrap();
    st.begin_shutdown();
    let mut s = open(
        server.addr,
        "/api/library-events/stream",
        "Connection: close\r\n",
    )
    .await;
    let mut seen = String::new();
    read_to_eof(&mut s, &mut seen).await;
    assert!(seen.starts_with("HTTP/1.1 200"), "{seen}");
    assert!(seen.ends_with("0\r\n\r\n"), "{seen:?}");
    server.stop().await;
}

// The launch flow over TCP when the token is on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loopback_with_launch_token() {
    let dir = tempfile::tempdir().unwrap();
    let server = serve_loopback(
        state_at(dir.path()),
        LoopbackOptions {
            launch_token: true,
            ..Default::default()
        },
        |r, bound| {
            assert!(bound.launch.is_some());
            r
        },
    )
    .await
    .unwrap();
    let launch = server.launch.clone().unwrap();
    let url = launch.launch_url();
    assert!(url.starts_with(&format!(
        "http://127.0.0.1:{}/__musaic/launch?t=",
        server.addr.port()
    )));
    let path = &url[url.find("/__musaic").unwrap()..];

    let r = get(server.addr, "/api/health", None).await;
    assert!(r.starts_with("HTTP/1.1 403"), "{r}");
    let r = get(server.addr, path, None).await;
    assert!(r.starts_with("HTTP/1.1 200"), "{r}");
    let cookie = r
        .lines()
        .find_map(|l| l.strip_prefix("set-cookie: "))
        .and_then(|v| v.split(';').next())
        .unwrap()
        .to_string();
    let r = get(server.addr, "/api/health", Some(&cookie)).await;
    assert!(r.starts_with("HTTP/1.1 200"), "{r}");
    server.stop().await;
}

async fn get(addr: std::net::SocketAddr, path: &str, cookie: Option<&str>) -> String {
    let extra = cookie.map_or_else(String::new, |c| format!("Cookie: {c}\r\n"));
    let mut s = open(addr, path, &format!("Connection: close\r\n{extra}")).await;
    let mut seen = String::new();
    read_to_eof(&mut s, &mut seen).await;
    seen
}

/// Holds the scan after its first album until released.
#[derive(Clone, Default)]
struct Hold(Arc<(Mutex<(usize, bool)>, Condvar)>);

impl Hold {
    fn hook(&self) -> musaic_services::scan::AlbumHook {
        let g = self.clone();
        Arc::new(move |_album, _fresh| {
            let (m, cv) = &*g.0;
            let mut s = m.lock().unwrap();
            s.0 += 1;
            cv.notify_all();
            while !s.1 {
                s = cv.wait(s).unwrap();
            }
        })
    }
    fn reached(&self) -> usize {
        self.0.0.lock().unwrap().0
    }
    fn release(&self) {
        self.0.0.lock().unwrap().1 = true;
        self.0.1.notify_all();
    }
}

// A running scan is "work running" (labelled guard + scan
// service), named in the quit prompt; idle again once it ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn running_scan_asks_before_quit() {
    let sb = Sandbox::new();
    let hold = Hold::default();
    let st = AppState::builder(&sb.cfg)
        .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
        .network(Network::Offline)
        .fingerprinting(common::no_key_fingerprinting())
        .auto_snapshot(false)
        .on_scan_album(hold.hook())
        .build()
        .unwrap();
    assert_eq!(st.work_snapshot().quit_prompt(), None);
    let app = musaic_api::router(st.clone());
    let lib = Sandbox::s(&sb.lib);
    let g = common::call(
        &app,
        "POST",
        "/api/config",
        Some(&serde_json::json!({"library_dir": lib})),
    )
    .await;
    assert_eq!(g.status, 200, "{}", g.text());
    let g = common::call(&app, "POST", "/api/scan/rescan", None).await;
    assert_eq!(g.status, 200, "{}", g.text());
    let deadline = Instant::now() + Duration::from_secs(10);
    while hold.reached() == 0 {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let snap = st.work_snapshot();
    assert!(snap.kinds.contains(&(WorkKind::Scan, 1)), "{snap:?}");
    assert!(snap.scan_running);
    let p = snap.quit_prompt().unwrap();
    assert!(p.contains("a library scan"), "{p}");
    assert_eq!(p.matches("a library scan").count(), 1, "{p}");
    hold.release();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !st.work_snapshot().is_idle() {
        assert!(Instant::now() < deadline, "{:?}", st.work_snapshot());
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    st.shutdown();
}
