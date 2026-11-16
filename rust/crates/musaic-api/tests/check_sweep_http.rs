//! The background check sweep in the app (issue #187), on a copy of the
//! synthetic library, network offline: after a scan every registered check
//! has run on its own (including the ones that do not run during a scan)
//! and recorded its inputs for every Present album; a wake with nothing
//! changed runs nothing; a template change reruns only filename-mismatch;
//! `GET /api/checks/sweep` and the checks stream report progress; a
//! snapshot restore is not refused because of the sweep; shutdown stops it.
//! Throughout, no file of the library is written.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{Sandbox, call, no_key_fingerprinting};
use http_body_util::BodyExt;
use musaic_api::{AppState, Network, router};
use musaic_services::check_sweep::SweepStatus;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Every file under `dir`: relative path -> (size, mtime, SHA-256).
fn tree(dir: &Path) -> BTreeMap<PathBuf, (u64, std::time::SystemTime, Vec<u8>)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            let m = e.metadata().unwrap();
            if m.is_dir() {
                stack.push(p);
            } else {
                let bytes = std::fs::read(&p).unwrap();
                out.insert(
                    p.strip_prefix(dir).unwrap().to_path_buf(),
                    (
                        m.len(),
                        m.modified().unwrap(),
                        Sha256::digest(&bytes).to_vec(),
                    ),
                );
            }
        }
    }
    out
}

fn state(sb: &Sandbox) -> AppState {
    AppState::builder(&sb.cfg)
        .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
        .network(Network::Offline)
        .fingerprinting(no_key_fingerprinting())
        .frontend_dir(None)
        .auto_snapshot(false)
        .check_sweep(true)
        .sweep_settle(Duration::from_millis(50))
        .build()
        .unwrap()
}

async fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

/// Wait until the sweep has nothing planned or running for a while (and
/// the bus-driven reruns are done).
async fn quiet(st: &AppState) {
    let mut since = Instant::now();
    eventually("the sweep to go quiet", || {
        let busy = st.sweeper().is_busy() || st.sweep_should_wait();
        if busy {
            since = Instant::now();
        }
        since.elapsed() > Duration::from_millis(300)
    })
    .await;
    assert!(st.bus().wait_background_idle(Duration::from_secs(30)));
}

fn conn(st: &AppState) -> rusqlite::Connection {
    musaic_db::open_connection(st.db_path()).unwrap()
}

fn runs(st: &AppState) -> Vec<(i64, String, String, String)> {
    let c = conn(st);
    let mut s = c
        .prepare("SELECT id, check_id, scope, status FROM check_runs ORDER BY id")
        .unwrap();
    s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// check id -> albums with an inputs row ("" = the library row).
fn inputs(st: &AppState) -> BTreeMap<String, Vec<String>> {
    let c = conn(st);
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut s = c
        .prepare("SELECT check_id, album_id FROM check_inputs ORDER BY check_id, album_id")
        .unwrap();
    for row in s
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .unwrap()
    {
        let (c, a) = row.unwrap();
        out.entry(c).or_default().push(a);
    }
    out
}

fn present_albums(st: &AppState) -> Vec<String> {
    let c = conn(st);
    let mut s = c
        .prepare("SELECT id FROM albums WHERE missing_since IS NULL ORDER BY id")
        .unwrap();
    s.query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_check_runs_on_its_own_and_the_library_is_never_written() {
    let sb = Sandbox::new();
    let files_before = tree(&sb.lib);
    let st = state(&sb);
    let app = router(st.clone());

    // Nothing configured: the sweep is idle.
    let r = call(&app, "GET", "/api/checks/sweep", None).await;
    assert_eq!(r.status, 200);
    assert_eq!(
        r.json(),
        json!({"status": "idle", "total": 0, "completed": 0, "current_check": null,
               "started_at": null, "completed_at": null})
    );

    let r = call(
        &app,
        "POST",
        "/api/config",
        Some(&json!({"library_dir": Sandbox::s(&sb.lib)})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let r = call(&app, "GET", "/api/scan/stream", None).await;
    assert_eq!(r.status, 200);
    quiet(&st).await;

    // Every registered check ran and recorded its inputs: per album for
    // the per-album checks, once for the library-wide ones.
    let albums = present_albums(&st);
    assert!(albums.len() >= 10, "{albums:?}");
    let got = inputs(&st);
    let library_wide = [
        "inconsistent-artist-sort",
        "no-cover-art",
        "unidentified-album",
    ];
    let per_album = [
        "duplicate-tracks",
        "filename-mismatch",
        "tags-out-of-sync",
        "track-gaps",
    ];
    for c in library_wide {
        assert_eq!(got.get(c), Some(&vec![String::new()]), "{c}");
    }
    for c in per_album {
        assert_eq!(got.get(c), Some(&albums), "{c}");
    }
    let all_runs = runs(&st);
    for c in library_wide.iter().chain(&per_album) {
        assert!(
            all_runs
                .iter()
                .any(|(_, id, _, status)| id == c && status == "completed"),
            "{c} never completed a run"
        );
    }
    let sweep = st.sweeper().state();
    assert_eq!(sweep.status, SweepStatus::Idle);
    assert!(sweep.completed_at.is_some());

    // Nothing changed: a wake runs nothing.
    let n = all_runs.len();
    st.sweeper().wake();
    quiet(&st).await;
    assert_eq!(runs(&st).len(), n, "a pass with nothing stale ran checks");

    // A template change: filename-mismatch reruns over every album, and
    // nothing else does.
    let r = call(
        &app,
        "PATCH",
        "/api/settings",
        Some(&json!({"filename_template_single_artist": "{tracknumber:02} {title}"})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    quiet(&st).await;
    let new: Vec<_> = runs(&st).into_iter().skip(n).collect();
    assert!(!new.is_empty(), "no rerun after a template change");
    assert!(
        new.iter().all(|(_, id, _, _)| id == "filename-mismatch"),
        "{new:?}"
    );
    let rerun: usize = new
        .iter()
        .map(|(_, _, scope, _)| {
            serde_json::from_str::<Vec<String>>(scope)
                .map(|v| v.len())
                .unwrap_or(0)
        })
        .sum();
    assert_eq!(rerun, albums.len());

    // The checks stream: the summary, then the sweep's state.
    let resp = tower::ServiceExt::oneshot(app.clone(), common::get("/api/checks/stream"))
        .await
        .unwrap();
    let mut body = resp.into_body();
    let mut text = String::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !text.contains("event: sweep") && Instant::now() < deadline {
        if let Some(Ok(f)) = body.frame().await
            && let Ok(d) = f.into_data()
        {
            text.push_str(&String::from_utf8_lossy(&d));
        }
    }
    assert!(text.starts_with("event: state\n"), "{text}");
    let sweep_frame = text
        .split("\n\n")
        .find(|f| f.starts_with("event: sweep"))
        .unwrap();
    let data: Value =
        serde_json::from_str(sweep_frame.trim_start_matches("event: sweep\ndata: ")).unwrap();
    assert_eq!(data["status"], "idle");
    drop(body);

    // A restore is not refused because of the sweep, and the sweep goes on.
    let r = call(&app, "POST", "/api/db/snapshots", None).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let name = r.json()["filename"].as_str().unwrap().to_string();
    st.sweeper().wake();
    let r = call(
        &app,
        "POST",
        &format!("/api/db/snapshots/{name}/restore"),
        None,
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    quiet(&st).await;
    assert_eq!(st.sweeper().state().status, SweepStatus::Idle);

    // The quit prompt never names the sweep.
    assert_eq!(st.work_snapshot().quit_prompt(), None);

    // No library file was written, renamed, added or removed.
    assert_eq!(tree(&sb.lib), files_before, "the library changed");

    let report = tokio::task::spawn_blocking({
        let st = st.clone();
        move || st.shutdown()
    })
    .await
    .unwrap();
    assert!(report.sweep_stopped);
}
