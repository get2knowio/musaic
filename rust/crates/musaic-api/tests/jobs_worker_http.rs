//! The AI queue inside the app
//! with its dispatcher running (the lifespan's `worker_loop`), on the
//! fixture library (tests/common Sandbox), network offline:
//!
//! - The default provider ([`musaic_ai::Unconfigured`]) fails a metadata
//!   job the way the Python app's preflight fails when its provider cannot run
//!   (testdata/golden/ai/worker.json "provider-not-ready": status failed,
//!   no result), with this build's requirement / remedy (enumerated delta:
//!   the not-available message). Deliberate change: the queue is not paused, so the next job runs.
//! - A cover job with the default provider falls back to MusicBrainz, as the
//!   Python app's `suggest_cover` does on any provider error
//!   (worker.json "cover-job-provider-error-mb-fallback"); offline, the
//!   name search finds nothing, and the job completes with no cover (its
//!   notes open with the neutral prefix: no AI wording without a provider).
//! - The dispatcher wakes on an enqueue, a resume and a settings PATCH; the
//!   jobs stream follows every transition.
//! - Startup recovery: a row left `running` is requeued and run.
//! - Every writing AI route is refused with 409 during a restore, before
//!   its own 404 / 409 / 400; GET routes are not.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use common::{Sandbox, call};
use http_body_util::BodyExt;
use musaic_ai::Provider;
use musaic_ai::provider::ProviderEvent;
use musaic_ai::provider::replay::{Ending, ReplayProvider, Script};
use musaic_api::{AppState, Network, router};
use serde_json::{Value, json};
use tower::ServiceExt;

fn state(sb: &Sandbox, provider: Option<Arc<dyn Provider>>) -> AppState {
    let mut b = AppState::builder(&sb.cfg)
        .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
        .network(Network::Offline)
        .fingerprinting(common::no_key_fingerprinting())
        .subscribers(true)
        .auto_snapshot(false)
        .ping_interval(Duration::from_secs(30));
    if let Some(p) = provider {
        b = b.ai_provider(p);
    }
    b.build().unwrap()
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
    let s = call(app, "GET", "/api/scan/stream", None).await;
    assert!(s.text().contains("event: done"));
}

async fn job(app: &axum::Router, id: &str) -> Value {
    call(app, "GET", &format!("/api/ai/jobs/{id}"), None)
        .await
        .json()
}

async fn eventually(what: &str, mut f: impl AsyncFnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(20);
    while Instant::now() < end {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

async fn first_album(app: &axum::Router) -> (String, String) {
    let albums = call(app, "GET", "/api/albums", None).await.json();
    let a = &albums["albums"][0];
    (
        a["id"].as_str().unwrap().into(),
        a["dir_name"].as_str().unwrap().into(),
    )
}

// The Unconfigured provider. The Python app's recorded preflight
// failure is the reference shape.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unconfigured_provider_fails_like_the_python_apps_preflight() {
    let w = golden_ai("worker.json");
    let python = w["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "provider-not-ready")
        .unwrap();
    let sb = Sandbox::new();
    let st = state(&sb, None);
    assert!(st.ai_worker_running());
    let app = router(st.clone());
    configure(&app, &sb).await;
    let (id, _) = first_album(&app).await;
    let r = call(
        &app,
        "POST",
        "/api/ai/jobs",
        Some(&json!({"album_id": id, "job_type": "metadata", "attach_cover": false})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let jid = r.json()["id"].as_str().unwrap().to_string();
    let mut last = json!(null);
    eventually("failed", async || {
        last = job(&app, &jid).await;
        last["status"] == "failed"
    })
    .await;
    // The Python app's shape: failed, no result (nothing streamed), completed.
    assert_eq!(last["result"], python["job"]["result"]);
    assert_eq!(last["acted_on_action"], json!(null));
    assert!(last["completed_at"].is_string());
    // Enumerated delta:
    // no provider is ready, so instead of the Python app's template ("AI provider
    // '<id>' isn't ready — it needs <requirement>. <remedy>") the job fails
    // with this build's not-available message, which names no AI.
    let want = musaic_ai::provider::NOT_AVAILABLE.to_string();
    let python_err = python["job"]["error"].as_str().unwrap();
    assert!(python_err.starts_with("AI provider 'claude' isn't ready \u{2014} it needs "));
    assert_eq!(last["error"], json!(want));
    // The job needed a model
    // and this build has no backend, so the queue is NOT paused (the Python app
    // paused: worker.json "provider-not-ready"); the next job runs at once
    // and fails the same way.
    let s = call(&app, "GET", "/api/settings", None).await.json();
    assert_eq!(
        s["ai_queue_paused"], false,
        "a job needing a model does not pause the queue"
    );
    let albums = call(&app, "GET", "/api/albums", None).await.json();
    let other = albums["albums"][1]["id"].as_str().unwrap().to_string();
    assert_ne!(other, id);
    let r2 = call(
        &app,
        "POST",
        "/api/ai/jobs",
        Some(&json!({"album_id": other, "job_type": "metadata", "attach_cover": false})),
    )
    .await;
    let jid2 = r2.json()["id"].as_str().unwrap().to_string();
    let mut second = json!(null);
    eventually("second failed", async || {
        second = job(&app, &jid2).await;
        second["status"] == "failed"
    })
    .await;
    assert_eq!(second["error"], json!(want));
    let s = call(&app, "GET", "/api/settings", None).await.json();
    assert_eq!(s["ai_queue_paused"], false, "still not paused");
    st.shutdown();
    assert!(!st.ai_worker_running());
}

fn golden_ai(name: &str) -> Value {
    let p = musaic_testdata::public("golden/ai").join(name);
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

// A cover job with the Unconfigured provider takes the
// Python app's MusicBrainz fallback (ai.py:1376-1384) and completes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unconfigured_cover_job_falls_back_to_musicbrainz() {
    let w = golden_ai("worker.json");
    let python = w["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "cover-job-provider-error-mb-fallback")
        .unwrap();
    assert!(
        python["job"]["result"]["cover"]["notes"]
            .as_str()
            .unwrap()
            .starts_with(musaic_ai::worker::FALLBACK_COVER_PREFIX)
    );
    // No provider is ready, so the
    // Python app's prefix (which names Claude) reads as the neutral one.
    let prefix = musaic_ai::worker::FALLBACK_COVER_PREFIX_NEUTRAL;
    let sb = Sandbox::new();
    let st = state(&sb, None);
    let app = router(st.clone());
    configure(&app, &sb).await;
    let (id, _) = first_album(&app).await;
    let r = call(
        &app,
        "POST",
        "/api/ai/jobs",
        Some(&json!({"album_id": id, "job_type": "cover"})),
    )
    .await;
    let jid = r.json()["id"].as_str().unwrap().to_string();
    let mut last = json!(null);
    eventually("completed", async || {
        last = job(&app, &jid).await;
        last["status"] == "completed" || last["status"] == "failed"
    })
    .await;
    assert_eq!(last["status"], "completed", "{last}");
    let cover = &last["result"]["cover"];
    assert_eq!(cover["cover_url"], json!(null), "offline: no match");
    assert!(
        cover["notes"].as_str().unwrap().starts_with(prefix),
        "{cover}"
    );
    let q = call(&app, "POST", "/api/ai/queue/resume", None)
        .await
        .json();
    assert_eq!(q["queue_state"]["queued"], 0);
    st.shutdown();
}

fn ok(text: &str) -> Script {
    Script {
        events: vec![ProviderEvent::TurnComplete { text: text.into() }],
        ending: Ending::Done,
    }
}

async fn read_frame(body: &mut Body) -> String {
    let mut buf = String::new();
    loop {
        let f = tokio::time::timeout(Duration::from_secs(20), body.frame())
            .await
            .expect("frame")
            .unwrap()
            .unwrap();
        if let Ok(d) = f.into_data() {
            buf.push_str(&String::from_utf8_lossy(&d));
        }
        if buf.ends_with("\n\n") {
            return buf;
        }
    }
}

// The dispatcher is woken by an enqueue, stays put
// while paused, resumes on POST /api/ai/queue/resume and on a settings
// PATCH; the stream shows each step.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dispatcher_wakes_on_enqueue_resume_and_settings() {
    let sb = Sandbox::new();
    let replay = ReplayProvider::new("claude");
    let st = state(&sb, Some(Arc::new(replay.clone())));
    let app = router(st.clone());
    configure(&app, &sb).await;
    let albums = call(&app, "GET", "/api/albums", None).await.json();
    let ids: Vec<String> = albums["albums"]
        .as_array()
        .unwrap()
        .iter()
        .take(3)
        .map(|a| a["id"].as_str().unwrap().to_string())
        .collect();

    let resp = app
        .clone()
        .oneshot(common::get("/api/ai/jobs/stream"))
        .await
        .unwrap();
    let mut body = resp.into_body();
    let first = read_frame(&mut body).await;
    assert!(
        first.starts_with("event: jobs\ndata: {\"jobs\": []"),
        "{first}"
    );

    // 1. A failure pauses the queue.
    replay.push(Script {
        events: vec![],
        ending: Ending::Fail(musaic_ai::ProviderError::Transient("503".into())),
    });
    let a = call(
        &app,
        "POST",
        "/api/ai/jobs",
        Some(&json!({"album_id": ids[0], "job_type": "metadata"})),
    )
    .await
    .json()["id"]
        .as_str()
        .unwrap()
        .to_string();
    eventually("a failed", async || {
        job(&app, &a).await["status"] == "failed"
    })
    .await;
    let mut saw_paused = false;
    for _ in 0..10 {
        let f = read_frame(&mut body).await;
        if f.contains("\"paused\": true") && f.contains("\"status\": \"failed\"") {
            saw_paused = true;
            break;
        }
    }
    assert!(saw_paused, "the stream shows the failure and the pause");

    // 2. Paused: a new job waits.
    replay.push(ok(r#"{"suggestions": {"genre": "Pop"}}"#));
    let b = call(
        &app,
        "POST",
        "/api/ai/jobs",
        Some(&json!({"album_id": ids[1], "job_type": "metadata"})),
    )
    .await
    .json()["id"]
        .as_str()
        .unwrap()
        .to_string();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(job(&app, &b).await["status"], "queued");

    // 3. Resume: dispatched at once (not on the 30 s idle wake).
    let t = Instant::now();
    assert_eq!(
        call(&app, "POST", "/api/ai/queue/resume", None)
            .await
            .status,
        200
    );
    eventually("b completed", async || {
        job(&app, &b).await["status"] == "completed"
    })
    .await;
    assert!(t.elapsed() < Duration::from_secs(10));

    // 4. Pause through settings, enqueue, unpause through settings.
    let r = call(
        &app,
        "PATCH",
        "/api/settings",
        Some(&json!({"ai_queue_paused": true})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    replay.push(ok("{}"));
    let c = call(
        &app,
        "POST",
        "/api/ai/jobs",
        Some(&json!({"album_id": ids[2], "job_type": "metadata"})),
    )
    .await
    .json()["id"]
        .as_str()
        .unwrap()
        .to_string();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(job(&app, &c).await["status"], "queued");
    let r = call(
        &app,
        "PATCH",
        "/api/settings",
        Some(&json!({"ai_queue_paused": false})),
    )
    .await;
    assert_eq!(r.status, 200);
    eventually("c completed", async || {
        job(&app, &c).await["status"] == "completed"
    })
    .await;
    assert_eq!(replay.pending(), 0);
    drop(body);
    st.shutdown();
    assert_eq!(st.gate().snapshot(), (0, false));
}

// Worker.py:498-505: a job left `running` (an unclean shutdown) is
// requeued when the app starts and then dispatched.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn startup_recovers_jobs_left_running() {
    let sb = Sandbox::new();
    let replay = ReplayProvider::new("claude");
    let jid;
    {
        let st = AppState::builder(&sb.cfg)
            .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
            .network(Network::Offline)
            .fingerprinting(common::no_key_fingerprinting())
            .subscribers(false)
            .auto_snapshot(false)
            .build()
            .unwrap();
        let app = router(st.clone());
        configure(&app, &sb).await;
        let (id, _) = first_album(&app).await;
        let r = call(
            &app,
            "POST",
            "/api/ai/jobs",
            Some(&json!({"album_id": id, "job_type": "metadata"})),
        )
        .await;
        jid = r.json()["id"].as_str().unwrap().to_string();
        let conn = musaic_db::open_connection(st.db_path()).unwrap();
        musaic_db::ai_jobs::mark_running(&conn, &jid, "2001-01-01T00:00:00Z").unwrap();
    }
    replay.push(ok("{}"));
    let st = state(&sb, Some(Arc::new(replay.clone())));
    let app = router(st.clone());
    eventually("recovered and run", async || {
        job(&app, &jid).await["status"] == "completed"
    })
    .await;
    let j = job(&app, &jid).await;
    assert_ne!(j["started_at"], "2001-01-01T00:00:00Z");
    st.shutdown();
}

/// The writing AI routes, with the requests the restore-gate inventory
/// (tests/deliberate.rs `gate_probe`) should send: unknown ids, so a 409
/// also shows the busy answer comes before the handler's own 404.
fn writing_requests() -> Vec<(&'static str, String, Option<Value>)> {
    let unknown = "0000000000000000";
    vec![
        (
            "POST",
            "/api/ai/jobs".into(),
            Some(json!({"album_id": unknown, "job_type": "metadata"})),
        ),
        ("DELETE", "/api/ai/jobs?status=failed".into(), None),
        ("DELETE", format!("/api/ai/jobs/{unknown}"), None),
        (
            "POST",
            format!("/api/ai/jobs/{unknown}/acted-on"),
            Some(json!({"action": "applied"})),
        ),
        ("POST", format!("/api/ai/jobs/{unknown}/retry"), None),
        ("POST", "/api/ai/queue/resume".into(), None),
    ]
}

// Every writing AI route is refused with 409
// while a restore holds the gate; the reads are not; nothing changes. After
// the restore the same requests get their own answers (404s, 200).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ai_routes_take_the_restore_gate() {
    let sb = Sandbox::new();
    let st = sb.state_no_lifespan();
    let app = router(st.clone());
    configure(&app, &sb).await;
    let (id, _) = first_album(&app).await;
    let r = call(
        &app,
        "POST",
        "/api/ai/jobs",
        Some(&json!({"album_id": id, "job_type": "metadata"})),
    )
    .await;
    assert_eq!(r.status, 200);
    let before = call(&app, "GET", "/api/ai/jobs", None).await.text();
    let restore = st.gate().begin_restore().unwrap();
    for (m, p, b) in writing_requests() {
        let got = call(&app, m, &p, b.as_ref()).await;
        assert_eq!(got.status, 409, "{m} {p}: {}", got.text());
        assert_eq!(
            got.json()["detail"],
            musaic_api::gate::WORK_REFUSED,
            "{m} {p}"
        );
    }
    for p in [
        "/api/ai/jobs",
        "/api/ai/providers",
        &format!("/api/ai/jobs/active/{id}"),
    ] {
        assert_eq!(call(&app, "GET", p, None).await.status, 200, "{p}");
    }
    // 422 (request validation) still comes first.
    let got = call(&app, "DELETE", "/api/ai/jobs", None).await;
    assert_eq!(got.status, 422);
    assert_eq!(call(&app, "GET", "/api/ai/jobs", None).await.text(), before);
    drop(restore);
    let want = [404, 200, 404, 404, 404, 200];
    for ((m, p, b), w) in writing_requests().into_iter().zip(want) {
        let got = call(&app, m, &p, b.as_ref()).await;
        assert_eq!(got.status, w, "{m} {p}: {}", got.text());
    }
}
