//! Before a database this build wrote (v17+) is upgraded, startup takes
//! one copy of it as a snapshot (listed, restorable and pruned with the
//! others), with no secret row in it; when the copy cannot be written the
//! database is not upgraded and startup fails with a clear message.

use std::path::Path;
use std::sync::Arc;

use musaic_api::{AppState, Network};
use rusqlite::Connection;

/// A v18 database as the previous build left it, with a key row the OS
/// keyring has not taken yet.
fn make_v18(cfg: &Path) {
    std::fs::create_dir_all(cfg).unwrap();
    let p = cfg.join("library.db");
    drop(musaic_db::Db::open_or_create(&p).unwrap());
    let c = Connection::open(&p).unwrap();
    c.execute_batch(
        "DROP INDEX idx_track_fingerprints_inputs; DROP TABLE check_inputs; \
         UPDATE schema_version SET version = 18; \
         INSERT INTO settings(key, value) VALUES ('acoustid_api_key', 'SECRET-KEY-VALUE'); \
         INSERT INTO settings(key, value) VALUES ('snapshot_retention_count', '10');",
    )
    .unwrap();
}

fn version(p: &Path) -> i64 {
    Connection::open(p)
        .unwrap()
        .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
        .unwrap()
}

fn build(cfg: &Path) -> Result<AppState, String> {
    AppState::builder(cfg)
        .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
        .network(Network::Offline)
        .frontend_dir(None)
        .subscribers(false)
        .auto_snapshot(false)
        .build()
        .map_err(|e| e.to_string())
}

#[test]
fn a_copy_is_taken_before_the_upgrade() {
    let d = tempfile::tempdir().unwrap();
    let cfg = d.path().join("config");
    make_v18(&cfg);
    let st = build(&cfg).unwrap();
    assert_eq!(version(&cfg.join("library.db")), musaic_db::SCHEMA_VERSION);
    let snaps =
        musaic_db::backup::list_snapshots(&cfg.join(musaic_core::config::SNAPSHOT_DIR_NAME))
            .unwrap();
    assert_eq!(snaps.len(), 1, "{snaps:?}");
    let s = &snaps[0];
    assert!(s.filename.ends_with("-v18.db"), "{}", s.filename);
    assert!(s.valid);
    assert_eq!(s.schema_version, Some(18));
    let copy = cfg
        .join(musaic_core::config::SNAPSHOT_DIR_NAME)
        .join(&s.filename);
    assert_eq!(version(&copy), 18);
    // No secret in the copy, not even in free pages.
    let bytes = std::fs::read(&copy).unwrap();
    assert!(
        !bytes
            .windows(b"SECRET-KEY-VALUE".len())
            .any(|w| w == b"SECRET-KEY-VALUE"),
        "the copy holds a key"
    );
    drop(st);
    // Already current: no second copy.
    drop(build(&cfg).unwrap());
    assert_eq!(
        musaic_db::backup::list_snapshots(&cfg.join(musaic_core::config::SNAPSHOT_DIR_NAME))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn no_copy_no_upgrade() {
    let d = tempfile::tempdir().unwrap();
    let cfg = d.path().join("config");
    make_v18(&cfg);
    // The snapshot folder cannot be made: a file is in its place.
    std::fs::write(
        cfg.join(musaic_core::config::SNAPSHOT_DIR_NAME),
        b"not a folder",
    )
    .unwrap();
    let err = build(&cfg).err().expect("startup must fail");
    assert!(err.contains("before upgrading it from schema v18"), "{err}");
    assert!(err.contains("not changed"), "{err}");
    assert_eq!(version(&cfg.join("library.db")), 18);
}
