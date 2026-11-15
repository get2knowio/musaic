//! Thumbnails written by the scan (services/scan.py:255, 304-305, 406).
//! An earlier comparison run found every `config.thumbs/<album>.jpg` present on the Python app side after
//! the `scan` scenario and absent on the Rust side; and present for the
//! Missing albums of the `missing` scenario. The Python app's expectations
//! pinned here: which albums get one (every album with a source image,
//! testdata/golden/thumbs/thumbs.json, written by the Python app's own
//! `get_source_image`), that they exist by the time the scan reports
//! `done`, that a fresh album's thumbnail is regenerated (`force=True`)
//! and a cached album's only when missing, and that an album going Missing
//! keeps its thumbnail.

mod common;

use std::path::Path;
use std::time::Duration;

use common::{Sandbox, call};
use serde_json::{Value, json};

async fn scan_until_done(app: &axum::Router) {
    let r = call(app, "GET", "/api/scan/stream", None).await;
    assert_eq!(r.status, 200);
    for _ in 0..2000 {
        let s = call(app, "GET", "/api/scan/status", None).await.json();
        if s["status"] == "done" {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("scan did not finish");
}

fn thumb_dirs_with_covers() -> Vec<String> {
    let g: Value = musaic_testdata::golden("thumbs/thumbs.json");
    let mut v: Vec<String> = g["libraries"]["synthetic"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(_, r)| !r["thumb"].is_null())
        .map(|(k, _)| k.clone())
        .collect();
    v.sort();
    v
}

/// album dir name -> album id, from the snapshot.
async fn ids(app: &axum::Router) -> Vec<(String, String)> {
    let snap = call(app, "GET", "/api/albums", None).await.json();
    let albums = snap["albums"]
        .as_array()
        .or(snap.as_array())
        .expect("albums list")
        .clone();
    albums
        .iter()
        .map(|a| {
            let dir = Path::new(a["path"].as_str().unwrap())
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            (dir, a["id"].as_str().unwrap().to_string())
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn scan_writes_thumbnails_force_for_fresh_and_keeps_missing() {
    let sb = Sandbox::new();
    let app = musaic_api::router(sb.state_no_lifespan());
    let r = call(
        &app,
        "POST",
        "/api/config",
        Some(&json!({"library_dir": Sandbox::s(&sb.lib)})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let thumbs = sb.cfg.join("thumbs");

    // Album ids are path-derived; a first scan tells us them. Before it,
    // plant a stale thumbnail under every id the scan will produce: a
    // fresh album must get it replaced (force=True).
    scan_until_done(&app).await;
    let ids = ids(&app).await;
    let with_cover = thumb_dirs_with_covers();
    let mut have: Vec<String> = ids
        .iter()
        .filter(|(_, id)| thumbs.join(format!("{id}.jpg")).is_file())
        .map(|(d, _)| d.clone())
        .collect();
    have.sort();
    // Every album with a source image, and only those, at `done`.
    assert_eq!(have, with_cover);
    for (_, id) in &ids {
        let p = thumbs.join(format!("{id}.jpg"));
        if p.is_file() {
            let b = std::fs::read(&p).unwrap();
            assert_eq!(&b[..2], &[0xFF, 0xD8], "{id}: a JPEG");
        }
    }

    // force=True for fresh albums: touch every album dir (new mtime ->
    // read fresh) after planting junk thumbnails; all are rewritten.
    let id_of = |dir: &str| ids.iter().find(|(d, _)| d == dir).unwrap().1.clone();
    for d in &with_cover {
        std::fs::write(thumbs.join(format!("{}.jpg", id_of(d))), b"stale").unwrap();
    }
    let first = &with_cover[0];
    let second = &with_cover[1];
    // Make only `first` fresh: rewrite a file so its effective mtime moves.
    std::thread::sleep(Duration::from_millis(1100));
    let marker = sb.lib.join(first).join("touch.txt");
    std::fs::write(&marker, b"x").unwrap();
    // A cached album whose thumbnail is missing gets one again.
    std::fs::remove_file(thumbs.join(format!("{}.jpg", id_of(second)))).unwrap();
    scan_until_done(&app).await;
    let read = |d: &str| std::fs::read(thumbs.join(format!("{}.jpg", id_of(d)))).ok();
    assert_ne!(
        read(first).as_deref(),
        Some(&b"stale"[..]),
        "fresh album: forced"
    );
    assert!(
        read(second).is_some_and(|b| b.starts_with(&[0xFF, 0xD8])),
        "cached, missing: made"
    );
    for d in &with_cover[2..] {
        assert_eq!(
            read(d).as_deref(),
            Some(&b"stale"[..]),
            "{d}: cached album kept its thumb"
        );
    }

    // An album going Missing keeps its thumbnail.
    let gone = &with_cover[2];
    std::fs::rename(sb.lib.join(gone), sb.lib_gone.join(gone)).unwrap();
    scan_until_done(&app).await;
    assert_eq!(
        read(gone).as_deref(),
        Some(&b"stale"[..]),
        "Missing album keeps its thumb"
    );
}

/// Thumbnail files present in `dir`, by album id.
fn thumb_ids(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    e.file_name()
                        .to_str()
                        .and_then(|n| n.strip_suffix(".jpg"))
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn state_with(
    sb: &Sandbox,
    hook: Option<musaic_services::scan::AlbumHook>,
) -> musaic_api::AppState {
    let mut b = musaic_api::AppState::builder(&sb.cfg)
        .secrets(std::sync::Arc::new(musaic_secrets::MemoryStore::new()))
        .network(musaic_api::Network::Offline)
        .fingerprinting(common::no_key_fingerprinting())
        .subscribers(false)
        .auto_snapshot(false);
    if let Some(h) = hook {
        b = b.on_scan_album(h);
    }
    b.build().unwrap()
}

/// The thumbnails present at the instant `LibraryScanCompleted` is
/// published (a bus subscriber runs synchronously on the scan thread).
fn record_thumbs_at_event(
    st: &musaic_api::AppState,
    thumbs: std::path::PathBuf,
) -> std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>> {
    use musaic_events::{Event, Subscriber, Topic};
    let seen: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>> = Default::default();
    let s2 = seen.clone();
    st.bus().register(
        Topic::Event,
        Subscriber::from_fn(move |ev| {
            if let Event::LibraryScanCompleted { .. } = ev {
                s2.lock().unwrap().push(thumb_ids(&thumbs));
            }
            Ok(())
        })
        .name("thumbs_at_event"),
    );
    seen
}

/// The Python app's order (services/scan.py:395-404 then 183-222): the scan worker's
/// `finally` drains the thumbnail pool (`shutdown(wait=True)`) before
/// `_run_scan` publishes `LibraryScanCompleted`, so every thumbnail of the
/// scan exists when the event is published. The set is the Python app's
/// (testdata/golden/thumbs/thumbs.json; a recorded comparison run has the
/// same thumbs after the scan scenario; no recording holds the instant of
/// the event, so the order is the Python app source's).
#[tokio::test(flavor = "multi_thread")]
async fn every_thumbnail_exists_when_library_scan_completed_is_published() {
    let sb = Sandbox::new();
    let st = state_with(&sb, None);
    let thumbs = sb.cfg.join("thumbs");
    let seen = record_thumbs_at_event(&st, thumbs.clone());
    let app = musaic_api::router(st.clone());
    let r = call(
        &app,
        "POST",
        "/api/config",
        Some(&json!({"library_dir": Sandbox::s(&sb.lib)})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    scan_until_done(&app).await;
    let ids = ids(&app).await;
    let mut want: Vec<String> = thumb_dirs_with_covers()
        .iter()
        .map(|d| ids.iter().find(|(x, _)| x == d).unwrap().1.clone())
        .collect();
    want.sort();
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "one LibraryScanCompleted");
    assert_eq!(seen[0], want, "thumbnails present at the event");
}

/// The error path (services/scan.py:395-404: `except` emits `error`, the
/// `finally` still drains the pool, then `_run_scan` sets `error` and
/// publishes): a scan whose body panics after three albums ends in
/// `error`, the thumbnails of the albums it submitted exist when the event
/// is published, and the restore gate is free once the status is `error`
/// (the gate does not outlive the reported end of the scan).
#[tokio::test(flavor = "multi_thread")]
async fn failing_scan_drains_thumbnails_before_the_event_and_releases_the_gate() {
    let sb = Sandbox::new();
    let submitted: std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>> = Default::default();
    let s2 = submitted.clone();
    let hook: musaic_services::scan::AlbumHook = std::sync::Arc::new(move |album, _fresh| {
        let mut s = s2.lock().unwrap();
        s.push((
            album
                .get("dir_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            album
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        ));
        let n = s.len();
        drop(s);
        if n == 3 {
            panic!("scan body fails after three albums");
        }
    });
    let st = state_with(&sb, Some(hook));
    let thumbs = sb.cfg.join("thumbs");
    let seen = record_thumbs_at_event(&st, thumbs.clone());
    let app = musaic_api::router(st.clone());
    let r = call(
        &app,
        "POST",
        "/api/config",
        Some(&json!({"library_dir": Sandbox::s(&sb.lib)})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let r = call(&app, "GET", "/api/scan/stream", None).await;
    assert!(r.text().contains("event: error"), "{}", r.text());
    let mut status = Value::Null;
    for _ in 0..2000 {
        status = call(&app, "GET", "/api/scan/status", None).await.json();
        if status["status"] != "running" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(status["status"], "error", "{status}");
    // Released: the job returns right after the terminal status. (The bound
    // only catches a gate that is never released; it is generous because a
    // loaded CI runner can stall the job thread for seconds.)
    let t0 = std::time::Instant::now();
    while st.gate().snapshot() != (0, false) {
        assert!(t0.elapsed() < Duration::from_secs(20), "gate still held");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let with_cover = thumb_dirs_with_covers();
    let mut want: Vec<String> = submitted
        .lock()
        .unwrap()
        .iter()
        .filter(|(d, _)| with_cover.contains(d))
        .map(|(_, id)| id.clone())
        .collect();
    want.sort();
    assert!(!want.is_empty(), "at least one submitted album has a cover");
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "one LibraryScanCompleted");
    assert_eq!(seen[0], want, "thumbnails present at the event");
}
