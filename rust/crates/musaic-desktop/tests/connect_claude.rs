//! `POST /__musaic/connect/claude-desktop` (spec 007): the same checks as the
//! reveal endpoints, no body, the hook gets exactly the path the operation
//! wrote, a failed open removes the file, and the answer never names the
//! file. The machine is a scratch one: no real Claude directory is read and no
//! file is opened (the hook is a recording double).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use musaic_desktop::gate::{self, Gate, Hooks};
use musaic_desktop::open_file;
use tower::ServiceExt;

const PORT: u16 = 41998;
const TOKEN: &str = "aa11bb22cc33dd44ee55ff6600112233aa11bb22cc33dd44ee55ff6600112233";
const FOCUS: &str = "f0f0f0f0";

#[derive(Default)]
struct Recording {
    opened: Mutex<Vec<PathBuf>>,
    /// Whether the file existed at the moment the hook was called.
    existed: Mutex<Vec<bool>>,
    fails: std::sync::atomic::AtomicBool,
    /// A stand-in for the system's open helper: when set, the hook runs it
    /// through the real `open_file::open_and_wait`, as the macOS shell does.
    program: Mutex<Option<(PathBuf, Duration)>>,
}

impl Hooks for Recording {
    fn focus(&self) {}
    fn dialog(&self, _: gate::DialogKind, _: String) -> tokio::sync::oneshot::Receiver<bool> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = tx.send(false);
        rx
    }
    fn open_bundle(&self, path: &std::path::Path) -> Result<(), String> {
        self.existed.lock().unwrap().push(path.is_file());
        self.opened.lock().unwrap().push(path.to_path_buf());
        if let Some((program, timeout)) = self.program.lock().unwrap().clone() {
            return open_file::open_and_wait(&program, path, timeout).map(|_| ());
        }
        if self.fails.load(std::sync::atomic::Ordering::SeqCst) {
            Err("no program opens it".into())
        } else {
            Ok(())
        }
    }
}

struct World {
    tmp: tempfile::TempDir,
    app: axum::Router,
    hooks: Arc<Recording>,
}

/// `claude`: whether Claude Desktop is "installed" on the scratch machine.
fn world(claude: bool) -> World {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let exe = root.join("app/Musaic");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(root.join("app/musaic-cli"), b"cli").unwrap();
    std::fs::create_dir_all(root.join("tmproot")).unwrap();
    let env = musaic_api::HostEnv {
        platform: musaic_api::Platform::Darwin,
        home: Some(root.join("home")),
        appdata: None,
        local_appdata: None,
        application_dirs: vec![root.join("Applications")],
        executable: Some(exe),
        temp_root: root.join("tmproot"),
        allow_temporary_program: false,
        non_default_config_dir: None,
        cleanup: false,
        mount: |_| musaic_api::MountKind::Unknown,
    };
    if claude {
        let file = env.config_file().unwrap();
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    }
    let state = musaic_api::AppState::builder(root.join("config"))
        .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
        .network(musaic_api::Network::Offline)
        .host_env(env)
        .build()
        .unwrap();
    let hooks = Arc::new(Recording::default());
    let launch = Arc::new(musaic_api::LaunchGate::new(PORT, TOKEN.into()));
    let g = Arc::new(Gate::new(
        launch.clone(),
        FOCUS.into(),
        hooks.clone(),
        state.clone(),
    ));
    let app = musaic_api::router_with(
        state,
        musaic_api::RouterOptions {
            launch: Some(launch),
        },
    )
    .layer(axum::middleware::from_fn_with_state(g, gate::guard));
    World { tmp, app, hooks }
}

fn request(
    method: Method,
    origin: Option<&str>,
    cookie: bool,
    body: &str,
    host: Option<&str>,
) -> Request<Body> {
    let mut b = Request::builder()
        .method(method)
        .uri(gate::CONNECT_CLAUDE_PATH)
        .header(
            header::HOST,
            host.map_or_else(|| format!("127.0.0.1:{PORT}"), str::to_string),
        );
    if let Some(o) = origin {
        b = b.header(header::ORIGIN, o);
    }
    if cookie {
        b = b.header(header::COOKIE, format!("{}={TOKEN}", gate::COOKIE));
    }
    b.body(Body::from(body.to_string())).unwrap()
}

fn good(body: &str) -> Request<Body> {
    request(
        Method::POST,
        Some(&format!("http://127.0.0.1:{PORT}")),
        true,
        body,
        None,
    )
}

async fn send(w: &World, r: Request<Body>) -> (StatusCode, String) {
    let resp = w.app.clone().oneshot(r).await.unwrap();
    let (parts, body) = resp.into_parts();
    assert!(parts.headers.contains_key(header::CONTENT_SECURITY_POLICY));
    (
        parts.status,
        String::from_utf8_lossy(&body.collect().await.unwrap().to_bytes()).into_owned(),
    )
}

fn temp_entries(w: &World) -> usize {
    std::fs::read_dir(w.tmp.path().join("tmproot"))
        .unwrap()
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_request_is_guarded_like_the_reveal_endpoints() {
    let w = world(true);
    let origin = format!("http://127.0.0.1:{PORT}");
    // No cookie.
    let (s, _) = send(&w, request(Method::POST, Some(&origin), false, "", None)).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // Wrong Host.
    let (s, _) = send(
        &w,
        request(
            Method::POST,
            Some(&origin),
            true,
            "",
            Some("evil.example:80"),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // No Origin, a foreign Origin.
    let (s, _) = send(&w, request(Method::POST, None, true, "", None)).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = send(
        &w,
        request(Method::POST, Some("http://evil.example"), true, "", None),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // GET.
    let (s, _) = send(&w, request(Method::GET, Some(&origin), true, "", None)).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // A body is refused.
    let (s, _) = send(&w, good("{\"path\":\"/etc/passwd\"}")).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(
        w.hooks.opened.lock().unwrap().is_empty(),
        "nothing was opened"
    );
    assert_eq!(temp_entries(&w), 0, "nothing was written");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_hook_gets_exactly_the_file_the_operation_wrote_and_the_answer_names_no_path() {
    let w = world(true);
    let (s, body) = send(&w, good("")).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["opened"], true);
    assert_eq!(v["shape"], "installed");
    assert_eq!(v["command"]["args"], serde_json::json!(["mcp"]));
    let opened = w.hooks.opened.lock().unwrap().clone();
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0].file_name().unwrap(), "Musaic.mcpb");
    assert_eq!(
        *w.hooks.existed.lock().unwrap(),
        [true],
        "written before it was opened"
    );
    assert_eq!(temp_entries(&w), 1, "the file stays for the host to read");
    let dir = opened[0]
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(
        !body.contains(".mcpb") && !body.contains(&dir),
        "no path to the generated file: {body}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_open_is_reported_and_the_file_removed() {
    let w = world(true);
    w.hooks
        .fails
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let (s, body) = send(&w, good("")).await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["opened"], false);
    assert_eq!(v["reason"], "open-failed");
    assert_eq!(v["message"], "no program opens it");
    assert_eq!(w.hooks.opened.lock().unwrap().len(), 1);
    assert_eq!(temp_entries(&w), 0, "removed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn not_ready_creates_nothing_and_opens_nothing() {
    let w = world(false);
    let (s, body) = send(&w, good("")).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        (v["opened"].clone(), v["reason"].clone()),
        (false.into(), "not-ready".into())
    );
    assert!(w.hooks.opened.lock().unwrap().is_empty());
    assert_eq!(temp_entries(&w), 0);
}

#[test]
fn the_marker_advertises_the_request() {
    let js = gate::init_script();
    assert!(js.contains("connectClaude: true"));
    let guard = js.find("location.hostname !== \"127.0.0.1\"").unwrap();
    assert!(guard < js.find("connectClaude").unwrap());
}

#[test]
fn a_shell_without_a_window_refuses_by_default() {
    struct Bare;
    impl Hooks for Bare {
        fn focus(&self) {}
        fn dialog(&self, _: gate::DialogKind, _: String) -> tokio::sync::oneshot::Receiver<bool> {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let _ = tx.send(false);
            rx
        }
    }
    assert!(
        Bare.open_bundle(std::path::Path::new("/nonexistent"))
            .is_err()
    );
}

/// A stand-in helper: a shell script in its own scratch folder that records
/// each argument on a line of `args`, writes them to stderr (which must not
/// reach the answer), then runs `then`.
#[cfg(unix)]
fn stand_in(dir: &std::path::Path, then: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let program = dir.join("open-stand-in");
    let args = dir.join("args");
    let script = format!(
        "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> '{}'; done\necho \"$@\" >&2\n{then}\n",
        args.display()
    );
    std::fs::write(&program, script).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    program
}

#[cfg(unix)]
fn recorded_args(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("args"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// The case of issue #388: the helper starts, finds no application for the
/// file and exits non-zero. The answer is `open-failed` with a fixed
/// sentence, and the file is removed.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_open_no_application_handled_is_reported_and_the_file_removed() {
    let w = world(true);
    let bin = tempfile::tempdir().unwrap();
    let program = stand_in(bin.path(), "exit 1");
    *w.hooks.program.lock().unwrap() = Some((program, Duration::from_secs(10)));
    let (s, body) = send(&w, good("")).await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["opened"], false);
    assert_eq!(v["reason"], "open-failed");
    assert_eq!(v["message"], open_file::NOT_HANDLED);
    let opened = w.hooks.opened.lock().unwrap().clone();
    assert_eq!(
        recorded_args(bin.path()),
        [opened[0].to_string_lossy().into_owned()],
        "the helper got the file as its one argument"
    );
    assert!(!body.contains(".mcpb"), "no path: {body}");
    assert_eq!(temp_entries(&w), 0, "removed");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_open_the_helper_reports_handled_keeps_the_file() {
    let w = world(true);
    let bin = tempfile::tempdir().unwrap();
    let program = stand_in(bin.path(), "exit 0");
    *w.hooks.program.lock().unwrap() = Some((program, Duration::from_secs(10)));
    let (s, body) = send(&w, good("")).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["opened"], true);
    assert_eq!(recorded_args(bin.path()).len(), 1);
    assert_eq!(temp_entries(&w), 1, "the file stays for the host to read");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_helper_that_cannot_start_is_reported_and_the_file_removed() {
    let w = world(true);
    let bin = tempfile::tempdir().unwrap();
    *w.hooks.program.lock().unwrap() =
        Some((bin.path().join("no-such-helper"), Duration::from_secs(10)));
    let (s, body) = send(&w, good("")).await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["reason"], "open-failed");
    assert_eq!(v["message"], open_file::COULD_NOT_START);
    assert_eq!(temp_entries(&w), 0, "removed");
}

/// A helper still running at the deadline counts as handled (an application
/// that takes long to start), and the wait is bounded.
#[cfg(unix)]
#[test]
fn a_slow_helper_is_not_waited_for_past_the_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let program = stand_in(dir.path(), "sleep 5");
    let file = dir.path().join("not-opened.txt");
    let started = std::time::Instant::now();
    let r = open_file::open_and_wait(&program, &file, Duration::from_millis(300));
    assert_eq!(r, Ok(open_file::Opened::StillRunning));
    assert!(started.elapsed() < Duration::from_secs(3), "bounded wait");
}

#[cfg(unix)]
#[test]
fn the_helper_gets_the_path_as_one_argument_without_a_shell() {
    let dir = tempfile::tempdir().unwrap();
    let program = stand_in(dir.path(), "exit 0");
    let file = dir.path().join("a file; with $(spaces) and `ticks`");
    let r = open_file::open_and_wait(&program, &file, Duration::from_secs(10));
    assert_eq!(r, Ok(open_file::Opened::Handled));
    assert_eq!(
        recorded_args(dir.path()),
        [file.to_string_lossy().into_owned()]
    );
}

#[test]
fn a_relative_path_is_refused_without_starting_anything() {
    let r = open_file::open_and_wait(
        std::path::Path::new("/nonexistent-helper"),
        std::path::Path::new("-a Calculator"),
        Duration::from_secs(1),
    );
    assert_eq!(r, Err(open_file::COULD_NOT_START.to_string()));
}
