//! `POST /api/scan/cancel` and the app's shutdown over a running scan
//! (issue #193), on a copy of the synthetic library. Not a route of the
//! Python app (`EXTRA_ROUTES`), so nothing is recorded: the contract is the
//! scan service's (musaic-services `src/scan.rs`, "Cancelling").

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use common::{Sandbox, call};
use musaic_api::{AppState, Fingerprinting, Network, router};
use musaic_events::{Event, Subscriber, Topic};
use musaic_services::ScanService;
use musaic_services::fingerprint::{CancelToken, Fingerprinter};
use musaic_services::fingerprint_pool::{PoolSleeper, PoolStatus};
use serde_json::json;

struct Fixed;

impl Fingerprinter for Fixed {
    fn fingerprint(&self, _path: &std::path::Path) -> Result<(String, u64), String> {
        Ok(("AQAAstub".into(), 3))
    }
    fn implementation(&self) -> &str {
        musaic_db::FINGERPRINT_IMPL_RUST
    }
}

struct NoSleep;

impl PoolSleeper for NoSleep {
    fn sleep(&self, _d: Duration, cancel: &CancelToken) -> bool {
        !cancel.is_cancelled()
    }
}

/// A state whose scans park in their first album until a cancel is
/// requested (while `park` is set), so the cancel lands mid-scan without
/// any timing assumption. Records the scan-end bus events.
struct Rig {
    st: AppState,
    app: axum::Router,
    ends: Arc<Mutex<Vec<String>>>,
    park: Arc<AtomicUsize>,
}

fn rig(sb: &Sandbox) -> Rig {
    let svc: Arc<OnceLock<ScanService>> = Arc::default();
    let park = Arc::new(AtomicUsize::new(1));
    let (s2, p2) = (Arc::clone(&svc), Arc::clone(&park));
    let st = AppState::builder(&sb.cfg)
        .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
        .network(Network::Offline)
        .frontend_dir(None)
        .subscribers(false)
        .auto_snapshot(false)
        .fingerprinting(Fingerprinting::Local(Arc::new(Fixed)))
        .pool_sleeper(Arc::new(NoSleep))
        .on_scan_album(Arc::new(move |_album, _fresh| {
            if p2.load(Ordering::SeqCst) == 0 {
                return;
            }
            let svc = s2.get().expect("scan service set");
            let deadline = Instant::now() + Duration::from_secs(30);
            while !svc.state().cancel_requested {
                assert!(Instant::now() < deadline, "no cancel arrived");
                std::thread::sleep(Duration::from_millis(2));
            }
        }))
        .build()
        .unwrap();
    svc.set(st.scan().clone()).ok().unwrap();
    let ends: Arc<Mutex<Vec<String>>> = Arc::default();
    let e2 = Arc::clone(&ends);
    st.bus().register(
        Topic::Event,
        Subscriber::from_fn(move |ev| {
            match ev {
                Event::LibraryScanCompleted { .. } => {
                    e2.lock().unwrap().push("completed".into());
                }
                Event::LibraryScanStopped { reason, .. } => {
                    e2.lock().unwrap().push(format!("stopped:{reason}"));
                }
                _ => {}
            }
            Ok(())
        }),
    );
    let app = router(st.clone());
    Rig {
        st,
        app,
        ends,
        park,
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
    assert_eq!(r.status, 200, "{}", r.text());
}

async fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    // Generous: only a condition that never comes true should fail here.
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_with_no_scan_answers_not_running() {
    let sb = Sandbox::new();
    let r = rig(&sb);
    let got = call(&r.app, "POST", "/api/scan/cancel", None).await;
    assert_eq!(got.status, 200, "{}", got.text());
    let v = got.json();
    assert_eq!(v["outcome"], "not_running");
    assert_eq!(v["scan"]["status"], "idle");
    // POST only: a GET falls through to the `/api` JSON 404, as every
    // POST-only route does here (the Python app's catch-all routing).
    let got = call(&r.app, "GET", "/api/scan/cancel", None).await;
    assert_eq!(got.status, 404);
}

/// The ribbon's Cancel: the stream ends with `cancelling` + `cancelled`
/// (never `done`), the status says `cancelled`, the bus gets
/// `LibraryScanStopped(cancelled)` and no completion, no fingerprint pass
/// starts, the restore gate is released, and nothing was saved (the
/// library has no snapshot yet). The next scan runs to `done`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_from_the_route_ends_the_scan_without_saving() {
    let sb = Sandbox::new();
    let r = rig(&sb);
    configure(&r.app, &sb).await;
    let app = r.app.clone();
    let stream = tokio::spawn(async move { call(&app, "GET", "/api/scan/stream", None).await });
    let st = r.st.clone();
    eventually("the first album", || st.scan().state().scanned >= 1).await;

    let got = call(&r.app, "POST", "/api/scan/cancel", None).await;
    assert_eq!(got.status, 200, "{}", got.text());
    let v = got.json();
    assert_eq!(v["outcome"], "requested", "{v}");
    assert_eq!(v["scan"]["status"], "cancelling", "{v}");

    let text = tokio::time::timeout(Duration::from_secs(30), stream)
        .await
        .expect("the stream ends")
        .unwrap()
        .text();
    assert!(text.contains("event: cancelling\n"), "{text}");
    assert!(text.contains("event: cancelled\n"), "{text}");
    assert!(!text.contains("event: done\n"), "{text}");
    assert!(
        text.trim_end()
            .rsplit("\n\n")
            .next()
            .unwrap()
            .starts_with("event: cancelled"),
        "cancelled is the last event: {text}"
    );

    let s = call(&r.app, "GET", "/api/scan/status", None).await.json();
    assert_eq!(s["status"], "cancelled", "{s}");
    assert_eq!(s["scanned"], 1, "{s}");
    let st = r.st.clone();
    eventually("guard released", || st.gate().snapshot() == (0, false)).await;
    let ends = Arc::clone(&r.ends);
    eventually("scan end published", || !ends.lock().unwrap().is_empty()).await;
    assert_eq!(*r.ends.lock().unwrap(), ["stopped:cancelled"]);
    assert_eq!(r.st.pool().state().status, PoolStatus::Idle, "no pool pass");
    let albums = call(&r.app, "GET", "/api/albums", None).await.json();
    assert_eq!(albums["count"], 0, "nothing saved: {albums}");

    // Too late to cancel a finished scan.
    let v = call(&r.app, "POST", "/api/scan/cancel", None).await.json();
    assert_eq!(v["outcome"], "not_running");
    assert_eq!(v["scan"]["status"], "cancelled");

    // The next scan runs normally.
    r.park.store(0, Ordering::SeqCst);
    let text = call(&r.app, "GET", "/api/scan/stream", None).await.text();
    assert!(text.contains("event: done\n"), "{text}");
    let s = call(&r.app, "GET", "/api/scan/status", None).await.json();
    assert_eq!(s["status"], "done");
    let ends = Arc::clone(&r.ends);
    eventually("completion published", || ends.lock().unwrap().len() == 2).await;
    assert_eq!(*r.ends.lock().unwrap(), ["stopped:cancelled", "completed"]);
    let albums = call(&r.app, "GET", "/api/albums", None).await.json();
    assert!(albums["count"].as_u64().unwrap() > 1, "{albums}");
}

/// The app's shutdown (the desktop quit path ends here) cancels a running
/// scan through the same cancel and waits for it, instead of leaving it
/// running while the database is checkpointed and the process exits.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_cancels_a_running_scan() {
    let sb = Sandbox::new();
    let r = rig(&sb);
    configure(&r.app, &sb).await;
    let app = r.app.clone();
    let stream = tokio::spawn(async move { call(&app, "GET", "/api/scan/stream", None).await });
    let st = r.st.clone();
    eventually("the first album", || st.scan().state().scanned >= 1).await;

    let st = r.st.clone();
    let report = tokio::task::spawn_blocking(move || st.shutdown())
        .await
        .unwrap();
    assert!(report.scan_stopped, "{report:?}");
    assert_eq!(r.st.scan().state().status.as_str(), "cancelled");
    assert_eq!(*r.ends.lock().unwrap(), ["stopped:cancelled"]);
    assert_eq!(
        r.st.pool().state().status,
        PoolStatus::Idle,
        "no pool pass started by the cancelled scan"
    );
    let text = tokio::time::timeout(Duration::from_secs(30), stream)
        .await
        .expect("the stream ends")
        .unwrap()
        .text();
    assert!(text.contains("event: cancelled\n"), "{text}");
}
