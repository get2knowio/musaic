//! The desktop app writes the same
//! rotating `<config>/server.log` the server binary writes, through the same
//! `musaic_api::logging::start_process_log`, and the app state logs into
//! it with secret values masked (a key written through `PATCH /api/settings`
//! never appears in the file).
//!
//! One test per binary: the process log is process-wide.

use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, header};
use http_body_util::BodyExt;
use musaic_desktop::options::{NetworkMode, Options};
use tower::ServiceExt;

#[tokio::test]
async fn desktop_writes_the_masked_server_log() {
    let dir = tempfile::tempdir().unwrap();
    let opts = Options {
        config_dir: dir.path().join("config"),
        port: 0,
        network: NetworkMode::Offline,
        frontend_dir: None,
        dev_memory_secrets: true,
        selftest_report: None,
        hidden: true,
    };
    let log = musaic_desktop::backend::start_log(&opts);
    let path = log.opened.clone().expect("log file opened");
    assert_eq!(path, opts.config_dir.join("server.log"));
    let state = musaic_desktop::backend::build_state(&opts, Some(&log)).unwrap();

    // A secret goes in through the API, then a line mentioning it is logged.
    let app = musaic_api::router(state.clone());
    let secret = "desk-secret-0123456789";
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::PATCH)
                .uri("/api/settings")
                .header(header::HOST, "127.0.0.1:8765")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(r#"{{"acoustid_api_key": "{secret}"}}"#)))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    state
        .log()
        .warn("musaic.desktop.test", &format!("saw {secret} here"));
    // A lower crate's line (musaic-log facade) reaches the same file.
    musaic_log::warning("musaic.scanner", &format!("lower crate {secret}"));
    assert!(log.log.flush(Duration::from_secs(5)));

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        text.contains("[INFO] musaic.desktop: musaic-desktop "),
        "{text}"
    );
    assert!(
        text.contains("[WARNING] musaic.desktop.test: saw <redacted> here"),
        "{text}"
    );
    assert!(
        text.contains("[WARNING] musaic.scanner: lower crate <redacted>"),
        "{text}"
    );
    assert!(!text.contains(secret), "{text}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    state.shutdown();
}
