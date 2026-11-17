//! A blank genre is filled by "Apply MusicBrainz canonical" from the genre the
//! resolver already computes (the top-voted release-group tag), written as
//! the resolver gives it. A genre the user set is never replaced and a
//! different one is not drift. With no genre to give, the fix keeps its
//! answer ("not available"). A blank genre alone is a warning only when
//! there is something to fill it with.
//!
//! The resolver's answers are stubs built here: no recorded MusicBrainz
//! answer in the public tier carries release-group tags.

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use common::{Got, Sandbox, call, no_key_fingerprinting};
use musaic_api::{AppState, Network, router};
use musaic_checks::{Fact, Resolution, ResolutionSource};
use musaic_events::{Event, Subscriber, Topic};
use rusqlite::Connection;
use serde_json::{Value, json};

/// `AlbumTagsChanged`s seen: (album id, fields).
type Changes = Arc<Mutex<Vec<(String, Vec<String>)>>>;

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
    changed: Changes,
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
    let changed: Changes = Arc::default();
    let c2 = Arc::clone(&changed);
    st.bus().register(
        Topic::Event,
        Subscriber::from_fn(move |ev| {
            if let Event::AlbumTagsChanged { album_id, fields } = ev {
                c2.lock().unwrap().push((album_id.clone(), fields.clone()));
            }
            Ok(())
        }),
    );
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
    let rig = Rig {
        sb,
        app,
        stub,
        changed,
        ids,
    };
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_blank_genre_with_a_canonical_genre_warns_is_filled_and_resolves() {
    let r = rig().await;
    r.run_check().await;
    let (id, f) = r.finding(0).await.expect("a finding for a blank genre");
    assert_eq!(f["severity"], "warn", "{f}");
    assert_eq!(missing(&f), json!(["genre"]), "{f}");
    // Labels only: says one is available, and the fix does not need it.
    assert_eq!(f["detail"]["genre_available"], "folk", "{f}");
    assert!(
        !f["detail"]["ai_fallback_fields"]
            .as_array()
            .unwrap()
            .contains(&json!("genre"))
    );

    let got = r.fix(id).await;
    assert_eq!(got.status, 200, "{}", got.text());
    let body = got.json();
    assert_eq!(body["ok"], true, "{body}");
    assert_eq!(body["message"], "Wrote 1 album field(s)", "{body}");
    assert_eq!(r.col(0, "genre").as_deref(), Some("folk"));
    assert!(
        r.changed
            .lock()
            .unwrap()
            .iter()
            .any(|(a, fields)| *a == r.ids[0] && fields.iter().any(|x| x == "genre")),
        "AlbumTagsChanged names genre: {:?}",
        r.changed.lock().unwrap()
    );

    r.run_check().await;
    let left = r.finding(0).await;
    assert!(left.is_none(), "resolved on the rerun: {left:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_blank_genre_with_nothing_to_fill_it_stays_info_and_not_available() {
    let r = rig().await;
    r.run_check().await;
    let (id, f) = r.finding(1).await.expect("a finding");
    assert_eq!(f["severity"], "info", "{f}");
    assert_eq!(missing(&f), json!(["genre"]), "{f}");
    assert!(f["detail"].get("genre_available").is_none(), "{f}");
    let body = r.fix(id).await.json();
    assert_eq!(body["ok"], false, "{body}");
    assert_eq!(
        body["message"],
        musaic_ai::provider::NOT_AVAILABLE,
        "{body}"
    );
    assert_eq!(r.col(1, "genre"), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_genre_the_user_set_is_untouched_and_is_not_drift() {
    let r = rig().await;
    r.run_check().await;
    let with = r.finding(2).await.map(|(_, f)| f);
    // MusicBrainz now has no genre for it: nothing about the finding moves.
    r.stub(2, resolution(None, Some("2019")));
    r.run_check().await;
    let without = r.finding(2).await.map(|(_, f)| f);
    let strip = |f: &Option<Value>| {
        f.clone().map(|mut f| {
            f.as_object_mut().unwrap().remove("id");
            f
        })
    };
    assert_eq!(strip(&with), strip(&without));
    if let Some(f) = &with {
        assert!(!missing(f).as_array().unwrap().contains(&json!("genre")));
        assert!(
            f["detail"]["album_diffs"]
                .as_array()
                .unwrap()
                .iter()
                .all(|d| d["field"] != "genre"),
            "{f}"
        );
    }
    // Even asked to fix whatever else is open, the genre is never written.
    r.stub(2, resolution(Some("folk"), Some("2019")));
    if let Some((id, _)) = r.finding(2).await {
        let _ = r.fix(id).await;
    }
    assert_eq!(r.col(2, "genre").as_deref(), Some("Jazz"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_missing_year_and_genre_are_written_in_one_apply() {
    let r = rig().await;
    r.run_check().await;
    let (id, f) = r.finding(3).await.expect("a finding");
    assert_eq!(f["severity"], "warn", "{f}");
    let before = r.changed.lock().unwrap().len();
    let body = r.fix(id).await.json();
    assert_eq!(body["ok"], true, "{body}");
    assert_eq!(body["message"], "Wrote 2 album field(s)", "{body}");
    assert_eq!(r.col(3, "year").as_deref(), Some("1999"));
    assert_eq!(r.col(3, "genre").as_deref(), Some("dub"));
    let ev: Vec<_> = r.changed.lock().unwrap()[before..]
        .iter()
        .filter(|(a, _)| *a == r.ids[3])
        .cloned()
        .collect();
    assert_eq!(ev.len(), 1, "one apply, one event: {ev:?}");
    assert!(ev[0].1.iter().any(|x| x == "genre") && ev[0].1.iter().any(|x| x == "year"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_bulk_action_fills_blank_genres_too() {
    let r = rig().await;
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
    assert_eq!(r.col(0, "genre").as_deref(), Some("folk"));
    assert_eq!(r.col(3, "genre").as_deref(), Some("dub"));
    assert_eq!(r.col(1, "genre"), None, "nothing to fill it with");
    assert_eq!(r.col(2, "genre").as_deref(), Some("Jazz"));
}
