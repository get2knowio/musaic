//! Against the real OS keyring. IGNORED: running these raises OS
//! prompts (macOS Keychain) or needs a Secret Service session (Linux). Run on
//! a throwaway account / CI runner with `cargo test -p musaic-secrets -- --ignored`.
//! They use a dedicated service name and clean up after themselves.

use musaic_secrets::{OsKeyring, SecretStore, secret, set_secret};

const TEST_SERVICE: &str = "musaic-keyring-test";

#[test]
#[ignore = "touches the real OS keyring"]
fn os_keyring_round_trip() {
    let store = OsKeyring::new(TEST_SERVICE);
    store.delete("acoustid_api_key").unwrap();
    assert_eq!(store.get("acoustid_api_key").unwrap(), None);
    set_secret(&store, "acoustid_api_key", " test-key-not-real ").unwrap();
    assert_eq!(
        secret(&store, "acoustid_api_key").unwrap(),
        "test-key-not-real"
    );
    set_secret(&store, "acoustid_api_key", "").unwrap();
    assert_eq!(store.get("acoustid_api_key").unwrap(), None);
    store.delete("acoustid_api_key").unwrap(); // absent: still Ok
}

#[test]
#[ignore = "touches the real OS keyring"]
fn os_keyring_migration_from_a_database() {
    use musaic_db::{Db, settings};
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("library.db");
    std::fs::copy(musaic_testdata::public("db/v16-empty.db"), &p).unwrap();
    let (db, _) = Db::open(&p).unwrap();
    settings::set_raw(
        db.connection(),
        "acoustid_api_key",
        "test-acoustid-key-not-real",
    )
    .unwrap();
    let store = OsKeyring::new(TEST_SERVICE);
    let r = musaic_secrets::migrate_settings_secrets(&db, &store).unwrap();
    assert_eq!(r.moved, vec!["acoustid_api_key"]);
    assert_eq!(
        settings::get_raw(db.connection(), "acoustid_api_key").unwrap(),
        None
    );
    musaic_secrets::clear_all(&store).unwrap();
    assert_eq!(store.get("acoustid_api_key").unwrap(), None);
}
