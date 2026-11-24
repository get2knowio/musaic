//! The secret-settings migration as the API runs it (keys move to the OS
//! keyring and out of the database and its pre-upgrade backup): every outcome that is not clean is logged without the
//! key's value, surfaced in the settings body, and retried (timed, after a
//! restore, on shutdown). In-memory secrets store only; the blocking reader
//! is a real second SQLite connection holding a read transaction.
//!
//! Enumerated delta (no Python counterpart, like the settings body's
//! `keyring_available`): the settings body's `keys_pending_removal`, true
//! while keys (or old copies of them) are left in the database file, its
//! pre-migration backup or an older copy (snapshots, restore asides). golden_cases.rs / deliberate.rs / capture_replay.rs
//! add it as `false` to the Python app's settings bodies.

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{Sandbox, call};
use musaic_api::{AppState, Network, router};
use musaic_secrets::{MemoryStore, SecretStore};
use rusqlite::Connection;
use serde_json::json;

const SECRET: &str = "acoustid-SECRET-value-7d1f";

type Log = Arc<Mutex<Vec<String>>>;

fn build(sb: &Sandbox, store: Arc<MemoryStore>, log: &Log, delays: Vec<Duration>) -> AppState {
    let l = log.clone();
    AppState::builder(&sb.cfg)
        .secrets(store)
        .network(Network::Offline)
        .fingerprinting(common::no_key_fingerprinting())
        .subscribers(false)
        .auto_snapshot(false)
        .log(Arc::new(move |s: &str| {
            l.lock().unwrap().push(s.to_string())
        }))
        .key_scrub_retry_delays(delays)
        .build()
        .unwrap()
}

fn put_secret_row(db: &std::path::Path) {
    let c = Connection::open(db).unwrap();
    c.execute(
        "INSERT OR REPLACE INTO settings(key, value) VALUES ('acoustid_api_key', ?1)",
        [SECRET],
    )
    .unwrap();
}

fn secret_row(db: &std::path::Path) -> Option<String> {
    let c = Connection::open(db).unwrap();
    c.query_row(
        "SELECT value FROM settings WHERE key = 'acoustid_api_key'",
        [],
        |r| r.get(0),
    )
    .ok()
}

/// Whether `needle` occurs in the database file or its WAL.
fn on_disk(db: &std::path::Path, needle: &str) -> bool {
    [db.to_path_buf(), db.with_extension("db-wal")]
        .iter()
        .filter_map(|p| std::fs::read(p).ok())
        .any(|b| b.windows(needle.len()).any(|w| w == needle.as_bytes()))
}

async fn pending(app: &axum::Router) -> serde_json::Value {
    let r = call(app, "GET", "/api/settings", None).await;
    assert_eq!(r.status, 200);
    r.json()["keys_pending_removal"].clone()
}

fn no_value_logged(log: &Log) {
    let lines = log.lock().unwrap();
    for l in lines.iter() {
        assert!(!l.contains(SECRET), "a key value was logged: {l}");
    }
}

async fn eventually(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !f() {
        assert!(Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// At startup another connection's read transaction
// blocks the scrub's WAL checkpoint, so the migration returns an error (the
// keyring already holds the key). The outcome is logged (key value never),
// the settings body says keys_pending_removal: true, and the timed retry
// finishes the scrub once the reader is gone: the row and every copy of its
// value leave the database file and its WAL, and the flag goes back to false.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn blocked_startup_scrub_is_logged_surfaced_and_retried() {
    let sb = Sandbox::new();
    let log: Log = Arc::default();
    // First launch: creates the database (WAL mode).
    drop(build(&sb, Arc::new(MemoryStore::new()), &log, vec![]));
    let db = sb.cfg.join("library.db");
    put_secret_row(&db);
    let reader = Connection::open(&db).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    let _: i64 = reader
        .query_row("SELECT COUNT(*) FROM settings", [], |r| r.get(0))
        .unwrap();

    let store = Arc::new(MemoryStore::new());
    let st = build(&sb, store.clone(), &log, vec![Duration::from_millis(100)]);
    let app = router(st.clone());
    assert!(st.key_scrub_pending());
    assert_eq!(pending(&app).await, json!(true));
    assert_eq!(
        store.entries()["acoustid_api_key"],
        SECRET,
        "moved to the keyring"
    );
    {
        let lines = log.lock().unwrap();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("(startup)") && l.contains("will retry")),
            "{lines:?}"
        );
    }
    no_value_logged(&log);

    reader.execute_batch("COMMIT").unwrap();
    drop(reader);
    eventually("retry completed the scrub", 30, || !st.key_scrub_pending()).await;
    assert_eq!(pending(&app).await, json!(false));
    assert_eq!(secret_row(&db), None);
    assert!(!on_disk(&db, SECRET), "no copy of the value left on disk");
    assert!(
        log.lock().unwrap().iter().any(|l| l.contains("completed")),
        "{:?}",
        log.lock().unwrap()
    );
    no_value_logged(&log);
}

/// Restore a snapshot that carries a secret row, through the API.
async fn restore_snapshot_with_secret(app: &axum::Router, st: &AppState) {
    put_secret_row(st.db_path());
    let snap = call(app, "POST", "/api/db/snapshots", None).await;
    assert_eq!(snap.status, 200, "{}", snap.text());
    let name = snap.json()["filename"].as_str().unwrap().to_string();
    let r = call(
        app,
        "POST",
        &format!("/api/db/snapshots/{name}/restore"),
        None,
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
}

// After a restore the migration's result is not
// discarded. With the keyring unavailable the restored row cannot move: the
// outcome is logged (key name, never the value), keys_pending_removal is
// true and the row stays. Once the keyring is back, an explicit retry
// (`AppState::retry_key_scrub`, what the timed retry runs) moves it and
// clears the flag.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unfinished_scrub_after_a_restore_is_surfaced_then_retried() {
    let sb = Sandbox::new();
    let log: Log = Arc::default();
    let store = Arc::new(MemoryStore::new());
    let st = build(&sb, store.clone(), &log, vec![Duration::from_secs(3600)]);
    let app = router(st.clone());
    assert_eq!(pending(&app).await, json!(false));
    store.set_unavailable(true);
    restore_snapshot_with_secret(&app, &st).await;
    assert!(st.key_scrub_pending());
    assert_eq!(pending(&app).await, json!(true));
    assert_eq!(secret_row(st.db_path()).as_deref(), Some(SECRET));
    {
        let lines = log.lock().unwrap();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("(after restore)") && l.contains("acoustid_api_key")),
            "{lines:?}"
        );
    }
    no_value_logged(&log);

    store.set_unavailable(false);
    assert_eq!(
        st.retry_key_scrub(),
        Some(musaic_api::keyscrub::ScrubOutcome::Clean)
    );
    assert!(!st.key_scrub_pending());
    assert_eq!(pending(&app).await, json!(false));
    assert_eq!(secret_row(st.db_path()), None);
    assert_eq!(store.entries()["acoustid_api_key"], SECRET);
    no_value_logged(&log);
}

// An unfinished scrub gets one more try on shutdown
// (`AppState::shutdown`, after the server and the pool have stopped).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_retries_an_unfinished_scrub() {
    let sb = Sandbox::new();
    let log: Log = Arc::default();
    let store = Arc::new(MemoryStore::new());
    let st = build(&sb, store.clone(), &log, vec![Duration::from_secs(3600)]);
    let app = router(st.clone());
    store.set_unavailable(true);
    restore_snapshot_with_secret(&app, &st).await;
    assert!(st.key_scrub_pending());
    store.set_unavailable(false);
    let st2 = st.clone();
    tokio::task::spawn_blocking(move || st2.shutdown())
        .await
        .unwrap();
    assert!(!st.key_scrub_pending());
    assert_eq!(secret_row(st.db_path()), None);
    assert!(
        log.lock().unwrap().iter().any(|l| l.contains("(shutdown)")),
        "{:?}",
        log.lock().unwrap()
    );
    no_value_logged(&log);
}

// The retry waits while writing work holds the restore gate (it would race
// the work's connections): `None`, nothing done.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retry_waits_while_work_holds_the_gate() {
    let sb = Sandbox::new();
    let log: Log = Arc::default();
    let st = build(&sb, Arc::new(MemoryStore::new()), &log, vec![]);
    let g = st.gate().begin_work().unwrap();
    assert_eq!(st.retry_key_scrub(), None);
    drop(g);
    assert_eq!(
        st.retry_key_scrub(),
        Some(musaic_api::keyscrub::ScrubOutcome::Clean)
    );
}

// Deliberate addition (no Python counterpart): a PATCH
// that sets a non-empty AcoustID key restarts the fingerprint pool on the
// configured library (stop, then warm_library, as after a scan), so stored
// fingerprints are looked up without waiting for the next scan. Observed
// through the pool's state: idle before (no scan ran), a pass for the
// library after the key (here `unavailable`, the harness's no-key
// fingerprinter). Another field, or a blank key, leaves it alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn setting_the_acoustid_key_kicks_the_fingerprint_pool() {
    use musaic_services::fingerprint_pool::PoolStatus;
    let sb = Sandbox::new();
    let log: Log = Arc::default();
    let st = build(&sb, Arc::new(MemoryStore::new()), &log, vec![]);
    let app = router(st.clone());
    let lib = Sandbox::s(&sb.lib);
    let r = call(
        &app,
        "POST",
        "/api/config",
        Some(&json!({"library_dir": lib})),
    )
    .await;
    assert_eq!(r.status, 200);
    for body in [
        json!({"snapshot_retention_count": 5}),
        json!({"acoustid_api_key": "  "}),
    ] {
        let r = call(&app, "PATCH", "/api/settings", Some(&body)).await;
        assert_eq!(r.status, 200, "{}", r.text());
        assert_eq!(st.pool().state().status, PoolStatus::Idle, "{body}");
    }
    let r = call(
        &app,
        "PATCH",
        "/api/settings",
        Some(&json!({"acoustid_api_key": "k-new"})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    eventually("pool pass started", 10, || {
        st.pool().state().status != PoolStatus::Idle
    })
    .await;
    let s = st.pool().state();
    assert_eq!(s.library_dir.as_deref(), Some(lib.as_str()));
    assert_eq!(s.status, PoolStatus::Unavailable);
}

/// Whether `needle` occurs in any file under `dir`.
fn anywhere_under(dir: &std::path::Path, needle: &str) -> Vec<std::path::PathBuf> {
    let mut hits = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if std::fs::read(&p)
                .map(|b| b.windows(needle.len()).any(|w| w == needle.as_bytes()))
                .unwrap_or(false)
            {
                hits.push(p);
            }
        }
    }
    hits
}

// A first launch over a config dir whose database and older snapshots (as
// the Python app left them) hold a key: the key moves to the keyring, every
// snapshot is scrubbed, the startup line says what was done, and no file in
// the config dir keeps the value.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn older_snapshots_are_scrubbed_at_startup() {
    let sb = Sandbox::new();
    let log: Log = Arc::default();
    drop(build(&sb, Arc::new(MemoryStore::new()), &log, vec![]));
    let db = sb.cfg.join("library.db");
    put_secret_row(&db);
    let snaps = sb.cfg.join("backups");
    std::fs::create_dir_all(&snaps).unwrap();
    for name in [
        "library-20260101-000000Z-v16.db",
        "library-20260201-000000Z-v16.db",
    ] {
        let c = Connection::open(&db).unwrap();
        c.execute(&format!("VACUUM INTO '{}'", snaps.join(name).display()), [])
            .unwrap();
    }
    assert_eq!(anywhere_under(&snaps, SECRET).len(), 2, "precondition");
    log.lock().unwrap().clear();

    let store = Arc::new(MemoryStore::new());
    let st = build(&sb, store.clone(), &log, vec![]);
    let app = router(st.clone());
    assert!(!st.key_scrub_pending());
    assert_eq!(pending(&app).await, json!(false));
    assert_eq!(store.entries()["acoustid_api_key"], SECRET);
    {
        let lines = log.lock().unwrap();
        assert!(
            lines.iter().any(|l| l.contains(
                "settings secret migration (startup): moved acoustid_api_key to the system \
                 keyring; removed keys from 2 older snapshot(s) or restore backup(s); no API \
                 key left in the database file, its backup or older snapshots"
            )),
            "{lines:?}"
        );
    }
    no_value_logged(&log);
    drop(app);
    let _ = st.shutdown();
    drop(st);
    assert_eq!(
        anywhere_under(&sb.cfg, SECRET),
        Vec::<std::path::PathBuf>::new()
    );
}

// A snapshot that cannot be opened (damaged) and still holds the key's
// bytes: startup goes ahead, the file is named in a WARNING (never the
// value), and the settings body says keys are left.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_damaged_snapshot_is_reported_and_does_not_block_startup() {
    let sb = Sandbox::new();
    let log: Log = Arc::default();
    let snaps = sb.cfg.join("backups");
    std::fs::create_dir_all(&snaps).unwrap();
    let bad = snaps.join("library-20260101-000000Z-v16.db");
    std::fs::write(&bad, format!("damaged acoustid_api_key {SECRET}")).unwrap();
    let store = Arc::new(MemoryStore::new());
    store.set("acoustid_api_key", SECRET).unwrap();
    let st = build(&sb, store, &log, vec![Duration::from_secs(3600)]);
    let app = router(st.clone());
    assert!(st.key_scrub_pending());
    assert_eq!(pending(&app).await, json!(true));
    {
        let lines = log.lock().unwrap();
        assert!(
            lines.iter().any(|l| l.contains("[WARNING]")
                && l.contains("older copies of the database may still hold API keys")
                && l.contains(&bad.display().to_string())
                && l.contains("could not be scrubbed")),
            "{lines:?}"
        );
    }
    no_value_logged(&log);
    assert!(
        anywhere_under(&snaps, SECRET).contains(&bad),
        "left as it was"
    );
}
