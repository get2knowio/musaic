//! `clear` / `track_clear` on `POST /api/albums/{id}/apply/stream`
//! (issue #210): the explicit request that removes a tag, its dry-run
//! preview, and the 422s for requests that cannot be honoured.

mod common;

use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use common::{Got, Sandbox, call, send};
use serde_json::{Value, json};

const ALBUM: &str = "Mono Harbor - Sorted [FLAC]";

async fn post(app: &axum::Router, id: &str, body: Value) -> Got {
    let req = Request::post(format!("/api/albums/{id}/apply/stream"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    send(app, common::with_host(req)).await
}

/// A 422's `(type, loc, msg)` triples.
fn errors(got: &Got) -> Vec<(String, Value, String)> {
    assert_eq!(got.status, 422, "{}", got.text());
    got.json()["detail"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["type"].as_str().unwrap().to_string(),
                e["loc"].clone(),
                e["msg"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[tokio::test]
async fn refused_before_anything_runs() {
    let sb = Sandbox::new();
    let app = musaic_api::router(sb.state_no_lifespan());
    // Validation comes before the album lookup: an unknown id is fine here.
    let id = "0000000000000000";

    let got = post(&app, id, json!({"clear": ["comment", "genre"]})).await;
    let e = errors(&got);
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].0, "literal_error");
    assert_eq!(e[0].1, json!(["body", "clear", 0]));
    assert!(
        e[0].2
            .starts_with("Input should be 'title', 'album_artist'"),
        "{}",
        e[0].2
    );

    let got = post(
        &app,
        id,
        json!({"track_clear": {"01 - Ordered.flac": ["tracknumber", "isrc"]}}),
    )
    .await;
    let e = errors(&got);
    assert_eq!(
        e.iter()
            .map(|x| (x.0.as_str(), x.1.clone()))
            .collect::<Vec<_>>(),
        [
            (
                "value_error",
                json!(["body", "track_clear", "01 - Ordered.flac", 0])
            ),
            (
                "literal_error",
                json!(["body", "track_clear", "01 - Ordered.flac", 1])
            ),
        ]
    );
    assert_eq!(
        e[0].2,
        "Value error, tracknumber cannot be removed; set a value instead"
    );

    // Both set and removed for the same target: ambiguous, refused.
    let got = post(
        &app,
        id,
        json!({
            "suggestions": {"genre": "Krautrock"},
            "clear": ["genre"],
            "track_suggestions": {"01 - Ordered.flac": {"title": "Ordered Again"}},
            "track_clear": {"01 - Ordered.flac": ["title"], "02 - Indexed.flac": ["title"]},
        }),
    )
    .await;
    let e = errors(&got);
    assert_eq!(
        e.iter().map(|x| x.1.clone()).collect::<Vec<_>>(),
        [
            json!(["body", "clear", 0]),
            json!(["body", "track_clear", "01 - Ordered.flac", 0])
        ]
    );

    // Wrong types are the usual pydantic 422s.
    let got = post(
        &app,
        id,
        json!({"clear": "genre", "track_clear": {"a.flac": "title"}}),
    )
    .await;
    let e = errors(&got);
    assert_eq!(
        e.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(),
        ["list_type", "list_type"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn clear_previews_then_removes() {
    let sb = Sandbox::new();
    let st = sb.state_no_lifespan();
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
    let album = |albums: &Value| {
        albums["albums"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["path"].as_str().unwrap().ends_with(ALBUM))
            .unwrap()
            .clone()
    };
    let row = album(&call(&app, "GET", "/api/albums", None).await.json());
    let id = row["id"].as_str().unwrap().to_string();
    assert_eq!(row["album_artist_sort"], json!("Harbor, Mono"));

    let body = json!({"clear": ["album_artist_sort"], "dry_run": true});
    let got = post(&app, &id, body).await;
    assert_eq!(got.status, 200);
    let text = got.text();
    assert!(
        text.contains(r#""albumartistsort": {"old": "Harbor, Mono", "new": null}"#),
        "{text}"
    );
    assert!(
        text.contains(r#""cleared": ["album_artist_sort"]"#),
        "{text}"
    );
    let row = album(&call(&app, "GET", "/api/albums", None).await.json());
    assert_eq!(row["album_artist_sort"], json!("Harbor, Mono"));

    let got = post(&app, &id, json!({"clear": ["album_artist_sort"]})).await;
    assert!(
        got.text().contains("event: updated_album"),
        "{}",
        got.text()
    );
    let row = album(&call(&app, "GET", "/api/albums", None).await.json());
    assert_eq!(row["album_artist_sort"], Value::Null);
    // Other fields are untouched.
    assert_eq!(row["album_artist"], json!("Mono Harbor"));
}
