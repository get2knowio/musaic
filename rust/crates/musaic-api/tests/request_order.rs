//! Request bodies keep their key order. The Python app's pydantic models hold dicts in the order
//! `json.loads` read them; serde_json's `Map` is sorted, so before this fix
//! the apply wrote Vorbis comments, and listed `AlbumTagsChanged.fields`,
//! alphabetically.
//!
//! Evidence: a recorded run of the Python app, scenario `apply` (the
//! request body below; the Python app's event in
//! captures/request-order/library-events.json and its FLAC comment list in
//! captures/request-order/state/apply.json).

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use common::{Sandbox, call, send};
use musaic_api::lax::KeyOrder;
use musaic_events::{Event, EventKind, Subscriber, Topic};
use serde_json::{Value, json};

const CAPTURE: &str = "captures/request-order";
const ALBUM: &str = "Aurora Fields - First Light [FLAC]";

/// The recorded `apply` request body, as the Python app received it (the
/// raw text: key order is the point).
const APPLY_BODY: &str = r#"{"suggestions": {"genre": "Verify Ambient", "year": "2001", "album_artist_sort": "Fields, Aurora", "artist_sort": "   ", "album_sort": ""}, "track_suggestions": {"01 - Dawn Chorus.flac": {"title": "Dawn Chorus (Edit)"}, "03 - Meridian.flac": {"tracknumber": "6"}}, "cover_url": null, "dry_run": false}"#;

fn capture(rel: &str) -> Value {
    musaic_testdata::public_json(format!("{CAPTURE}/{rel}"))
}

/// The VORBIS_COMMENT entries of a FLAC, in file order, keys as written.
fn comment_keys(path: &std::path::Path) -> Vec<String> {
    let b = std::fs::read(path).unwrap();
    assert_eq!(&b[..4], b"fLaC");
    let mut i = 4;
    loop {
        let last = b[i] & 0x80 != 0;
        let kind = b[i] & 0x7f;
        let len = u32::from_be_bytes([0, b[i + 1], b[i + 2], b[i + 3]]) as usize;
        let block = &b[i + 4..i + 4 + len];
        if kind == 4 {
            let u32le = |p: usize| u32::from_le_bytes(block[p..p + 4].try_into().unwrap()) as usize;
            let mut p = 4 + u32le(0);
            let n = u32le(p);
            p += 4;
            let mut out = Vec::new();
            for _ in 0..n {
                let l = u32le(p);
                let s = String::from_utf8_lossy(&block[p + 4..p + 4 + l]).into_owned();
                out.push(s.split('=').next().unwrap().to_string());
                p += 4 + l;
            }
            return out;
        }
        assert!(!last, "no VORBIS_COMMENT");
        i += 4 + len;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn apply_keeps_payload_key_order() {
    let sb = Sandbox::new();
    let st = sb.state_no_lifespan();
    let seen: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
    let s2 = seen.clone();
    st.bus().register(
        Topic::Kind(EventKind::AlbumTagsChanged),
        Subscriber::from_fn(move |e| {
            if let Event::AlbumTagsChanged { fields, .. } = e {
                s2.lock().unwrap().push(fields.clone());
            }
            Ok(())
        }),
    );
    let app = musaic_api::router(st.clone());
    let r = call(
        &app,
        "POST",
        "/api/config",
        Some(&json!({"library_dir": Sandbox::s(&sb.lib)})),
    )
    .await;
    assert_eq!(r.status, 200);
    assert!(
        call(&app, "GET", "/api/scan/stream", None)
            .await
            .text()
            .contains("event: done")
    );
    assert!(st.bus().wait_background_idle(Duration::from_secs(30)));
    let albums = call(&app, "GET", "/api/albums", None).await.json();
    let id = albums["albums"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["path"].as_str().unwrap().ends_with(ALBUM))
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    seen.lock().unwrap().clear();
    let req = Request::post(format!("/api/albums/{id}/apply/stream"))
        .header("content-type", "application/json")
        .body(Body::from(APPLY_BODY))
        .unwrap();
    let got = send(&app, common::with_host(req)).await;
    assert_eq!(got.status, 200);
    assert!(got.text().contains("event: done"), "{}", got.text());

    // The Python app's AlbumTagsChanged for this apply.
    let ev = capture("library-events.json");
    let python_fields: Vec<String> = ev["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["phase"] == "apply/apply" && e["event"] == "AlbumTagsChanged")
        .map(|e| serde_json::from_value(e["data"]["fields"].clone()).unwrap())
        .next()
        .expect("python AlbumTagsChanged in the apply scenario");
    assert_eq!(
        python_fields,
        [
            "genre",
            "year",
            "album_artist_sort",
            "artist_sort",
            "album_sort"
        ]
    );
    assert_eq!(seen.lock().unwrap().first(), Some(&python_fields));

    // Comment order of the first track equals the Python app's.
    let state = capture("state/apply.json");
    let python_keys: Vec<String> =
        state["library"][format!("{ALBUM}/01 - Dawn Chorus.flac")]["flac"]["comment_list"]
            .as_array()
            .unwrap()
            .iter()
            .map(|kv| kv[0].as_str().unwrap().to_string())
            .collect();
    let ours = comment_keys(&sb.lib.join(ALBUM).join("01 - Dawn Chorus.flac"));
    assert_eq!(ours, python_keys);
}

/// Python dict semantics of the order itself — first-seen
/// position, a repeated key keeps its place and takes the last value
/// (`json.loads('{"b":1,"a":2,"b":3}')` == `{'b': 3, 'a': 2}`), nested
/// objects carry their own order, arrays and scalars carry none.
#[test]
fn key_order_follows_json_loads() {
    let o = KeyOrder::of_body(
        br#"{"b": 1, "a": {"z": 1, "y": [ {"q": 1} ]}, "b": {"k2": 0, "k1": 0}}"#,
    );
    assert_eq!(o.keys().collect::<Vec<_>>(), ["b", "a"]);
    assert_eq!(o.child("a").keys().collect::<Vec<_>>(), ["z", "y"]);
    assert_eq!(o.child("b").keys().collect::<Vec<_>>(), ["k2", "k1"]);
    assert_eq!(o.child("a").child("y").keys().count(), 0);
    assert_eq!(KeyOrder::of_body(b"not json").keys().count(), 0);
}

/// A 422 for a bad dict entry lists the errors in body
/// order, as pydantic validates dict items in insertion order.
#[tokio::test]
async fn dict_entry_errors_in_body_order() {
    let sb = Sandbox::new();
    let app = musaic_api::router(sb.state_no_lifespan());
    let req = Request::post("/api/albums/0000000000000000/apply/stream")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"suggestions": {"zeta": 1, "alpha": 2}}"#))
        .unwrap();
    let got = send(&app, common::with_host(req)).await;
    assert_eq!(got.status, 422);
    let locs: Vec<Value> = got.json()["detail"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["loc"].clone())
        .collect();
    assert_eq!(
        locs,
        [
            json!(["body", "suggestions", "zeta"]),
            json!(["body", "suggestions", "alpha"])
        ]
    );
}

/// `PUT /api/albums/{id}/tags`: the Python app walks
/// `track_tags.items()` in body order (cache.replace_album_track_tags);
/// filenames are validated against the album listing (a deliberate
/// change), so the first
/// bad filename in that order is the one named, and with valid names
/// the rows land in body order (rowid order of track_tags).
#[tokio::test(flavor = "multi_thread")]
async fn track_tags_put_reads_body_order() {
    let sb = Sandbox::new();
    let st = sb.state_no_lifespan();
    let app = musaic_api::router(st.clone());
    call(
        &app,
        "POST",
        "/api/config",
        Some(&json!({"library_dir": Sandbox::s(&sb.lib)})),
    )
    .await;
    assert!(
        call(&app, "GET", "/api/scan/stream", None)
            .await
            .text()
            .contains("event: done")
    );
    let albums = call(&app, "GET", "/api/albums", None).await.json();
    let id = albums["albums"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["path"].as_str().unwrap().ends_with(ALBUM))
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let put = |body: &'static str| {
        common::with_host(
            Request::put(format!("/api/albums/{id}/tags"))
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
    };
    let r = send(
        &app,
        put(r#"{"track_tags": {"zz-not-here.flac": ["a"], "aa-not-here.flac": ["b"]}}"#),
    )
    .await;
    assert_eq!(r.status, 400, "{}", r.text());
    assert!(r.text().contains("zz-not-here.flac"), "{}", r.text());
    let r = send(
        &app,
        put(r#"{"track_tags": {"05 - Afterglow.flac": ["late"], "01 - Dawn Chorus.flac": ["early"]}}"#),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let conn = rusqlite::Connection::open(st.db_path()).unwrap();
    let rows: Vec<String> = conn
        .prepare("SELECT filename FROM track_tags WHERE album_id = ?1 ORDER BY rowid")
        .unwrap()
        .query_map([&id], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(rows, ["05 - Afterglow.flac", "01 - Dawn Chorus.flac"]);
}
