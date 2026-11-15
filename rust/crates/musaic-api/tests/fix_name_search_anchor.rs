//! A deliberate change, through the app: a confident
//! MusicBrainz name-search hit is stored as the album's anchor, with
//! `AlbumMbAnchorSet("musicbrainz")`, only when the name-search track-count /
//! duration check passes. Both callers the app wires:
//!
//! - the check-fix path: `POST /api/checks/no-cover-art/findings/{id}/fix`
//!   (`check_fixes::cover_fix` -> `AppFixSources::find_cover`);
//! - the AI worker's MusicBrainz cover fallback
//!   (`ai._fallback_cover_lookup` -> `WorkerEnv::find_cover`, here called
//!   on the app's `AppEnv` as the worker calls it).
//!
//! Replays the five `with id:` cases of
//! testdata/golden/resolver/find_release.json (the Python app's
//! `mb_lookup.find_release` on an album that has an id; the same cases
//! musaic-services' fix-anchor test replays at the service
//! level) through the app's transport (`Network::Live` over the cases'
//! overlay + the recorded replay set (private)). Compared per case and per path: the anchor
//! columns, the bus events and the MusicBrainz requests. Enumerated deltas
//! (each must be used):
//! - every confident case: one more MusicBrainz request, the
//!   release listing the track-count check reads (`browse_releases`);
//! - "listing has 9 tracks" / "listing 20% longer" / "listing
//!   unrecorded": no anchor written and no `AlbumMbAnchorSet`, where the
//!   Python app wrote and published;
//! - both paths ask for the cover (`fetch_cover`), so requests to
//!   coverartarchive.org may follow the match (they are not compared: no
//!   case records them; they fail as unobserved).

mod common;

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use base64::Engine;
use common::call;
use musaic_ai::{Py, WorkerEnv};
use musaic_api::ai::AppEnv;
use musaic_api::{AppState, Network, router};
use musaic_core::Album;
use musaic_events::{Event, Subscriber, Topic};
use musaic_net::{ReplayTransport, Request, Response, Transport, TransportError};
use serde_json::{Value, json};

const REJECTED: [&str; 3] = [
    "with id: confident, listing has 9 tracks",
    "with id: confident, listing 20% longer",
    "with id: confident, listing unrecorded",
];

/// The case's overlay by replay key, else the recorded replay set (private); every request
/// logged as (host, key).
struct Overlay {
    by_key: HashMap<String, Response>,
    replay: ReplayTransport,
    log: Mutex<Vec<(String, String)>>,
}

impl Overlay {
    fn new(overlay: &Value) -> Self {
        let by_key = overlay
            .as_array()
            .unwrap()
            .iter()
            .map(|o| {
                let body = base64::engine::general_purpose::STANDARD
                    .decode(o["content_b64"].as_str().unwrap())
                    .unwrap();
                let headers = o["headers"]
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
                    .collect();
                (
                    o["key"].as_str().unwrap().to_string(),
                    Response {
                        status: o["status"].as_u64().unwrap() as u16,
                        headers,
                        body,
                    },
                )
            })
            .collect();
        Overlay {
            by_key,
            replay: ReplayTransport::new(musaic_testdata::private_required("replay")),
            log: Mutex::default(),
        }
    }
}

impl Transport for Overlay {
    fn send(&self, request: &Request) -> Result<Response, TransportError> {
        let key = request.replay_key();
        self.log
            .lock()
            .unwrap()
            .push((request.host().to_string(), key.clone()));
        match self.by_key.get(&key) {
            Some(r) => Ok(r.clone()),
            None => self.replay.send(request),
        }
    }
}

fn s(v: &Value) -> Option<String> {
    v.as_str().map(str::to_string)
}

struct World {
    _td: tempfile::TempDir,
    st: AppState,
    app: axum::Router,
    transport: Arc<Overlay>,
    events: Arc<Mutex<Vec<Value>>>,
    lib: String,
    id: String,
}

async fn world(case: &Value) -> World {
    let td = tempfile::tempdir().unwrap();
    let lib = td.path().join("lib").canonicalize().unwrap_or_else(|_| {
        std::fs::create_dir(td.path().join("lib")).unwrap();
        musaic_testdata::canonical(td.path().join("lib"))
    });
    let transport = Arc::new(Overlay::new(&case["overlay"]));
    let st = AppState::builder(td.path().join("cfg"))
        .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
        .network(Network::Live(transport.clone()))
        .fingerprinting(common::no_key_fingerprinting())
        .subscribers(false)
        .auto_snapshot(false)
        .build()
        .unwrap();
    let app = router(st.clone());
    let lib_s = lib.to_string_lossy().into_owned();
    let r = call(
        &app,
        "POST",
        "/api/config",
        Some(&json!({"library_dir": lib_s})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    // The case's album, as the Python app's generator described it.
    let a = &case["album"];
    let id = a["id"].as_str().unwrap().to_string();
    let dir_name = "Ana Artist - Album One [FLAC]";
    let path = lib.join(dir_name);
    std::fs::create_dir(&path).unwrap();
    let album = Album {
        id: id.clone(),
        path: path.to_string_lossy().into_owned(),
        dir_name: dir_name.into(),
        title: s(&a["title"]),
        artist: s(&a["artist"]),
        album_artist: s(&a["album_artist"]),
        year: None,
        genre: None,
        track_count: a["track_count"].as_u64().unwrap() as usize,
        total_seconds: a["total_seconds"].as_f64().unwrap(),
        has_embedded_cover: false,
        cover_file: None,
        sample_track: "01.flac".into(),
        dir_mtime: 0.0,
        dir_artist: s(&a["dir_artist"]),
        dir_album: s(&a["dir_album"]),
        is_compilation: a["is_compilation"].as_bool().unwrap(),
        disc_number: None,
        disc_total: None,
        sample_rate: None,
        bits_per_sample: None,
        channels: None,
        track_numbers: vec![Some(1), Some(2)],
        album_artist_sort: None,
        artist_sort: None,
        album_sort: None,
        track_artists: vec![],
    };
    let mut conn = rusqlite::Connection::open(st.db_path()).unwrap();
    musaic_db::albums::upsert_album(&mut conn, &lib_s, "2001-01-01T00:00:00Z", &album).unwrap();
    drop(conn);
    let events: Arc<Mutex<Vec<Value>>> = Arc::default();
    let e2 = events.clone();
    st.bus().register(
        Topic::Event,
        Subscriber::from_fn(move |ev| {
            if let Event::AlbumMbAnchorSet { album_id, source } = ev {
                e2.lock().unwrap().push(
                    json!({"type": "AlbumMbAnchorSet", "album_id": album_id, "source": source}),
                );
            }
            Ok(())
        })
        .name("recorder"),
    );
    World {
        _td: td,
        st,
        app,
        transport,
        events,
        lib: lib_s,
        id,
    }
}

/// The check-fix path: run no-cover-art, fix its finding.
async fn via_check_fix(w: &World) {
    let r = call(
        &w.app,
        "POST",
        "/api/checks/no-cover-art/run",
        Some(&json!({"scope": "all"})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let f = call(&w.app, "GET", "/api/checks/no-cover-art", None)
        .await
        .json();
    let findings = f["findings"].as_array().expect("findings").clone();
    let fid = findings
        .iter()
        .find(|x| x["album_id"] == w.id.as_str())
        .unwrap_or_else(|| panic!("no finding for the album: {f}"))["id"]
        .as_i64()
        .unwrap();
    // The finding of the cover check is the only state the run adds;
    // nothing was requested yet.
    assert!(w.transport.log.lock().unwrap().is_empty());
    let r = call(
        &w.app,
        "POST",
        &format!("/api/checks/no-cover-art/findings/{fid}/fix"),
        Some(&json!({})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
}

/// The AI worker's MusicBrainz cover fallback (`WorkerEnv::find_cover`).
fn via_ai_fallback(st: &AppState, id: &str, lib: &str) {
    let env = AppEnv::new(st);
    let conn = rusqlite::Connection::open(st.db_path()).unwrap();
    let album = Py::dict([
        ("id", Py::str(id.to_string())),
        (
            "path",
            Py::str(format!("{lib}/Ana Artist - Album One [FLAC]")),
        ),
    ]);
    env.find_cover(&conn, &album, "").unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn both_app_paths_store_the_anchor_only_after_() {
    musaic_testdata::require_private!("replay");
    let g: Value = musaic_testdata::golden("resolver/find_release.json");
    let cases: Vec<&Value> = g["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["name"].as_str().unwrap().starts_with("with id:"))
        .collect();
    assert_eq!(
        cases.len(),
        5,
        "the generator's confident name-search cases"
    );
    let mut used_rejected = BTreeSet::new();
    let mut used_browse = 0;
    let mut saved = 0;
    for path in ["check-fix", "ai-fallback"] {
        for case in &cases {
            let name = case["name"].as_str().unwrap();
            assert_eq!(
                case["hint"], "",
                "{name}: both app paths search without a hint"
            );
            let label = format!("{path}: {name}");
            let w = world(case).await;
            match path {
                "check-fix" => via_check_fix(&w).await,
                _ => {
                    let (st, id, lib) = (w.st.clone(), w.id.clone(), w.lib.clone());
                    tokio::task::spawn_blocking(move || via_ai_fallback(&st, &id, &lib))
                        .await
                        .unwrap();
                }
            }

            let mut want_writes = case["store_writes"].clone();
            let mut want_events: Vec<Value> = case["events"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| json!({"type": e["event"], "album_id": e["album_id"], "source": e["source"]}))
                .collect();
            if REJECTED.contains(&name) {
                assert_ne!(want_writes, json!([]), "{label}: delta changes nothing");
                assert!(!want_events.is_empty(), "{label}: delta changes nothing");
                want_writes = json!([]);
                want_events.clear();
                used_rejected.insert(label.clone());
            }
            let conn = rusqlite::Connection::open(w.st.db_path()).unwrap();
            let row: (Option<String>, Option<String>, Option<i64>) = conn
                .query_row(
                    "SELECT mb_release_group_mbid, mb_release_mbid, mb_match_score \
                     FROM albums WHERE id = ?1",
                    [&w.id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            let got_writes = match row {
                (Some(rg), release, score) => {
                    saved += 1;
                    json!([{"album_id": w.id, "release_group_mbid": rg,
                        "release_mbid": release, "score": score}])
                }
                (None, ..) => json!([]),
            };
            assert_eq!(got_writes, want_writes, "{label}: anchor written");
            assert_eq!(*w.events.lock().unwrap(), want_events, "{label}: events");

            // MusicBrainz requests: the Python app's, plus the listing.
            let mut want_req: Vec<String> = case["requests"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["key"].as_str().unwrap().to_string())
                .collect();
            if case["match"]["confident"].as_bool().unwrap() {
                let browse = musaic_net::MusicBrainzClient::<ReplayTransport>::browse_request(
                    case["match"]["mbid"].as_str().unwrap(),
                )
                .replay_key();
                assert!(
                    !want_req.contains(&browse),
                    "{label}: the Python app never browsed"
                );
                want_req.push(browse);
                used_browse += 1;
            }
            let log = w.transport.log.lock().unwrap().clone();
            let mb: Vec<String> = log
                .iter()
                .filter(|(h, _)| h == "musicbrainz.org")
                .map(|(_, k)| k.clone())
                .collect();
            assert_eq!(mb, want_req, "{label}: MusicBrainz requests");
            for (h, _) in &log {
                assert!(
                    h == "musicbrainz.org" || h == "coverartarchive.org",
                    "{label}: unexpected host {h}"
                );
            }
        }
    }
    assert_eq!(
        used_rejected.len(),
        2 * REJECTED.len(),
        "every rejection delta used"
    );
    assert_eq!(
        used_browse, 8,
        "the browse delta on every confident case, both paths"
    );
    assert_eq!(saved, 2, "only the agreeing listing is saved, on each path");
}
