//! The secret
//! audit, end to end. The app is built as the binary builds it (the
//! `ServerLog` sink writing `<config>/server.log`, shared masks), then:
//!
//! 1. a key-scrub report: a key row left in the database by an older build
//!    is moved to a keyring that refuses it with an error text that carries
//!    the value (a hostile backend), so the startup scrub logs its report;
//! 2. a settings save of all three keys (`PATCH /api/settings`);
//! 3. an AcoustID lookup: the save starts a pool pass whose lookups carry
//!    the key (`client=`) to a transport that fails with the raw URL in its
//!    error text;
//! 4. a provider failure whose message carries the AI key and the
//!    FireCrawl key (the job is logged as failed with its message).
//!
//! Then the log file is read: none of the four fake values appears, and the
//! lines that prove each step ran (and that values were replaced) do.
//! The Python app's server.log was observed to hold no key value; this pins
//! the same property for this build's log.

mod common;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{Sandbox, call};
use musaic_ai::provider::replay::{Ending, ReplayProvider, Script};
use musaic_api::logging::{Masks, ServerLog, ServerLogOptions};
use musaic_api::{AppState, Fingerprinting, Network, router};
use musaic_net::{Request, Response, Transport, TransportError};
use musaic_secrets::{MemoryStore, SecretStore, StoreError};
use musaic_services::fingerprint::{CancelToken, Fingerprinter};
use musaic_services::fingerprint_pool::{PoolSleeper, PoolStatus};
use serde_json::json;

const FAKE_ROW: &str = "fakeRowKeyLeftInTheDatabase24680";
const FAKE_ACOUSTID: &str = "FAKEacoustidKEY0123456789";
const FAKE_AI: &str = "fakeAiProviderKeyZYXWVU98765";
const FAKE_FC: &str = "fakeFirecrawlKey13579ACE";

/// Refuses to store the leftover row's value, quoting it in the error.
struct Hostile(MemoryStore);

impl SecretStore for Hostile {
    fn get(&self, key: &str) -> Result<Option<String>, StoreError> {
        self.0.get(key)
    }
    fn set(&self, key: &str, value: &str) -> Result<(), StoreError> {
        if value == FAKE_ROW {
            return Err(StoreError::Backend(format!(
                "refused to store {value} for {key}"
            )));
        }
        self.0.set(key, value)
    }
    fn delete(&self, key: &str) -> Result<(), StoreError> {
        self.0.delete(key)
    }
}

/// Fails every request with the raw (unredacted) URL in its error text and
/// records the raw URLs.
#[derive(Default)]
struct Leaky(Mutex<Vec<String>>);

impl Transport for Leaky {
    fn send(&self, request: &Request) -> Result<Response, TransportError> {
        let mut url = request.url();
        if let Some(form) = &request.form {
            for (k, v) in form {
                url.push_str(&format!(" {k}={v}"));
            }
        }
        self.0.lock().unwrap().push(url.clone());
        Err(TransportError::Other(format!("upstream refused {url}")))
    }
}

struct Fixed;

impl Fingerprinter for Fixed {
    fn fingerprint(&self, _path: &Path) -> Result<(String, u64), String> {
        Ok(("AQAAstub".into(), 3))
    }
    fn implementation(&self) -> &str {
        musaic_db::FINGERPRINT_IMPL_RUST
    }
}

struct NoSleep;

impl PoolSleeper for NoSleep {
    fn sleep(&self, _d: Duration, cancel: &CancelToken) -> bool {
        !cancel.is_cancelled()
    }
}

async fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..3000 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("timed out: {what}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_key_value_reaches_the_server_log() {
    let sb = Sandbox::new();
    // A first launch creates the database; then an older build's key row.
    drop(
        AppState::builder(&sb.cfg)
            .subscribers(false)
            .auto_snapshot(false)
            .log(Arc::new(|_: &str| {}))
            .build()
            .unwrap(),
    );
    let c = rusqlite::Connection::open(sb.cfg.join("library.db")).unwrap();
    c.execute(
        "INSERT OR REPLACE INTO settings(key, value) VALUES ('acoustid_api_key', ?1)",
        [FAKE_ROW],
    )
    .unwrap();
    drop(c);

    // As the binary: one Masks shared by the file sink and the app.
    let masks = Masks::new();
    let (log, opened) = ServerLog::start(
        &sb.cfg,
        20,
        masks.clone(),
        ServerLogOptions {
            console: false,
            ..ServerLogOptions::default()
        },
    );
    let log_path = opened.unwrap();
    let transport = Arc::new(Leaky::default());
    let replay = ReplayProvider::new("claude");
    let st = AppState::builder(&sb.cfg)
        .secrets(Arc::new(Hostile(MemoryStore::new())))
        .network(Network::Live(transport.clone()))
        .fingerprinting(Fingerprinting::Local(Arc::new(Fixed)))
        .pool_sleeper(Arc::new(NoSleep))
        .ai_provider(Arc::new(replay.clone()))
        .subscribers(true)
        .ai_worker(false)
        .auto_snapshot(false)
        .log_sink(log.sink())
        .log_masks(masks)
        .build()
        .unwrap();
    let app = router(st.clone());

    // Library + scan (the pool's first pass fingerprints, no key yet).
    let r = call(
        &app,
        "POST",
        "/api/config",
        Some(&json!({"library_dir": Sandbox::s(&sb.lib)})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let r = call(&app, "GET", "/api/scan/stream", None).await;
    assert!(r.text().contains("event: done"), "{}", r.text());
    eventually("first pool pass", || {
        st.pool().state().status == PoolStatus::Done
    })
    .await;

    // 2 + 3: the settings save; the new AcoustID key restarts the pool,
    // whose lookups carry it.
    let r = call(
        &app,
        "PATCH",
        "/api/settings",
        Some(&json!({
            "acoustid_api_key": FAKE_ACOUSTID,
            "ai_api_key": FAKE_AI,
            "firecrawl_api_key": FAKE_FC,
        })),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let t2 = transport.clone();
    eventually("lookup pass done", || {
        let looked = t2.0.lock().unwrap().iter().any(|u| u.contains("client="));
        looked && st.pool().state().status == PoolStatus::Done
    })
    .await;
    assert!(
        transport
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|u| u.contains(&format!("client={FAKE_ACOUSTID}"))),
        "an AcoustID lookup carried the key: {:?}",
        transport.0.lock().unwrap()
    );

    // 4: a provider failure quoting the keys.
    replay.push(Script {
        events: vec![],
        ending: Ending::Fail(musaic_ai::ProviderError::Other {
            message: format!("401: key {FAKE_AI} rejected (firecrawl {FAKE_FC})"),
            cause: None,
        }),
    });
    let albums = call(&app, "GET", "/api/albums", None).await.json();
    let album = albums["albums"][0]["id"].as_str().unwrap().to_string();
    let r = call(
        &app,
        "POST",
        "/api/ai/jobs",
        Some(&json!({"album_id": album, "job_type": "metadata"})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let job = r.json()["id"].as_str().unwrap().to_string();
    let st2 = st.clone();
    tokio::task::spawn_blocking(move || st2.ai_run_next())
        .await
        .unwrap();
    let failed = call(&app, "GET", &format!("/api/ai/jobs/{job}"), None)
        .await
        .json();
    assert_eq!(failed["status"], "failed", "{failed}");
    assert!(
        failed["error"].as_str().unwrap_or("").contains(FAKE_AI),
        "the provider's message carried the key: {failed}"
    );
    let failed_line = format!("job {job} (metadata, album={album}) failed: ");
    eventually("job failure logged", || {
        log.flush(Duration::from_secs(5));
        std::fs::read_to_string(&log_path).is_ok_and(|t| t.contains(&failed_line))
    })
    .await;

    st.shutdown();
    assert!(log.flush(Duration::from_secs(5)));
    let text = std::fs::read_to_string(&log_path).unwrap();
    for (name, v) in [
        ("leftover row", FAKE_ROW),
        ("AcoustID", FAKE_ACOUSTID),
        ("AI", FAKE_AI),
        ("FireCrawl", FAKE_FC),
    ] {
        assert!(!text.contains(v), "the {name} key reached the log:\n{text}");
    }
    // Each step ran and was logged, values replaced.
    let has = |s: &str| text.lines().any(|l| l.contains(s));
    assert!(
        has("settings secret migration (startup): keys left in the database file")
            && has(
                "acoustid_api_key (the OS keyring failed: refused to store <redacted> for acoustid_api_key)"
            ),
        "{text}"
    );
    assert!(
        has("settings saved: acoustid_api_key (set), ai_api_key (set), firecrawl_api_key (set)")
            || has(
                "settings saved: ai_api_key (set), acoustid_api_key (set), firecrawl_api_key (set)"
            ),
        "{text}"
    );
    assert!(has("Eager fingerprinting done:"), "{text}");
    assert!(
        has(&format!(
            "{failed_line}401: key <redacted> rejected (firecrawl <redacted>)"
        )) || text
            .lines()
            .any(|l| l.contains(&failed_line) && l.contains("<redacted>")),
        "{text}"
    );
    // Every line has the Python app's shape.
    for l in text.lines() {
        let ok = l.len() > 22
            && l.as_bytes()[4] == b'-'
            && l.as_bytes()[10] == b' '
            && l[19..].starts_with(" [")
            && l.contains("] musaic");
        assert!(ok, "line shape: {l}");
    }
}
