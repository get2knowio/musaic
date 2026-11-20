//! SC-007: a stream the HTTP face sends is the rendering of what the operation
//! reports. For the scan, one scan is followed through `ops().scan()` and through
//! `GET /api/scan/stream`; for each run (analyze, apply, quick-fix-all and the
//! conversion) two identical copies of an album are driven, one through the
//! operation's sink and one through the HTTP route, and the frames are equal in
//! order once the two copies' paths and ids are normalised; for each feed the
//! first HTTP frame is the rendering of the feed's first value. Pings are never
//! compared (they are the face's, and the ping interval is 30 s).

mod common;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::Request;
use common::{Norm, Sandbox, call, get, send};
use http_body_util::BodyExt;
use musaic_api::render;
use musaic_api::{AppState, Fingerprinting, Network, router};
use musaic_app::ops::Listening;
use musaic_app::ops::analyze::AnalysisKind;
use musaic_app::ops::apply::ApplyChanges;
use musaic_app::ops::fixes::ConvertEvent;
use musaic_events::Event;
use musaic_services::fingerprint::{CancelToken, Fingerprinter};
use musaic_services::fingerprint_pool::PoolSleeper;
use serde_json::{Value, json};
use tower::ServiceExt;

// ------------------------------------------------------------------ harness

struct World {
    sb: Sandbox,
    st: AppState,
    app: Router,
}

impl World {
    fn new() -> World {
        let sb = Sandbox::new();
        let st = sb.state_no_lifespan();
        let app = router(st.clone());
        World { sb, st, app }
    }

    /// Choose the library and read it once, through the routes (the same for
    /// both copies: what is compared is the run that follows).
    async fn configure_and_scan(&self) {
        let r = call(
            &self.app,
            "POST",
            "/api/config",
            Some(&json!({"library_dir": Sandbox::s(&self.sb.lib)})),
        )
        .await;
        assert_eq!(r.status, 200, "{}", r.text());
        let scan = call(&self.app, "GET", "/api/scan/stream", None).await;
        assert!(scan.text().contains("event: done"), "{}", scan.text());
    }

    /// The id of the library's first album (the same one in both copies, by
    /// position).
    async fn first_album_id(&self) -> String {
        let r = call(&self.app, "GET", "/api/albums", None).await;
        r.json()["albums"][0]["id"].as_str().unwrap().to_string()
    }

    async fn run_check(&self, check: &str) {
        let r = call(
            &self.app,
            "POST",
            &format!("/api/checks/{check}/run"),
            Some(&json!({"scope": "all"})),
        )
        .await;
        assert_eq!(r.status, 200, "{}", r.text());
    }

    async fn findings(&self, check: &str) -> Vec<Value> {
        let r = call(
            &self.app,
            "GET",
            &format!("/api/checks/findings?check_id={check}"),
            None,
        )
        .await;
        r.json()["findings"].as_array().unwrap().clone()
    }

    /// What the two copies share once their own paths and album ids are
    /// replaced.
    fn norm(&self, album_ids: &[&str]) -> Norm {
        let mut n = Norm::new(&self.sb);
        for id in album_ids {
            n.add(id, "<ALBUM>");
        }
        n
    }
}

/// `text` cut into frames (each ends with its blank line).
fn frames(text: &str) -> Vec<String> {
    text.split_inclusive("\n\n").map(str::to_string).collect()
}

/// The frames of an HTTP body, without pings.
fn http_frames(text: &str) -> Vec<String> {
    frames(text)
        .into_iter()
        .filter(|f| f != musaic_api::sse::PING)
        .collect()
}

/// The numbers a copy makes its own: modification times and the like.
fn masked(n: &Norm, text: &str) -> String {
    let t = n.raw_json_text(&n.text(text));
    regex::Regex::new(r#""(\w*mtime\w*)": -?[0-9.e+-]+"#)
        .unwrap()
        .replace_all(&t, r#""$1": "<MTIME>""#)
        .into_owned()
}

fn assert_same_frames(
    label: &str,
    ops: &[String],
    ops_norm: &Norm,
    http: &[String],
    http_norm: &Norm,
) {
    let a: Vec<String> = ops.iter().map(|f| masked(ops_norm, f)).collect();
    let b: Vec<String> = http.iter().map(|f| masked(http_norm, f)).collect();
    assert!(!a.is_empty(), "{label}: the operation reported nothing");
    assert_eq!(
        a, b,
        "{label}: the HTTP frames differ from the rendered events"
    );
}

/// The first `n` data frames of a streaming response, read without waiting for
/// the stream to end.
async fn first_frames(app: &Router, path: &str, n: usize) -> Vec<String> {
    let resp = app.clone().oneshot(get(path)).await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let mut body = resp.into_body();
    let mut out = Vec::new();
    while out.len() < n {
        let frame = tokio::time::timeout(Duration::from_secs(10), body.frame())
            .await
            .unwrap_or_else(|_| panic!("{path}: no frame {} in time", out.len() + 1))
            .unwrap_or_else(|| panic!("{path}: the stream ended after {} frames", out.len()))
            .unwrap();
        if let Ok(data) = frame.into_data() {
            out.push(String::from_utf8(data.to_vec()).unwrap());
        }
    }
    out
}

fn collect_into<'a, T>(
    out: &'a mut Vec<String>,
    render: impl Fn(&T) -> String + 'a,
) -> impl FnMut(T) -> Listening + 'a {
    move |event| {
        out.push(render(&event));
        Listening::Yes
    }
}

// --------------------------------------------------------------------- scan

struct Fixed;

impl Fingerprinter for Fixed {
    fn fingerprint(&self, _path: &Path) -> Result<(String, u64), String> {
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

/// One scan, followed through the operation and through the route. The scan is
/// held in its first album until both have subscribed (each replays the scan's
/// events from the first), so neither can start a scan of its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_scan_stream_is_the_rendering_of_the_scan_events() {
    let sb = Sandbox::new();
    let release = Arc::new(AtomicBool::new(false));
    let hold = Arc::clone(&release);
    let st = AppState::builder(&sb.cfg)
        .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
        .network(Network::Offline)
        .frontend_dir(None)
        .subscribers(false)
        .auto_snapshot(false)
        .fingerprinting(Fingerprinting::Local(Arc::new(Fixed)))
        .pool_sleeper(Arc::new(NoSleep))
        .on_scan_album(Arc::new(move |_album, _fresh| {
            let deadline = std::time::Instant::now() + Duration::from_secs(30);
            while !hold.load(Ordering::SeqCst) {
                assert!(std::time::Instant::now() < deadline, "never released");
                std::thread::sleep(Duration::from_millis(2));
            }
        }))
        .build()
        .unwrap();
    let app = router(st.clone());
    let r = call(
        &app,
        "POST",
        "/api/config",
        Some(&json!({"library_dir": Sandbox::s(&sb.lib)})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());

    // The operation starts the scan; the route's response is returned once its
    // subscription exists, and then joins it.
    let sub = st.ops().scan().subscribe(false).expect("scan subscription");
    let resp = app.clone().oneshot(get("/api/scan/stream")).await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    release.store(true, Ordering::SeqCst);

    let ops_side = tokio::task::spawn_blocking(move || {
        sub.map(|e| render::library::scan_frame(&e))
            .collect::<Vec<_>>()
    });
    let http_side = async {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    };
    let (ops, http) = tokio::join!(ops_side, http_side);
    let ops = ops.unwrap();
    let http = http_frames(&http);
    assert!(
        ops.first().unwrap().starts_with("event: start\n"),
        "{ops:?}"
    );
    assert!(ops.last().unwrap().starts_with("event: done\n"), "{ops:?}");
    assert_eq!(ops, http, "one scan, two readers");
}

// ------------------------------------------------------------------ analyze

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_analyze_stream_is_the_rendering_of_the_analyze_events() {
    let (a, b) = (World::new(), World::new());
    a.configure_and_scan().await;
    b.configure_and_scan().await;
    let (ida, idb) = (a.first_album_id().await, b.first_album_id().await);

    let mut ops = Vec::new();
    a.st.ops()
        .analyze()
        .run(
            &ida,
            AnalysisKind::Bpm,
            &mut collect_into(&mut ops, render::analyze::frame),
        )
        .expect("analyze through the operation");

    let got = call(
        &b.app,
        "POST",
        &format!("/api/albums/{idb}/analyze/stream"),
        Some(&json!({"what": "bpm"})),
    )
    .await;
    assert_eq!(got.status, 200, "{}", got.text());
    assert_same_frames(
        "analyze",
        &ops,
        &a.norm(&[&ida]),
        &http_frames(&got.text()),
        &b.norm(&[&idb]),
    );
}

// -------------------------------------------------------------------- apply

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_apply_stream_is_the_rendering_of_the_apply_events() {
    let (a, b) = (World::new(), World::new());
    a.configure_and_scan().await;
    b.configure_and_scan().await;
    let (ida, idb) = (a.first_album_id().await, b.first_album_id().await);

    let mut ops = Vec::new();
    let changes = ApplyChanges {
        suggestions: vec![("year".into(), "2020".into())],
        ..ApplyChanges::default()
    };
    a.st.ops()
        .apply()
        .apply_changes(
            &ida,
            &changes,
            &mut collect_into(&mut ops, render::apply::frame),
        )
        .expect("apply through the operation");

    let got = call(
        &b.app,
        "POST",
        &format!("/api/albums/{idb}/apply/stream"),
        Some(&json!({"suggestions": {"year": "2020"}})),
    )
    .await;
    assert_eq!(got.status, 200, "{}", got.text());
    assert_same_frames(
        "apply",
        &ops,
        &a.norm(&[&ida]),
        &http_frames(&got.text()),
        &b.norm(&[&idb]),
    );
}

// ------------------------------------------------------------ quick-fix-all

/// The check with the most open findings (the same one in both copies).
async fn busiest_check(w: &World) -> String {
    let mut best = (String::new(), 0usize);
    for check in ["unidentified-album", "no-cover-art", "track-gaps"] {
        w.run_check(check).await;
        let n = w.findings(check).await.len();
        if n > best.1 {
            best = (check.to_string(), n);
        }
    }
    assert!(best.1 >= 1, "no check has an open finding");
    best.0
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_quick_fix_all_stream_is_the_rendering_of_the_quick_fix_events() {
    let (a, b) = (World::new(), World::new());
    a.configure_and_scan().await;
    b.configure_and_scan().await;
    let (ca, cb) = (busiest_check(&a).await, busiest_check(&b).await);
    assert_eq!(ca, cb);
    let (ida, idb) = (a.first_album_id().await, b.first_album_id().await);

    let mut ops = Vec::new();
    a.st.ops()
        .fixes()
        .quick_fix_all(
            &ca,
            &mut collect_into(&mut ops, render::fixes::quick_fix_frame),
        )
        .expect("quick-fix-all through the operation");

    let got = call(
        &b.app,
        "POST",
        &format!("/api/checks/{cb}/quick-fix-all"),
        None,
    )
    .await;
    assert_eq!(got.status, 200, "{}", got.text());
    assert_same_frames(
        "quick-fix-all",
        &ops,
        &a.norm(&[&ida]),
        &http_frames(&got.text()),
        &b.norm(&[&idb]),
    );
}

// --------------------------------------------------------------- conversion

fn flac_bytes(tags: &[(&str, String)]) -> Vec<u8> {
    let mut si = Vec::new();
    si.extend_from_slice(&4096u16.to_be_bytes());
    si.extend_from_slice(&4096u16.to_be_bytes());
    si.extend_from_slice(&[0; 6]);
    let packed: u64 = (44100u64 << 44) | (1 << 41) | (15 << 36) | 44100;
    si.extend_from_slice(&packed.to_be_bytes());
    si.extend_from_slice(&[0; 16]);
    let vendor = b"stream-equivalence-test";
    let mut vc = Vec::new();
    vc.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    vc.extend_from_slice(vendor);
    vc.extend_from_slice(&(tags.len() as u32).to_le_bytes());
    for (k, v) in tags {
        let e = format!("{k}={v}");
        vc.extend_from_slice(&(e.len() as u32).to_le_bytes());
        vc.extend_from_slice(e.as_bytes());
    }
    let mut out = b"fLaC".to_vec();
    out.push(0);
    out.extend_from_slice(&(si.len() as u32).to_be_bytes()[1..]);
    out.extend_from_slice(&si);
    out.push(0x04);
    out.extend_from_slice(&(vc.len() as u32).to_be_bytes()[1..]);
    out.extend_from_slice(&vc);
    out.push(0x81);
    out.extend_from_slice(&1024u32.to_be_bytes()[1..]);
    out.extend_from_slice(&[0; 1024]);
    out
}

/// A two-disc set in one folder (3 and 2 tracks, numbered per disc): the
/// library of a conversion's world, in place of the synthetic one.
fn write_two_disc_library(lib: &Path) {
    std::fs::remove_dir_all(lib).unwrap();
    let dir = lib.join("Band - Record [FLAC]");
    std::fs::create_dir_all(&dir).unwrap();
    for (disc, count, total) in [(1, 3, 3), (2, 2, 2)] {
        for k in 1..=count {
            let tags = vec![
                ("ARTIST", "Band".to_string()),
                ("ALBUMARTIST", "Band".to_string()),
                ("ALBUM", "Record".to_string()),
                ("DATE", "1999".to_string()),
                ("TITLE", format!("D{disc}T{k}")),
                ("TRACKNUMBER", k.to_string()),
                ("DISCNUMBER", disc.to_string()),
                ("DISCTOTAL", "2".to_string()),
                ("TRACKTOTAL", total.to_string()),
            ];
            let name = format!("{disc}-{k:02} - T{k}.flac");
            std::fs::write(dir.join(name), flac_bytes(&tags)).unwrap();
        }
    }
}

const CHECK: &str = "multi-disc-layout";

/// A world whose library is the two-disc set, scanned and checked; the finding
/// and its preview's digest.
async fn conversion_world() -> (World, i64, String) {
    let w = World::new();
    write_two_disc_library(&w.sb.lib);
    w.configure_and_scan().await;
    w.run_check(CHECK).await;
    let findings = w.findings(CHECK).await;
    assert_eq!(findings.len(), 1, "{findings:?}");
    let id = findings[0]["id"].as_i64().unwrap();
    let p = call(
        &w.app,
        "GET",
        &format!("/api/checks/{CHECK}/findings/{id}/conversion-preview"),
        None,
    )
    .await;
    assert_eq!(p.status, 200, "{}", p.text());
    let p = p.json();
    assert_eq!(p["conversion_available"], json!(true), "{p}");
    let digest = p["digest"].as_str().unwrap().to_string();
    (w, id, digest)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_conversion_stream_is_the_rendering_of_the_conversion_events() {
    let ((a, ida, da), (b, idb, db)) = (conversion_world().await, conversion_world().await);
    let (alb_a, alb_b) = (a.first_album_id().await, b.first_album_id().await);

    let mut ops = Vec::new();
    let mut started = 0;
    let rec =
        a.st.ops()
            .fixes()
            .convert_multi_disc(
                ida,
                &da,
                Some(&mut |event| {
                    match event {
                        ConvertEvent::Started => started += 1,
                        ConvertEvent::Progress(p) => {
                            ops.push(render::fixes::convert_progress_frame(&p))
                        }
                    }
                    Listening::Yes
                }),
            )
            .expect("conversion through the operation");
    assert_eq!(started, 1, "Started is the first and only start");
    ops.push(musaic_api::sse::frame(
        "result",
        &rec.document().to_string(),
    ));

    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/checks/{CHECK}/findings/{idb}/convert"))
        .header("content-type", "application/json")
        .header("accept", "text/event-stream")
        .body(Body::from(json!({"digest": db}).to_string()))
        .unwrap();
    let got = send(&b.app, req).await;
    assert_eq!(got.status, 200, "{}", got.text());
    assert!(
        got.ctype().starts_with("text/event-stream"),
        "{}",
        got.ctype()
    );
    assert_same_frames(
        "conversion",
        &ops,
        &a.norm(&[&alb_a]),
        &http_frames(&got.text()),
        &b.norm(&[&alb_b]),
    );
}

// -------------------------------------------------------------------- feeds

/// The first frame of each feed the HTTP face streams is the rendering of the
/// feed's first value (for the check feed, its first value's frames).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_feeds_first_http_frame_is_the_rendering_of_its_first_value() {
    let w = World::new();
    w.configure_and_scan().await;
    // Let the pool's pass after the scan settle, so the two reads see one state.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // The fingerprint pool.
    let mut pool = w.st.ops().system().pool_feed();
    let first = pool.next(Duration::ZERO).expect("a first value");
    let http = first_frames(&w.app, "/api/fingerprint-pool/stream", 1).await;
    assert_eq!(http[0], render::system::pool_frame(&first));

    // The check runs: `state`, then `sweep`.
    let mut runs = w.st.ops().checks().runs_feed();
    let first = runs.next(Duration::ZERO).expect("a first value");
    let want = render::checks::frames(&first).expect("the checks are readable");
    let http = first_frames(&w.app, "/api/checks/stream", want.len()).await;
    assert_eq!(http, want);

    // The AI jobs.
    let mut jobs = w.st.ops().jobs().feed();
    let first = jobs
        .next(Duration::ZERO)
        .expect("a first value")
        .expect("jobs list");
    let http = first_frames(&w.app, "/api/ai/jobs/stream", 1).await;
    assert_eq!(http[0], render::jobs::jobs_frame(&first));

    // The library events: `ready`, then each bus event as the operation hands it
    // over.
    let events = w.st.ops().system().library_events();
    let resp = w
        .app
        .clone()
        .oneshot(get("/api/library-events/stream"))
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let mut body = resp.into_body();
    let mut next = async || {
        let f = tokio::time::timeout(Duration::from_secs(10), body.frame())
            .await
            .expect("a frame in time")
            .expect("not ended")
            .unwrap();
        String::from_utf8(f.into_data().unwrap().to_vec()).unwrap()
    };
    assert_eq!(next().await, render::system::library_ready());
    w.st.bus().publish(&Event::SettingsChanged {
        keys: vec!["library_dir".into()],
    });
    let event = events
        .next(Duration::from_secs(10))
        .expect("the relayed event");
    assert_eq!(next().await, render::system::library_event_frame(&event));
}
