//! The startup snapshot reads `snapshot_min_interval_hours` as the settings
//! body shows it (issue #213): a stored infinity or NaN is unreadable and
//! means the 24-hour default, not "never" or "every start".

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use musaic_db::{Db, backup, settings};
use musaic_services::host_log::HostLog;
use musaic_services::startup::auto_snapshot_on_startup;

struct Quiet;

impl HostLog for Quiet {
    fn info(&self, _: &str, _: &str) {}
    fn warn(&self, _: &str, msg: &str) {
        panic!("unexpected warning: {msg}");
    }
    fn error(&self, _: &str, msg: &str) {
        panic!("unexpected error: {msg}");
    }
}

const NOW: u64 = 1_767_225_600; // 2026-01-01T00:00:00Z

fn snapshots(dir: &Path) -> usize {
    backup::list_snapshots(dir).unwrap().len()
}

/// A config dir with an open copy of the empty fixture, one snapshot
/// `age_hours` old, and the interval stored as `raw`.
fn setup(raw: &str, age_hours: u64) -> (tempfile::TempDir, Db, std::path::PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let cfg = t.path().canonicalize().unwrap();
    let db_path = cfg.join("library.db");
    std::fs::copy(musaic_testdata::public("db/v16-empty.db"), &db_path).unwrap();
    let (db, _) = Db::open(&db_path).unwrap();
    let dir = cfg.join("backups");
    let s = backup::create_snapshot(&db, &dir, 10, "20251201-000000Z", SystemTime::now()).unwrap();
    std::fs::File::options()
        .write(true)
        .open(dir.join(&s.filename))
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_secs(NOW - age_hours * 3600))
        .unwrap();
    settings::set_raw(db.connection(), "snapshot_min_interval_hours", raw).unwrap();
    (t, db, dir)
}

fn run(db: &Db, dir: &Path) {
    let clock = || UNIX_EPOCH + Duration::from_secs(NOW);
    auto_snapshot_on_startup(db, dir, &clock, &Quiet);
}

#[test]
fn infinite_interval_reads_as_the_default() {
    for raw in ["inf", "Infinity", "1e999"] {
        let (_t, db, dir) = setup(raw, 48);
        run(&db, &dir);
        assert_eq!(snapshots(&dir), 2, "{raw}: 48h old, 24h default: taken");
    }
}

#[test]
fn nan_interval_reads_as_the_default() {
    let (_t, db, dir) = setup("nan", 1);
    run(&db, &dir);
    assert_eq!(snapshots(&dir), 1, "1h old, 24h default: not taken");
}

/// The startup reading and the settings body agree on what the Python app
/// parses as a float (`1_0` is 10 there).
#[test]
fn startup_reads_the_interval_the_settings_body_shows() {
    let (_t, db, dir) = setup("1_0", 11);
    assert_eq!(
        musaic_secrets::snapshot_min_interval_hours(db.connection()).unwrap(),
        10.0
    );
    run(&db, &dir);
    assert_eq!(snapshots(&dir), 2, "11h old, 10h interval: taken");
}
