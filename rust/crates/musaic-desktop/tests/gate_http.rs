//! The desktop gate in front of the real musaic-api router (in-memory
//! secrets, offline network, temp config dir), driven with tower `oneshot`,
//! stacked as `backend::start` stacks it: this crate's window layer outside
//! `musaic_api::router_with(.., launch: Some(..))`.
//!
//! Nothing answers
//! without the launch cookie; the launch URL sets it once; the cookie does
//! not bypass musaic-api's own Host / Origin check, which runs
//! first. The dialog endpoint asks the same `LaunchGate`. Every
//! response carries the CSP. The focus endpoint needs the instance
//! secret. The reveal endpoints take an album id (the
//! folder is looked up server-side) or reveal the trash, behind the same
//! checks as the folder picker.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use musaic_desktop::gate::{self, DialogKind, Gate, Hooks};
use tokio::sync::oneshot;
use tower::ServiceExt;

const PORT: u16 = 41999;
const TOKEN: &str = "aa11bb22cc33dd44ee55ff6600112233aa11bb22cc33dd44ee55ff6600112233";
const FOCUS: &str = "f0f0f0f0";

#[derive(Default)]
struct Counting {
    focus: AtomicUsize,
    dialogs: AtomicUsize,
    pickers: AtomicUsize,
    /// What the folder picker answers (None: cancelled).
    picked: std::sync::Mutex<Option<std::path::PathBuf>>,
    /// Paths the reveal hook was asked to show.
    revealed: std::sync::Mutex<Vec<std::path::PathBuf>>,
    /// The reveal hook fails (the file manager could not be asked).
    reveal_fails: std::sync::atomic::AtomicBool,
    /// Menu states the page posted: (id, enabled, checked) per item.
    menu: std::sync::Mutex<Vec<Vec<MenuItem>>>,
}

/// (id, enabled, checked)
type MenuItem = (String, bool, Option<bool>);

impl Hooks for Counting {
    fn focus(&self) {
        self.focus.fetch_add(1, Ordering::SeqCst);
    }
    fn dialog(&self, kind: DialogKind, message: String) -> oneshot::Receiver<bool> {
        self.dialogs.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        let _ = tx.send(kind == DialogKind::Confirm && message == "yes?");
        rx
    }
    fn pick_folder(&self) -> oneshot::Receiver<Option<std::path::PathBuf>> {
        self.pickers.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        let _ = tx.send(self.picked.lock().unwrap().clone());
        rx
    }
    fn reveal(&self, path: std::path::PathBuf) -> oneshot::Receiver<Result<(), String>> {
        self.revealed.lock().unwrap().push(path);
        let (tx, rx) = oneshot::channel();
        let _ = tx.send(if self.reveal_fails.load(Ordering::SeqCst) {
            Err("no file manager".into())
        } else {
            Ok(())
        });
        rx
    }
    fn menu_state(&self, items: Vec<musaic_desktop::menu::ItemState>) {
        self.menu.lock().unwrap().push(
            items
                .iter()
                .map(|s| (s.command.id.to_string(), s.enabled, s.checked))
                .collect(),
        );
    }
}

fn app(dir: &std::path::Path, hooks: Arc<Counting>) -> axum::Router {
    let state = musaic_api::AppState::builder(dir.to_path_buf())
        .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
        .network(musaic_api::Network::Offline)
        .build()
        .unwrap();
    let launch = Arc::new(musaic_api::LaunchGate::new(PORT, TOKEN.into()));
    let g = Arc::new(Gate::new(
        launch.clone(),
        FOCUS.into(),
        hooks,
        state.clone(),
    ));
    musaic_api::router_with(
        state,
        musaic_api::RouterOptions {
            launch: Some(launch),
        },
    )
    .layer(axum::middleware::from_fn_with_state(g, gate::guard))
}

fn req(method: Method, path: &str) -> axum::http::request::Builder {
    Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, format!("127.0.0.1:{PORT}"))
}

async fn send(app: &axum::Router, r: Request<Body>) -> (StatusCode, axum::http::HeaderMap, String) {
    let resp = app.clone().oneshot(r).await.unwrap();
    let (parts, body) = resp.into_parts();
    let bytes = body.collect().await.unwrap().to_bytes();
    (
        parts.status,
        parts.headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

fn cookie() -> String {
    format!("{}={TOKEN}", gate::COOKIE)
}

// The per-launch token (in musaic-api) and the Host / Origin check.
#[tokio::test]
async fn token_cookie_gates_every_route_and_launch_is_one_shot() {
    let dir = tempfile::tempdir().unwrap();
    let hooks = Arc::new(Counting::default());
    let app = app(dir.path(), hooks.clone());

    // Without the cookie: API, SPA fallback and streams are all refused.
    for p in ["/api/health", "/", "/library", "/api/library-events/stream"] {
        let (s, h, b) = send(&app, req(Method::GET, p).body(Body::empty()).unwrap()).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{p}");
        assert!(
            b.contains("\"detail\""),
            "{p}: the Python app's error shape"
        );
        assert_eq!(h[header::CONTENT_SECURITY_POLICY], gate::APP_CSP);
    }
    // Wrong token, then a wrong Host, on the launch URL.
    let (s, _, _) = send(
        &app,
        req(Method::GET, "/__musaic/launch?t=nope")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _, _) = send(
        &app,
        Request::builder()
            .uri(format!("/__musaic/launch?t={TOKEN}"))
            .header(header::HOST, "localhost:41999")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // The real launch sets the cookie and refreshes to `/` without script.
    let (s, h, b) = send(
        &app,
        req(Method::GET, &format!("/__musaic/launch?t={TOKEN}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let set = h[header::SET_COOKIE].to_str().unwrap();
    assert!(set.starts_with(&cookie()), "{set}");
    assert!(set.contains("HttpOnly") && set.contains("SameSite=Strict"));
    assert!(b.contains("http-equiv=\"refresh\" content=\"0;url=/\""));
    assert!(!b.contains("<script"));
    // One shot.
    let (s, _, _) = send(
        &app,
        req(Method::GET, &format!("/__musaic/launch?t={TOKEN}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::GONE);

    // With the cookie the backend answers (health; config is the Python app shape).
    let (s, h, _) = send(
        &app,
        req(Method::GET, "/api/health")
            .header(header::COOKIE, format!("other=1; {}", cookie()))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h[header::CONTENT_SECURITY_POLICY], gate::APP_CSP);
    let (s, _, b) = send(
        &app,
        req(Method::GET, "/api/config")
            .header(header::COOKIE, cookie())
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(b.contains("library_dir"));

    // The cookie does not bypass musaic-api's Host / Origin check.
    let (s, _, b) = send(
        &app,
        Request::builder()
            .uri("/api/health")
            .header(header::HOST, "evil.example")
            .header(header::COOKIE, cookie())
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert!(b.contains("Host header refused"), "{b}");
    let (s, _, _) = send(
        &app,
        req(Method::POST, "/api/scan/rescan")
            .header(header::COOKIE, cookie())
            .header(header::ORIGIN, "https://evil.example")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

// Second launch focuses the first; window.confirm shim
#[tokio::test]
async fn focus_and_dialog_endpoints() {
    let dir = tempfile::tempdir().unwrap();
    let hooks = Arc::new(Counting::default());
    let app = app(dir.path(), hooks.clone());

    let (s, _, _) = send(
        &app,
        req(Method::POST, gate::FOCUS_PATH)
            .header(gate::FOCUS_HEADER, "wrong")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _, _) = send(
        &app,
        req(Method::POST, gate::FOCUS_PATH)
            .header(gate::FOCUS_HEADER, FOCUS)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(hooks.focus.load(Ordering::SeqCst), 1);

    let origin = format!("http://127.0.0.1:{PORT}");
    let dialog = |body: &'static str, origin: Option<&str>, with_cookie: bool| {
        let mut b = req(Method::POST, "/__musaic/dialog/confirm");
        if let Some(o) = origin {
            b = b.header(header::ORIGIN, o);
        }
        if with_cookie {
            b = b.header(header::COOKIE, cookie());
        }
        b.body(Body::from(body)).unwrap()
    };
    let (s, _, b) = send(&app, dialog("yes?", Some(&origin), true)).await;
    assert_eq!((s, b.as_str()), (StatusCode::OK, "true"));
    let (s, _, b) = send(&app, dialog("no?", Some(&origin), true)).await;
    assert_eq!((s, b.as_str()), (StatusCode::OK, "false"));
    // No cookie, or a foreign / missing Origin: refused before any dialog.
    assert_eq!(
        send(&app, dialog("yes?", Some(&origin), false)).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&app, dialog("yes?", Some("http://127.0.0.1:1"), true))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&app, dialog("yes?", None, true)).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(hooks.dialogs.load(Ordering::SeqCst), 2);
}

// The native folder picker endpoint. Same gate as the
// confirm shim (cookie, exact Host, the app's own Origin, POST); answers
// the chosen directory or null, as JSON.
#[tokio::test]
async fn folder_picker_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let hooks = Arc::new(Counting::default());
    let app = app(dir.path(), hooks.clone());
    let origin = format!("http://127.0.0.1:{PORT}");
    let pick = |method: Method, origin: Option<&str>, with_cookie: bool| {
        let mut b = req(method, gate::FOLDER_PATH);
        if let Some(o) = origin {
            b = b.header(header::ORIGIN, o);
        }
        if with_cookie {
            b = b.header(header::COOKIE, cookie());
        }
        b.body(Body::empty()).unwrap()
    };

    // Cancelled.
    let (s, h, b) = send(&app, pick(Method::POST, Some(&origin), true)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h[header::CONTENT_TYPE], "application/json");
    assert_eq!(h[header::CONTENT_SECURITY_POLICY], gate::APP_CSP);
    assert_eq!(b, r#"{"path":null}"#);

    // Chosen.
    let chosen = dir.path().join("Music Library");
    *hooks.picked.lock().unwrap() = Some(chosen.clone());
    let (s, _, b) = send(&app, pick(Method::POST, Some(&origin), true)).await;
    assert_eq!(s, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert_eq!(v["path"], chosen.to_str().unwrap());
    assert_eq!(hooks.pickers.load(Ordering::SeqCst), 2);

    // A path that is not valid UTF-8 cannot be stored as the library dir.
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        *hooks.picked.lock().unwrap() = Some(std::ffi::OsStr::from_bytes(b"/tmp/\xff").into());
        let (s, _, b) = send(&app, pick(Method::POST, Some(&origin), true)).await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(b.contains("\"detail\""), "{b}");
        *hooks.picked.lock().unwrap() = None;
    }
    let base = hooks.pickers.load(Ordering::SeqCst);

    // No cookie, a foreign or missing Origin, or GET: refused, no picker.
    assert_eq!(
        send(&app, pick(Method::POST, Some(&origin), false)).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&app, pick(Method::POST, Some("http://127.0.0.1:1"), true))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&app, pick(Method::POST, None, true)).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&app, pick(Method::GET, Some(&origin), true)).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(hooks.pickers.load(Ordering::SeqCst), base);
}

// The initialization script defines the marker
// the frontend's lib/desktop.ts reads, and only on the backend origin; it
// advertises reveal with the platform's file manager name.
#[test]
fn shim_defines_the_desktop_marker() {
    let js = gate::init_script();
    let guard = js.find("location.hostname !== \"127.0.0.1\"").unwrap();
    let marker = js.find("\"__musaicDesktop\"").unwrap();
    assert!(guard < marker, "marker set after the origin guard");
    assert!(js.contains("folderPicker: true"));
    assert!(js.contains("reveal: true"));
    assert!(js.contains(&format!("fileManager: \"{}\"", gate::FILE_MANAGER)));
    assert!(!js.contains("__MUSAIC_FILE_MANAGER__"));
    #[cfg(target_os = "macos")]
    assert_eq!(gate::FILE_MANAGER, "Finder");
    #[cfg(windows)]
    assert_eq!(gate::FILE_MANAGER, "Explorer");
    assert!(js.contains("Object.freeze"));
    assert!(js.contains(&format!("nativeMenu: {}", gate::NATIVE_MENU)));
    assert!(!js.contains("__MUSAIC_NATIVE_MENU__"));
    assert_eq!(gate::NATIVE_MENU, cfg!(target_os = "macos"));
}

fn menu_req(body: &str, method: Method, origin: Option<&str>, with_cookie: bool) -> Request<Body> {
    let mut b = req(method, gate::MENU_STATE_PATH).header(header::CONTENT_TYPE, "application/json");
    if let Some(o) = origin {
        b = b.header(header::ORIGIN, o);
    }
    if with_cookie {
        b = b.header(header::COOKIE, cookie());
    }
    b.body(Body::from(body.to_string())).unwrap()
}

// The page reports the native menu's state: only allow-listed command ids
// reach the shell, behind the same checks as the reveal endpoints.
#[tokio::test]
async fn menu_state_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let hooks = Arc::new(Counting::default());
    let app = app(dir.path(), hooks.clone());
    let origin = format!("http://127.0.0.1:{PORT}");
    let body = r#"{"items": [
        {"id": "album.reveal", "enabled": false},
        {"id": "view.list", "enabled": true, "checked": true},
        {"id": "not.a.command", "enabled": true}
    ]}"#;

    let (s, h, b) = send(&app, menu_req(body, Method::POST, Some(&origin), true)).await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{b}");
    assert_eq!(h[header::CONTENT_SECURITY_POLICY], gate::APP_CSP);
    assert_eq!(
        *hooks.menu.lock().unwrap(),
        vec![vec![
            ("album.reveal".to_string(), false, None),
            ("view.list".to_string(), true, Some(true)),
        ]]
    );

    // Malformed: 400, nothing applied.
    let (s, _, _) = send(&app, menu_req("nope", Method::POST, Some(&origin), true)).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // No cookie, another Origin, no Origin, GET: refused, nothing applied.
    for r in [
        menu_req(body, Method::POST, Some(&origin), false),
        menu_req(body, Method::POST, Some("http://evil.example"), true),
        menu_req(body, Method::POST, None, true),
        menu_req(body, Method::GET, Some(&origin), true),
    ] {
        let (s, _, _) = send(&app, r).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }
    // Too large.
    let big = format!(r#"{{"items": [], "pad": "{}"}}"#, "x".repeat(20_000));
    let (s, _, _) = send(&app, menu_req(&big, Method::POST, Some(&origin), true)).await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(hooks.menu.lock().unwrap().len(), 1);
}

const ON_DISK: (&str, &str) = ("3a95f73281595529", "Aurora Fields - Second Light [FLAC]");
const NOT_ON_DISK: &str = "ecbbf886397962f1";

/// A config dir holding a copy of the Python app's v16 database whose album rows
/// are moved under a temp library (one album folder created, the others
/// absent), with that library configured.
fn configured(dir: &std::path::Path) -> std::path::PathBuf {
    std::fs::create_dir_all(dir.join("library")).unwrap();
    let lib = musaic_testdata::canonical(dir.join("library"));
    let config = dir.join("config");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::copy(
        musaic_testdata::public("db/v16-synthetic-offline.db"),
        config.join("library.db"),
    )
    .unwrap();
    let new = lib.to_str().unwrap();
    let mut db = rusqlite::Connection::open(config.join("library.db")).unwrap();
    // albums.library_path references libraries(path): move both in one
    // transaction, the foreign key checked at commit.
    // The library root stored in testdata/db/v16-synthetic-offline.db (the
    // recording machine's, rewritten to a neutral root; data, not a place to
    // open; testdata/db/README.md).
    let fixture_lib: String = db
        .query_row("SELECT path FROM libraries", [], |r| r.get(0))
        .unwrap();
    let tx = db.transaction().unwrap();
    tx.execute_batch("PRAGMA defer_foreign_keys = ON").unwrap();
    tx.execute(
        "UPDATE albums SET path = replace(path, ?1, ?2), library_path = ?2",
        [fixture_lib.as_str(), new],
    )
    .unwrap();
    tx.execute("UPDATE libraries SET path = ?1", [new]).unwrap();
    tx.commit().unwrap();
    drop(db);
    std::fs::create_dir_all(lib.join(ON_DISK.1)).unwrap();
    std::fs::write(
        config.join("config.json"),
        serde_json::json!({ "library_dir": new }).to_string(),
    )
    .unwrap();
    lib
}

fn reveal_req(
    path: &str,
    method: Method,
    origin: Option<&str>,
    with_cookie: bool,
) -> Request<Body> {
    let mut b = req(method, path);
    if let Some(o) = origin {
        b = b.header(header::ORIGIN, o);
    }
    if with_cookie {
        b = b.header(header::COOKIE, cookie());
    }
    b.body(Body::empty()).unwrap()
}

// Reveal an album's folder by id. The page sends an id; the
// folder is the configured library's row for it. Unknown ids, malformed
// ids and folders not on disk are 404s; nothing is revealed for them.
#[tokio::test]
async fn reveal_album_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let lib = configured(dir.path());
    let hooks = Arc::new(Counting::default());
    let app = app(&dir.path().join("config"), hooks.clone());
    let origin = format!("http://127.0.0.1:{PORT}");
    let path = |id: &str| format!("{}{id}", gate::REVEAL_ALBUM_PREFIX);

    let (s, h, b) = send(
        &app,
        reveal_req(&path(ON_DISK.0), Method::POST, Some(&origin), true),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{b}");
    assert_eq!(h[header::CONTENT_SECURITY_POLICY], gate::APP_CSP);
    assert_eq!(*hooks.revealed.lock().unwrap(), vec![lib.join(ON_DISK.1)]);

    // A cached album whose folder is gone (Missing / not on disk).
    let (s, _, b) = send(
        &app,
        reveal_req(&path(NOT_ON_DISK), Method::POST, Some(&origin), true),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert!(b.contains("isn't on disk"), "{b}");
    // Well-formed but unknown (the live walk finds nothing either).
    let (s, _, b) = send(
        &app,
        reveal_req(&path("0123456789abcdef"), Method::POST, Some(&origin), true),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert!(b.contains("\"detail\""), "{b}");
    // Not an album id: refused before any lookup. A path in the URL is
    // never a path to reveal.
    for bad in [
        "",
        "3A95F73281595529",
        "3a95f7328159552",
        "3a95f73281595529/..",
        "..%2F..%2Fetc",
        "%2Fetc%2Fpasswd",
    ] {
        let (s, _, _) = send(
            &app,
            reveal_req(&path(bad), Method::POST, Some(&origin), true),
        )
        .await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{bad:?}");
    }
    let (s, _, _) = send(
        &app,
        reveal_req("/__musaic/reveal/other", Method::POST, Some(&origin), true),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(hooks.revealed.lock().unwrap().len(), 1);

    // The file manager could not be asked: 500 with a message.
    hooks.reveal_fails.store(true, Ordering::SeqCst);
    let (s, _, b) = send(
        &app,
        reveal_req(&path(ON_DISK.0), Method::POST, Some(&origin), true),
    )
    .await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(b.contains("file manager"), "{b}");
    hooks.reveal_fails.store(false, Ordering::SeqCst);
    let base = hooks.revealed.lock().unwrap().len();

    // No cookie, a foreign or missing Origin, GET, or a foreign Host:
    // refused, nothing revealed.
    let p = path(ON_DISK.0);
    for r in [
        reveal_req(&p, Method::POST, Some(&origin), false),
        reveal_req(&p, Method::POST, Some("http://127.0.0.1:1"), true),
        reveal_req(&p, Method::POST, Some("https://evil.example"), true),
        reveal_req(&p, Method::POST, None, true),
        reveal_req(&p, Method::GET, Some(&origin), true),
        Request::builder()
            .method(Method::POST)
            .uri(&p)
            .header(header::HOST, "localhost:41999")
            .header(header::ORIGIN, &origin)
            .header(header::COOKIE, cookie())
            .body(Body::empty())
            .unwrap(),
    ] {
        let (s, h, _) = send(&app, r).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        assert_eq!(h[header::CONTENT_SECURITY_POLICY], gate::APP_CSP);
    }
    assert_eq!(hooks.revealed.lock().unwrap().len(), base);
}

// With no library configured an album cannot be revealed (the Python
// app's 409); the trash still can, and is created when absent.
#[tokio::test]
async fn reveal_trash_endpoint_and_no_library() {
    let dir = tempfile::tempdir().unwrap();
    let config = musaic_testdata::canonical(dir.path());
    let hooks = Arc::new(Counting::default());
    let app = app(&config, hooks.clone());
    let origin = format!("http://127.0.0.1:{PORT}");

    let (s, _, b) = send(
        &app,
        reveal_req(
            &format!("{}{}", gate::REVEAL_ALBUM_PREFIX, ON_DISK.0),
            Method::POST,
            Some(&origin),
            true,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");
    assert!(hooks.revealed.lock().unwrap().is_empty());

    assert!(!config.join("trash").exists());
    let (s, _, b) = send(
        &app,
        reveal_req(gate::REVEAL_TRASH_PATH, Method::POST, Some(&origin), true),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{b}");
    assert!(config.join("trash").is_dir());
    assert_eq!(*hooks.revealed.lock().unwrap(), vec![config.join("trash")]);

    for r in [
        reveal_req(gate::REVEAL_TRASH_PATH, Method::POST, Some(&origin), false),
        reveal_req(gate::REVEAL_TRASH_PATH, Method::POST, None, true),
        reveal_req(
            gate::REVEAL_TRASH_PATH,
            Method::POST,
            Some("http://127.0.0.1:1"),
            true,
        ),
        reveal_req(gate::REVEAL_TRASH_PATH, Method::GET, Some(&origin), true),
    ] {
        assert_eq!(send(&app, r).await.0, StatusCode::FORBIDDEN);
    }
    assert_eq!(hooks.revealed.lock().unwrap().len(), 1);
}

// The selftest controls exist only in a
// `--selftest-report` run. A normal run's hooks answer 404 even with the
// cookie, and without the cookie, from a foreign Origin or with GET the
// gate refuses before asking the hooks.
#[tokio::test]
async fn selftest_controls_are_absent_in_normal_runs() {
    let dir = tempfile::tempdir().unwrap();
    let hooks = Arc::new(Counting::default());
    let app = app(dir.path(), hooks);
    let origin = format!("http://127.0.0.1:{PORT}");
    let call = |method: Method, origin: Option<&str>, with_cookie: bool| {
        let mut b = req(method, &format!("{}hold-work", gate::SELFTEST_PREFIX));
        if let Some(o) = origin {
            b = b.header(header::ORIGIN, o);
        }
        if with_cookie {
            b = b.header(header::COOKIE, cookie());
        }
        b.body(Body::empty()).unwrap()
    };
    let (s, h, _) = send(&app, call(Method::POST, Some(&origin), true)).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(h[header::CONTENT_SECURITY_POLICY], gate::APP_CSP);
    for (m, o, c) in [
        (Method::POST, Some(origin.as_str()), false),
        (Method::POST, Some("http://127.0.0.1:1"), true),
        (Method::POST, None, true),
        (Method::GET, Some(origin.as_str()), true),
    ] {
        assert_eq!(
            send(&app, call(m.clone(), o, c)).await.0,
            StatusCode::FORBIDDEN,
            "{m} {o:?} cookie={c}"
        );
    }
}
