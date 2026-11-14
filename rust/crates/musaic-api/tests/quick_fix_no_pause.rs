//! Quick-fix-all no longer sleeps a
//! fixed second between findings (the Python app's `await asyncio.sleep(1.0)`,
//! musaic/api/health.py:650-653); the MusicBrainz limiter paces the
//! requests. With the default app state (no test hook), a run over n open
//! findings with the network offline finishes in well under the n - 1
//! seconds the Python app's pauses alone took (a deliberate change).

mod common;

use std::time::{Duration, Instant};

use common::{Sandbox, call};
use musaic_api::router;
use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn quick_fix_all_does_not_pause_between_findings() {
    let sb = Sandbox::new();
    // Defaults except what keeps the run local (no subscribers, offline,
    // the Python app's no-key fingerprinting); the pause hook is the default.
    let st = sb.state_no_lifespan();
    let app = router(st.clone());
    let r = call(
        &app,
        "POST",
        "/api/config",
        Some(&json!({"library_dir": Sandbox::s(&sb.lib)})),
    )
    .await;
    assert_eq!(r.status, 200);
    let scan = call(&app, "GET", "/api/scan/stream", None).await;
    assert!(scan.text().contains("event: done"));
    // The check with the most open findings (offline, each fix fails or
    // falls through to the AI queue quickly).
    let mut best = (String::new(), 0usize);
    for check in ["unidentified-album", "no-cover-art", "track-gaps"] {
        let run = call(
            &app,
            "POST",
            &format!("/api/checks/{check}/run"),
            Some(&json!({"scope": "all"})),
        )
        .await;
        assert_eq!(run.status, 200, "{}", run.text());
        let n = call(
            &app,
            "GET",
            &format!("/api/checks/findings?check_id={check}"),
            None,
        )
        .await
        .json()["findings"]
            .as_array()
            .unwrap()
            .len();
        if n > best.1 {
            best = (check.to_string(), n);
        }
    }
    let (check, open) = best;
    assert!(
        open >= 2,
        "need two findings to have a gap between them: {open}"
    );

    let t = Instant::now();
    let got = call(
        &app,
        "POST",
        &format!("/api/checks/{check}/quick-fix-all"),
        None,
    )
    .await;
    let took = t.elapsed();
    assert_eq!(got.status, 200);
    let text = got.text();
    assert_eq!(text.matches("event: progress").count(), open, "{text}");
    assert!(text.contains("event: done"), "{text}");
    let python_pauses = Duration::from_secs((open - 1) as u64);
    assert!(
        took < python_pauses,
        "{check}: {open} findings took {took:?}; the Python app's pauses alone were {python_pauses:?}"
    );
}
