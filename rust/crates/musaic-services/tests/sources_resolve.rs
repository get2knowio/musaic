//! The production sources end to end: `ResolveParts` moved into a plain
//! worker thread (what `spawn_blocking` does), the database album rows of
//! the real fixture library, `DbAlbums`, `NetMusicBrainz` and
//! `AcoustIdFingerprints` over replayed traffic, Tier 0c reachable.
//!
//! Compared with:
//! - captures/expected-anchors/expected-anchors.json (private): which real
//!   albums anchor under the Tier 0c rule, and to which release-group;
//! - testdata/golden/fingerprint/album.json: the Python app's own
//!   `fingerprint_album` rows and `TrackFingerprinted` event for the same
//!   album (Tier 0c fingerprints first, as `_try_fingerprint_vote` does).

mod fp_common;

use std::sync::Arc;

use fp_common::*;
use musaic_events::Event;
use musaic_net::FollowRedirects;
use musaic_resolve::Resolver;
use musaic_services::fingerprint::Fingerprinter;
use musaic_services::sources::ResolveParts;
use serde_json::Value;

const NOW: &str = "2026-11-10T00:00:00Z";
/// The generator's fixed `cache._now_iso()`.
const ROW_TS: &str = "2001-01-01T00:00:00Z";

fn first_call<'a>(real: &'a Value, rel: &str) -> &'a Value {
    real["calls"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["album"] == format!("<ID:{rel}>") && c["label"] == "first")
        .unwrap()
}

fn assert_send_sync_static<T: Send + Sync + 'static>(_: &T) {}

// Tier 0c in production wiring.
#[test]
fn tier_0c_anchors_through_the_production_sources_on_a_worker_thread() {
    musaic_testdata::require_private!(
        "replay",
        "captures/expected-anchors/expected-anchors.json",
        "real-library/library"
    );
    let g = album_golden();
    let Some(real) = g["libraries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["library"] == "real")
    else {
        panic!(
            "the golden has no real-library section (private half of golden/fingerprint/album.json)"
        );
    };
    let expected = {
        let p =
            musaic_testdata::private_required("captures/expected-anchors/expected-anchors.json");
        serde_json::from_str::<Value>(&std::fs::read_to_string(p).unwrap()).unwrap()
    };
    let lib = Lib::build(&real["albums"], &real["fpcalc_files"]);
    lib.insert_albums(&lib.conn(), &real["albums"]);
    let fps: Arc<dyn Fingerprinter> =
        Arc::new(GoldenFingerprints::new(&lib.root, &real["fpcalc_files"]));
    let net = Arc::new(TestNet::default());
    let (bus, events) = recording_bus();
    let parts = ResolveParts {
        resolver: Arc::new(Resolver::new()),
        transport: Arc::new(FollowRedirects(Arc::clone(&net))),
        acoustid_key: Some("golden-test-key".into()),
        fingerprinter: fps,
        bus,
        // One "now" for both clocks, 2026-11-10T00:00:00Z as the resolver
        // goldens (an anchor written now is fresh for Tier 1).
        now_iso: Arc::new(|| NOW.to_owned()),
        now_epoch: Arc::new(|| 1_794_268_800),
    };
    assert_send_sync_static(&parts);

    for rel in ["rip-0001", "rip-0002", "rip-0003"] {
        let row = expected["albums"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["album"] == rel)
            .unwrap();
        let album_id = lib.real(&format!("<ID:{rel}>"));
        let db = lib.db.clone();
        let lib_dir = lib.lib_str();
        let p = parts.clone();
        let id = album_id.clone();
        let res = std::thread::spawn(move || {
            let conn = rusqlite::Connection::open(db).unwrap();
            conn.pragma_update(None, "foreign_keys", "ON").unwrap();
            p.resolve(&conn, Some(&lib_dir), &id, "", false)
        })
        .join()
        .unwrap()
        .unwrap();
        let d = res.to_dict();
        let anchor = d["anchor"]["value"].as_str();
        assert_eq!(anchor, row["anchor"].as_str(), "{rel}: {:#}", d["notes"]);
        if anchor.is_some() {
            assert_eq!(d["anchor_strategy"], "fingerprint-vote", "{rel}");
        }

        // One AcoustID lookup per file, as the Python app's fingerprint_album
        // made: Tier 0c reuses the answers instead of asking again.
        let lookups = net
            .take_log()
            .into_iter()
            .filter(|r| r["url"].as_str().is_some_and(|u| u.contains("acoustid")))
            .count();
        assert_eq!(
            lookups,
            first_call(real, rel)["requests"].as_array().unwrap().len(),
            "{rel}"
        );

        // The rows Tier 0c's fingerprinting wrote are the Python app's
        // fingerprint_album rows for the album; announced once.
        let first = first_call(real, rel);
        let (mut rows, _) = album_rows(&lib.conn(), &album_id);
        for r in &mut rows {
            // The row stamp is the clock's; the generator pinned its own.
            assert_eq!(r["last_computed_at"], NOW);
            r["last_computed_at"] = ROW_TS.into();
        }
        assert_eq!(Value::Array(rows), first["rows_after"], "{rel}");
        let ev: Vec<Event> = std::mem::take(&mut *events.lock().unwrap());
        let fp_events: Vec<Value> = ev
            .iter()
            .filter(|e| matches!(e, Event::TrackFingerprinted { .. }))
            .map(|e| lib.norm_value(&event_json(e)))
            .collect();
        assert_eq!(Value::Array(fp_events), first["events"], "{rel}");
        // resolver.py:490-501: a resolve that wrote a new anchor ends with
        // AlbumMbAnchorSet(source = the anchor's source); Tier 1 recording
        // the release it aligned against publishes "musicbrainz" before it
        // (the cascade golden's order, pinned in musaic-resolve).
        let anchor_events: Vec<String> = ev
            .iter()
            .filter_map(|e| match e {
                Event::AlbumMbAnchorSet {
                    album_id: a,
                    source,
                } if *a == album_id => Some(source.clone()),
                _ => None,
            })
            .collect();
        if anchor.is_some() {
            assert_eq!(
                anchor_events.last().map(String::as_str),
                Some("fingerprint+mb"),
                "{rel}"
            );
        } else {
            assert!(anchor_events.is_empty(), "{rel}: {anchor_events:?}");
        }
    }
}
