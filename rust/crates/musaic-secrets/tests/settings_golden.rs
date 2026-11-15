//! The settings read shape, against
//! testdata/golden/persistence/settings.json (the Python app's
//! settings.all_settings() for scripted rows).
//!
//! Each scenario puts the Python app's rows into a v16 database, opens it, runs
//! the secrets migration, and then reads: the body must equal the Python app's,
//! even though the secrets now come from the store. The store contents must
//! equal what the Python app's accessors returned (settings.ai_api_key() etc.).

mod common;

use common::{golden, open_copy, settings_rows};
use musaic_db::settings;
use musaic_secrets::{
    MemoryStore, SECRET_KEYS, SecretStore, StoreError, all_settings, migrate_settings_secrets,
    secret, set_secret,
};
use serde_json::{Value, json};

/// The Python app's body plus exactly one added field: asserts the Python app
/// does not already have it, so the delta stays exactly `keyring_available`.
fn with_flag(python: &Value, available: bool) -> Value {
    let mut m = python.as_object().unwrap().clone();
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

        // The store holds exactly what the Python app's accessors returned.
        for k in SECRET_KEYS {
            assert_eq!(
                json!(secret(&store, k).unwrap()),
                s["accessors"][k],
                "{label} {k}"
            );
        }
        let stored: Vec<String> = store.entries().into_keys().collect();
        let want: Vec<String> = SECRET_KEYS
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
/// SQLite, every secret reads as not set, and the rest of the body is unchanged.
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
    let before = settings_rows(conn);
    let store = MemoryStore::unavailable();
    let report = migrate_settings_secrets(&db, &store).unwrap();
    assert_eq!(report.failed.len(), 3);
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
    for k in SECRET_KEYS {
        want[format!("{k}_set")] = json!(false);
    }
    assert_eq!(Value::Object(all_settings(conn, &store).unwrap()), want);

    // A PATCH of a secret fails and does not fall back to SQLite.
    assert!(matches!(
        set_secret(&store, "ai_api_key", "test-key-not-real"),
        Err(StoreError::Unavailable(_))
    ));
    assert_eq!(settings_rows(conn), before);

    // Keyring comes back: the next launch completes the move.
    store.set_unavailable(false);
    let report = migrate_settings_secrets(&db, &store).unwrap();
    assert_eq!(report.moved.len(), 3);
    assert_eq!(
        Value::Object(all_settings(conn, &store).unwrap()),
        with_flag(&s["all_settings"], true)
    );
}

/// PATCH writes go to the store with the Python app's stripping rules
/// (the Python app: the rows its setters stored for the same calls).
#[test]
fn setters_store_like_the_python_app() {
    let g = golden("settings.json");
    let st = &g["setters_store"];
    let (_t, _dir, db) = open_copy("v16-empty.db");
    let store = MemoryStore::new();
    for call in st["calls"].as_array().unwrap() {
        let key = call[0].as_str().unwrap().trim_start_matches("set_");
        set_secret(&store, key, call[1].as_str().unwrap_or("")).unwrap();
    }
    for (k, v) in st["rows"].as_object().unwrap() {
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
