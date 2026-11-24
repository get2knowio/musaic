//! Connect Claude Desktop over HTTP (spec 007): the readiness report, the skill
//! archive and the diagnostics additions. The machine is a scratch one; no real
//! Claude directory is read, and no route here writes or opens anything.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use musaic_app::agent::claude_desktop::HostEnv;
use musaic_commands::claude_desktop::Platform;
use serde_json::Value;
use tower::ServiceExt;

struct World {
    _tmp: tempfile::TempDir,
    app: axum::Router,
    state: musaic_api::AppState,
}

fn world(config: Option<&str>) -> World {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let exe = root.join("app/Musaic");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(root.join("app/musaic-cli"), b"cli").unwrap();
    std::fs::create_dir_all(root.join("tmproot")).unwrap();
    let env = HostEnv {
        platform: Platform::Darwin,
        home: Some(root.join("home")),
        appdata: None,
        local_appdata: None,
        application_dirs: vec![root.join("Applications")],
        executable: Some(exe),
        temp_root: root.join("tmproot"),
        allow_temporary_program: false,
        non_default_config_dir: None,
        cleanup: false,
    };
    if let Some(text) = config {
        let file = env.config_file().unwrap();
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let state = musaic_api::AppState::builder(root.join("config"))
        .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
        .agent_secrets(Arc::new(musaic_secrets::MemoryStore::new()))
        .network(musaic_api::Network::Offline)
        .subscribers(false)
        .auto_snapshot(false)
        .check_sweep(false)
        .host_env(env)
        .build()
        .unwrap();
    World {
        _tmp: tmp,
        app: musaic_api::router(state.clone()),
        state,
    }
}

async fn send(
    app: &axum::Router,
    method: Method,
    path: &str,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, "127.0.0.1:8765")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let (parts, body) = resp.into_parts();
    (
        parts.status,
        parts.headers,
        body.collect().await.unwrap().to_bytes().to_vec(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_status_has_the_documented_shape() {
    let w = world(None);
    // Claude Desktop "found" through its configuration directory.
    let file: PathBuf = w
        ._tmp
        .path()
        .join("home/Library/Application Support/Claude");
    std::fs::create_dir_all(&file).unwrap();
    let (status, _, body) = send(&w.app, Method::GET, "/api/agent/claude-desktop").await;
    assert_eq!(status, StatusCode::OK);
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["platform"], "darwin");
    assert_eq!(v["bundleSupported"], true);
    assert_eq!(v["claudeFound"], true);
    assert_eq!(v["configEntry"], "absent");
    assert_eq!(v["extension"], "not-detected");
    assert_eq!(v["cli"]["state"], "found");
    assert_eq!(v["cli"]["command"]["args"], serde_json::json!(["mcp"]));
    assert!(
        v["cli"]["command"]["program"]
            .as_str()
            .unwrap()
            .ends_with("musaic-cli")
    );
    assert_eq!(v["shape"], "installed");
    assert_eq!(v["route"], "bundle");
    assert_eq!(
        v["setupCommand"]["args"],
        serde_json::json!(["setup", "--host", "claude-desktop"])
    );
    assert_eq!(v["uninstallCommand"], Value::Null);
    assert_eq!(v["appVersion"], env!("CARGO_PKG_VERSION"));
    // Reading changes nothing: no bundle was written.
    assert_eq!(
        std::fs::read_dir(w._tmp.path().join("tmproot"))
            .unwrap()
            .count(),
        0
    );
    // Not a hand-off: no body of this answer names the generated file.
    let text = String::from_utf8_lossy(&body);
    assert!(!text.contains(".mcpb") && !text.contains("musaic-connect"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_detected_extension_is_reported_and_does_not_close_the_route() {
    let w = world(None);
    let dir = w
        ._tmp
        .path()
        .join("home/Library/Application Support/Claude/Claude Extensions/local.mcpb.musaic.musaic");
    std::fs::create_dir_all(&dir).unwrap();
    let (_, _, body) = send(&w.app, Method::GET, "/api/agent/claude-desktop").await;
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["extension"], "detected");
    assert_eq!(v["route"], "bundle");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_entry_of_ours_gives_the_removal_command_and_the_other_route() {
    let w = world(Some(
        r#"{"mcpServers":{"musaic":{"command":"/x/musaic-cli","args":["mcp"]}}}"#,
    ));
    let (_, _, body) = send(&w.app, Method::GET, "/api/agent/claude-desktop").await;
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["configEntry"], "ours");
    assert_eq!(v["route"], "config-file-only");
    assert_eq!(
        v["uninstallCommand"]["args"],
        serde_json::json!(["uninstall", "--host", "claude-desktop"])
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_skill_archive_is_a_download_of_nine_files() {
    let w = world(None);
    let (status, headers, body) = send(&w.app, Method::GET, "/api/agent/skill-archive").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "application/zip");
    let version = musaic_commands::skill::bundle().version().to_string();
    assert_eq!(
        headers[header::CONTENT_DISPOSITION],
        format!("attachment; filename=\"musaic-work-delegation-{version}.zip\"").as_str()
    );
    // End of central directory: total entries.
    let eocd = body.windows(4).rposition(|w| w == b"PK\x05\x06").unwrap();
    let entries = u16::from_le_bytes([body[eocd + 10], body[eocd + 11]]);
    assert_eq!(entries, 9);
    let (_, _, again) = send(&w.app, Method::GET, "/api/agent/skill-archive").await;
    assert_eq!(body, again, "the same bytes");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_diagnostics_carry_the_program_version_and_the_app_version() {
    let w = world(None);
    let made = w
        .state
        .ops()
        .agent()
        .create_pairing("Claude", &[musaic_commands::PermissionKind::LibraryRead])
        .unwrap();
    let (_, _, body) = send(&w.app, Method::GET, "/api/agent/diagnostics").await;
    let v: Value = serde_json::from_slice(&body).unwrap();
    let item = &v[0];
    assert_eq!(item["pairingId"], made.pairing.id.as_str());
    assert_eq!(item["cliVersion"], Value::Null);
    assert_eq!(item["cliOlder"], Value::Null);
    assert_eq!(item["appVersion"], env!("CARGO_PKG_VERSION"));
}

#[test]
fn no_api_route_hands_a_file_to_the_system_or_returns_its_path() {
    let rows = musaic_api::route_table();
    let claude: Vec<String> = rows
        .iter()
        .filter(|r| {
            r.path.to_lowercase().contains("claude")
                || r.path.contains("connect")
                || r.path.contains("mcpb")
        })
        .map(|r| format!("{} {}", r.method, r.path))
        .collect();
    assert_eq!(
        claude,
        ["GET /api/agent/claude-desktop"],
        "only the read-only report"
    );
    assert!(rows.iter().all(|r| !(r.path.starts_with("/api/")
        && r.method == "POST"
        && r.path.contains("claude"))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_report_accepts_no_write() {
    let w = world(None);
    for m in [Method::POST, Method::PUT, Method::DELETE] {
        let (status, _, _) = send(&w.app, m, "/api/agent/claude-desktop").await;
        assert!(
            status == StatusCode::METHOD_NOT_ALLOWED || status == StatusCode::NOT_FOUND,
            "{status}"
        );
    }
}
