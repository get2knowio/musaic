//! SSE over a real socket (127.0.0.1, ephemeral port):
//! the library-events stream stays open, relays bus events, pings, and
//! unregisters its relay connection as soon as the client goes away; a
//! reconnect works. The scan stream replays the running scan's buffer from
//! seq 0 to a late subscriber, and a client that disconnects mid-scan leaves
//! the scan running. An apply whose client disconnects still completes.

mod common;

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use common::{Sandbox, copy_dir};
use musaic_api::{AppState, Network, router};
use musaic_events::Event;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

async fn serve(state: AppState) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router(state)).await.unwrap();
    });
    addr
}

struct Client {
    sock: TcpStream,
    seen: String,
}

impl Client {
    async fn open(
        addr: std::net::SocketAddr,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Client {
        let mut sock = TcpStream::connect(addr).await.unwrap();
        let body = body.unwrap_or("");
        let req = format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAccept: text/event-stream\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        sock.write_all(req.as_bytes()).await.unwrap();
        Client {
            sock,
            seen: String::new(),
        }
    }

    /// Read until `needle` has been seen (in total), or panic after `secs`.
    async fn until(&mut self, needle: &str, secs: u64) {
        let deadline = Instant::now() + Duration::from_secs(secs);
        let mut buf = [0u8; 8192];
        while !self.seen.contains(needle) {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(
                !left.is_zero(),
                "timed out waiting for {needle:?}; got:\n{}",
                self.seen
            );
            match tokio::time::timeout(left, self.sock.read(&mut buf)).await {
                Ok(Ok(0)) => panic!("closed before {needle:?}; got:\n{}", self.seen),
                Ok(Ok(n)) => self.seen.push_str(&String::from_utf8_lossy(&buf[..n])),
                Ok(Err(e)) => panic!("read error {e}"),
                Err(_) => {}
            }
        }
    }
}

async fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !f() {
        assert!(Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn state(sb: &Sandbox) -> AppState {
    AppState::builder(&sb.cfg)
        .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
        .network(Network::Offline)
        .fingerprinting(common::no_key_fingerprinting())
        .ping_interval(Duration::from_millis(300))
        .auto_snapshot(false)
        .build()
        .unwrap()
}

// Library-events stream: relay, teardown, cancellation.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn library_events_stream_relays_pings_and_cleans_up() {
    let sb = Sandbox::new();
    let st = state(&sb);
    let addr = serve(st.clone()).await;
    let base = st.relay().subscriber_count();

    let mut c = Client::open(addr, "GET", "/api/library-events/stream", None).await;
    c.until("event: ready\ndata: {}\n\n", 5).await;
    assert!(
        c.seen
            .contains("content-type: text/event-stream; charset=utf-8"),
        "{}",
        c.seen
    );
    assert!(
        c.seen.contains("cache-control: no-cache, no-transform"),
        "{}",
        c.seen
    );
    assert!(c.seen.contains("x-accel-buffering: no"), "{}", c.seen);
    assert_eq!(st.relay().subscriber_count(), base + 1);

    st.bus().publish(&Event::AlbumDeleted {
        album_id: "abc".into(),
    });
    c.until("event: AlbumDeleted\ndata: {\"album_id\": \"abc\"}\n\n", 5)
        .await;
    c.until(": ping\n\n", 5).await;

    // The client goes away: the relay connection is dropped at once (not at
    // the next ping or event).
    drop(c);
    eventually("relay connection unregistered", || {
        st.relay().subscriber_count() == base
    })
    .await;

    // Reconnect works and gets a fresh ready frame and new events.
    let mut c2 = Client::open(addr, "GET", "/api/library-events/stream", None).await;
    c2.until("event: ready", 5).await;
    st.bus().publish(&Event::BulkBatchCompleted);
    c2.until("event: BulkBatchCompleted\ndata: {}\n\n", 5).await;
    drop(c2);
    eventually("second connection unregistered", || {
        st.relay().subscriber_count() == base
    })
    .await;
}

/// Holds the scan after its third album until released.
#[derive(Clone, Default)]
struct Gate(Arc<(Mutex<(usize, bool)>, Condvar)>);

impl Gate {
    fn hook(&self) -> musaic_services::scan::AlbumHook {
        let g = self.clone();
        Arc::new(move |_album, _fresh| {
            let (m, cv) = &*g.0;
            let mut s = m.lock().unwrap();
            s.0 += 1;
            cv.notify_all();
            if s.0 == 3 {
                while !s.1 {
                    s = cv.wait(s).unwrap();
                }
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

// Scan stream (concurrency, cancellation): replay from seq 0 for a late
// subscriber, disconnect leaves the scan running; SSE framing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scan_stream_replays_buffer_and_survives_disconnect() {
    let sb = Sandbox::new();
    let gate = Gate::default();
    let st = AppState::builder(&sb.cfg)
        .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
        .network(Network::Offline)
        .fingerprinting(common::no_key_fingerprinting())
        .auto_snapshot(false)
        .on_scan_album(gate.hook())
        .build()
        .unwrap();
    let addr = serve(st.clone()).await;
    let lib = Sandbox::s(&sb.lib);
    let mut cfg = Client::open(
        addr,
        "POST",
        "/api/config",
        Some(&serde_json::json!({"library_dir": lib}).to_string()),
    )
    .await;
    cfg.until("\"exists\":true", 5).await;

    let mut a = Client::open(addr, "GET", "/api/scan/stream", None).await;
    a.until("event: start\ndata: {\"library_dir\": ", 5).await;
    a.until("event: progress\ndata: {\"scanned\": 2, \"total\": 13}", 5)
        .await;
    eventually("scan held at album 3", || gate.reached() == 3).await;
    assert!(st.scan().is_running());

    // A late subscriber gets the buffer from seq 0: start first.
    let mut b = Client::open(addr, "GET", "/api/scan/stream", None).await;
    b.until("event: progress\ndata: {\"scanned\": 3, \"total\": 13}", 5)
        .await;
    let body = &b.seen[b.seen.find("\r\n\r\n").unwrap()..];
    let start = body.find("event: start").unwrap();
    let first_album = body.find("event: album").unwrap();
    assert!(start < first_album, "replay starts at seq 0:\n{body}");
    assert_eq!(body.matches("event: start").count(), 1);

    // Client A disconnects mid-scan; the scan keeps running.
    drop(a);
    gate.release();
    b.until("event: done\ndata: {\"count\": 13", 10).await;
    eventually("scan done", || !st.scan().is_running()).await;
    assert_eq!(st.scan().state().scanned, 13);
}

// An apply whose client disconnects after the
// first frame still runs to completion; the database row is updated.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn apply_completes_when_the_client_disconnects() {
    let sb = Sandbox::new();
    let st = state(&sb);
    let addr = serve(st.clone()).await;
    let lib = Sandbox::s(&sb.lib);
    let mut cfg = Client::open(
        addr,
        "POST",
        "/api/config",
        Some(&serde_json::json!({"library_dir": lib}).to_string()),
    )
    .await;
    cfg.until("\"exists\":true", 5).await;
    let mut scan = Client::open(addr, "GET", "/api/scan/stream", None).await;
    scan.until("event: done", 10).await;
    let conn = rusqlite::Connection::open(st.db_path()).unwrap();
    let (id, genre): (String, Option<String>) = conn
        .query_row(
            "SELECT id, genre FROM albums WHERE dir_name = 'Aurora Fields - First Light [FLAC]'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_ne!(genre.as_deref(), Some("Disconnected"));
    let body = serde_json::json!({"suggestions": {"genre": "Disconnected"}}).to_string();
    let mut c = Client::open(
        addr,
        "POST",
        &format!("/api/albums/{id}/apply/stream"),
        Some(&body),
    )
    .await;
    c.until("event: start", 5).await;
    drop(c);
    eventually("apply finished without its client", || {
        conn.query_row("SELECT genre FROM albums WHERE id = ?1", [&id], |r| {
            r.get::<_, Option<String>>(0)
        })
        .unwrap()
        .as_deref()
            == Some("Disconnected")
    })
    .await;
    let _ = copy_dir;
}
