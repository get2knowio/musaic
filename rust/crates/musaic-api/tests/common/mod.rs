//! Shared harness: a sandbox (fresh config dir, fresh copy of
//! testdata/synthetic-library/library, a minimal SPA dir) laid out like the one
//! the API goldens were recorded in, an in-process client (tower `oneshot`),
//! and the generator's normalisation.
#![allow(dead_code)]

pub mod ai_wording;
pub mod artist_only;
pub mod conflict_severity;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, Response};
use http_body_util::BodyExt;
use musaic_api::{AppState, Fingerprinting, Network, RouteTag};
use regex::Regex;
use serde_json::Value;
use tower::ServiceExt;

pub fn golden(name: &str) -> Value {
    let p = musaic_testdata::public("golden/api").join(name);
    serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap()
}

pub fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap().flatten() {
        let to = dst.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &to);
        } else {
            std::fs::copy(e.path(), &to).unwrap();
        }
    }
}

/// The generator's starting state, in a temp dir whose path has no symlinks.
pub struct Sandbox {
    _tmp: tempfile::TempDir,
    pub work: PathBuf,
    pub cfg: PathBuf,
    pub lib: PathBuf,
    pub lib_gone: PathBuf,
    pub front: PathBuf,
}

impl Sandbox {
    pub fn new() -> Sandbox {
        let tmp = tempfile::tempdir().unwrap();
        let root = musaic_testdata::canonical(tmp.path());
        let work = root.join("api-golden");
        let cfg = root.join("api-golden-config");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&cfg).unwrap();
        let src = musaic_testdata::synthetic_library();
        let lib = work.join("library");
        copy_dir(&src, &lib);
        let lib_gone = work.join("library-gone");
        std::fs::create_dir_all(&lib_gone).unwrap();
        let front = work.join("frontend");
        std::fs::create_dir_all(front.join("assets")).unwrap();
        std::fs::write(
            front.join("index.html"),
            "<!doctype html><html><body><div id=\"root\"></div></body></html>\n",
        )
        .unwrap();
        std::fs::write(front.join("assets/app.js"), "console.log('spa');\n").unwrap();
        std::fs::write(front.join("favicon.ico"), b"\x00\x00\x01\x00").unwrap();
        std::fs::write(work.join("secret.txt"), "outside the frontend dir\n").unwrap();
        Sandbox {
            _tmp: tmp,
            work,
            cfg,
            lib,
            lib_gone,
            front,
        }
    }

    pub fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    /// The Python app run without lifespan: no subscribers, no auto-snapshot.
    pub fn state_no_lifespan(&self) -> AppState {
        AppState::builder(&self.cfg)
            .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
            .network(Network::Offline)
            .fingerprinting(no_key_fingerprinting())
            .frontend_dir(Some(self.front.clone()))
            .subscribers(false)
            .auto_snapshot(false)
            .build()
            .unwrap()
    }
}

/// The fingerprint pool's state in the Python app's recordings: no AcoustID key
/// was configured, so its `is_available()` said
/// [`PYTHON_NO_KEY`] and the pool reported `unavailable` after every scan
/// (testdata/golden/api/sse_first_frames.json). This build fingerprints
/// without a key (a deliberate change); the harness configures the pool as
/// unavailable with the Python app's reason so the recorded check results
/// (which assume no fingerprints were computed) stay reproducible, and the
/// first pool frame is compared byte for byte. Test configuration, not a
/// behaviour claim about the default.
pub const PYTHON_NO_KEY: &str = "AcoustID API key not set (configure in Settings)";

pub fn no_key_fingerprinting() -> Fingerprinting {
    Fingerprinting::Unavailable(PYTHON_NO_KEY.into())
}

/// A response, read in full.
pub struct Got {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
    pub route: Option<String>,
}

impl Got {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("not JSON ({e}): {}", String::from_utf8_lossy(&self.body)))
    }
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    pub fn ctype(&self) -> &str {
        self.headers.get("content-type").map_or("", String::as_str)
    }
}

pub async fn read(resp: Response<Body>) -> Got {
    let status = resp.status().as_u16();
    let route = resp.extensions().get::<RouteTag>().map(|t| t.0.clone());
    let headers = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let body = resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    Got {
        status,
        headers,
        body,
        route,
    }
}

/// The Host every harness request carries: the server's own
/// default address. The Python app's TestClient sent `Host: testserver`, which
/// the Host check refuses; the recorded cases do not include the Host, so
/// the harness supplies a loopback one instead of the rule being relaxed.
pub const HOST: &str = "127.0.0.1:8765";

/// Add [`HOST`] unless the request names its own.
pub fn with_host(mut req: Request<Body>) -> Request<Body> {
    if !req.headers().contains_key("host") {
        req.headers_mut()
            .insert("host", axum::http::HeaderValue::from_static(HOST));
    }
    req
}

pub async fn send(app: &Router, req: Request<Body>) -> Got {
    read(app.clone().oneshot(with_host(req)).await.unwrap()).await
}

/// A request sent as is (no Host added), for the Host check's own tests.
pub async fn send_raw(app: &Router, req: Request<Body>) -> Got {
    read(app.clone().oneshot(req).await.unwrap()).await
}

pub async fn call(app: &Router, method: &str, path: &str, json: Option<&Value>) -> Got {
    let mut b = Request::builder().method(method).uri(path);
    let body = match json {
        Some(v) => {
            b = b.header("content-type", "application/json");
            Body::from(serde_json::to_vec(v).unwrap())
        }
        None => Body::empty(),
    };
    send(
        app,
        b.body(body)
            .unwrap_or_else(|e| panic!("{method} {path}: {e}")),
    )
    .await
}

pub fn get(path: &str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .header("host", HOST)
        .body(Body::empty())
        .unwrap()
}

/// The normalisation the API goldens were recorded with.
pub struct Norm {
    pub repl: Vec<(String, String)>,
    iso: Regex,
    tag: Regex,
    mtime: Regex,
    ms: Regex,
    size: Regex,
}

impl Norm {
    pub fn new(sb: &Sandbox) -> Norm {
        let mut n = Norm {
            repl: Vec::new(),
            iso: Regex::new(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?(?:Z|\+00:00)").unwrap(),
            tag: Regex::new(r"\d{8}-\d{6}Z").unwrap(),
            mtime: Regex::new(r#""dir_mtime": -?[0-9.e+-]+"#).unwrap(),
            ms: Regex::new(r#""duration_ms": \d+"#).unwrap(),
            size: Regex::new(r#""size_bytes": \d+"#).unwrap(),
        };
        n.add(&Sandbox::s(&sb.lib), "<LIB>");
        n.add(&Sandbox::s(&sb.cfg), "<CFG>");
        n.add(&Sandbox::s(&sb.work), "<WORK>");
        n
    }

    pub fn add(&mut self, from: &str, to: &str) {
        self.repl.push((from.to_string(), to.to_string()));
        self.repl.sort_by_key(|(k, _)| std::cmp::Reverse(k.len()));
    }

    pub fn text(&self, s: &str) -> String {
        let mut s = s.to_string();
        for (k, v) in &self.repl {
            s = musaic_testdata::replace_root(&s, k, v);
        }
        let s = self.iso.replace_all(&s, "<NOW>").into_owned();
        self.tag.replace_all(&s, "<TAG>").into_owned()
    }

    pub fn raw_json_text(&self, s: &str) -> String {
        let s = self
            .mtime
            .replace_all(s, r#""dir_mtime": "<MTIME>""#)
            .into_owned();
        let s = self
            .ms
            .replace_all(&s, r#""duration_ms": "<MS>""#)
            .into_owned();
        let s = self
            .size
            .replace_all(&s, r#""size_bytes": "<SIZE>""#)
            .into_owned();
        self.text(&s)
    }

    pub fn value(&self, v: &Value, key: Option<&str>) -> Value {
        match (key, v) {
            (Some("dir_mtime"), Value::Number(_)) => Value::String("<MTIME>".into()),
            (Some("duration_ms"), Value::Number(n)) if n.is_i64() || n.is_u64() => {
                Value::String("<MS>".into())
            }
            (Some("size_bytes"), Value::Number(n)) if n.is_i64() || n.is_u64() => {
                Value::String("<SIZE>".into())
            }
            (_, Value::String(s)) => Value::String(self.text(s)),
            (_, Value::Array(a)) => Value::Array(a.iter().map(|x| self.value(x, None)).collect()),
            (_, Value::Object(m)) => Value::Object(
                m.iter()
                    .map(|(k, x)| (self.text(k), self.value(x, Some(k))))
                    .collect(),
            ),
            (_, other) => other.clone(),
        }
    }

    /// Undo the tokens in a recorded request.
    pub fn unnorm(&self, s: &str) -> String {
        let mut s = s.to_string();
        for (k, v) in &self.repl {
            s = s.replace(v, k);
        }
        s
    }

    /// [`Norm::unnorm`] for raw JSON text: each path is put in JSON-escaped
    /// (a Windows path's `\` must become `\\` inside a JSON string).
    pub fn unnorm_json_text(&self, s: &str) -> String {
        let mut s = s.to_string();
        for (k, v) in &self.repl {
            let escaped = serde_json::to_string(k).unwrap();
            s = s.replace(v, &escaped[1..escaped.len() - 1]);
        }
        s
    }

    pub fn unnorm_value(&self, v: &Value) -> Value {
        match v {
            Value::String(s) => Value::String(self.unnorm(s)),
            Value::Array(a) => Value::Array(a.iter().map(|x| self.unnorm_value(x)).collect()),
            Value::Object(m) => Value::Object(
                m.iter()
                    .map(|(k, x)| (self.unnorm(k), self.unnorm_value(x)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }
}

/// The bytes FastAPI sends for a 422 whose parsed body is `v`
/// (`{"detail": [...]}` as the goldens record it, keys sorted):
/// Starlette's `json.dumps(ensure_ascii=False, separators=(",", ":"))` with
/// each error item's keys in pydantic's `ErrorDetails` order (`type, loc,
/// msg, input, ctx`). Written independently of `musaic_api::lax`; checked
/// against every 422 of the Python app the goldens hold as bytes
/// (tests/validation_bytes.rs).
pub fn fastapi_422_text(v: &Value) -> String {
    fn plain(v: &Value, out: &mut String) {
        match v {
            Value::Array(xs) => {
                out.push('[');
                for (i, x) in xs.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    plain(x, out);
                }
                out.push(']');
            }
            Value::Object(m) => {
                out.push('{');
                for (i, (k, x)) in m.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(k).unwrap());
                    out.push(':');
                    plain(x, out);
                }
                out.push('}');
            }
            scalar => out.push_str(&serde_json::to_string(scalar).unwrap()),
        }
    }
    const ORDER: [&str; 5] = ["type", "loc", "msg", "input", "ctx"];
    let items = v["detail"].as_array().expect("a 422 detail list");
    let mut out = String::from("{\"detail\":[");
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let m = item.as_object().unwrap();
        assert!(
            m.keys().all(|k| ORDER.contains(&k.as_str())),
            "unexpected key in {item}"
        );
        out.push('{');
        let mut first = true;
        for k in ORDER {
            if let Some(x) = m.get(k) {
                if !first {
                    out.push(',');
                }
                first = false;
                out.push_str(&format!("\"{k}\":"));
                plain(x, &mut out);
            }
        }
        out.push('}');
    }
    out.push_str("]}");
    out
}
