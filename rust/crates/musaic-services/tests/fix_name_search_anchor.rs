//! The check fixes' MusicBrainz name search
//! saves a confident hit as the album's anchor only after the
//! track-count and duration check
//! (`musaic_services::sources::find_release_for_fix`).
//!
//! Replays the `with id:` cases of testdata/golden/resolver/find_release.json
//! (recorded for this rule from the Python app's
//! `mb_lookup.find_release` on an album that has an id, so its step 4 saved
//! and published). Compared per case: the match, the anchor columns written,
//! the bus events and the requests. Enumerated deltas, each must be
//! used and must change something:
//! - every confident case: one more request, the release listing the check
//!   reads (`browse_releases`, served by the overlay or unobserved);
//! - "listing has 9 tracks", "listing 20% longer", "listing unrecorded": the
//!   check rejects the hit: no anchor written, no `AlbumMbAnchorSet`.
//!
//! The match itself is the Python app's; a saved anchor publishes
//! `AlbumMbAnchorSet`.

mod common;

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use base64::Engine;
use common::{connect, event_json, fresh_db, recording_bus};
use musaic_core::Album;
use musaic_net::{ReplayTransport, Request, Response, Transport, TransportError};
use musaic_resolve::find_release::FindOptions;
use musaic_resolve::source::AlbumRecord;
use musaic_services::sources::{NetMusicBrainz, find_release_for_fix};
use serde_json::{Value, json};

/// Overlay responses by replay key, else the recorded replay set (private); every request logged.
#[derive(Clone)]
struct Overlay {
    by_key: Arc<HashMap<String, Response>>,
    replay: Arc<ReplayTransport>,
    log: Arc<Mutex<Vec<(String, &'static str)>>>,
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
        Self {
            by_key: Arc::new(by_key),
            replay: Arc::new(ReplayTransport::new(musaic_testdata::private_required(
                "replay",
            ))),
            log: Arc::default(),
        }
    }
}

impl Transport for Overlay {
    fn send(&self, request: &Request) -> Result<Response, TransportError> {
        let key = request.replay_key();
        if let Some(r) = self.by_key.get(&key) {
            self.log.lock().unwrap().push((key, "constructed"));
            return Ok(r.clone());
        }
        let r = self.replay.send(request);
        let served = if r.is_ok() { "recorded" } else { "unobserved" };
        self.log.lock().unwrap().push((key, served));
        r
    }
}

fn album_row(id: &str) -> Album {
    Album {
        id: id.into(),
        path: "/lib/Ana Artist - Album One [FLAC]".into(),
        dir_name: "Ana Artist - Album One [FLAC]".into(),
        title: None,
        artist: None,
        album_artist: None,
        year: None,
        genre: None,
        track_count: 2,
        total_seconds: 600.0,
        has_embedded_cover: false,
        cover_file: None,
        sample_track: "01.flac".into(),
        dir_mtime: 0.0,
        dir_artist: Some("Ana Artist".into()),
        dir_album: Some("Album One".into()),
        is_compilation: false,
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
    }
}

fn s(v: &Value) -> Option<String> {
    v.as_str().map(str::to_string)
}

const REJECTED: [&str; 3] = [
    "with id: confident, listing has 9 tracks",
    "with id: confident, listing 20% longer",
    "with id: confident, listing unrecorded",
];

#[test]
fn fix_name_search_saves_only_after_the_track_count_and_duration_check() {
    musaic_testdata::require_private!("replay");
    let g = musaic_testdata::golden("resolver/find_release.json");
    let cases: Vec<&Value> = g["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["name"].as_str().unwrap().starts_with("with id:"))
        .collect();
    assert_eq!(cases.len(), 5, "the golden's `with id:` cases");
    let mut used_rejected = BTreeSet::new();
    let mut used_browse = 0;
    let mut saved = 0;
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let a = &case["album"];
        let id = a["id"].as_str().unwrap();
        let td = tempfile::tempdir().unwrap();
        let db = fresh_db(td.path());
        let mut conn = connect(&db);
        musaic_db::albums::upsert_album(&mut conn, "/lib", "t", &album_row(id)).unwrap();
        let rec = AlbumRecord {
            id: id.into(),
            dir_name: Some("Ana Artist - Album One [FLAC]".into()),
            dir_artist: s(&a["dir_artist"]),
            dir_album: s(&a["dir_album"]),
            is_compilation: a["is_compilation"].as_bool().unwrap(),
            track_count: a["track_count"].as_i64(),
            total_seconds: a["total_seconds"].as_f64(),
            title: s(&a["title"]),
            album_artist: s(&a["album_artist"]),
            artist: s(&a["artist"]),
            ..Default::default()
        };
        let transport = Overlay::new(&case["overlay"]);
        let mb = NetMusicBrainz::new(transport.clone());
        let (bus, log) = recording_bus();
        let blocked = BTreeSet::new();
        let m = find_release_for_fix(
            &conn,
            &bus,
            &rec,
            FindOptions {
                hint: case["hint"].as_str().unwrap(),
                blocked: &blocked,
                fetch_cover: false,
                fetch_tracklist: false,
            },
            &mb,
            "2030-01-01T00:00:00Z",
        );
        // The match is the Python app's.
        let want = &case["match"];
        assert_eq!(m.confident, want["confident"].as_bool().unwrap(), "{name}");
        assert_eq!(m.score, want["score"].as_i64().unwrap(), "{name}");
        assert_eq!(m.mbid, s(&want["mbid"]), "{name}");
        assert_eq!(m.release_mbid, s(&want["release_mbid"]), "{name}");

        // Writes and events, with the deliberate deltas.
        let mut want_writes = case["store_writes"].clone();
        let mut want_events: Vec<Value> = case["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| json!({"type": e["event"], "album_id": e["album_id"], "source": e["source"]}))
            .collect();
        if REJECTED.contains(&name) {
            assert_ne!(want_writes, json!([]), "{name}: delta changes nothing");
            assert!(!want_events.is_empty(), "{name}: delta changes nothing");
            want_writes = json!([]);
            want_events.clear();
            used_rejected.insert(name);
        }
        let row: (Option<String>, Option<String>, Option<i64>, Option<String>) = conn
            .query_row(
                "SELECT mb_release_group_mbid, mb_release_mbid, mb_match_score, mb_last_checked_at \
                 FROM albums WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        let got_writes = match row {
            (Some(rg), release, score, checked) => {
                assert_eq!(checked.as_deref(), Some("2030-01-01T00:00:00Z"), "{name}");
                saved += 1;
                json!([{"album_id": id, "release_group_mbid": rg, "release_mbid": release,
                    "score": score}])
            }
            (None, ..) => json!([]),
        };
        assert_eq!(got_writes, want_writes, "{name}: anchor written");
        let got_events: Vec<Value> = log.lock().unwrap().iter().map(event_json).collect();
        assert_eq!(got_events, want_events, "{name}: events");

        // Requests: the Python app's, plus (delta) the release listing the check reads.
        let mut want_req: Vec<String> = case["requests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["key"].as_str().unwrap().to_string())
            .collect();
        if m.confident {
            let browse = musaic_net::MusicBrainzClient::<ReplayTransport>::browse_request(
                m.mbid.as_deref().unwrap(),
            )
            .replay_key();
            assert!(
                !want_req.contains(&browse),
                "{name}: the Python app never browsed"
            );
            want_req.push(browse);
            used_browse += 1;
        }
        let got_req: Vec<String> = transport
            .log
            .lock()
            .unwrap()
            .iter()
            .map(|(k, _)| k.clone())
            .collect();
        assert_eq!(got_req, want_req, "{name}: requests");
    }
    assert_eq!(
        used_rejected.len(),
        REJECTED.len(),
        "every rejection delta used"
    );
    assert_eq!(used_browse, 4, "the browse delta on every confident case");
    assert_eq!(saved, 1, "only the agreeing listing is saved");
}
