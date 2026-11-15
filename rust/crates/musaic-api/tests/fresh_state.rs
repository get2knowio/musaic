//! A fresh config dir. The app creates its
//! database with `Db::open_or_create` at the current schema (no embedded
//! v16 file, so no pre-migration backup left next to it), and the
//! settings-secret migration that runs on every open copes with that fresh
//! database: nothing to move on the first open; a secret row written into
//! it afterwards (as the Python app would have) is moved to the keyring and
//! scrubbed on the next open.
//!
//! Compared with: the Python app's fresh database as recorded in
//! testdata/golden/api/cases.json ("db info": the Python app's v16 there, v17
//! here under the schema-upgrade delta enumerated in golden_cases.rs).

mod common;

use std::sync::Arc;

use common::{Sandbox, call, golden};
use musaic_api::{AppState, Network, router};
use musaic_db::{BACKUP_FILE_NAME, Db, SCHEMA_VERSION};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fresh_config_dir_gets_a_current_database_and_no_backup() {
    let sb = Sandbox::new();
    let store = Arc::new(musaic_secrets::MemoryStore::new());
    let st = AppState::builder(&sb.cfg)
        .secrets(store.clone())
        .network(Network::Offline)
        .fingerprinting(common::no_key_fingerprinting())
        .subscribers(false)
        .auto_snapshot(false)
        .build()
        .unwrap();
    let mut names: Vec<String> = std::fs::read_dir(&sb.cfg)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert!(names.contains(&"library.db".to_string()), "{names:?}");
    assert!(
        !names.iter().any(|n| n.starts_with(BACKUP_FILE_NAME)),
        "no pre-migration backup for a database that never existed: {names:?}"
    );
    // The keyring was consulted for nothing (no secret rows).
    assert!(store.entries().is_empty());

    // The Python app's "db info" on its fresh database, at this build's schema.
    let app = router(st.clone());
    let info = call(&app, "GET", "/api/db/info", None).await.json();
    let cases = golden("cases.json");
    let python = cases["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["label"] == "db info")
        .unwrap();
    assert_eq!(python["response"]["json"]["schema_version"], 16);
    assert_eq!(info["schema_version"], SCHEMA_VERSION);
    assert_eq!(info["current_schema"], SCHEMA_VERSION);
    let keys =
        |v: &serde_json::Value| -> Vec<String> { v.as_object().unwrap().keys().cloned().collect() };
    assert_eq!(keys(&info), keys(&python["response"]["json"]));

    // The migration itself on the fresh file: a no-op that reports no
    // error, also for the (absent) backup.
    let (db, _) = Db::open_or_create(st.db_path()).unwrap();
    let report = musaic_secrets::migrate_settings_secrets(&db, store.as_ref()).unwrap();
    assert!(report.is_noop(), "{report:?}");
    assert!(report.backup_error.is_none(), "{report:?}");

    // A secret row in the fresh database moves on the next open.
    musaic_db::settings::set_raw(db.connection(), "acoustid_api_key", " k-123 ").unwrap();
    drop(db);
    drop(app);
    drop(st);
    let _st2 = AppState::builder(&sb.cfg)
        .secrets(store.clone())
        .network(Network::Offline)
        .fingerprinting(common::no_key_fingerprinting())
        .subscribers(false)
        .auto_snapshot(false)
        .build()
        .unwrap();
    assert_eq!(
        store.entries().get("acoustid_api_key").map(String::as_str),
        Some("k-123")
    );
    let (db, _) = Db::open_or_create(sb.cfg.join("library.db")).unwrap();
    assert_eq!(
        musaic_db::settings::get_raw(db.connection(), "acoustid_api_key").unwrap(),
        None
    );
}
