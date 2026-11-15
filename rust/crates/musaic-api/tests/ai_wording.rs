//! Backend text names AI only when a provider is ready, through the HTTP face on the fixture library:
//!
//! - readiness is read when text is shown: one app whose provider's `ready`
//!   flag flips shows the Python app's wording while ready and the neutral one
//!   otherwise, for check descriptions (`GET /api/checks`, which also builds
//!   the checks stream's frames, and check detail), finding summaries
//!   (`GET /api/checks/findings`, check detail,
//!   ignore / unignore answers), the resolver's trace note
//!   (`GET /api/albums/{id}/resolution`) and the quit prompt;
//! - stored text never changes: the tags-out-of-sync finding of an
//!   unanchored album keeps the Python app's summary in the database whatever the
//!   readiness, so re-running the check after a flip upserts the same row
//!   (same id, still ignored if it was), with no second finding and nothing
//!   resolved.
//!
//! Expected texts come from the Python app's record (testdata/golden/checks/pure.json
//! meta, testdata/golden/api/cases.json resolution notes) through the
//! enumerated delta in tests/common/ai_wording.rs.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use common::{Sandbox, call};
use musaic_ai::Provider;
use musaic_ai::provider::{ProviderError, ProviderEvent, ProviderInfo, ProviderRequest};
use musaic_api::{AppState, Network, router};
use serde_json::{Value, json};

/// A provider whose readiness the test flips.
struct Switch(Arc<AtomicBool>);

impl Provider for Switch {
    fn id(&self) -> &str {
        "claude"
    }
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "claude".into(),
            accepts_api_key: false,
            auth_hint: None,
            ready: self.0.load(Ordering::SeqCst),
            requirement: None,
            remedy: None,
        }
    }
    fn run(
        &self,
        _request: &ProviderRequest,
        _on_event: &mut dyn FnMut(ProviderEvent),
    ) -> Result<(), ProviderError> {
        Err(ProviderError::Other {
            message: "not called in this test".into(),
            cause: None,
        })
    }
}

fn state(sb: &Sandbox, provider: Arc<dyn Provider>) -> AppState {
    AppState::builder(&sb.cfg)
        .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
        .network(Network::Offline)
        .fingerprinting(common::no_key_fingerprinting())
        .subscribers(true)
        .auto_snapshot(false)
        .ping_interval(Duration::from_secs(30))
        .ai_provider(provider)
        .build()
        .unwrap()
}

fn checks_golden_meta() -> Value {
    let p = musaic_testdata::public("golden/checks/pure.json");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
    v["meta"].clone()
}

fn ai_words(s: &str) -> bool {
    regex::Regex::new(r"\bAI\b|Claude").unwrap().is_match(s)
}

async fn run_check(app: &axum::Router, id: &str) {
    let r = call(
        app,
        "POST",
        &format!("/api/checks/{id}/run"),
        Some(&json!({"scope": "all"})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
}

/// Every tags-out-of-sync finding row, as stored.
fn stored(st: &AppState) -> Vec<(i64, String, Option<String>, Option<String>)> {
    let conn = musaic_db::open_connection(st.db_path()).unwrap();
    let mut q = conn
        .prepare(
            "SELECT id, summary, resolved_at, ignored_at FROM check_findings \
             WHERE check_id = 'tags-out-of-sync' ORDER BY id",
        )
        .unwrap();
    q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ai_wording_wording_follows_readiness_and_stored_text_does_not() {
    let sb = Sandbox::new();
    let ready = Arc::new(AtomicBool::new(false));
    let st = state(&sb, Arc::new(Switch(ready.clone())));
    let app = router(st.clone());
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

    // --- check descriptions: the Python app's record, and the delta without a provider.
    let python_meta = checks_golden_meta();
    let mut neutral_meta = python_meta.clone();
    assert_eq!(common::ai_wording::json(&mut neutral_meta), 3);
    let shown_meta = |v: &Value| -> Value {
        Value::Array(
            v["checks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c["meta"].clone())
                .collect(),
        )
    };
    let list = call(&app, "GET", "/api/checks", None).await.json();
    assert_eq!(shown_meta(&list), neutral_meta);
    assert!(!ai_words(&list.to_string()), "{list}");
    ready.store(true, Ordering::SeqCst);
    let list = call(&app, "GET", "/api/checks", None).await.json();
    assert_eq!(shown_meta(&list), python_meta);

    // --- the tags-out-of-sync finding of an unanchored album, found while ready.
    run_check(&app, "tags-out-of-sync").await;
    let rows = stored(&st);
    let open: Vec<_> = rows.iter().filter(|r| r.2.is_none()).collect();
    let (fid, summary) = open
        .iter()
        .find(|r| {
            r.1.ends_with(musaic_checks::checks::tags_out_of_sync::NO_ANCHOR_SUFFIX)
        })
        .map(|r| (r.0, r.1.clone()))
        .expect("an unanchored tags-out-of-sync finding");
    let mut neutral_summary = json!({"check_id": "tags-out-of-sync", "summary": summary});
    assert_eq!(common::ai_wording::json(&mut neutral_summary), 1);
    let neutral_summary = neutral_summary["summary"].as_str().unwrap().to_string();
    assert!(!ai_words(&neutral_summary));

    let shown = |v: &Value, id: i64| -> String {
        v["findings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["id"] == id)
            .unwrap_or_else(|| panic!("{v}"))["summary"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let all = call(
        &app,
        "GET",
        "/api/checks/findings?check_id=tags-out-of-sync",
        None,
    )
    .await
    .json();
    assert_eq!(shown(&all, fid), summary);
    let detail = call(&app, "GET", "/api/checks/tags-out-of-sync", None)
        .await
        .json();
    assert_eq!(shown(&detail, fid), summary);
    assert_eq!(detail["meta"], meta_of(&python_meta, "tags-out-of-sync"));

    // Ignore it while ready, then lose the provider.
    let ig = call(
        &app,
        "POST",
        &format!("/api/checks/tags-out-of-sync/findings/{fid}/ignore"),
        Some(&json!({"reason": "later"})),
    )
    .await;
    assert_eq!(ig.status, 200, "{}", ig.text());
    assert_eq!(ig.json()["summary"], json!(summary));
    ready.store(false, Ordering::SeqCst);

    // Shown neutral everywhere, stored unchanged.
    let all = call(
        &app,
        "GET",
        "/api/checks/findings?check_id=tags-out-of-sync&include_ignored=true",
        None,
    )
    .await
    .json();
    assert_eq!(shown(&all, fid), neutral_summary);
    assert!(!ai_words(&all.to_string()), "{all}");
    let un = call(
        &app,
        "POST",
        &format!("/api/checks/tags-out-of-sync/findings/{fid}/unignore"),
        None,
    )
    .await;
    assert_eq!(un.status, 200, "{}", un.text());
    assert_eq!(un.json()["summary"], json!(neutral_summary));
    let ig = call(
        &app,
        "POST",
        &format!("/api/checks/tags-out-of-sync/findings/{fid}/ignore"),
        Some(&json!({"reason": "later"})),
    )
    .await;
    assert_eq!(ig.json()["summary"], json!(neutral_summary));
    let before = stored(&st);

    // Re-run without a provider: the same rows, the same stored summary, the
    // finding still ignored, nothing new and nothing resolved.
    run_check(&app, "tags-out-of-sync").await;
    let after = stored(&st);
    assert_eq!(
        after
            .iter()
            .map(|r| (r.0, r.1.clone(), r.2.clone()))
            .collect::<Vec<_>>(),
        before
            .iter()
            .map(|r| (r.0, r.1.clone(), r.2.clone()))
            .collect::<Vec<_>>()
    );
    let row = after.iter().find(|r| r.0 == fid).unwrap();
    assert_eq!(row.1, summary, "stored summary is the Python app's");
    assert!(row.3.is_some(), "still ignored");
    let detail = call(&app, "GET", "/api/checks/tags-out-of-sync", None)
        .await
        .json();
    assert!(!ai_words(&detail.to_string()), "{detail}");

    // And back: ready again, re-run, same rows, the original wording shown.
    ready.store(true, Ordering::SeqCst);
    run_check(&app, "tags-out-of-sync").await;
    let again = stored(&st);
    assert_eq!(
        again
            .iter()
            .map(|r| (r.0, r.1.clone(), r.2.clone()))
            .collect::<Vec<_>>(),
        before
            .iter()
            .map(|r| (r.0, r.1.clone(), r.2.clone()))
            .collect::<Vec<_>>()
    );
    let all = call(
        &app,
        "GET",
        "/api/checks/findings?check_id=tags-out-of-sync&include_ignored=true",
        None,
    )
    .await
    .json();
    assert_eq!(shown(&all, fid), summary);

    // --- the resolver's trace note (memoized: the same resolution both ways).
    let albums = call(&app, "GET", "/api/albums", None).await.json();
    let mut checked = 0;
    for a in albums["albums"].as_array().unwrap() {
        let id = a["id"].as_str().unwrap();
        ready.store(true, Ordering::SeqCst);
        let with = call(&app, "GET", &format!("/api/albums/{id}/resolution"), None)
            .await
            .json();
        if !with["notes"]
            .as_array()
            .unwrap()
            .contains(&json!(musaic_resolve::resolution::UNANCHORED_NOTE))
        {
            continue;
        }
        ready.store(false, Ordering::SeqCst);
        let without = call(&app, "GET", &format!("/api/albums/{id}/resolution"), None)
            .await
            .json();
        let mut want = with.clone();
        assert_eq!(common::ai_wording::json(&mut want), 1);
        assert_eq!(without, want);
        assert!(!ai_words(&without.to_string()), "{without}");
        checked += 1;
    }
    assert!(checked > 0, "an unanchored album");

    // --- the quit prompt.
    ready.store(false, Ordering::SeqCst);
    let mut snap = st.work_snapshot();
    snap.kinds = vec![(musaic_services::gate::WorkKind::AiJob, 1)];
    assert!(!snap.ai_ready);
    let p = snap.quit_prompt().unwrap();
    assert!(p.contains("a job") && !ai_words(&p), "{p}");
    ready.store(true, Ordering::SeqCst);
    let mut snap = st.work_snapshot();
    snap.kinds = vec![(musaic_services::gate::WorkKind::AiJob, 1)];
    assert!(snap.quit_prompt().unwrap().contains("an AI job"));
    st.shutdown();
}

/// The Python app's meta of check `id`.
fn meta_of(all: &Value, id: &str) -> Value {
    all.as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == id)
        .unwrap()
        .clone()
}
