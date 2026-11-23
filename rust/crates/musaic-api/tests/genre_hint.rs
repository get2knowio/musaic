//! A blank genre is never written by the tags-out-of-sync fix or its bulk
//! action, whatever MusicBrainz suggests. The finding only says (detail key
//! `genre_available`) that a suggestion exists, so the album panel's
//! corrections list can be found; severity is the same with or without one.
//!
//! The resolver's answers are stubs built here: no recorded MusicBrainz
//! answer in the public tier carries release-group tags.

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use common::{Got, Sandbox, call, no_key_fingerprinting};
use musaic_api::{AppState, Network, router};
use musaic_checks::{Fact, Resolution, ResolutionSource};
use rusqlite::Connection;
use serde_json::{Value, json};

const CHECK: &str = "tags-out-of-sync";

#[derive(Default)]
struct Stub(Mutex<HashMap<String, Resolution>>);

impl ResolutionSource for Stub {
    fn resolve(&self, _conn: &Connection, album_id: &str) -> Result<Resolution, String> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .get(album_id)
            .cloned()
            .unwrap_or_default())
    }
}

fn fact(v: &str) -> Option<Fact> {
    Some(Fact {
        value: v.into(),
        source: "musicbrainz".into(),
    })
}

fn resolution(genre: Option<&str>, year: Option<&str>) -> Resolution {
    Resolution {
        anchor: fact("rg"),
        canonical_genre: genre.and_then(fact),
        canonical_year: year.and_then(fact),
        ..Resolution::default()
    }
}

struct Rig {
    sb: Sandbox,
    app: axum::Router,
    stub: Arc<Stub>,
    ids: Vec<String>,
}

impl Rig {
    fn conn(&self) -> Connection {
        Connection::open(self.sb.cfg.join("library.db")).unwrap()
    }

    fn stub(&self, i: usize, r: Resolution) {
        self.stub.0.lock().unwrap().insert(self.ids[i].clone(), r);
    }

    async fn run_check(&self) {
        let r = call(
            &self.app,
            "POST",
            &format!("/api/checks/{CHECK}/run"),
            Some(&json!({"scope": "all"})),
        )
        .await;
        assert_eq!(r.status, 200, "{}", r.text());
    }

    /// The open finding for album `i`: `(id, severity, summary, detail)`.
    async fn finding(&self, i: usize) -> Option<(i64, Value)> {
        let got = call(
            &self.app,
            "GET",
            &format!("/api/checks/findings?check_id={CHECK}"),
            None,
        )
        .await;
        got.json()["findings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["album_id"] == json!(self.ids[i]))
            .map(|f| (f["id"].as_i64().unwrap(), f.clone()))
    }

    async fn fix(&self, finding: i64) -> Got {
        call(
            &self.app,
            "POST",
            &format!("/api/checks/{CHECK}/findings/{finding}/fix"),
            None,
        )
        .await
    }

    /// Whether any FLAC of album `i` carries a GENRE comment (Vorbis
    /// comments are plain text in the file header).
    fn file_has_genre(&self, i: usize) -> bool {
        let path: String = self
            .conn()
            .query_row(
                "SELECT path FROM albums WHERE id = ?1",
                [&self.ids[i]],
                |r| r.get(0),
            )
            .unwrap();
        let mut any = false;
        for e in std::fs::read_dir(path).unwrap() {
            let p = e.unwrap().path();
            if p.extension().is_some_and(|x| x == "flac") {
                let bytes = std::fs::read(p).unwrap();
                any |= bytes.windows(6).any(|w| w.eq_ignore_ascii_case(b"genre="));
            }
        }
        any
    }

    fn col(&self, i: usize, col: &str) -> Option<String> {
        self.conn()
            .query_row(
                &format!("SELECT {col} FROM albums WHERE id = ?1"),
                [&self.ids[i]],
                |r| r.get(0),
            )
            .unwrap()
    }
}

/// Albums 0..4: 0 blank genre, 1 blank genre (nothing to fill it with),
/// 2 a genre of its own, 3 blank genre and year. All anchored.
async fn rig() -> Rig {
    let sb = Sandbox::new();
    let stub = Arc::new(Stub::default());
    let st: AppState = AppState::builder(&sb.cfg)
        .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
        .network(Network::Offline)
        .fingerprinting(no_key_fingerprinting())
        .frontend_dir(None)
        .subscribers(false)
        .auto_snapshot(false)
        .checks_resolution(stub.clone())
        .build()
        .unwrap();
    let app = router(st);
    let r = call(
        &app,
        "POST",
        "/api/config",
        Some(&json!({"library_dir": Sandbox::s(&sb.lib)})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(
        call(&app, "GET", "/api/scan/stream", None)
            .await
            .text()
            .contains("event: done")
    );
    let list = call(&app, "GET", "/api/albums", None).await.json();
    let ids: Vec<String> = list["albums"]
        .as_array()
        .unwrap()
        .iter()
        .take(4)
        .map(|a| a["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids.len(), 4);
    let rig = Rig { sb, app, stub, ids };
    {
        let c = rig.conn();
        for (i, id) in rig.ids.iter().enumerate() {
            let (genre, year) = match i {
                0 | 1 => (None, Some("2019")),
                2 => (Some("Jazz"), Some("2019")),
                _ => (None, None),
            };
            c.execute(
                "UPDATE albums SET genre = ?1, year = ?2, mb_release_group_mbid = 'rg' \
                 WHERE id = ?3",
                rusqlite::params![genre, year, id],
            )
            .unwrap();
        }
    }
    rig.stub(0, resolution(Some("folk"), Some("2019")));
    rig.stub(1, resolution(None, Some("2019")));
    rig.stub(2, resolution(Some("folk"), Some("2019")));
    rig.stub(3, resolution(Some("dub"), Some("1999")));
    rig
}

fn missing(f: &Value) -> Value {
    f["detail"]["missing_core"].clone()
}

/// Remove GENRE from the files of albums 0, 1 and 3 so the rows and the
/// files agree that it is blank.
async fn blank_files(r: &Rig) {
    for i in [0, 1, 3] {
        let req = axum::http::Request::post(format!("/api/albums/{}/apply/stream", r.ids[i]))
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({"clear": ["genre"]}).to_string(),
            ))
            .unwrap();
        let got = common::send(&r.app, common::with_host(req)).await;
        assert_eq!(got.status, 200, "{}", got.text());
        assert!(!r.file_has_genre(i), "album {i} still has a GENRE tag");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_blank_genre_alone_is_info_with_or_without_a_suggestion_and_says_so() {
    let r = rig().await;
    r.run_check().await;
    let (_, with) = r.finding(0).await.expect("a finding");
    assert_eq!(with["severity"], "info", "{with}");
    assert_eq!(missing(&with), json!(["genre"]), "{with}");
    assert_eq!(with["detail"]["genre_available"], "folk", "{with}");
    let (_, without) = r.finding(1).await.expect("a finding");
    assert_eq!(without["severity"], "info", "{without}");
    assert!(
        without["detail"].get("genre_available").is_none(),
        "{without}"
    );
    // A genre the album has is not a finding about the genre at all.
    let set = r.finding(2).await.map(|(_, f)| f);
    if let Some(f) = set {
        assert!(f["detail"].get("genre_available").is_none(), "{f}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_blank_title_or_year_still_warns_and_still_carries_the_hint() {
    let r = rig().await;
    r.run_check().await;
    let (_, f) = r.finding(3).await.expect("a finding");
    assert_eq!(f["severity"], "warn", "{f}");
    assert_eq!(f["detail"]["genre_available"], "dub", "{f}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_album_without_an_anchor_has_no_hint() {
    let r = rig().await;
    r.conn()
        .execute(
            "UPDATE albums SET mb_release_group_mbid = NULL WHERE id = ?1",
            [&r.ids[0]],
        )
        .unwrap();
    r.stub(
        0,
        Resolution {
            canonical_genre: fact("folk"),
            ..Resolution::default()
        },
    );
    r.run_check().await;
    let (_, f) = r.finding(0).await.expect("a finding");
    assert!(f["detail"].get("genre_available").is_none(), "{f}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_fix_never_writes_a_genre() {
    let r = rig().await;
    blank_files(&r).await;
    r.run_check().await;
    for i in [0, 3] {
        let (id, f) = r.finding(i).await.expect("a finding");
        assert!(f["detail"]["genre_available"].is_string(), "{f}");
        let _ = r.fix(id).await;
        assert_eq!(r.col(i, "genre"), None, "album {i}");
        assert!(!r.file_has_genre(i), "album {i}: the fix wrote GENRE");
    }
    // A genre-only finding has nothing the fix may write.
    let (id, _) = r.finding(0).await.expect("still open");
    let body = r.fix(id).await.json();
    assert_eq!(body["ok"], true, "{body}");
    assert!(body["job_id"].is_string(), "Unresolved genres go to Review");
    assert!(!r.file_has_genre(0));
    // Album 3's year is still written: only the genre is left alone.
    assert_eq!(r.col(3, "year").as_deref(), Some("1999"));
    // A genre the user set is untouched too.
    r.run_check().await;
    if let Some((id, _)) = r.finding(2).await {
        let _ = r.fix(id).await;
    }
    assert_eq!(r.col(2, "genre").as_deref(), Some("Jazz"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_bulk_action_never_writes_a_genre() {
    let r = rig().await;
    blank_files(&r).await;
    r.run_check().await;
    let got = call(
        &r.app,
        "POST",
        &format!("/api/checks/{CHECK}/quick-fix-all"),
        None,
    )
    .await;
    assert_eq!(got.status, 200);
    assert!(got.text().contains("event: done"), "{}", got.text());
    for i in [0, 1, 3] {
        assert_eq!(r.col(i, "genre"), None, "album {i}");
        assert!(
            !r.file_has_genre(i),
            "album {i}: the bulk action wrote GENRE"
        );
    }
    assert_eq!(r.col(2, "genre").as_deref(), Some("Jazz"));
}
