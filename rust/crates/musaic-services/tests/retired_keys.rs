//! The one-time removal of the retired keyring entries (the old built-in
//! provider's model key and FireCrawl key; FR-027): entries present are
//! deleted once and the marker row written; a later start makes no keyring
//! call; a failed delete writes no marker, logs the key name and never a
//! value, and is retried at the next start; an entry that is not there
//! counts as removed.

use std::sync::Mutex;

use musaic_db::Db;
use musaic_secrets::{MemoryStore, RETIRED_SECRET_KEYS, SecretStore};
use musaic_services::host_log::HostLog;
use musaic_services::keyscrub::{RETIRED_KEYS_MARKER, RetiredKeys, remove_retired_keys};

const AI_VALUE: &str = "retired-model-key-0123456789";
const FC_VALUE: &str = "retired-firecrawl-key-9876543210";

#[derive(Default)]
struct Lines(Mutex<Vec<String>>);

impl HostLog for Lines {
    fn info(&self, target: &str, msg: &str) {
        self.0.lock().unwrap().push(format!("INFO {target} {msg}"));
    }
    fn warn(&self, target: &str, msg: &str) {
        self.0.lock().unwrap().push(format!("WARN {target} {msg}"));
    }
    fn error(&self, target: &str, msg: &str) {
        self.0.lock().unwrap().push(format!("ERROR {target} {msg}"));
    }
}

impl Lines {
    fn text(&self) -> String {
        self.0.lock().unwrap().join("\n")
    }
}

fn db() -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let (db, _) = Db::open_or_create(dir.path().join("library.db")).unwrap();
    (dir, db)
}

fn marker(db: &Db) -> Option<String> {
    musaic_db::settings::get_raw(db.connection(), RETIRED_KEYS_MARKER).unwrap()
}

fn retired_calls(store: &MemoryStore) -> Vec<String> {
    store
        .calls()
        .into_iter()
        .filter(|c| RETIRED_SECRET_KEYS.iter().any(|k| c.ends_with(k)))
        .collect()
}

#[test]
fn entries_present_are_deleted_once_and_the_marker_written() {
    let (_dir, db) = db();
    let store = MemoryStore::new();
    store.set("ai_api_key", AI_VALUE).unwrap();
    store.set("firecrawl_api_key", FC_VALUE).unwrap();
    store
        .set("acoustid_api_key", "kept-acoustid-key-value")
        .unwrap();
    let log = Lines::default();

    assert_eq!(remove_retired_keys(&db, &store, &log), RetiredKeys::Removed);
    let entries = store.entries();
    assert!(!entries.contains_key("ai_api_key"));
    assert!(!entries.contains_key("firecrawl_api_key"));
    assert_eq!(
        entries.get("acoustid_api_key").map(String::as_str),
        Some("kept-acoustid-key-value"),
        "the AcoustID key is not touched"
    );
    assert_eq!(marker(&db).as_deref(), Some("1"));

    // A second start makes no keyring call at all.
    let before = store.calls().len();
    assert_eq!(
        remove_retired_keys(&db, &store, &log),
        RetiredKeys::AlreadyRemoved
    );
    assert_eq!(store.calls().len(), before, "no keyring call");
    let text = log.text();
    assert!(
        !text.contains(AI_VALUE) && !text.contains(FC_VALUE),
        "{text}"
    );
}

#[test]
fn no_entry_counts_as_removed() {
    let (_dir, db) = db();
    let store = MemoryStore::new();
    let log = Lines::default();
    assert_eq!(remove_retired_keys(&db, &store, &log), RetiredKeys::Removed);
    assert_eq!(
        retired_calls(&store),
        ["delete:ai_api_key", "delete:firecrawl_api_key"],
        "deletes only, never a read"
    );
    assert_eq!(marker(&db).as_deref(), Some("1"));
    assert!(log.text().is_empty(), "{}", log.text());
}

#[test]
fn a_failed_delete_is_retried_at_the_next_start_and_logs_no_value() {
    let (_dir, db) = db();
    let store = MemoryStore::new();
    store.set("ai_api_key", AI_VALUE).unwrap();
    store.set("firecrawl_api_key", FC_VALUE).unwrap();
    let log = Lines::default();

    // The secret service is locked.
    store.set_unavailable(true);
    assert_eq!(
        remove_retired_keys(&db, &store, &log),
        RetiredKeys::Failed(RETIRED_SECRET_KEYS.to_vec())
    );
    assert_eq!(marker(&db), None, "no marker after a failure");
    let text = log.text();
    assert!(
        text.contains("ai_api_key") && text.contains("firecrawl_api_key"),
        "the failure names the keys: {text}"
    );
    assert!(
        !text.contains(AI_VALUE) && !text.contains(FC_VALUE),
        "{text}"
    );

    // The next start tries again and succeeds.
    store.set_unavailable(false);
    assert_eq!(remove_retired_keys(&db, &store, &log), RetiredKeys::Removed);
    assert!(store.entries().is_empty());
    assert_eq!(marker(&db).as_deref(), Some("1"));
    let text = log.text();
    assert!(
        !text.contains(AI_VALUE) && !text.contains(FC_VALUE),
        "{text}"
    );
}

#[test]
fn the_marker_is_not_in_the_settings_body() {
    let (_dir, db) = db();
    let store = MemoryStore::new();
    remove_retired_keys(&db, &store, &Lines::default());
    let view = musaic_secrets::all_settings(db.connection(), &store).unwrap();
    assert!(!view.contains_key(RETIRED_KEYS_MARKER));
}
