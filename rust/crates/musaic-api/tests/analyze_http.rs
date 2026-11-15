//! `POST /api/albums/{album_id}/analyze/stream` through the router, against
//! testdata/golden/services/analyze.json (recorded through the Python app's own route
//! with real librosa / pyloudnorm).
//!
//! * `http` cases: the recorded 404 / 422 / 409 bodies.
//! * Complete-run scenarios (TestClient driver: the whole body went out):
//!   the album copied into a library of its own, configured and scanned
//!   through the API, the scenario's out-of-band changes applied, then the
//!   route with the real analyzers; the SSE body (byte for byte after the
//!   generator's normalisation) and the bus events equal the Python app's.
//! * Not replayed here, because another agent's decided service deltas
//!   change their expected output (they are pinned in musaic-services):
//!   `rg-broken-file`, `rg-readonly`, `rg-metadata-only-file` (an error counted once),
//!   `missing-dir-present`, `missing-dir-gone` (409 for a Missing
//!   album), and every `disconnect-*` scenario (run to
//!   completion). `real-bpm` / `real-rg` need the real-audio set (private) and are
//!   skipped with a message when it is absent.
//! * This build's own behaviour, no Python counterpart: the restore gate's
//!   guard is held for the whole run, also after the client went away, and
//!   the run completes.
//! * `present-dir-gone` (directory gone under a cached, non-Missing
//!   row) is refused with 409 before streaming; the Python app's stream ended
//!   with no frame.

mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use common::call;
use musaic_api::{AppState, Network, router};
use musaic_events::{Event, FieldValue, Subscriber, Topic};
use musaic_services::analyze::Analyzer;
use serde_json::{Map, Value, json};

fn golden() -> &'static Value {
    static G: OnceLock<Value> = OnceLock::new();
    G.get_or_init(|| musaic_testdata::golden("services/analyze.json"))
}

fn scenario(id: &str) -> &'static Value {
    golden()["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == id)
        .unwrap_or_else(|| panic!("no scenario {id}"))
}

/// The album's folder in the source library. The synthetic library writes one
/// folder name in decomposed Unicode (NFD) while the scenarios name it in
/// composed form; macOS finds it either way, Linux only by its exact bytes.
fn source_album_dir(root: &Path, album: &str) -> std::path::PathBuf {
    use unicode_normalization::UnicodeNormalization;
    let direct = root.join(album);
    if direct.is_dir() {
        return direct;
    }
    let want: String = album.nfc().collect();
    std::fs::read_dir(root)
        .unwrap()
        .flatten()
        .find(|e| e.file_name().to_string_lossy().nfc().collect::<String>() == want)
        .map(|e| e.path())
        .unwrap_or(direct)
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap().flatten() {
        let p = e.path();
        if e.file_type().unwrap().is_dir() {
            copy_dir(&p, &dst.join(e.file_name()));
        } else {
            std::fs::copy(&p, dst.join(e.file_name())).unwrap();
        }
    }
}

fn make_writable(dir: &Path) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            make_writable(&p);
        } else {
            let mut perm = std::fs::metadata(&p).unwrap().permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perm.set_readonly(false);
            let _ = std::fs::set_permissions(&p, perm);
        }
    }
}

fn event_json(ev: &Event) -> Value {
    let mut o = Map::new();
    o.insert("type".into(), json!(ev.name()));
    for (k, v) in ev.fields() {
        o.insert(
            k.into(),
            match v {
                FieldValue::Str(s) => json!(s),
                FieldValue::Int(n) => json!(n),
                FieldValue::StrList(l) => json!(l),
            },
        );
    }
    Value::Object(o)
}

/// A library holding one album of testdata/synthetic-library/library (or library-real), the
/// app on it (no subscribers, no fingerprinting: the Python app's harness had
/// neither), configured and scanned through the API.
struct World {
    _td: tempfile::TempDir,
    lib: PathBuf,
    album_dir: PathBuf,
    st: AppState,
    app: axum::Router,
    album_id: String,
    events: Arc<Mutex<Vec<Event>>>,
}

impl World {
    /// `None` when the album's source set is absent (real audio).
    async fn new(sc: &Value, analyzer: Option<musaic_api::SharedAnalyzer>) -> Option<World> {
        // Real-audio scenarios (private half of the golden) need the local
        // copy of the real-audio set (private, never committed).
        let src_root = if sc["source"] == "real" {
            match musaic_testdata::real_library() {
                Some(p) => p,
                None => {
                    musaic_testdata::skip_private_cases();
                    return None;
                }
            }
        } else {
            musaic_testdata::synthetic_library()
        };
        let album = sc["album"].as_str().unwrap();
        let td = tempfile::tempdir().unwrap();
        let root = musaic_testdata::canonical(td.path());
        let lib = root.join("lib");
        let album_dir = lib.join(album);
        copy_dir(&source_album_dir(&src_root, album), &album_dir);
        for (name, text) in sc["extra_files"].as_object().into_iter().flatten() {
            std::fs::write(album_dir.join(name), text.as_str().unwrap()).unwrap();
        }
        let log: Arc<Mutex<Vec<String>>> = Arc::default();
        let l2 = log.clone();
        let mut b = AppState::builder(root.join("cfg"))
            .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
            .network(Network::Offline)
            .fingerprinting(common::no_key_fingerprinting())
            .subscribers(false)
            .auto_snapshot(false)
            .log(Arc::new(move |l: &str| {
                l2.lock().unwrap().push(l.to_string())
            }));
        if let Some(a) = analyzer {
            b = b.analyzer(a);
        }
        let st = b.build().unwrap();
        let app = router(st.clone());
        let r = call(
            &app,
            "POST",
            "/api/config",
            Some(&json!({"library_dir": lib.to_string_lossy()})),
        )
        .await;
        assert_eq!(r.status, 200, "{}", r.text());
        let scan = call(&app, "GET", "/api/scan/stream", None).await;
        assert!(scan.text().contains("event: done"), "{}", scan.text());
        eventually("scan released the gate", || {
            st.gate().snapshot() == (0, false)
        })
        .await;
        let albums = call(&app, "GET", "/api/albums", None).await.json();
        let album_id = albums["albums"][0]["id"].as_str().unwrap().to_string();
        assert_eq!(albums["albums"][0]["dir_name"], album);
        let events: Arc<Mutex<Vec<Event>>> = Arc::default();
        let e2 = events.clone();
        st.bus().register(
            Topic::Event,
            Subscriber::from_fn(move |ev| {
                e2.lock().unwrap().push(ev.clone());
                Ok(())
            })
            .priority(400)
            .name("test_recorder"),
        );
        if sc["delete_flacs"] == true {
            for e in std::fs::read_dir(&album_dir).unwrap().flatten() {
                if e.path().extension().is_some_and(|x| x == "flac") {
                    std::fs::remove_file(e.path()).unwrap();
                }
            }
        }
        for rel in sc["readonly"].as_array().into_iter().flatten() {
            let p = album_dir.join(rel.as_str().unwrap());
            let mut perm = std::fs::metadata(&p).unwrap().permissions();
            perm.set_readonly(true);
            std::fs::set_permissions(&p, perm).unwrap();
        }
        Some(World {
            _td: td,
            lib,
            album_dir,
            st,
            app,
            album_id,
            events,
        })
    }

    fn norm(&self, s: &str) -> String {
        let label = format!(
            "<ID:{}>",
            self.album_dir.file_name().unwrap().to_string_lossy()
        );
        let s = s.replace(&self.album_id, &label);
        let s = musaic_testdata::replace_root(&s, &self.lib.to_string_lossy(), "<LIB>");
        regex::Regex::new(r#""dir_mtime": -?[0-9.e+-]+"#)
            .unwrap()
            .replace_all(&s, r#""dir_mtime": "<MTIME>""#)
            .into_owned()
    }

    fn bus_events(&self) -> Value {
        let evs: Vec<Value> = self
            .events
            .lock()
            .unwrap()
            .iter()
            .map(|e| serde_json::from_str(&self.norm(&event_json(e).to_string())).unwrap())
            .collect();
        Value::Array(evs)
    }

    async fn analyze(&self, body: Value) -> common::Got {
        call(
            &self.app,
            "POST",
            &format!("/api/albums/{}/analyze/stream", self.album_id),
            Some(&body),
        )
        .await
    }
}

impl Drop for World {
    fn drop(&mut self) {
        make_writable(&self.lib);
    }
}

async fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !f() {
        assert!(Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn replay(id: &str) {
    // The real-audio scenarios are in the private half of the golden.
    if id.starts_with("real-") && !musaic_testdata::json("golden/services/analyze.json").private {
        musaic_testdata::skip();
        return;
    }
    let sc = scenario(id);
    assert!(sc["driver"].is_null(), "{id}: a complete-run scenario");
    let Some(w) = World::new(sc, None).await else {
        return;
    };
    let what = sc["what"].as_str().unwrap();
    let got = w.analyze(json!({"what": what})).await;
    assert_eq!(json!(got.status), sc["status"], "{id}: {}", got.text());
    assert_eq!(got.ctype(), musaic_api::sse::CONTENT_TYPE, "{id}");
    assert_eq!(
        w.norm(&got.text()),
        sc["sse_body"].as_str().unwrap(),
        "{id}: SSE body"
    );
    assert_eq!(w.bus_events(), sc["bus_events"], "{id}: bus events");
    eventually("guard released", || w.st.gate().snapshot() == (0, false)).await;
}

macro_rules! complete_runs {
    ($($name:ident => $id:literal),* $(,)?) => {
        $(
            #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
            async fn $name() {
                replay($id).await;
            }
        )*

        /// Every scenario of the golden is replayed above or listed as
        /// owned by a service delta.
        #[test]
        fn every_scenario_is_replayed_or_listed() {
            let replayed = [$($id),*];
            for s in golden()["scenarios"].as_array().unwrap() {
                let id = s["id"].as_str().unwrap();
                assert!(
                    replayed.contains(&id) || SERVICE_DELTAS.iter().any(|(p, _)| id.starts_with(p)),
                    "scenario {id} neither replayed nor listed"
                );
            }
        }
    };
}

/// Scenarios whose expected output another agent's decided service delta
/// changes (id or id prefix, and the UNK). Pinned in musaic-services.
const SERVICE_DELTAS: &[(&str, &str)] = &[
    (
        "rg-broken-file",
        "deliberate change: a failed album-gain write counted once",
    ),
    (
        "rg-readonly",
        "deliberate change: a failed album-gain write counted once",
    ),
    (
        "rg-metadata-only-file",
        "recorded for the counted-once change",
    ),
    (
        "missing-dir-present",
        "deliberate change: 409 for a Missing album",
    ),
    (
        "missing-dir-gone",
        "deliberate change: 409 for a Missing album",
    ),
    (
        "present-dir-gone",
        "deliberate change: 409 for an unreachable album directory (replayed below)",
    ),
    (
        "disconnect-",
        "deliberate change: run to completion after a disconnect",
    ),
];

// Complete runs: AlbumTagsChanged fields ["bpm"] / ["replaygain"]; the ._
// file and notes.txt are skipped.
complete_runs! {
    http_bpm_dotfiles => "bpm-dotfiles",
    http_rg_dotfiles => "rg-dotfiles",
    http_bpm_first_light => "bpm-first-light",
    http_rg_first_light => "rg-first-light",
    http_rg_non_ascii_names => "rg-cafe",
    http_bpm_broken_file => "bpm-broken-file",
    http_bpm_readonly_file => "bpm-readonly",
    http_bpm_all_readonly => "bpm-all-readonly",
    http_no_flacs_left => "no-flacs-left",
    http_real_bpm => "real-bpm",
    http_real_rg => "real-rg",
}

// The golden's `http` cases through the router: 404 for
// an unknown album, 422 for a bad or missing `what` (pydantic's
// literal_error / missing, compared whole), 409 without a library.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn http_refusals() {
    let http = golden()["http"].as_array().unwrap();
    let case = |id: &str| http.iter().find(|c| c["id"] == id).unwrap();
    let sc = scenario("bpm-dotfiles");
    let w = World::new(sc, None).await.unwrap();
    let post = |id: &str, body: Value| {
        let app = w.app.clone();
        let id = id.to_string();
        async move {
            call(
                &app,
                "POST",
                &format!("/api/albums/{id}/analyze/stream"),
                Some(&body),
            )
            .await
        }
    };
    let r = post("0123456789abcdef", json!({"what": "bpm"})).await;
    assert_eq!(json!({"status": r.status, "json": r.json()}), {
        let c = case("unknown-album");
        json!({"status": c["status"], "json": c["json"]})
    });
    for (cid, body) in [
        ("bad-what", json!({"what": "loudness"})),
        ("no-what", json!({})),
    ] {
        let r = post(&w.album_id, body).await;
        let c = case(cid);
        assert_eq!(
            json!({"status": r.status, "json": r.json()}),
            json!({"status": c["status"], "json": c["json"]}),
            "{cid}"
        );
    }
    // No library: a fresh app with nothing configured.
    let td = tempfile::tempdir().unwrap();
    let st = AppState::builder(td.path().join("cfg"))
        .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
        .network(Network::Offline)
        .subscribers(false)
        .auto_snapshot(false)
        .build()
        .unwrap();
    let r = call(
        &router(st),
        "POST",
        "/api/albums/0123456789abcdef/analyze/stream",
        Some(&json!({"what": "bpm"})),
    )
    .await;
    let c = case("no-library");
    assert_eq!(
        json!({"status": r.status, "json": r.json()}),
        json!({"status": c["status"], "json": c["json"]})
    );
    assert_eq!(w.st.gate().snapshot(), (0, false), "refusals hold no guard");
}

/// BPM 120 for every file; the first call waits until released.
struct HeldAnalyzer {
    calls: AtomicUsize,
    release: AtomicBool,
}

impl Analyzer for HeldAnalyzer {
    fn bpm(&self, _path: &Path) -> Result<f64, String> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            let deadline = Instant::now() + Duration::from_secs(60);
            while !self.release.load(Ordering::SeqCst) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        Ok(120.0)
    }
    fn loudness(&self, _path: &Path) -> Result<(f64, f64), String> {
        Ok((-18.0, 0.5))
    }
}

// The restore gate and run-to-completion as the HTTP layer sees them: the
// analysis holds the gate's guard for its whole run; the client going away
// after the first frame neither releases it early nor stops the run: every
// FLAC gets its BPM tag and AlbumTagsChanged is published, then the guard is
// released. A restore asked for meanwhile is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn analyze_holds_the_gate_and_completes_after_a_disconnect() {
    use http_body_util::BodyExt;
    let sc = scenario("bpm-first-light");
    let held = Arc::new(HeldAnalyzer {
        calls: AtomicUsize::new(0),
        release: AtomicBool::new(false),
    });
    struct Release(Arc<HeldAnalyzer>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.release.store(true, Ordering::SeqCst);
        }
    }
    let _release = Release(held.clone());
    let w = World::new(sc, Some(held.clone())).await.unwrap();
    let req = common::with_host(
        axum::http::Request::builder()
            .method("POST")
            .uri(format!("/api/albums/{}/analyze/stream", w.album_id))
            .header("content-type", "application/json")
            .body(axum::body::Body::from(json!({"what": "bpm"}).to_string()))
            .unwrap(),
    );
    let resp = tower::ServiceExt::oneshot(w.app.clone(), req)
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let mut body = resp.into_body();
    let first = body.frame().await.unwrap().unwrap().into_data().unwrap();
    assert!(String::from_utf8_lossy(&first).starts_with("event: start"));
    eventually("first file in analysis", || {
        held.calls.load(Ordering::SeqCst) == 1
    })
    .await;
    drop(body);
    assert_eq!(
        w.st.gate().snapshot(),
        (1, false),
        "guard held after disconnect"
    );
    let r = call(
        &w.app,
        "POST",
        "/api/db/snapshots/library-x.db/restore",
        None,
    )
    .await;
    assert_eq!(r.status, 409);
    assert_eq!(r.json()["detail"], musaic_api::gate::RESTORE_REFUSED);
    held.release.store(true, Ordering::SeqCst);
    eventually("guard released", || w.st.gate().snapshot() == (0, false)).await;
    let flacs: Vec<PathBuf> = musaic_scan::list_flacs(&w.album_dir);
    assert_eq!(flacs.len(), 5);
    for f in &flacs {
        let m = musaic_scan::flacmeta::read(f).unwrap();
        assert_eq!(m.values("bpm").next(), Some("120.00"), "{}", f.display());
    }
    let evs = w.bus_events();
    assert_eq!(
        evs,
        json!([{"type": "AlbumTagsChanged", "album_id": format!("<ID:{}>", sc["album"].as_str().unwrap()), "fields": ["bpm"]}])
    );
}

// Deliberate change, scenario `present-dir-gone`: a cached row that is not Missing whose
// album directory was removed out of band. The Python app answered 200 and its
// stream ended with no frame (`iterdir()` raised); now the route refuses
// with 409 before streaming, the status used for an unreachable album
// directory elsewhere (delete-duplicate), with `analyze::unreachable_album_detail`. Delta: status
// 200 -> 409, the empty SSE body -> that JSON detail; no event in either,
// and the guard is released.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn present_album_with_its_folder_gone_is_refused_before_streaming() {
    let sc = scenario("present-dir-gone");
    assert_eq!(sc["status"], 200, "the Python app's status");
    assert_eq!(sc["frames"], json!([]), "the Python app sent no frame");
    assert_eq!(sc["exception"]["type"], "FileNotFoundError");
    assert!(sc["mark_missing"].is_null() && sc["remove_dir"] == true);
    let w = World::new(sc, None).await.unwrap();
    std::fs::remove_dir_all(&w.album_dir).unwrap();
    let got = w.analyze(json!({"what": sc["what"]})).await;
    assert_eq!(got.status, 409, "{}", got.text());
    // The OS's own error for listing a missing directory: ENOENT on Unix,
    // ERROR_PATH_NOT_FOUND (3) on Windows.
    let gone = std::fs::read_dir(&w.album_dir).unwrap_err();
    assert_eq!(gone.kind(), std::io::ErrorKind::NotFound);
    if cfg!(unix) {
        assert_eq!(gone.raw_os_error(), Some(2));
    }
    assert_eq!(
        got.json(),
        json!({"detail": musaic_services::analyze::unreachable_album_detail(
            &w.album_dir,
            &gone,
        )})
    );
    eventually("guard released", || w.st.gate().snapshot() == (0, false)).await;
    assert!(
        w.events.lock().unwrap().is_empty(),
        "{:?}",
        sc["bus_events"]
    );
    assert_eq!(sc["bus_events"], json!([]));
}
