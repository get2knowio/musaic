//! Folders and files whose names are not valid UTF-8 (possible on Linux; macOS
//! file systems refuse them). The API and the `path` column are UTF-8 text, so
//! discovery leaves them out; it counts them, logs one line each, and reports
//! the count in the `done` event and the scan status.

#![cfg(target_os = "linux")]

#[path = "../../musaic-services/tests/common/mod.rs"]
mod common;
mod scan_wire;

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{connect, flac_bytes, fresh_db, recording_bus};
use musaic_scan::TrackOrder;
use musaic_services::scan::{ScanDeps, ScanService, StartScan, discover_album_dirs_reporting};

fn bad_name(prefix: &str) -> std::ffi::OsString {
    let mut b = prefix.as_bytes().to_vec();
    b.extend_from_slice(&[0xff, 0xfe]);
    OsStr::from_bytes(&b).to_os_string()
}

fn album(dir: &Path, files: &[std::ffi::OsString]) {
    std::fs::create_dir_all(dir).unwrap();
    let tags = [("ARTIST", "A"), ("ALBUM", "B"), ("TITLE", "T")]
        .map(|(k, v)| (k.to_string(), v.to_string()));
    for f in files {
        std::fs::write(dir.join(f), flac_bytes(&tags, 1, 0)).unwrap();
    }
}

/// `lib/good` (one valid file, plus one whose name is not UTF-8),
/// `lib/<bad folder>` with a valid album folder beneath it.
fn build(lib: &Path) {
    album(
        &lib.join("good"),
        &["01.flac".into(), {
            let mut n = bad_name("02-");
            n.push(".flac");
            n
        }],
    );
    let bad_dir = lib.join(bad_name("bad-"));
    album(&bad_dir.join("beneath"), &["01.flac".into()]);
}

#[test]
fn discovery_skips_and_counts_names_that_are_not_utf8() {
    let td = tempfile::tempdir().unwrap();
    let lib = musaic_scan::py_resolve(td.path()).join("lib");
    build(&lib);
    let list = |d: &Path| -> Option<Vec<std::ffi::OsString>> {
        Some(
            std::fs::read_dir(d)
                .ok()?
                .flatten()
                .map(|e| e.file_name())
                .collect(),
        )
    };
    let (found, lines) =
        musaic_log::capture(|| discover_album_dirs_reporting(&lib, &list, &|| false).unwrap());
    // The valid folder is found; the bad folder and everything beneath it is not.
    assert_eq!(found.dirs.len(), 1);
    assert!(found.dirs[0].ends_with("/good"), "{:?}", found.dirs);
    // One bad folder and one bad `.flac` name.
    assert_eq!(found.skipped_non_utf8, 2);
    let warned: Vec<_> = lines
        .iter()
        .filter(|l| l.message.contains("not valid UTF-8"))
        .collect();
    assert_eq!(warned.len(), 2, "{lines:?}");
    assert!(warned.iter().any(|l| l.message.contains("folder")));
    assert!(warned.iter().any(|l| l.message.contains("file")));
}

#[test]
fn the_done_event_and_status_carry_the_count() {
    let td = tempfile::tempdir().unwrap();
    let root: PathBuf = musaic_scan::py_resolve(td.path());
    let lib = root.join("lib");
    build(&lib);
    let db = fresh_db(&root);
    let (bus, _events) = recording_bus();
    let deps = ScanDeps {
        open_db: Arc::new(move || Ok(connect(&db))),
        bus,
        clock: Arc::new(|| "2030-01-01T00:00:00Z".to_string()),
        track_order: TrackOrder::DiscTrack,
        after_scan: None,
        on_album: None,
        on_worker_done: None,
        log: None,
    };
    let svc = ScanService::new();
    let StartScan::Started(_, job) = svc.start_scan(lib.to_str().unwrap(), false, deps) else {
        panic!("a scan was already running")
    };
    job.run();
    assert_eq!(svc.state().status.as_str(), "done");
    let done = scan_wire::data(svc.buffer().last().unwrap()).to_value();
    assert_eq!(done["count"], 1);
    assert_eq!(done["skipped_non_utf8_count"], 2);
    assert_eq!(
        scan_wire::state(&svc.state()).to_value()["skipped_non_utf8_count"],
        2
    );
}

#[test]
fn a_clean_library_keeps_the_usual_payload() {
    let td = tempfile::tempdir().unwrap();
    let root: PathBuf = musaic_scan::py_resolve(td.path());
    let lib = root.join("lib");
    album(&lib.join("good"), &["01.flac".into()]);
    let db = fresh_db(&root);
    let (bus, _events) = recording_bus();
    let deps = ScanDeps {
        open_db: Arc::new(move || Ok(connect(&db))),
        bus,
        clock: Arc::new(|| "2030-01-01T00:00:00Z".to_string()),
        track_order: TrackOrder::DiscTrack,
        after_scan: None,
        on_album: None,
        on_worker_done: None,
        log: None,
    };
    let svc = ScanService::new();
    let StartScan::Started(_, job) = svc.start_scan(lib.to_str().unwrap(), false, deps) else {
        panic!("a scan was already running")
    };
    job.run();
    let done = scan_wire::data(svc.buffer().last().unwrap()).to_value();
    assert!(done.get("skipped_non_utf8_count").is_none());
    assert!(
        scan_wire::state(&svc.state())
            .get("skipped_non_utf8_count")
            .is_none()
    );
}
