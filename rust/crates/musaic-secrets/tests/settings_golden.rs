//! The settings read shape, against
//! testdata/golden/persistence/settings.json (the Python app's
//! settings.all_settings() for scripted rows).
//!
//! Each scenario puts the Python app's rows into a v16 database, opens it, runs
//! the secrets migration, and then reads: the body must equal the Python app's,
//! even though the secrets now come from the store, minus the six fields of
//! the retired model provider and FireCrawl (FR-003, [`RETIRED_FIELDS`]). The
//! store contents must equal what the Python app's accessors returned for the
//! migrated key (settings.acoustid_api_key()); a retired key's row is scrubbed
//! and never reaches the store (FR-027).

mod common;

use common::{golden, open_copy, settings_rows};
use musaic_db::settings;
use musaic_secrets::{
    MIGRATED_SECRET_KEYS, MemoryStore, RETIRED_SECRET_KEYS, SECRET_KEYS, SecretStore, StoreError,
    all_settings, migrate_settings_secrets, secret, set_secret,
};
use serde_json::{Value, json};

/// Fields of the Python app's body that this build no longer returns: the
/// retired model provider's and FireCrawl's settings (FR-003). The golden
/// keeps them, as the Python app's record; the comparison drops them.
const RETIRED_FIELDS: [&str; 6] = [
    "ai_max_parallel",
    "ai_queue_paused",
    "ai_provider_id",
    "ai_model_id",
    "ai_api_key_set",
    "firecrawl_api_key_set",
];

/// The Python app's body minus [`RETIRED_FIELDS`] (each must be in it, so the
/// removal stays exactly those six), plus exactly one added field: asserts the
/// Python app does not already have it, so the addition stays exactly
/// `keyring_available`.
fn with_flag(python: &Value, available: bool) -> Value {
    let mut m = python.as_object().unwrap().clone();
    for f in RETIRED_FIELDS {
        assert!(m.remove(f).is_some(), "{f} is in the Python app's body");
    }
    assert!(!m.contains_key("keyring_available"));
    let before = m.len();
    m.insert("keyring_available".into(), json!(available));
    assert_eq!(m.len(), before + 1);
    Value::Object(m)
}

/// Settings reads, positive and malformed; secrets moved, positive.
#[test]
fn read_shape_matches_the_python_app_after_migration() {
    let g = golden("settings.json");
    let keys: Vec<&str> = g["secret_keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k.as_str().unwrap())
        .collect();
    assert_eq!(keys, SECRET_KEYS);
    let scenarios = g["scenarios"].as_array().unwrap();
    assert_eq!(scenarios.len(), 8);
    for s in scenarios {
        let label = s["label"].as_str().unwrap();
        let (_t, _dir, db) = open_copy("v16-empty.db");
        let conn = db.connection();
        for (k, v) in s["rows"].as_object().unwrap() {
            settings::set_raw(conn, k, v.as_str().unwrap()).unwrap();
        }
        let store = MemoryStore::new();
        migrate_settings_secrets(&db, &store).unwrap();

        let got = Value::Object(all_settings(conn, &store).unwrap());
        assert_eq!(got, with_flag(&s["all_settings"], true), "{label}");

        // Secrets left SQLite; everything else stayed.
        let mut want_rows = s["rows"].as_object().unwrap().clone();
        for k in SECRET_KEYS {
            want_rows.remove(k);
        }
        assert_eq!(settings_rows(conn), want_rows, "{label}");

        // The store holds exactly what the Python app's accessors returned
        // for the migrated key, and nothing for a retired one.
        for k in MIGRATED_SECRET_KEYS {
            assert_eq!(
                json!(secret(&store, k).unwrap()),
                s["accessors"][k],
                "{label} {k}"
            );
        }
        let stored: Vec<String> = store.entries().into_keys().collect();
        let want: Vec<String> = MIGRATED_SECRET_KEYS
            .iter()
            .filter(|k| !s["accessors"][**k].as_str().unwrap().is_empty())
            .map(|k| (*k).to_owned())
            .collect();
        let mut want = want;
        want.sort();
        assert_eq!(stored, want, "{label}");
    }
}

/// With no keyring, nothing moves, nothing is written to
/// SQLite, the secret reads as not set, and the rest of the body is unchanged.
/// Retired rows are scrubbed anyway: they need no keyring.
#[test]
fn unavailable_keyring_leaves_rows_and_reports_unset() {
    let g = golden("settings.json");
    let s = g["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["label"] == "secrets set")
        .unwrap();
    let (_t, _dir, db) = open_copy("v16-empty.db");
    let conn = db.connection();
    for (k, v) in s["rows"].as_object().unwrap() {
        settings::set_raw(conn, k, v.as_str().unwrap()).unwrap();
    }
    let mut before = settings_rows(conn);
    let store = MemoryStore::unavailable();
    let report = migrate_settings_secrets(&db, &store).unwrap();
    assert_eq!(report.failed.len(), MIGRATED_SECRET_KEYS.len());
    assert_eq!(report.dropped_retired, RETIRED_SECRET_KEYS);
    for k in RETIRED_SECRET_KEYS {
        assert!(before.remove(k).is_some(), "the scenario sets {k}");
    }
    assert!(
        report
            .failed
            .iter()
            .all(|(_, e)| matches!(e, StoreError::Unavailable(_)))
    );
    assert_eq!(
        settings_rows(conn),
        before,
        "rows must survive a failed move"
    );

    let mut want = with_flag(&s["all_settings"], false);
    for k in MIGRATED_SECRET_KEYS {
        want[format!("{k}_set")] = json!(false);
    }
    assert_eq!(Value::Object(all_settings(conn, &store).unwrap()), want);

    // A PATCH of a secret fails and does not fall back to SQLite.
    assert!(matches!(
        set_secret(&store, "acoustid_api_key", "test-key-not-real"),
        Err(StoreError::Unavailable(_))
    ));
    assert_eq!(settings_rows(conn), before);

    // Keyring comes back: the next launch completes the move.
    store.set_unavailable(false);
    let report = migrate_settings_secrets(&db, &store).unwrap();
    assert_eq!(report.moved, MIGRATED_SECRET_KEYS);
    assert!(report.dropped_retired.is_empty());
    assert_eq!(
        store.entries().into_keys().collect::<Vec<_>>(),
        MIGRATED_SECRET_KEYS
    );
    assert_eq!(
        Value::Object(all_settings(conn, &store).unwrap()),
        with_flag(&s["all_settings"], true)
    );
}

/// PATCH writes go to the store with the Python app's stripping rules
/// (the Python app: the rows its setters stored for the same calls). A
/// retired key is no longer writable (FR-027): its call is refused and the
/// store holds nothing for it.
#[test]
fn setters_store_like_the_python_app() {
    let g = golden("settings.json");
    let st = &g["setters_store"];
    let (_t, _dir, db) = open_copy("v16-empty.db");
    let store = MemoryStore::new();
    for call in st["calls"].as_array().unwrap() {
        let key = call[0].as_str().unwrap().trim_start_matches("set_");
        let r = set_secret(&store, key, call[1].as_str().unwrap_or(""));
        if RETIRED_SECRET_KEYS.contains(&key) {
            assert!(r.is_err(), "{key} is retired");
        } else {
            r.unwrap();
        }
    }
    for k in RETIRED_SECRET_KEYS {
        assert_eq!(store.get(k).unwrap(), None, "{k}");
    }
    for (k, v) in st["rows"].as_object().unwrap() {
        if RETIRED_SECRET_KEYS.contains(&k.as_str()) {
            continue;
        }
        assert_eq!(
            store.get(k).unwrap().unwrap_or_default(),
            v.as_str().unwrap(),
            "{k}"
        );
    }
    assert!(
        settings_rows(db.connection()).is_empty(),
        "never written to SQLite"
    );
}
