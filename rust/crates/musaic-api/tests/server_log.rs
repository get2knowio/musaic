//! The binary's `server.log` sink
//! (`musaic_api::logging::ServerLog`) against the Python app's handler, recorded
//! into testdata/golden/logging/rotation.json
//! (the Python app's own `RotatingFileHandler` + formatter from musaic/main.py,
//! Python 3.13, only `maxBytes` / `backupCount` shrunk and the clock pinned).
//!
//! Pinned: line format, rotation rule (bytes on disk + characters of the new
//! line >= max, never an empty file; `.1` newest, oldest dropped), every
//! generation's bytes, and the level names (`getattr(logging, NAME.upper(),
//! INFO)`). Enumerated deltas: no MEDIA_MANAGER_LOG_LEVEL (not exercised
//! here, main.rs reads only MUSAIC_LOG_LEVEL), timestamps in UTC (the
//! golden pins `converter = time.gmtime`).
//! Beyond the Python app: user-only file mode, logging that survives an
//! unwritable config dir or a failing rotation.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use musaic_api::logging::{
    Level, Masks, ServerLog, ServerLogOptions, level_from_env_value, parse_level,
};
use serde_json::Value;

fn golden() -> Value {
    common::golden("../logging/rotation.json")
}

fn level(name: &str) -> Level {
    match name {
        "DEBUG" => Level::Debug,
        "INFO" => Level::Info,
        "WARNING" => Level::Warning,
        "ERROR" => Level::Error,
        _ => Level::Critical,
    }
}

fn opts(g: &Value) -> ServerLogOptions {
    let created = g["test"]["created_utc"].as_f64().unwrap();
    ServerLogOptions {
        max_bytes: g["test"]["max_bytes"].as_u64().unwrap(),
        backups: g["test"]["backups"].as_u64().unwrap() as usize,
        console: false,
        now: Arc::new(move || UNIX_EPOCH + Duration::from_secs_f64(created)),
    }
}

fn files(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut v: Vec<(String, Vec<u8>)> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                std::fs::read(e.path()).unwrap(),
            )
        })
        .collect();
    v.sort();
    v
}

#[test]
fn rotation_and_format_match_the_python_apps_handler() {
    let g = golden();
    assert_eq!(
        g["format"],
        "%(asctime)s [%(levelname)s] %(name)s: %(message)s"
    );
    assert_eq!(g["datefmt"], "%Y-%m-%d %H:%M:%S");
    // The production sizes the Python app uses are this build's defaults.
    assert_eq!(
        g["production"]["max_bytes"].as_u64(),
        Some(musaic_api::logging::MAX_BYTES)
    );
    assert_eq!(
        g["production"]["backups"].as_u64(),
        Some(musaic_api::logging::BACKUPS as u64)
    );
    assert_eq!(g["production"]["file"], musaic_api::logging::FILE_NAME);

    for (backups, key) in [
        (g["test"]["backups"].as_u64().unwrap(), "files_b64"),
        (g["wide_backups"].as_u64().unwrap(), "files_wide_b64"),
    ] {
        let td = tempfile::tempdir().unwrap();
        let mut o = opts(&g);
        o.backups = backups as usize;
        let (log, opened) = ServerLog::start(td.path(), 0, Masks::new(), o);
        assert_eq!(opened.unwrap(), td.path().join("server.log"));
        for r in g["records"].as_array().unwrap() {
            log.log(
                level(r["level"].as_str().unwrap()),
                r["logger"].as_str().unwrap(),
                r["message"].as_str().unwrap(),
            );
        }
        assert!(log.flush(Duration::from_secs(5)));
        let want: Vec<(String, Vec<u8>)> = g[key]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    base64::engine::general_purpose::STANDARD
                        .decode(v.as_str().unwrap())
                        .unwrap(),
                )
            })
            .collect();
        let got = files(td.path());
        for ((gn, gb), (wn, wb)) in got.iter().zip(&want) {
            assert_eq!(gn, wn, "{key}");
            assert_eq!(
                String::from_utf8_lossy(gb),
                String::from_utf8_lossy(wb),
                "{key}: {gn}"
            );
        }
        assert_eq!(got.len(), want.len(), "{key}: generations");
    }
}

#[test]
fn level_names_resolve_as_python_does() {
    let g = golden();
    for (name, want) in g["level_of_name"].as_object().unwrap() {
        let want = want.as_u64().unwrap() as u8;
        // The variable: Python's value, unknown names meaning INFO.
        assert_eq!(level_from_env_value(name), want, "{name:?}");
        // The flag refuses what Python would not resolve.
        if let Some(t) = parse_level(name) {
            assert_eq!(t, want, "{name:?}");
        }
    }
    assert_eq!(parse_level("verbose"), None);
}

#[test]
fn threshold_filters_records() {
    let td = tempfile::tempdir().unwrap();
    let mut o = opts(&golden());
    o.max_bytes = 0;
    let (log, _) = ServerLog::start(td.path(), 30, Masks::new(), o);
    log.log(Level::Info, "musaic.x", "hidden");
    log.log(Level::Warning, "musaic.x", "shown");
    assert!(log.flush(Duration::from_secs(5)));
    let text = std::fs::read_to_string(td.path().join("server.log")).unwrap();
    assert_eq!(text, "2026-11-11 19:17:09 [WARNING] musaic.x: shown\n");
}

#[cfg(unix)]
#[test]
fn the_file_and_its_generations_are_user_only() {
    use std::os::unix::fs::PermissionsExt;
    let td = tempfile::tempdir().unwrap();
    let path = td.path().join("server.log");
    // An older file with wider permissions is narrowed.
    std::fs::write(&path, b"old line\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let mut o = opts(&golden());
    o.max_bytes = 120;
    let (log, _) = ServerLog::start(td.path(), 0, Masks::new(), o);
    for i in 0..10 {
        log.log(
            Level::Info,
            "musaic.x",
            &format!("line {i} of a rotating file"),
        );
    }
    assert!(log.flush(Duration::from_secs(5)));
    let names: Vec<String> = files(td.path()).into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["server.log", "server.log.1", "server.log.2"]);
    for n in names {
        let mode = std::fs::metadata(td.path().join(&n))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "{n}");
    }
}

#[cfg(unix)]
#[test]
fn an_unwritable_config_dir_leaves_the_console_and_never_fails() {
    use std::os::unix::fs::PermissionsExt;
    let td = tempfile::tempdir().unwrap();
    let ro = td.path().join("ro");
    std::fs::create_dir(&ro).unwrap();
    std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o500)).unwrap();
    let (log, opened) = ServerLog::start(&ro, 0, Masks::new(), opts(&golden()));
    assert!(opened.is_err(), "no file in a read-only dir");
    assert!(log.path().is_none());
    for i in 0..10_000 {
        log.log(Level::Info, "musaic.x", &format!("line {i}"));
    }
    assert!(log.flush(Duration::from_secs(10)));
    std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(!ro.join("server.log").exists());
}

#[cfg(unix)]
#[test]
fn a_failing_rotation_keeps_logging_to_the_file() {
    use std::os::unix::fs::PermissionsExt;
    let td = tempfile::tempdir().unwrap();
    let dir = td.path().join("cfg");
    std::fs::create_dir(&dir).unwrap();
    let mut o = opts(&golden());
    o.max_bytes = 100;
    let (log, _) = ServerLog::start(&dir, 0, Masks::new(), o);
    log.log(
        Level::Info,
        "musaic.x",
        "first line, before the dir turns read-only",
    );
    assert!(log.flush(Duration::from_secs(5)));
    // The rename of the rollover fails (no write permission on the dir);
    // the open file stays writable.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    for i in 0..5 {
        log.log(
            Level::Info,
            "musaic.x",
            &format!("line {i} after the dir turned read-only"),
        );
    }
    assert!(log.flush(Duration::from_secs(5)));
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let text = std::fs::read_to_string(dir.join("server.log")).unwrap();
    assert!(text.contains("line 4 after"), "{text}");
    assert!(!dir.join("server.log.1").exists());
}

#[test]
fn a_full_queue_drops_records_without_blocking() {
    // The writer thread is fast; flood far past the queue and check the
    // callers never block (the whole flood returns promptly) and that what
    // was dropped is accounted for in the file.
    let td = tempfile::tempdir().unwrap();
    let mut o = opts(&golden());
    o.max_bytes = 0;
    let (log, _) = ServerLog::start(td.path(), 0, Masks::new(), o);
    let t0 = std::time::Instant::now();
    let n = musaic_api::logging::QUEUE * 20;
    for i in 0..n {
        log.log(Level::Info, "musaic.x", &format!("flood {i}"));
    }
    let took = t0.elapsed();
    assert!(log.flush(Duration::from_secs(30)));
    log.log(Level::Info, "musaic.x", "after the flood");
    assert!(log.flush(Duration::from_secs(30)));
    let text = std::fs::read_to_string(td.path().join("server.log")).unwrap();
    let written = text
        .lines()
        .filter(|l| l.contains("musaic.x: flood "))
        .count();
    let dropped: usize = text
        .lines()
        .filter_map(|l| {
            l.split("musaic.log: ")
                .nth(1)
                .and_then(|r| r.split(' ').next())
                .and_then(|n| n.parse::<usize>().ok())
        })
        .sum();
    assert_eq!(written + dropped, n, "every record written or counted");
    assert!(took < Duration::from_secs(20), "flood took {took:?}");
    let _ = SystemTime::now();
}
