//! Tier 0c (per-track AcoustID
//! fingerprint vote) reached through `GET /api/albums/{id}/resolution`,
//! with the app's production sources (`ResolveParts` over the replay
//! transport wrapped in `FollowRedirects`, `DbAlbums`,
//! `AcoustIdFingerprints`) and the AcoustID key read from the secrets store.
//!
//! Compared with:
//! - captures/expected-anchors/expected-anchors.json in the private test
//!   data (the Tier 0c rule's expected result): which albums anchor, to which
//!   release-group;
//! - testdata/golden/fingerprint/album.json: the Python app's own
//!   `fingerprint_album` rows and `TrackFingerprinted` event for the same
//!   album (the resolve fingerprints first, as `_try_fingerprint_vote`).
//!
//! The library is the golden's stand-ins (file names, no audio) and the
//! fingerprinter answers each file with the Python app's fpcalc print, so the
//! AcoustID requests are the recorded ones (the recorded replay set (private)); the Rust
//! Chromaprint port is checked against fpcalc by musaic-dsp.

mod common;

#[path = "../../musaic-services/tests/fp_common/mod.rs"]
mod fp_common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use fp_common::{GoldenFingerprints, Lib, album_golden, album_rows, event_json};
use musaic_api::{AppState, Fingerprinting, Network, router};
use musaic_events::{Event, Subscriber, Topic};
use serde_json::{Value, json};

/// The services golden's clock: an anchor written now is fresh for Tier 1.
const NOW_EPOCH: u64 = 1_794_268_800; // 2026-11-10T00:00:00Z
const NOW: &str = "2026-11-10T00:00:00Z";
const ROW_TS: &str = "2001-01-01T00:00:00Z";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tier_0c_anchors_through_the_resolution_route() {
    musaic_testdata::require_private!("replay", "captures/expected-anchors/expected-anchors.json");
    let g = album_golden();
    let Some(real) = g["libraries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["library"] == "real")
    else {
        panic!("the golden has no real-library section");
    };
    let expected: Value = serde_json::from_str(
        &std::fs::read_to_string(musaic_testdata::private_required(
            "captures/expected-anchors/expected-anchors.json",
        ))
        .unwrap(),
    )
    .unwrap();
    let lib = Lib::build(&real["albums"], &real["fpcalc_files"]);
    lib.insert_albums(&lib.conn(), &real["albums"]);
    let fps = Arc::new(GoldenFingerprints::new(&lib.root, &real["fpcalc_files"]));
    let secrets = Arc::new(musaic_secrets::MemoryStore::new());
    musaic_secrets::set_secret(secrets.as_ref(), "acoustid_api_key", "golden-test-key").unwrap();
    // The config dir holds the library database Lib built.
    let cfg = lib.db.parent().unwrap().to_path_buf();
    let st = AppState::builder(&cfg)
        .secrets(secrets)
        .network(Network::Replay(musaic_testdata::private_required("replay")))
        .fingerprinting(Fingerprinting::Local(fps))
        .subscribers(false)
        .auto_snapshot(false)
        .clock(Arc::new(|| UNIX_EPOCH + Duration::from_secs(NOW_EPOCH)))
        .build()
        .unwrap();
    let log: Arc<Mutex<Vec<Event>>> = Arc::default();
    let l = log.clone();
    st.bus().register(
        Topic::Event,
        Subscriber::from_fn(move |ev| {
            l.lock().unwrap().push(ev.clone());
            Ok(())
        }),
    );
    let app = router(st);
    let r = common::call(
        &app,
        "POST",
        "/api/config",
        Some(&json!({"library_dir": lib.lib_str()})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());

    let mut anchored = 0;
    for rel in ["rip-0001", "rip-0002", "rip-0003"] {
        let row = expected["albums"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["album"] == rel)
            .unwrap();
        let album_id = lib.real(&format!("<ID:{rel}>"));
        log.lock().unwrap().clear();
        let r = common::call(
            &app,
            "GET",
            &format!("/api/albums/{album_id}/resolution"),
            None,
        )
        .await;
        assert_eq!(r.status, 200, "{rel}: {}", r.text());
        let d = r.json();
        let anchor = d["anchor"]["value"].as_str();
        assert_eq!(anchor, row["anchor"].as_str(), "{rel}: {:#}", d["notes"]);
        if anchor.is_some() {
            anchored += 1;
            assert_eq!(d["anchor_strategy"], "fingerprint-vote", "{rel}");
        }
        let first = real["calls"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["album"] == format!("<ID:{rel}>") && c["label"] == "first")
            .unwrap();
        let (mut rows, impls) = album_rows(&lib.conn(), &album_id);
        for r in &mut rows {
            assert_eq!(r["last_computed_at"], NOW);
            r["last_computed_at"] = ROW_TS.into();
        }
        assert_eq!(Value::Array(rows), first["rows_after"], "{rel}");
        assert!(impls.iter().all(|i| i == musaic_db::FINGERPRINT_IMPL_RUST));
        let ev: Vec<Event> = std::mem::take(&mut *log.lock().unwrap());
        let fp_events: Vec<Value> = ev
            .iter()
            .filter(|e| matches!(e, Event::TrackFingerprinted { .. }))
            .map(|e| lib.norm_value(&event_json(e)))
            .collect();
        assert_eq!(Value::Array(fp_events), first["events"], "{rel}");
        if anchor.is_some() {
            let sources: Vec<String> = ev
                .iter()
                .filter_map(|e| match e {
                    Event::AlbumMbAnchorSet {
                        album_id: a,
                        source,
                    } if *a == album_id => Some(source.clone()),
                    _ => None,
                })
                .collect();
            assert_eq!(sources.last().map(String::as_str), Some("fingerprint+mb"));
        }
    }
    assert!(anchored >= 1, "Tier 0c anchored nothing");
}

// Without an AcoustID key Tier 0c is unavailable on the
// same route: no anchor, nothing fingerprinted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tier_0c_without_a_key_fingerprints_nothing() {
    musaic_testdata::require_private!("replay", "captures/expected-anchors/expected-anchors.json");
    let g = album_golden();
    let real = g["libraries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["library"] == "real")
        .unwrap();
    let lib = Lib::build(&real["albums"], &real["fpcalc_files"]);
    lib.insert_albums(&lib.conn(), &real["albums"]);
    let fps = Arc::new(GoldenFingerprints::new(&lib.root, &real["fpcalc_files"]));
    let cfg = lib.db.parent().unwrap().to_path_buf();
    let st = AppState::builder(&cfg)
        .network(Network::Replay(musaic_testdata::private_required("replay")))
        .fingerprinting(Fingerprinting::Local(fps.clone()))
        .subscribers(false)
        .auto_snapshot(false)
        .clock(Arc::new(|| UNIX_EPOCH + Duration::from_secs(NOW_EPOCH)))
        .build()
        .unwrap();
    let app = router(st);
    common::call(
        &app,
        "POST",
        "/api/config",
        Some(&json!({"library_dir": lib.lib_str()})),
    )
    .await;
    let id = lib.real("<ID:rip-0001>");
    let d = common::call(&app, "GET", &format!("/api/albums/{id}/resolution"), None)
        .await
        .json();
    assert_eq!(d["anchor"], Value::Null);
    assert!(fps.calls.lock().unwrap().is_empty());
}
