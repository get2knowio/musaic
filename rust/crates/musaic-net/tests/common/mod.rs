//! Shared test support: virtual per-thread time, and a scripted HTTP server
//! on 127.0.0.1 that speaks the step vocabulary of the fault golden
//! (testdata/golden/net/faults.json).
#![allow(dead_code)]

use std::cell::{Cell, RefCell};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use base64::Engine as _;
use musaic_net::{Clock, Request, Response, Transport, TransportError};
use serde_json::Value;

/// A golden under testdata/golden/, merged with its private half when that
/// is available.
pub fn golden(rel: &str) -> Value {
    musaic_testdata::golden(rel)
}

/// A public JSON file under testdata/.
pub fn read_json(rel: &str) -> Value {
    musaic_testdata::public_json(rel)
}

/// A private JSON file (the calling test is gated on it).
pub fn read_private_json(rel: &str) -> Value {
    musaic_testdata::read_json(musaic_testdata::private_required(rel))
}

/// A value recorded from MusicBrainz / AcoustID / the Cover Art Archive for
/// the private tier (a real release-group id, a real title, a recorded
/// redirect target). Such values live only in the private test data
/// (`expected/musaic-net.json`), never in this repository; only tests gated
/// on private data call this.
pub fn rec(key: &str) -> &'static str {
    static V: OnceLock<Value> = OnceLock::new();
    V.get_or_init(|| read_private_json("expected/musaic-net.json"))[key]
        .as_str()
        .unwrap_or_else(|| panic!("expected/musaic-net.json: no string {key:?}"))
}

/// The recorded MusicBrainz / AcoustID / CAA traffic (private).
pub fn replay_dir() -> PathBuf {
    musaic_testdata::private_required("replay")
}

/// One recorded answer by replay key (private).
pub fn replay_record(key: &str) -> Value {
    read_private_json(&format!("replay/{key}.json"))
}

thread_local! {
    static NOW: Cell<Duration> = const { Cell::new(Duration::ZERO) };
    static SLEEPS: RefCell<Vec<Duration>> = const { RefCell::new(Vec::new()) };
}

/// Virtual time, one timeline per thread: `sleep` advances only the calling
/// thread's clock. Threads started together all begin at zero, so a send
/// time is "the slot this thread was given", whatever the interleaving.
#[derive(Debug, Clone, Copy, Default)]
pub struct ThreadClock;

impl ThreadClock {
    pub fn reset() {
        NOW.with(|n| n.set(Duration::ZERO));
        SLEEPS.with(|s| s.borrow_mut().clear());
    }

    pub fn sleeps() -> Vec<Duration> {
        SLEEPS.with(|s| s.borrow().clone())
    }
}

impl Clock for ThreadClock {
    fn now(&self) -> Duration {
        NOW.with(|n| n.get())
    }

    fn sleep(&self, d: Duration) {
        NOW.with(|n| n.set(n.get() + d));
        SLEEPS.with(|s| s.borrow_mut().push(d));
    }
}

/// Every request gets this timeout instead of the client's (the golden's
/// `short_timeout_s`), so a slow-server test takes about a second.
pub struct Shorten<T>(pub T, pub Duration);

impl<T: Transport> Transport for Shorten<T> {
    fn send(&self, request: &Request) -> Result<Response, TransportError> {
        let mut r = request.clone();
        r.timeout = Some(self.1);
        self.0.send(&r)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub method: String,
    pub path: String,
    pub user_agent: String,
    pub body: Vec<u8>,
}

pub struct FaultServer {
    pub addr: SocketAddr,
    seen: Arc<Mutex<Vec<Seen>>>,
    stop: Arc<AtomicBool>,
}

impl FaultServer {
    /// Serve `script` (the steps of one scenario). After the last step the
    /// last repeats when `repeat_last`, else 599.
    pub fn start(script: Vec<Value>, repeat_last: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let next = Arc::new(AtomicUsize::new(0));
        let script = Arc::new(script);
        {
            let (seen, stop) = (seen.clone(), stop.clone());
            std::thread::spawn(move || {
                for conn in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        return;
                    }
                    let Ok(conn) = conn else { continue };
                    let (seen, next, script) = (seen.clone(), next.clone(), script.clone());
                    std::thread::spawn(move || {
                        let _ = handle(conn, addr, &seen, &next, &script, repeat_last);
                    });
                }
            });
        }
        Self { addr, seen, stop }
    }

    pub fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.addr.port())
    }

    pub fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

impl Drop for FaultServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr); // wake the accept loop
    }
}

/// A port nothing listens on.
pub fn closed_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

pub fn body_bytes(body: &Value) -> Vec<u8> {
    if body.is_null() {
        return vec![];
    }
    if let Some(key) = body["replay"].as_str() {
        let rec = replay_record(key);
        let raw = base64::engine::general_purpose::STANDARD
            .decode(rec["content_b64"].as_str().unwrap_or(""))
            .unwrap();
        return match body["truncate"].as_u64() {
            Some(n) => raw[..n as usize].to_vec(),
            None => raw,
        };
    }
    if let Some(t) = body["text"].as_str() {
        return t.as_bytes().to_vec();
    }
    if let Some(f) = body["fill"].as_str() {
        return f
            .repeat(body["len"].as_u64().unwrap() as usize)
            .into_bytes();
    }
    panic!("unknown body {body}")
}

fn handle(
    mut conn: TcpStream,
    addr: SocketAddr,
    seen: &Mutex<Vec<Seen>>,
    next: &AtomicUsize,
    script: &[Value],
    repeat_last: bool,
) -> std::io::Result<()> {
    let mut data = Vec::new();
    let mut buf = [0u8; 65536];
    let head_end = loop {
        let n = conn.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        data.extend_from_slice(&buf[..n]);
        if let Some(i) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
    };
    let head = String::from_utf8_lossy(&data[..head_end]).into_owned();
    let mut body = data[head_end + 4..].to_vec();
    let mut lines = head.split("\r\n");
    let first: Vec<&str> = lines.next().unwrap_or("").splitn(3, ' ').collect();
    let (method, path) = (first[0].to_string(), first[1].to_string());
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let get = |k: &str| headers.iter().find(|(h, _)| h == k).map(|(_, v)| v.clone());
    let need: usize = get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    while body.len() < need {
        let n = conn.read(&mut buf)?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&buf[..n]);
    }
    seen.lock().unwrap().push(Seen {
        method: method.clone(),
        path: path.split('?').next().unwrap_or("").to_string(),
        user_agent: get("user-agent").unwrap_or_default(),
        body,
    });
    let i = next.fetch_add(1, Ordering::SeqCst);
    let fallback = serde_json::json!({"status": 599});
    let step = script
        .get(i)
        .or(if repeat_last { script.last() } else { None })
        .unwrap_or(&fallback);
    if let Some(d) = step["delay_s"].as_f64() {
        std::thread::sleep(Duration::from_secs_f64(d));
    }
    if step["action"] == "reset" {
        socket2::SockRef::from(&conn).set_linger(Some(Duration::ZERO))?;
        drop(conn);
        return Ok(());
    }
    let payload = body_bytes(&step["body"]);
    let status = step["status"].as_u64().unwrap_or(200);
    let origin = format!("http://127.0.0.1:{}", addr.port());
    let alt = format!("http://localhost:{}", addr.port());
    let mut out = format!("HTTP/1.1 {status} X\r\n");
    let mut has_len = false;
    if let Some(h) = step["headers"].as_object() {
        for (k, v) in h {
            has_len |= k.eq_ignore_ascii_case("content-length");
            let v = v
                .as_str()
                .unwrap()
                .replace("{origin}", &origin)
                .replace("{alt_origin}", &alt);
            out.push_str(&format!("{k}: {v}\r\n"));
        }
    }
    if !has_len {
        let extra = if step["action"] == "short_body" {
            100
        } else {
            0
        };
        out.push_str(&format!("Content-Length: {}\r\n", payload.len() + extra));
    }
    out.push_str("Connection: close\r\n\r\n");
    conn.write_all(out.as_bytes())?;
    if method != "HEAD" {
        conn.write_all(&payload)?;
    }
    conn.flush()
}
