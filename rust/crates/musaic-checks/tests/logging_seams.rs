//! `filename-mismatch: error scanning album %s` (the lower crates log through
//! `musaic-log`): the Python app's musaic/checks/filename_mismatch.py:139
//! `log.exception(...)` when planning an album raises during a check run.
//! The Python app's ERROR plus traceback is ERROR `msg: <error text>` here (the
//! convention of musaic-fsops tests/logging_seams.rs).

mod common;

use std::sync::Arc;

use common::{Sandbox, env, flac_bytes};
use musaic_checks::{NoResolver, Registry, ScopeKind, run_check};
use musaic_log::{Level, capture};
use musaic_scan::{TrackOrder, scan_library};

const NOW: &str = "2026-01-01T00:00:00Z";

// Driven by a template whose `:c` spec overflows (Python's
// OverflowError "%c arg not in range(0x110000)", which this build's
// renderer reproduces, as in musaic-fsops' derive_plan_error_is_logged).
// The album is skipped and the run completes.
#[test]
fn error_scanning_album_is_logged() {
    let (sb, mut db) = Sandbox::new();
    let album = sb.lib.join("Al");
    std::fs::create_dir_all(&album).unwrap();
    std::fs::write(
        album.join("1 a.flac"),
        flac_bytes(
            &[
                ("TRACKNUMBER".into(), "9999999".into()),
                ("TITLE".into(), "A".into()),
            ],
            1,
        ),
    )
    .unwrap();
    let scanned = scan_library(&sb.lib, TrackOrder::Filename);
    musaic_db::albums::save_snapshot(db.connection_mut(), &sb.lib_s, NOW, NOW, &scanned).unwrap();
    let conn = db.connection();
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('filename_template_single_artist', '{tracknumber:c}')",
        [],
    )
    .unwrap();
    let env = env(Registry::default(), Arc::new(NoResolver));
    let (run, lines) = capture(|| {
        run_check(
            conn,
            &env,
            "filename-mismatch",
            &sb.lib_s,
            ScopeKind::All,
            None,
        )
        .unwrap()
    });
    assert_eq!(run.status, "completed", "{:?}", run.error);
    assert_eq!(run.issue_count, 0);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0].level, Level::Error);
    assert_eq!(lines[0].target, "musaic.checks.filename_mismatch");
    assert_eq!(
        lines[0].message,
        format!(
            "filename-mismatch: error scanning album {}: %c arg not in range(0x110000)",
            album.display()
        )
    );
}
