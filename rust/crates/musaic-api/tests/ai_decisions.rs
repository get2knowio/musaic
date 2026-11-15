//! Deliberate changes to the AI queue routes: a check fix that enqueues a
//! job notifies the jobs stream and the worker; retry is refused while the
//! album has another active job; retry clears the failure excerpt. The
//! Python app's behaviour each replaces is in testdata/golden/ai/http.json
//! (`retry C failed`: a second queued job for the album, the excerpt kept)
//! and musaic/checks/no_cover_art.py (enqueue without `notify_jobs_changed`).

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use common::{Sandbox, call};
use http_body_util::BodyExt;
use musaic_api::{AppState, Network};
use serde_json::{Value, json};
use tower::ServiceExt;

const NO_COVER: &str = "Aurora Fields - Second Light [FLAC]";

fn state(sb: &Sandbox) -> AppState {
    AppState::builder(&sb.cfg)
        .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
        .network(Network::Offline)
        .fingerprinting(common::no_key_fingerprinting())
        .subscribers(false)
        .auto_snapshot(false)
        .ping_interval(Duration::from_secs(600))
        // A provider with a model backend (never called: no worker runs);
        // Without one a check fix queues no job.
        .ai_provider(Arc::new(musaic_ai::provider::replay::ReplayProvider::new(
            "claude",
        )))
        .build()
        .unwrap()
}

async fn scanned(sb: &Sandbox) -> (AppState, axum::Router, String) {
    let st = state(sb);
    let app = musaic_api::router(st.clone());
    let r = call(
        &app,
        "POST",
        "/api/config",
        Some(&json!({"library_dir": Sandbox::s(&sb.lib)})),
    )
    .await;
    assert_eq!(r.status, 200);
    assert!(
        call(&app, "GET", "/api/scan/stream", None)
            .await
            .text()
            .contains("event: done")
    );
    let albums = call(&app, "GET", "/api/albums", None).await.json();
    let id = albums["albums"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["path"].as_str().unwrap().ends_with(NO_COVER))
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    (st, app, id)
}

/// The next `event: jobs` frame within `wait`, skipping comments.
async fn next_jobs_frame(body: &mut Body, wait: Duration) -> Option<String> {
    let deadline = tokio::time::Instant::now() + wait;
    let mut buf = String::new();
    loop {
        let left = deadline.checked_duration_since(tokio::time::Instant::now())?;
        let f = tokio::time::timeout(left, body.frame()).await.ok()??.ok()?;
        if let Ok(d) = f.into_data() {
            buf.push_str(&String::from_utf8_lossy(&d));
        }
        while let Some(end) = buf.find("\n\n") {
            let frame: String = buf.drain(..end + 2).collect();
            if frame.starts_with("event: jobs") {
                return Some(frame);
            }
        }
    }
}

/// The fix's job shows up on the jobs stream at once (the Python app
/// sent nothing until something else notified).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn check_fix_enqueue_notifies_jobs_stream() {
    let sb = Sandbox::new();
    let (_st, app, album) = scanned(&sb).await;
    let run = call(
        &app,
        "POST",
        "/api/checks/no-cover-art/run",
        Some(&json!({"scope": "all"})),
    )
    .await;
    assert_eq!(run.status, 200, "{}", run.text());
    let findings = call(&app, "GET", "/api/checks/findings", None).await.json();
    let f = findings["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["check_id"] == "no-cover-art" && f["album_id"] == album.as_str())
        .expect("a no-cover-art finding")
        .clone();
    let resp = app
        .clone()
        .oneshot(common::get("/api/ai/jobs/stream"))
        .await
        .unwrap();
    let mut body = resp.into_body();
    let first = next_jobs_frame(&mut body, Duration::from_secs(5))
        .await
        .unwrap();
    assert!(first.contains("\"jobs\": []"), "{first}");
    // A hint goes straight to the AI job (no MusicBrainz attempt).
    let fix = call(
        &app,
        "POST",
        &format!("/api/checks/no-cover-art/findings/{}/fix", f["id"]),
        Some(&json!({"hint": "the blue one"})),
    )
    .await;
    assert_eq!(fix.status, 200, "{}", fix.text());
    let job_id = fix.json()["job_id"]
        .as_str()
        .expect("job enqueued")
        .to_string();
    let next = next_jobs_frame(&mut body, Duration::from_secs(3))
        .await
        .expect("the stream wakes for the fix's job");
    assert!(next.contains(&job_id), "{next}");
}

fn enqueue_failed(st: &AppState, album: &str, n: u32) -> String {
    let mut conn = musaic_db::open_connection(st.db_path()).unwrap();
    let id = format!("job-{n}");
    let out = musaic_db::ai_jobs::enqueue(
        &mut conn,
        &musaic_db::ai_jobs::NewJob {
            album_id: album,
            album_path: "/p",
            album_title: "t",
            job_type: "cover",
            hint: "",
            attach_cover: true,
        },
        || id.clone(),
        "2026-01-01T00:00:01Z",
    )
    .unwrap()
    .unwrap();
    musaic_db::ai_jobs::mark_failed(
        &conn,
        &out.id,
        "boom",
        Some("{\"failure_excerpt\": \"Failed to authenticate\"}"),
        "2026-01-01T00:00:02Z",
    )
    .unwrap();
    out.id
}

/// A retried job no longer shows the old failure excerpt.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retry_clears_failure_excerpt() {
    let sb = Sandbox::new();
    let (st, app, album) = scanned(&sb).await;
    let job = enqueue_failed(&st, &album, 1);
    let before = call(&app, "GET", &format!("/api/ai/jobs/{job}"), None)
        .await
        .json();
    assert_eq!(
        before["result"],
        json!({"failure_excerpt": "Failed to authenticate"})
    );
    let r = call(&app, "POST", &format!("/api/ai/jobs/{job}/retry"), None).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let j = r.json();
    assert_eq!(j["status"], "queued");
    assert_eq!(j["error"], Value::Null);
    assert_eq!(j["result"], Value::Null, "retry clears the previous result");
    let row = musaic_db::ai_jobs::get_job(&musaic_db::open_connection(st.db_path()).unwrap(), &job)
        .unwrap()
        .unwrap();
    assert_eq!(row.result_json, None);
}

/// Retry answers 409 (the enqueue conflict's shape) while the
/// album has another active job, and changes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retry_refused_with_another_active_job() {
    let sb = Sandbox::new();
    let (st, app, album) = scanned(&sb).await;
    let failed = enqueue_failed(&st, &album, 1);
    let r = call(
        &app,
        "POST",
        "/api/ai/jobs",
        Some(&json!({"album_id": album, "job_type": "metadata"})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let active = r.json()["id"].as_str().unwrap().to_string();
    let r = call(&app, "POST", &format!("/api/ai/jobs/{failed}/retry"), None).await;
    assert_eq!(r.status, 409, "{}", r.text());
    assert_eq!(
        r.json(),
        json!({"detail": {
            "error": "active_job_exists",
            "message": format!("Active job {active} (queued) already exists for this album"),
            "existing_job_id": active,
            "existing_status": "queued",
        }})
    );
    let row =
        musaic_db::ai_jobs::get_job(&musaic_db::open_connection(st.db_path()).unwrap(), &failed)
            .unwrap()
            .unwrap();
    assert_eq!(row.status, "failed");
    assert!(row.result_json.is_some(), "nothing changed");
    // Once the other job is gone, the retry goes through.
    let d = call(&app, "DELETE", &format!("/api/ai/jobs/{active}"), None).await;
    assert_eq!(d.status, 200, "{}", d.text());
    let r = call(&app, "POST", &format!("/api/ai/jobs/{failed}/retry"), None).await;
    assert_eq!(r.status, 200, "{}", r.text());
}
