//! Deliberate changes from the Python app: SPA path containment, the Host
//! and Origin check, the restore gate, upload caps, and keyring failures.
//!
//! Evidence: the Python app's recorded answers in testdata/golden/api/cases.json
//! and testdata/golden/api/deliberate.json, replayed against the router;
//! every difference is an entry of an explicit delta list naming the change,
//! and an entry that does not occur fails the test. Values that have no Python
//! counterpart (the 403 / 409 / 413 / 503 details) are this build's own
//! constants, asserted by reference to the code that defines them.

mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, Version};
use common::{Got, Norm, Sandbox, call, golden, send, send_raw};
use musaic_api::gate::{RESTORE_REFUSED, WORK_REFUSED};
use musaic_api::host_check::{HOST_MISSING, HOST_REFUSED, ORIGIN_REFUSED};
use musaic_api::{AllowedHosts, AppState, Network, router};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const MIB: usize = 1024 * 1024;

fn case<'a>(g: &'a Value, label: &str) -> &'a Value {
    g["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["label"] == label)
        .unwrap_or_else(|| panic!("no case {label}"))
}

/// The upload bytes of a recorded case, as the golden's generator built
/// them; checked against the recorded sha256.
fn upload_bytes(label: &str) -> Vec<u8> {
    match label {
        "restore tiny" => b"tiny".to_vec(),
        "restore not sqlite" => vec![b'x'; 100],
        "cover just over the Python app's limit" => vec![0; 25 * MIB + 1],
        "cover over the body cap" | "cover over the body cap not an image" => vec![0; 26 * MIB + 1],
        "restore 4 KiB not sqlite" => vec![b'x'; 4096],
        other => panic!("no upload bytes for {other}"),
    }
}

fn sha(b: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

/// The recorded request of `c` (headers included, unless `host` overrides
/// the Host), as the generator sent it.
fn request(c: &Value, norm: &Norm, host: Option<&str>) -> Request<Body> {
    let label = c["label"].as_str().unwrap();
    let req = &c["request"];
    let method = req["method"].as_str().unwrap();
    let mut path = norm.unnorm(req["path"].as_str().unwrap());
    if let Some(p) = req.get("params").and_then(Value::as_object) {
        let q: Vec<String> = p
            .iter()
            .map(|(k, v)| format!("{k}={}", norm.unnorm(v.as_str().unwrap())))
            .collect();
        path = format!("{path}?{}", q.join("&"));
    }
    // Album ids the sandbox has not learned (no scan ran) stay unknown ids.
    let path = regex::Regex::new(r"<ID:[^>]*>")
        .unwrap()
        .replace_all(&path, "0000000000000000")
        .into_owned();
    let mut b = Request::builder().method(method).uri(&path);
    if let Some(h) = req.get("headers").and_then(Value::as_object) {
        for (k, v) in h {
            if k == "host" && host.is_some() {
                continue;
            }
            b = b.header(k.as_str(), v.as_str().unwrap());
        }
    }
    if let Some(h) = host {
        b = b.header("host", h);
    }
    let body = if let Some(files) = req.get("files").and_then(Value::as_object) {
        let (name, f) = files.iter().next().unwrap();
        let data = upload_bytes(label);
        assert_eq!(sha(&data), f["sha256"].as_str().unwrap(), "{label}: upload");
        let boundary = "musaicgoldenboundary";
        let mut body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"{}\"\r\nContent-Type: {}\r\n\r\n",
            f["filename"].as_str().unwrap(),
            f["content_type"].as_str().unwrap()
        )
        .into_bytes();
        body.extend_from_slice(&data);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        b = b.header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        );
        Body::from(body)
    } else if let Some(raw) = req.get("raw_body") {
        b = b.header("content-type", "application/json");
        Body::from(norm.unnorm(raw.as_str().unwrap()))
    } else if let Some(j) = req.get("json") {
        b = b.header("content-type", "application/json");
        Body::from(serde_json::to_vec(&norm.unnorm_value(j)).unwrap())
    } else {
        Body::empty()
    };
    b.body(body).unwrap()
}

/// Status plus the body the generator records (json / text / sse).
fn rec(got: &Got, norm: &Norm) -> Value {
    let ct = got.ctype();
    let mut r = json!({"status": got.status});
    if ct.starts_with("application/json") {
        r["json"] = norm.value(&got.json(), None);
    } else if ct.starts_with("text/event-stream") {
        r["sse"] = json!(norm.raw_json_text(&got.text()));
    } else if ct.starts_with("text/") {
        r["text"] = json!(norm.text(&got.text()));
    }
    r
}

/// The Python app's recorded status + body.
fn python(c: &Value) -> Value {
    let resp = &c["response"];
    let mut r = json!({"status": resp["status"]});
    for k in ["json", "text", "sse"] {
        if let Some(v) = resp.get(k) {
            r[k] = v.clone();
        }
    }
    r
}

/// A 200 settings body gains
/// keyring_available, and its sibling keys_pending_removal (false: nothing
/// is left to scrub in these states; `musaic_api::keyscrub`).
fn with_keyring(mut v: Value, available: bool) -> Value {
    if v["status"] == 200 && v["json"].get("ai_max_parallel").is_some() {
        v["json"]["keyring_available"] = json!(available);
        v["json"]["keys_pending_removal"] = json!(false);
    }
    v
}

fn refused(status: u16, detail: &str) -> Value {
    json!({"status": status, "json": {"detail": detail}})
}

fn plain_state(sb: &Sandbox) -> AppState {
    sb.state_no_lifespan()
}

// ------------------------------------------------- Host / Origin check

// Every request of testdata/golden/api/cases.json (all
// 130: API, SSE, SPA, unknown paths, 405s, stubs), sent with a foreign Host,
// is refused with 403 in the Python app's error shape. The Python app answered each
// one as recorded; the delta is the whole set.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_recorded_route_refuses_a_foreign_host() {
    let g = golden("cases.json");
    let sb = Sandbox::new();
    let app = router(plain_state(&sb));
    let norm = Norm::new(&sb);
    let cases = g["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 130);
    for c in cases {
        let label = c["label"].as_str().unwrap();
        assert_ne!(
            c["response"]["status"], 403,
            "{label}: the Python app never 403s"
        );
        let got = send_raw(&app, request(c, &norm, Some("evil.example"))).await;
        assert_eq!(rec(&got, &norm), refused(403, HOST_REFUSED), "{label}");
    }
    // Nothing reached a handler: the library was never configured.
    let r = call(&app, "GET", "/api/config", None).await;
    assert_eq!(r.json()["library_dir"], Value::Null);
}

// Testdata/golden/api/deliberate.json Host / Origin
// cases. The Python app's answer, except the enumerated deltas.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_and_origin_cases() {
    let g = golden("deliberate.json");
    let sb = Sandbox::new();
    let app = router(plain_state(&sb));
    let norm = Norm::new(&sb);
    let deltas: BTreeMap<&str, Value> = [
        ("host evil health", refused(403, HOST_REFUSED)),
        ("host evil settings", refused(403, HOST_REFUSED)),
        ("host evil spa", refused(403, HOST_REFUSED)),
        ("host unspecified", refused(403, HOST_REFUSED)),
        ("origin evil rescan", refused(403, ORIGIN_REFUSED)),
        ("origin null settings", refused(403, ORIGIN_REFUSED)),
    ]
    .into_iter()
    .collect();
    let same = [
        "host localhost",
        "host loopback v4",
        "host loopback v6",
        // The Vite dev server (changeOrigin: Host 127.0.0.1:8765, Origin
        // http://localhost:5173) and the app's own origin pass.
        "origin vite dev rescan",
        "origin same app rescan",
    ];
    let mut seen = 0;
    for c in g["cases"].as_array().unwrap() {
        let label = c["label"].as_str().unwrap();
        let want = if let Some(d) = deltas.get(label) {
            d.clone()
        } else if same.contains(&label) {
            python(c)
        } else {
            continue;
        };
        seen += 1;
        let got = send_raw(&app, request(c, &norm, None)).await;
        assert_eq!(rec(&got, &norm), want, "{label}");
    }
    assert_eq!(seen, deltas.len() + same.len());
}

// A configured extra host name (non-loopback bind /
// MUSAIC_ALLOWED_HOSTS) gets the Python app's answer, Host and Origin alike;
// other names are still refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configured_host_is_allowed() {
    let g = golden("deliberate.json");
    let sb = Sandbox::new();
    let st = AppState::builder(&sb.cfg)
        .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
        .network(Network::Offline)
        .fingerprinting(common::no_key_fingerprinting())
        .frontend_dir(Some(sb.front.clone()))
        .subscribers(false)
        .auto_snapshot(false)
        .allowed_hosts(AllowedHosts::from_names(["musaic.lan"]))
        .build()
        .unwrap();
    let app = router(st);
    let norm = Norm::new(&sb);
    let c = case(&g, "host evil health");
    let got = send_raw(&app, request(c, &norm, Some("Musaic.LAN:8765"))).await;
    assert_eq!(rec(&got, &norm), python(c));
    let c = case(&g, "origin evil rescan");
    let mut req = request(c, &norm, Some("musaic.lan:8765"));
    req.headers_mut()
        .insert("origin", "http://musaic.lan:8765".parse().unwrap());
    let got = send_raw(&app, req).await;
    assert_eq!(rec(&got, &norm), python(c));
    let got = send_raw(&app, request(c, &norm, None)).await;
    assert_eq!(rec(&got, &norm), refused(403, ORIGIN_REFUSED));
}

// The Host forms that have no recording from the Python app. A request with no
// Host at all (in-process, as HTTP/1.0), two Host headers, two Origin
// headers, and an absolute-form target naming a foreign authority are
// refused; an absolute-form target naming loopback passes (the Python
// app's answer of "host loopback v4").
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_duplicate_and_absolute_form() {
    let g = golden("deliberate.json");
    let sb = Sandbox::new();
    let app = router(plain_state(&sb));
    let norm = Norm::new(&sb);
    let ok = python(case(&g, "host loopback v4"));

    let req = Request::builder()
        .uri("/api/health")
        .version(Version::HTTP_10)
        .body(Body::empty())
        .unwrap();
    let got = send_raw(&app, req).await;
    assert_eq!(rec(&got, &norm), refused(403, HOST_MISSING));

    let req = Request::builder()
        .uri("/api/health")
        .header("host", "127.0.0.1:8765")
        .header("host", "evil.example")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        rec(&send_raw(&app, req).await, &norm),
        refused(403, HOST_REFUSED)
    );

    let req = Request::builder()
        .uri("/api/health")
        .header("host", "127.0.0.1:8765")
        .header("origin", "http://localhost:5173")
        .header("origin", "http://evil.example")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        rec(&send_raw(&app, req).await, &norm),
        refused(403, ORIGIN_REFUSED)
    );

    let req = Request::builder()
        .uri("http://evil.example/api/health")
        .header("host", "127.0.0.1:8765")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        rec(&send_raw(&app, req).await, &norm),
        refused(403, HOST_REFUSED)
    );

    let req = Request::builder()
        .uri("http://127.0.0.1:8765/api/health")
        .body(Body::empty())
        .unwrap();
    assert_eq!(rec(&send_raw(&app, req).await, &norm), ok);
}

async fn raw_exchange(addr: std::net::SocketAddr, raw: &str) -> String {
    let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
    sock.write_all(raw.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), sock.read_to_end(&mut out)).await;
    String::from_utf8_lossy(&out).into_owned()
}

// The Host check over a real socket (127.0.0.1, ephemeral port): HTTP/1.0 and
// HTTP/1.1 without Host are refused with the 403; with a loopback Host the
// Python app's answer ("host loopback v4") comes back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_host_over_a_socket() {
    let g = golden("deliberate.json");
    let ok = python(case(&g, "host loopback v4"));
    let sb = Sandbox::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let st = plain_state(&sb);
    tokio::spawn(async move {
        axum::serve(listener, router(st)).await.unwrap();
    });
    let want_403 = format!("{{\"detail\":\"{HOST_MISSING}\"}}");
    let r = raw_exchange(addr, "GET /api/health HTTP/1.0\r\n\r\n").await;
    assert!(r.starts_with("HTTP/1.0 403"), "{r}");
    assert!(r.ends_with(&want_403), "{r}");
    let r = raw_exchange(
        addr,
        "GET /api/health HTTP/1.1\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(r.starts_with("HTTP/1.1 403"), "{r}");
    assert!(r.ends_with(&want_403), "{r}");
    let r = raw_exchange(
        addr,
        "GET /api/health HTTP/1.1\r\nHost: localhost:8765\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(r.starts_with("HTTP/1.1 200"), "{r}");
    assert!(r.ends_with(&ok["json"].to_string()), "{r}");
}

// ------------------------------------------------- SPA path containment

// Nothing outside the frontend dir is served through
// the SPA fallback, whatever the spelling. The Python app served
// `<WORK>/secret.txt` for "spa traversal encoded" and "spa traversal double
// encoded" (cases.json); here every variant answers the SPA's index.html
// (the recorded "spa client route" answer) or the recorded 404 of
// "spa asset missing" under /assets.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spa_traversal_variants() {
    let g = golden("cases.json");
    let sb = Sandbox::new();
    let app = router(plain_state(&sb));
    let norm = Norm::new(&sb);
    for l in ["spa traversal encoded", "spa traversal double encoded"] {
        assert_eq!(
            case(&g, l)["response"]["text"],
            json!("outside the frontend dir\n"),
            "{l}: the Python app leaked"
        );
    }
    let index = python(case(&g, "spa client route"));
    let missing_asset = python(case(&g, "spa asset missing"));
    let secret = Sandbox::s(&sb.work.join("secret.txt"));
    let abs_enc = secret.replace('/', "%2F");
    let variants: Vec<(String, &Value)> = vec![
        // encoded
        ("/..%2Fsecret.txt".into(), &index),
        ("/%2e%2e/secret.txt".into(), &index),
        ("/%2E%2E%2Fsecret.txt".into(), &index),
        ("/%2e%2e%2f%2e%2e%2fapi-golden%2fsecret.txt".into(), &index),
        // double encoded
        ("/%252e%252e%252fsecret.txt".into(), &index),
        ("/..%252Fsecret.txt".into(), &index),
        // backslash
        ("/..%5Csecret.txt".into(), &index),
        ("/..%5c..%5capi-golden%5csecret.txt".into(), &index),
        ("/%2e%2e%5csecret.txt".into(), &index),
        // absolute paths
        (format!("/{abs_enc}"), &index),
        (format!("/%2F{}", abs_enc.trim_start_matches("%2F")), &index),
        ("/%2Fetc%2Fpasswd".into(), &index),
        ("/C:%5CWindows%5Cwin.ini".into(), &index),
        // under /assets
        ("/assets/..%2F..%2Fsecret.txt".into(), &missing_asset),
        (
            "/assets/%2e%2e%2f%2e%2e%2fsecret.txt".into(),
            &missing_asset,
        ),
        ("/assets/..%5C..%5Csecret.txt".into(), &missing_asset),
        (format!("/assets/{abs_enc}"), &missing_asset),
    ];
    for (p, want) in variants {
        let got = call(&app, "GET", &p, None).await;
        assert!(!got.text().contains("outside the frontend dir"), "{p}");
        assert_eq!(&rec(&got, &norm), want, "{p}");
    }
}

// ---------------------------------------------------------- upload caps

// Cover uploads, default cap (25 MiB + 1 MiB of body).
// An image just over the Python app's 25 MiB limit still gets the Python app's own
// 413; a non-image over the body cap still gets the Python app's 400 (MIME is
// checked first); an image over the body cap gets a 413 of the same shape
// whose detail cannot carry the size (the body was not read past the cap).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cover_upload_cap() {
    let g = golden("deliberate.json");
    let sb = Sandbox::new();
    let app = router(plain_state(&sb));
    let norm = Norm::new(&sb);
    assert_eq!(musaic_api::COVER_BODY_LIMIT, 26 * MIB);
    for l in [
        "cover just over the Python app's limit",
        "cover over the body cap not an image",
    ] {
        let c = case(&g, l);
        let got = send(&app, request(c, &norm, None)).await;
        assert_eq!(rec(&got, &norm), python(c), "{l}");
    }
    let c = case(&g, "cover over the body cap");
    assert_eq!(
        c["response"]["json"]["detail"],
        "Cover too large (27262977 bytes). Max accepted is 25 MB."
    );
    let got = send(&app, request(c, &norm, None)).await;
    let delta = refused(
        413,
        &format!(
            "Cover too large (more than {} bytes in the request). Max accepted is 25 MB.",
            26 * MIB
        ),
    );
    assert_eq!(rec(&got, &norm), delta);
}

// The cover cap is the AppState's; a test-sized cap gives the
// same 413 shape, a body under it reaches the Python app's next check.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cover_cap_is_configurable() {
    let sb = Sandbox::new();
    let st = AppState::builder(&sb.cfg)
        .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
        .subscribers(false)
        .auto_snapshot(false)
        .cover_body_limit(4096)
        .build()
        .unwrap();
    let app = router(st);
    let up = |n: usize| {
        let b = "bnd";
        let mut body = format!(
            "--{b}\r\nContent-Disposition: form-data; name=\"cover\"; filename=\"c\"\r\nContent-Type: image/png\r\n\r\n"
        )
        .into_bytes();
        body.extend(vec![0u8; n]);
        body.extend_from_slice(format!("\r\n--{b}--\r\n").as_bytes());
        Request::builder()
            .method("POST")
            .uri("/api/albums/abc/cover/upload")
            .header("content-type", format!("multipart/form-data; boundary={b}"))
            .body(Body::from(body))
            .unwrap()
    };
    let r = send(&app, up(8192)).await;
    assert_eq!(
        (r.status, r.json()),
        (
            413,
            json!({"detail": "Cover too large (more than 4096 bytes in the request). Max accepted is 25 MB."})
        )
    );
    // Under the cap: the album lookup's 409 (no library configured; the
    // Python app's "cover unconfigured"-style answer for an apply).
    let r = send(&app, up(1024)).await;
    assert_ne!(r.status, 413);
}

fn tmp_leftovers(sb: &Sandbox) -> Vec<String> {
    let d = sb.cfg.join("tmp");
    if !d.exists() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut stack = vec![d];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            if e.path().is_dir() {
                stack.push(e.path());
            }
            out.push(e.path().to_string_lossy().into_owned());
        }
    }
    out
}

// A restore upload over the cap answers 413 and leaves
// no partial file; the Python app read the same 4 KiB upload and answered 400
// ("restore 4 KiB not sqlite"), which a cap above the size still gives.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restore_upload_cap() {
    let g = golden("deliberate.json");
    let c = case(&g, "restore 4 KiB not sqlite");
    assert_eq!(musaic_api::RESTORE_UPLOAD_LIMIT, 2 * 1024 * 1024 * 1024);
    for (limit, want) in [
        (
            1024u64,
            refused(
                413,
                "Database upload too large. Max accepted is 1024 bytes.",
            ),
        ),
        (8192, python(c)),
    ] {
        let sb = Sandbox::new();
        let st = AppState::builder(&sb.cfg)
            .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
            .subscribers(false)
            .auto_snapshot(false)
            .restore_upload_limit(limit)
            .build()
            .unwrap();
        let before = std::fs::read(st.db_path()).unwrap();
        let app = router(st.clone());
        let norm = Norm::new(&sb);
        let got = send(&app, request(c, &norm, None)).await;
        assert_eq!(rec(&got, &norm), want, "limit {limit}");
        assert_eq!(tmp_leftovers(&sb), Vec::<String>::new(), "limit {limit}");
        assert_eq!(std::fs::read(st.db_path()).unwrap(), before);
    }
}

// ---------------------------------------------------- keyring failures

fn state_with(sb: &Sandbox, store: Arc<musaic_secrets::MemoryStore>) -> AppState {
    AppState::builder(&sb.cfg)
        .secrets(store)
        .network(Network::Offline)
        .fingerprinting(common::no_key_fingerprinting())
        .subscribers(false)
        .auto_snapshot(false)
        .build()
        .unwrap()
}

// The keyring refuses the secret of a PATCH that also
// sets ai_max_parallel. The Python app (no keyring) saved both
// ("settings max_parallel and key" 200, "settings after" shows both).
// Delta: 503 naming the keyring, and nothing persisted: "settings after"
// equals the Python app's "settings before" (+ keyring_available).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keyring_failure_is_503_and_saves_nothing() {
    let g = golden("deliberate.json");
    let before = case(&g, "settings before");
    let patch = case(&g, "settings max_parallel and key");
    let after = case(&g, "settings after");
    assert_eq!(python(patch)["json"]["ai_max_parallel"], 5);
    assert_eq!(python(after)["json"]["acoustid_api_key_set"], true);
    let detail = |key: &str, why: &str| {
        format!("Could not save {key} to the OS keyring ({why}). No settings were changed.")
    };

    // A keyring that refuses the write.
    let sb = Sandbox::new();
    let store = Arc::new(musaic_secrets::MemoryStore::new());
    store.fail_set_for("acoustid_api_key");
    let app = router(state_with(&sb, store.clone()));
    let norm = Norm::new(&sb);
    let got = send(&app, request(before, &norm, None)).await;
    assert_eq!(rec(&got, &norm), with_keyring(python(before), true));
    let got = send(&app, request(patch, &norm, None)).await;
    assert_eq!(
        rec(&got, &norm),
        refused(
            503,
            &detail(
                "acoustid_api_key",
                "the OS keyring failed: injected failure for acoustid_api_key"
            )
        )
    );
    assert!(!got.text().contains("golden-key-2"));
    let got = send(&app, request(after, &norm, None)).await;
    assert_eq!(rec(&got, &norm), with_keyring(python(before), true));
    assert!(store.entries().is_empty());

    // A keyring that cannot be reached at all.
    let sb = Sandbox::new();
    let store = Arc::new(musaic_secrets::MemoryStore::unavailable());
    let app = router(state_with(&sb, store));
    let norm = Norm::new(&sb);
    let got = send(&app, request(patch, &norm, None)).await;
    assert_eq!(
        rec(&got, &norm),
        refused(
            503,
            &detail(
                "acoustid_api_key",
                "the OS keyring is unavailable: memory store switched off"
            )
        )
    );
    let got = send(&app, request(after, &norm, None)).await;
    assert_eq!(rec(&got, &norm), with_keyring(python(before), false));
}

// A second secret failing undoes the first secret of the same
// PATCH (its previous value is put back), and a read-back mismatch is a
// keyring failure too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn earlier_secret_writes_are_undone() {
    let sb = Sandbox::new();
    let store = Arc::new(musaic_secrets::MemoryStore::new());
    let app = router(state_with(&sb, store.clone()));
    let r = call(
        &app,
        "PATCH",
        "/api/settings",
        Some(&json!({"ai_api_key": "old-ai"})),
    )
    .await;
    assert_eq!(r.status, 200);
    store.fail_set_for("firecrawl_api_key");
    let r = call(
        &app,
        "PATCH",
        "/api/settings",
        Some(&json!({"ai_api_key": "new-ai", "acoustid_api_key": "new-ac", "firecrawl_api_key": "fc", "ai_model_id": "m"})),
    )
    .await;
    assert_eq!(r.status, 503, "{}", r.text());
    assert!(r.json()["detail"].as_str().unwrap().contains("OS keyring"));
    let e = store.entries();
    assert_eq!(e.get("ai_api_key").map(String::as_str), Some("old-ai"));
    assert_eq!(e.get("acoustid_api_key"), None);
    let s = call(&app, "GET", "/api/settings", None).await.json();
    assert_eq!(s["ai_model_id"], "claude-sonnet-4-6");

    store.corrupt_set_for("acoustid_api_key");
    let r = call(
        &app,
        "PATCH",
        "/api/settings",
        Some(&json!({"ai_max_parallel": 6, "acoustid_api_key": "zzz"})),
    )
    .await;
    assert_eq!(r.status, 503);
    assert_eq!(
        call(&app, "GET", "/api/settings", None).await.json()["ai_max_parallel"],
        json!(1)
    );
}

// The keyring rule keeps the Python app's order otherwise (musaic/api/settings.py:66-130
// writes each field as it goes): a later field refused with 400 leaves the
// earlier fields, the secret included, saved.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn python_order_kept_on_a_later_400() {
    let g = golden("cases.json");
    let sb = Sandbox::new();
    let store = Arc::new(musaic_secrets::MemoryStore::new());
    let app = router(state_with(&sb, store.clone()));
    let refused_101 = python(case(&g, "settings retention 101"));
    let r = call(
        &app,
        "PATCH",
        "/api/settings",
        Some(&json!({"ai_max_parallel": 4, "acoustid_api_key": "k1", "snapshot_retention_count": 101})),
    )
    .await;
    assert_eq!(json!({"status": r.status, "json": r.json()}), refused_101);
    let s = call(&app, "GET", "/api/settings", None).await.json();
    assert_eq!(s["ai_max_parallel"], 4);
    assert_eq!(s["acoustid_api_key_set"], true);
    assert_eq!(store.entries()["acoustid_api_key"], "k1");
}

// --------------------------------------------------------- restore gate

/// Blocks a hook until released; counts entries.
#[derive(Clone, Default)]
struct Hold(Arc<(Mutex<(usize, bool)>, Condvar)>);

impl Hold {
    /// Hold the `at`-th call (1-based); earlier and later calls pass.
    fn enter(&self, at: usize) {
        let (m, cv) = &*self.0;
        let mut s = m.lock().unwrap();
        s.0 += 1;
        cv.notify_all();
        if s.0 == at {
            while !s.1 {
                s = cv.wait(s).unwrap();
            }
        }
    }
    fn reached(&self) -> usize {
        self.0.0.lock().unwrap().0
    }
    fn release(&self) {
        let mut s = self.0.0.lock().unwrap_or_else(|p| p.into_inner());
        s.1 = true;
        self.0.1.notify_all();
    }
    /// Releases on drop, so a failing assertion cannot leave a blocking
    /// task held forever (the runtime waits for it on shutdown).
    fn release_on_drop(&self) -> ReleaseOnDrop {
        ReleaseOnDrop(self.clone())
    }
}

struct ReleaseOnDrop(Hold);

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

async fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !f() {
        assert!(Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn configure(app: &axum::Router, sb: &Sandbox) {
    let r = call(
        app,
        "POST",
        "/api/config",
        Some(&json!({"library_dir": Sandbox::s(&sb.lib)})),
    )
    .await;
    assert_eq!(r.status, 200);
}

// A restore (upload and snapshot) is refused with 409
// while a scan runs. The Python app answered these requests with the recorded
// "restore not sqlite" / "snapshot restore missing" 400s; once the scan
// ends they do again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restore_refused_while_a_scan_runs() {
    let g = golden("cases.json");
    let sb = Sandbox::new();
    let hold = Hold::default();
    let _release = hold.release_on_drop();
    let h2 = hold.clone();
    let st = AppState::builder(&sb.cfg)
        .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
        .network(Network::Offline)
        .fingerprinting(common::no_key_fingerprinting())
        .subscribers(false)
        .auto_snapshot(false)
        .on_scan_album(Arc::new(move |_, _| h2.enter(3)))
        .build()
        .unwrap();
    let app = router(st.clone());
    let norm = Norm::new(&sb);
    configure(&app, &sb).await;
    let r = call(&app, "POST", "/api/scan/rescan", None).await;
    assert_eq!(r.status, 200);
    eventually("scan held", || hold.reached() == 3).await;
    assert_eq!(st.gate().snapshot(), (1, false));
    let upload = case(&g, "restore not sqlite");
    let snap = case(&g, "snapshot restore missing");
    for c in [upload, snap] {
        let got = send(&app, request(c, &norm, None)).await;
        assert_eq!(rec(&got, &norm), refused(409, RESTORE_REFUSED));
    }
    hold.release();
    eventually("scan done", || {
        !st.scan().is_running() && st.gate().snapshot() == (0, false)
    })
    .await;
    for c in [upload, snap] {
        let got = send(&app, request(c, &norm, None)).await;
        assert_eq!(rec(&got, &norm), python(c));
    }
}

// A restore is refused while a check run is in progress
// (held here on a SQLite write lock the run waits for).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restore_refused_while_a_check_runs() {
    let g = golden("cases.json");
    let sb = Sandbox::new();
    let st = plain_state(&sb);
    let app = router(st.clone());
    let norm = Norm::new(&sb);
    configure(&app, &sb).await;
    let scan = call(&app, "GET", "/api/scan/stream", None).await;
    assert!(scan.text().contains("event: done"));
    let lock = rusqlite::Connection::open(st.db_path()).unwrap();
    lock.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let app2 = app.clone();
    let run = tokio::spawn(async move {
        call(
            &app2,
            "POST",
            "/api/checks/track-gaps/run",
            Some(&json!({"scope": "all"})),
        )
        .await
    });
    eventually("check run started", || st.gate().snapshot() == (1, false)).await;
    let c = case(&g, "restore not sqlite");
    let got = send(&app, request(c, &norm, None)).await;
    assert_eq!(rec(&got, &norm), refused(409, RESTORE_REFUSED));
    lock.execute_batch("ROLLBACK").unwrap();
    let r = run.await.unwrap();
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(st.gate().snapshot(), (0, false));
    let got = send(&app, request(c, &norm, None)).await;
    assert_eq!(rec(&got, &norm), python(c));
}

// While a restore runs, new scans (stream and rescan),
// check runs, applies and cover uploads are refused with 409; reads are
// not. A dry-run apply is refused too. The Python app answered each of these requests as
// recorded in cases.json (after its first scan); afterwards they answer so
// again here.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn work_refused_while_a_restore_runs() {
    let g = golden("cases.json");
    let sb = Sandbox::new();
    let hold = Hold::default();
    let _release = hold.release_on_drop();
    let h2 = hold.clone();
    let st = AppState::builder(&sb.cfg)
        .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
        .network(Network::Offline)
        .fingerprinting(common::no_key_fingerprinting())
        .subscribers(false)
        .auto_snapshot(false)
        .on_restore(Arc::new(move || h2.enter(1)))
        .build()
        .unwrap();
    let app = router(st.clone());
    let mut norm = Norm::new(&sb);
    configure(&app, &sb).await;
    let scan = call(&app, "GET", "/api/scan/stream", None).await;
    assert!(scan.text().contains("event: done"));
    let albums = call(&app, "GET", "/api/albums", None).await.json();
    for a in albums["albums"].as_array().unwrap() {
        norm.add(
            a["id"].as_str().unwrap(),
            &format!("<ID:{}>", a["dir_name"].as_str().unwrap()),
        );
    }
    let snap = call(&app, "POST", "/api/db/snapshots", None).await;
    assert_eq!(snap.status, 200);
    let name = snap.json()["filename"].as_str().unwrap().to_string();

    let app2 = app.clone();
    let path = format!("/api/db/snapshots/{name}/restore");
    let restore = tokio::spawn(async move { call(&app2, "POST", &path, None).await });
    eventually("restore held", || hold.reached() == 1).await;
    assert_eq!(st.gate().snapshot(), (0, true));

    let refused_labels = [
        "scan stream first",
        "run track-gaps",
        "apply genre",
        "scan stream again (new incremental scan)",
        // A dry run is refused too.
        "apply dry run",
    ];
    for l in refused_labels {
        let got = send(&app, request(case(&g, l), &norm, None)).await;
        assert_eq!(rec(&got, &norm), refused(409, WORK_REFUSED), "{l}");
    }
    // A configured rescan has no cases.json recording (the Python app answered
    // "scan started", musaic/api/library.py); refused here all the same.
    let got = call(&app, "POST", "/api/scan/rescan", None).await;
    assert_eq!(rec(&got, &norm), refused(409, WORK_REFUSED), "rescan");
    let id = albums["albums"][0]["id"].as_str().unwrap();
    let b = "bnd";
    let mut body = format!(
        "--{b}\r\nContent-Disposition: form-data; name=\"cover\"; filename=\"c\"\r\nContent-Type: image/png\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(b"png");
    body.extend_from_slice(format!("\r\n--{b}--\r\n").as_bytes());
    let up = Request::builder()
        .method("POST")
        .uri(format!("/api/albums/{id}/cover/upload"))
        .header("content-type", format!("multipart/form-data; boundary={b}"))
        .body(Body::from(body))
        .unwrap();
    let got = send(&app, up).await;
    assert_eq!(rec(&got, &norm), refused(409, WORK_REFUSED), "cover upload");
    // Another restore is refused too.
    let got = send(&app, request(case(&g, "restore not sqlite"), &norm, None)).await;
    assert_eq!(
        rec(&got, &norm),
        refused(409, WORK_REFUSED),
        "second restore"
    );
    // Reads are not gated.
    for l in ["albums", "settings", "checks"] {
        let got = send(&app, request(case(&g, l), &norm, None)).await;
        assert_eq!(got.status, 200, "{l}: {}", got.text());
    }
    // The db info / backup routes are not refused; they answer
    // (after the restore when they need the file).
    let app3 = app.clone();
    let info = tokio::spawn(async move {
        (
            call(&app3, "GET", "/api/db/info", None).await.status,
            call(&app3, "GET", "/api/db/backup", None).await.status,
        )
    });
    hold.release();
    assert_eq!(info.await.unwrap(), (200, 200), "db routes wait");
    let r = restore.await.unwrap();
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(st.gate().snapshot(), (0, false));
    let got = send(&app, request(case(&g, "run track-gaps"), &norm, None)).await;
    assert_eq!(got.status, 200, "{}", got.text());
}

// The guards are RAII. A restore whose task panics releases the
// gate (the request answers 500); an apply whose client goes away holds the
// gate until the apply ends and then releases it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn guards_release_on_panic_and_disconnect() {
    let sb = Sandbox::new();
    let st = AppState::builder(&sb.cfg)
        .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
        .network(Network::Offline)
        .fingerprinting(common::no_key_fingerprinting())
        .subscribers(false)
        .auto_snapshot(false)
        .on_restore(Arc::new(|| {
            panic!("injected restore panic (restore gate test)")
        }))
        .build()
        .unwrap();
    let app = router(st.clone());
    configure(&app, &sb).await;
    let r = call(&app, "POST", "/api/db/snapshots/library-x.db/restore", None).await;
    assert_eq!(r.status, 500);
    assert_eq!(st.gate().snapshot(), (0, false));
    let scan = call(&app, "GET", "/api/scan/stream", None).await;
    assert!(scan.text().contains("event: done"));
    eventually("scan guard released", || st.gate().snapshot() == (0, false)).await;

    let albums = call(&app, "GET", "/api/albums", None).await.json();
    let id = albums["albums"][0]["id"].as_str().unwrap();
    let req = common::with_host(
        Request::builder()
            .method("POST")
            .uri(format!("/api/albums/{id}/apply/stream"))
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"suggestions": {"genre": "Gone"}}).to_string(),
            ))
            .unwrap(),
    );
    let resp = tower::ServiceExt::oneshot(app.clone(), req).await.unwrap();
    assert_eq!(resp.status(), 200);
    drop(resp);
    eventually("apply guard released", || {
        st.gate().snapshot() == (0, false)
    })
    .await;
    let tracks = call(&app, "GET", &format!("/api/albums/{id}"), None).await;
    assert!(tracks.text().contains("Gone"), "{}", tracks.text());
}

// ------------------------------------- writing routes during a restore

/// The concrete request each writing route is probed with during a restore:
/// a body that passes the route's request validation (422 comes first), and
/// ids / names that do not exist (the busy 409 comes before the
/// handler's own 404 / 400). `None`: no case written for this route yet.
fn gate_probe(method: &str, path: &str, lib: &str) -> Option<Request<Body>> {
    let album = "0000000000000000";
    let json_req = |uri: String, body: Option<Value>| {
        let mut b = Request::builder().method(method).uri(uri);
        let body = match body {
            Some(v) => {
                b = b.header("content-type", "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        b.body(body).unwrap()
    };
    let multipart = |uri: String, field: &str, ctype: &str, data: &[u8]| {
        let bnd = "gateprobe";
        let mut body = format!(
            "--{bnd}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"f\"\r\nContent-Type: {ctype}\r\n\r\n"
        )
        .into_bytes();
        body.extend_from_slice(data);
        body.extend_from_slice(format!("\r\n--{bnd}--\r\n").as_bytes());
        Request::builder()
            .method(method)
            .uri(uri)
            .header(
                "content-type",
                format!("multipart/form-data; boundary={bnd}"),
            )
            .body(Body::from(body))
            .unwrap()
    };
    let r = match (method, path) {
        ("POST", "/api/config") => json_req(path.into(), Some(json!({"library_dir": lib}))),
        ("POST", "/api/scan/rescan") => json_req(path.into(), None),
        ("POST", "/api/ai/jobs") => json_req(
            path.into(),
            Some(json!({"album_id": album, "job_type": "metadata"})),
        ),
        ("DELETE", "/api/ai/jobs") => json_req("/api/ai/jobs?status=failed".into(), None),
        ("DELETE", "/api/ai/jobs/{job_id}") => json_req(format!("/api/ai/jobs/{album}"), None),
        ("POST", "/api/ai/jobs/{job_id}/acted-on") => json_req(
            format!("/api/ai/jobs/{album}/acted-on"),
            Some(json!({"action": "applied"})),
        ),
        ("POST", "/api/ai/jobs/{job_id}/retry") => {
            json_req(format!("/api/ai/jobs/{album}/retry"), None)
        }
        ("POST", "/api/ai/queue/resume") => json_req(path.into(), None),
        ("DELETE", "/api/albums/{album_id}") => json_req(format!("/api/albums/{album}"), None),
        ("POST", "/api/albums/{album_id}/analyze/stream") => json_req(
            format!("/api/albums/{album}/analyze/stream"),
            Some(json!({"what": "bpm"})),
        ),
        ("POST", "/api/albums/{album_id}/apply/stream") => json_req(
            format!("/api/albums/{album}/apply/stream"),
            Some(json!({"suggestions": {"genre": "Gated"}})),
        ),
        ("POST", "/api/albums/{album_id}/cover/upload") => multipart(
            format!("/api/albums/{album}/cover/upload"),
            "cover",
            "image/png",
            b"\x89PNG\r\n\x1a\n",
        ),
        ("POST", "/api/albums/{album_id}/mb-anchor/clear") => {
            json_req(format!("/api/albums/{album}/mb-anchor/clear"), None)
        }
        ("POST", "/api/albums/{album_id}/tags") => json_req(
            format!("/api/albums/{album}/tags"),
            Some(json!({"tag": "x"})),
        ),
        ("PUT", "/api/albums/{album_id}/tags") => json_req(
            format!("/api/albums/{album}/tags"),
            Some(json!({"track_tags": {}})),
        ),
        ("DELETE", "/api/albums/{album_id}/tags/{tag}") => {
            json_req(format!("/api/albums/{album}/tags/x"), None)
        }
        ("POST", "/api/albums/{album_id}/tracks/{filename}/tags") => json_req(
            format!("/api/albums/{album}/tracks/a.flac/tags"),
            Some(json!({"tag": "x"})),
        ),
        ("DELETE", "/api/albums/{album_id}/tracks/{filename}/tags/{tag}") => {
            json_req(format!("/api/albums/{album}/tracks/a.flac/tags/x"), None)
        }
        ("POST", "/api/checks/duplicate-tracks/findings/{finding_id}/delete-file") => json_req(
            "/api/checks/duplicate-tracks/findings/999999/delete-file".into(),
            Some(json!({"filename": "a.flac"})),
        ),
        ("POST", "/api/checks/filename-mismatch/findings/{finding_id}/apply") => json_req(
            "/api/checks/filename-mismatch/findings/999999/apply".into(),
            Some(json!({"plan": []})),
        ),
        ("POST", "/api/checks/inconsistent-artist-sort/findings/{finding_id}/apply-sort") => {
            json_req(
                "/api/checks/inconsistent-artist-sort/findings/999999/apply-sort".into(),
                Some(json!({"sort_value": "x"})),
            )
        }
        ("POST", "/api/checks/{check_id}/findings/{finding_id}/fix") => {
            json_req("/api/checks/nope/findings/999999/fix".into(), None)
        }
        ("POST", "/api/checks/{check_id}/findings/{finding_id}/ignore") => json_req(
            "/api/checks/nope/findings/999999/ignore".into(),
            Some(json!({})),
        ),
        ("POST", "/api/checks/{check_id}/findings/{finding_id}/unignore") => {
            json_req("/api/checks/nope/findings/999999/unignore".into(), None)
        }
        ("POST", "/api/checks/{check_id}/quick-fix-all") => {
            json_req("/api/checks/nope/quick-fix-all".into(), None)
        }
        ("POST", "/api/checks/{check_id}/run") => json_req(
            "/api/checks/nope/run".into(),
            Some(json!({"scope": "nope"})),
        ),
        ("POST", "/api/db/restore") => multipart(
            path.into(),
            "file",
            "application/octet-stream",
            &[b'x'; 100],
        ),
        ("POST", "/api/db/snapshots") => json_req(path.into(), None),
        ("DELETE", "/api/db/snapshots/{name}") => {
            json_req("/api/db/snapshots/.hidden".into(), None)
        }
        ("POST", "/api/db/snapshots/{name}/restore") => {
            json_req("/api/db/snapshots/library-nope.db/restore".into(), None)
        }
        ("PATCH", "/api/settings") => json_req(path.into(), Some(json!({"ai_max_parallel": 0}))),
        ("POST", "/api/vocabulary") => json_req(path.into(), Some(json!({"tag": "x"}))),
        ("POST", "/api/vocabulary/orphans/{tag}/promote") => {
            json_req("/api/vocabulary/orphans/x/promote".into(), None)
        }
        ("DELETE", "/api/vocabulary/{tag}") => json_req("/api/vocabulary/x".into(), None),
        ("PATCH", "/api/vocabulary/{tag}") => json_req(
            "/api/vocabulary/x".into(),
            Some(json!({"description": "d"})),
        ),
        ("POST", "/api/vocabulary/{tag}/merge") => json_req(
            "/api/vocabulary/x/merge".into(),
            Some(json!({"into_tag": "y"})),
        ),
        ("POST", "/api/vocabulary/{tag}/rename") => json_req(
            "/api/vocabulary/x/rename".into(),
            Some(json!({"new_tag": "y"})),
        ),
        // GET routes that write.
        ("GET", "/api/albums/{album_id}/resolution") => {
            json_req(format!("/api/albums/{album}/resolution"), None)
        }
        ("GET", "/api/scan/stream") => json_req(path.into(), None),
        ("GET", "/api/albums") => json_req("/api/albums?refresh=true".into(), None),
        _ => return None,
    };
    Some(r)
}

/// GET routes that write and are gated: the resolution (it can
/// store fingerprints and an anchor), a scan start, and a refresh of the
/// album list (it drops the snapshot; probed with `refresh=true`).
const GATED_GETS: [&str; 3] = [
    "/api/albums/{album_id}/resolution",
    "/api/scan/stream",
    "/api/albums",
];

/// GET routes that wait on the database file lock during a restore instead
/// of being refused (they read the whole file).
const WAITING_GETS: [&str; 2] = ["/api/db/backup", "/api/db/info"];

fn python_routes() -> Vec<(String, String)> {
    golden("routes.json")["routes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["method"].as_str().unwrap().to_string(),
                r["path"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn concrete(path: &str) -> String {
    regex::Regex::new(r"\{[^}]*\}")
        .unwrap()
        .replace_all(path, "x")
        .into_owned()
        .replace("/x", "/0000000000000000")
}

/// A state whose restores are held at the hook until released.
fn held_restore_state(sb: &Sandbox, hold: &Hold, subscribers: bool) -> AppState {
    let h2 = hold.clone();
    AppState::builder(&sb.cfg)
        .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
        .network(Network::Offline)
        .fingerprinting(common::no_key_fingerprinting())
        .subscribers(subscribers)
        .auto_snapshot(false)
        .on_restore(Arc::new(move || h2.enter(1)))
        .build()
        .unwrap()
}

/// Start a snapshot restore and wait until it holds the gate.
async fn hold_a_restore(
    app: &axum::Router,
    st: &AppState,
    hold: &Hold,
) -> tokio::task::JoinHandle<Got> {
    eventually("gate idle", || st.gate().snapshot() == (0, false)).await;
    let snap = call(app, "POST", "/api/db/snapshots", None).await;
    assert_eq!(snap.status, 200, "{}", snap.text());
    let name = snap.json()["filename"].as_str().unwrap().to_string();
    let app2 = app.clone();
    let path = format!("/api/db/snapshots/{name}/restore");
    let restore = tokio::spawn(async move { call(&app2, "POST", &path, None).await });
    eventually("restore held", || hold.reached() == 1).await;
    assert_eq!(st.gate().snapshot(), (0, true));
    restore
}

// During a restore every writing route answers 409 with
// WORK_REFUSED. The route list is the Python app's whole inventory
// (testdata/golden/api/routes.json): every non-GET route that is not a 501
// stub, plus the GET routes that write (GATED_GETS), must have a probe in
// `gate_probe` and be refused, so a route added later cannot be left
// ungated without this test failing. Every other GET route is not refused.
// The probes use unknown ids and invalid names, so a 409 also shows that the
// busy answer comes before the handler's own 404 / 400.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_writing_route_is_refused_during_a_restore() {
    let sb = Sandbox::new();
    let hold = Hold::default();
    let _release = hold.release_on_drop();
    let st = held_restore_state(&sb, &hold, false);
    let app = router(st.clone());
    let norm = Norm::new(&sb);
    configure(&app, &sb).await;
    let restore = hold_a_restore(&app, &st, &hold).await;

    let stubs: Vec<(String, String)> = musaic_api::STUBS
        .iter()
        .map(|(m, p, _)| (m.to_string(), p.to_string()))
        .collect();
    let lib = Sandbox::s(&sb.lib);
    let mut missing = Vec::new();
    let mut probed = 0;
    for (method, path) in python_routes() {
        if stubs.contains(&(method.clone(), path.clone())) {
            continue;
        }
        let gated = method != "GET" || GATED_GETS.contains(&path.as_str());
        if !gated {
            if WAITING_GETS.contains(&path.as_str()) {
                continue;
            }
            // Status only: the SSE routes never end.
            let uri = concrete(&path);
            let resp = tower::ServiceExt::oneshot(app.clone(), common::get(&uri))
                .await
                .unwrap();
            assert_ne!(resp.status().as_u16(), 409, "GET {path} ({uri}) is a read");
            continue;
        }
        let Some(req) = gate_probe(&method, &path, &lib) else {
            missing.push(format!("{method} {path}"));
            continue;
        };
        let got = send(&app, req).await;
        assert_eq!(
            rec(&got, &norm),
            refused(409, WORK_REFUSED),
            "{method} {path} during a restore"
        );
        probed += 1;
    }
    assert!(
        missing.is_empty(),
        "writing routes with no gate probe (add one to gate_probe and gate the route):\n{}",
        missing.join("\n")
    );
    // 71 Python app routes, no stub; of those, 37 non-GET + 3 gated GETs (the
    // duplicate-tracks canonical lookup is a read: probed as not refused).
    assert_eq!(probed, 40);
    assert_eq!(st.gate().snapshot(), (0, true), "no guard leaked");
    hold.release();
    let r = restore.await.unwrap();
    assert_eq!(r.status, 200, "{}", r.text());
}

// During a restore the busy 409 wins over every answer a
// writing handler would give itself. The requests are the Python app's own
// recordings (testdata/golden/api/cases.json) whose recorded answer is a
// handler 400 / 404 / 409; during the restore each is a 409 with
// WORK_REFUSED (delta), and once the restore has ended each gets the
// Python app's recorded answer again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn busy_409_comes_before_id_and_name_checks() {
    let g = golden("cases.json");
    let sb = Sandbox::new();
    let hold = Hold::default();
    let _release = hold.release_on_drop();
    let st = held_restore_state(&sb, &hold, false);
    let app = router(st.clone());
    let norm = Norm::new(&sb);
    configure(&app, &sb).await;
    let labels = [
        "config bad dir",
        "resolution unknown",
        "apply unknown album",
        "settings max_parallel 0",
        "snapshot delete hidden name",
        "snapshot delete missing",
        "snapshot restore missing",
        "restore not sqlite",
        "run unknown check",
        "run bad scope",
    ];
    let restore = hold_a_restore(&app, &st, &hold).await;
    for l in labels {
        let got = send(&app, request(case(&g, l), &norm, None)).await;
        assert_eq!(rec(&got, &norm), refused(409, WORK_REFUSED), "{l}");
    }
    hold.release();
    let r = restore.await.unwrap();
    assert_eq!(r.status, 200, "{}", r.text());
    for l in labels {
        let c = case(&g, l);
        let got = send(&app, request(c, &norm, None)).await;
        assert_eq!(rec(&got, &norm), python(c), "{l} after the restore");
    }
}

fn check_runs(path: &std::path::Path) -> i64 {
    let c = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap();
    c.query_row("SELECT COUNT(*) FROM check_runs", [], |r| r.get(0))
        .unwrap()
}

// A bus-driven check rerun refused because a restore
// runs is recorded and run once the restore has ended: one library-wide
// post-scan sweep over the restored database, however many reruns were
// skipped. Evidence: the restored snapshot's check_runs count (read from
// the snapshot file itself) grows by exactly one run per runs-during-scan
// check after the restore, and not before it ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reruns_skipped_during_a_restore_run_once_after_it() {
    let sb = Sandbox::new();
    let hold = Hold::default();
    let _release = hold.release_on_drop();
    let st = held_restore_state(&sb, &hold, true);
    let app = router(st.clone());
    configure(&app, &sb).await;
    let scan = call(&app, "GET", "/api/scan/stream", None).await;
    assert!(scan.text().contains("event: done"));
    assert!(st.bus().wait_background_idle(Duration::from_secs(30)));
    let albums = call(&app, "GET", "/api/albums", None).await.json();
    let ids: Vec<String> = albums["albums"]
        .as_array()
        .unwrap()
        .iter()
        .take(3)
        .map(|a| a["id"].as_str().unwrap().to_string())
        .collect();
    let restore = hold_a_restore(&app, &st, &hold).await;
    let snapshot = std::fs::read_dir(sb.cfg.join("backups"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "db"))
        .unwrap();
    let in_snapshot = check_runs(&snapshot);
    let sweep_size = musaic_checks::Registry::default()
        .all_meta()
        .iter()
        .filter(|m| m.runs_during_scan)
        .count() as i64;
    assert!(sweep_size > 0);

    // Three album-scoped reruns (AlbumTagsChanged -> post-scan rerun, a
    // background subscriber) and one library-wide one, all refused.
    for id in &ids {
        st.bus().publish(&musaic_events::Event::AlbumTagsChanged {
            album_id: id.clone(),
            fields: vec!["genre".into()],
        });
    }
    st.bus().publish(&musaic_events::Event::BulkBatchCompleted);
    assert!(st.bus().wait_background_idle(Duration::from_secs(30)));
    assert!(
        st.gate().deferred().is_some(),
        "the refused reruns are recorded"
    );
    assert_eq!(st.gate().snapshot(), (0, true));

    hold.release();
    let r = restore.await.unwrap();
    assert_eq!(r.status, 200, "{}", r.text());
    eventually("replayed sweep", || {
        check_runs(st.db_path()) >= in_snapshot + sweep_size
    })
    .await;
    eventually("sweep done", || st.gate().snapshot() == (0, false)).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(st.bus().wait_background_idle(Duration::from_secs(30)));
    assert_eq!(
        check_runs(st.db_path()),
        in_snapshot + sweep_size,
        "one coalesced sweep, not one per skipped rerun"
    );
    assert!(st.gate().deferred().is_none());
}
