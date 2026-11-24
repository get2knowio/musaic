//! The pre-migration backup of a database older than v16
//! (`library.v12.before-rust.db`) is scrubbed too. Fixture: the genuine v12
//! database written by the Python app's own v12 code
//! (testdata/golden/persistence/old-schemas/v12.db). Its AcoustID key moves
//! to the keyring; its FireCrawl key, a retired one, is scrubbed and never
//! moved.

mod common;

use musaic_db::{Db, backup_file_name};
use musaic_secrets::{MemoryStore, migrate_settings_secrets};

const KEYS: [&str; 2] = [
    "FAKE-ACOUSTID-KEY-old-schema-golden",
    "fc-FAKE-FIRECRAWL-KEY-old-schema-golden",
];

fn holds(bytes: &[u8], needle: &str) -> bool {
    bytes.windows(needle.len()).any(|w| w == needle.as_bytes())
}

#[test]
fn backup_of_a_v12_database_keeps_no_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.db");
    std::fs::copy(
        musaic_testdata::public("golden/persistence/old-schemas/v12.db"),
        &path,
    )
    .unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(
        KEYS.iter().all(|k| holds(&before, k)),
        "fixture holds the fake keys"
    );

    let (db, _) = Db::open(&path).unwrap();
    let backup = dir.path().join(backup_file_name(12));
    assert!(backup.is_file(), "backup is named after the version found");
    assert!(
        KEYS.iter()
            .any(|k| holds(&std::fs::read(&backup).unwrap(), k))
    );

    let store = MemoryStore::new();
    let report = migrate_settings_secrets(&db, &store).unwrap();
    assert!(report.backup_error.is_none(), "{:?}", report.backup_error);
    assert_eq!(report.moved, vec!["acoustid_api_key"]);
    assert_eq!(report.dropped_retired, vec!["firecrawl_api_key"]);
    assert!(report.backup_scrubbed.contains(&"firecrawl_api_key"));
    assert_eq!(
        store.entries().into_iter().collect::<Vec<_>>(),
        [("acoustid_api_key".to_owned(), KEYS[0].to_owned())]
    );
    assert!(
        !store.calls().iter().any(|c| c.contains("firecrawl")),
        "no keyring call for a retired key: {:?}",
        store.calls()
    );
    drop(db);

    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let p = entry.unwrap().path();
        let bytes = std::fs::read(&p).unwrap();
        for k in KEYS {
            assert!(!holds(&bytes, k), "{} still holds a key", p.display());
        }
    }
    assert!(backup.is_file(), "the backup itself is kept");
}
