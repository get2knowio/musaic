//! Log lines the Python app writes on the failure paths of
//! `checks/filename_mismatch.py` (`musaic.checks.filename_mismatch`),
//! `track_tags_sidecar.py` and `services/apply.py` that musaic-fsops ports
//! (the lower crates log through `musaic-log`). Each
//! test injects the failure and checks level, target and the message.
//!
//! Deliberate differences, said once: where the Python app calls
//! `log.exception(msg)` (ERROR plus a Python traceback) this build logs
//! ERROR with `msg: <error text>`; where the Python app interpolates an
//! exception (`%s` of `e`) the text is Rust's (io::Error / rusqlite), so
//! only the fixed part is asserted.

// Unix only: these tests drive failures through Unix permission bits
// (std::os::unix::fs::PermissionsExt), which Windows does not have.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use musaic_db::{Db, albums};
use musaic_fsops::plan::derive_plan;
use musaic_fsops::rename::{PlanEntry, RenameEnv, execute_renames};
use musaic_fsops::sidecar::write_sidecar;
use musaic_log::{Level, Record, capture};
use musaic_scan::{TrackOrder, py_resolve, scan_library};

const NOW: &str = "2026-01-01T00:00:00Z";
const FM: &str = "musaic.checks.filename_mismatch";

/// Same recipe as the golden generator's `flac_bytes` (metadata only).
fn flac_bytes(tags: &[(&str, &str)]) -> Vec<u8> {
    let mut si = Vec::new();
    si.extend_from_slice(&[0x10, 0, 0x10, 0, 0, 0, 0, 0, 0, 0]);
    let packed: u64 = (44100u64 << 44) | (1 << 41) | (15 << 36) | 44100;
    si.extend_from_slice(&packed.to_be_bytes());
    si.extend_from_slice(&[0; 16]);
    let mut vc = Vec::new();
    vc.extend_from_slice(&4u32.to_le_bytes());
    vc.extend_from_slice(b"test");
    vc.extend_from_slice(&(tags.len() as u32).to_le_bytes());
    for (k, v) in tags {
        let e = format!("{k}={v}");
        vc.extend_from_slice(&(e.len() as u32).to_le_bytes());
        vc.extend_from_slice(e.as_bytes());
    }
    let mut out = b"fLaC".to_vec();
    out.push(0);
    out.extend_from_slice(&(si.len() as u32).to_be_bytes()[1..]);
    out.extend_from_slice(&si);
    out.push(0x84);
    out.extend_from_slice(&(vc.len() as u32).to_be_bytes()[1..]);
    out.extend_from_slice(&vc);
    out
}

/// One album `<base>/lib/<dir>` of `(filename, tracknumber)` FLACs, scanned
/// into a copy of the fixture v16 database.
fn library(base: &Path, dir: &str, files: &[(&str, &str)]) -> (Db, String, String) {
    let lib = base.join("lib");
    let album = lib.join(dir);
    std::fs::create_dir_all(&album).unwrap();
    for (name, n) in files {
        std::fs::write(
            album.join(name),
            flac_bytes(&[("TRACKNUMBER", n), ("TITLE", "A"), ("ALBUM", "X")]),
        )
        .unwrap();
    }
    let db_path = base.join("library.db");
    let root = musaic_testdata::public("db/v16-empty.db");
    std::fs::copy(root, &db_path).unwrap();
    let (mut db, _) = Db::open(&db_path).unwrap();
    let lib_s = py_resolve(&lib).to_string_lossy().into_owned();
    let scanned = scan_library(&lib, TrackOrder::Filename);
    albums::save_snapshot(db.connection_mut(), &lib_s, NOW, NOW, &scanned).unwrap();
    let id = scanned[0].id.clone();
    (db, lib_s, id)
}

fn entry(old: &str, new: &str) -> PlanEntry {
    PlanEntry {
        old: old.into(),
        new: new.into(),
    }
}

fn run(
    db: &mut Db,
    lib: &str,
    id: &str,
    plan: &[PlanEntry],
    dir: Option<&PlanEntry>,
) -> (musaic_fsops::rename::RenameOutcome, Vec<Record>) {
    let env = RenameEnv {
        library_dir: Some(lib),
        now: NOW,
        track_order: TrackOrder::Filename,
    };
    capture(|| execute_renames(db.connection_mut(), &env, id, plan, dir))
}

fn has(lines: &[Record], level: Level, target: &str, prefix: &str) -> Record {
    let hits: Vec<&Record> = lines
        .iter()
        .filter(|r| r.message.starts_with(prefix))
        .collect();
    assert_eq!(hits.len(), 1, "{prefix}: {lines:?}");
    assert_eq!((hits[0].level, hits[0].target.as_str()), (level, target));
    hits[0].clone()
}

// the Python app: musaic/checks/filename_mismatch.py:602
// `log.warning("execute_renames: source missing, skipping: %s", old_p)`.
// The path names the current directory (preflight before any dir rename).
#[test]
fn missing_source_is_logged() {
    let tmp = tempfile::tempdir().unwrap();
    let base = musaic_testdata::canonical(tmp.path());
    let (mut db, lib, id) = library(&base, "Al", &[("1 a.flac", "1")]);
    let (out, lines) = run(&mut db, &lib, &id, &[entry("gone.flac", "x.flac")], None);
    // Deliberate change: the skip is also in the outcome, not only the log.
    assert_eq!(
        out.message,
        "Nothing to rename; 1 not found, skipped: 'gone.flac'"
    );
    let r = has(
        &lines,
        Level::Warning,
        FM,
        "execute_renames: source missing, skipping: ",
    );
    assert_eq!(
        r.message,
        format!(
            "execute_renames: source missing, skipping: {}",
            base.join("lib/Al/gone.flac").display()
        )
    );
}

// the Python app: musaic/checks/filename_mismatch.py:540
// `log.exception("execute_renames: cache migration failed; rolling back dir rename")`.
// Injected with a trigger that aborts the albums UPDATE.
#[test]
fn failed_dir_migration_is_logged() {
    let tmp = tempfile::tempdir().unwrap();
    let base = musaic_testdata::canonical(tmp.path());
    let (mut db, lib, id) = library(&base, "Old", &[("1 a.flac", "1")]);
    db.connection()
        .execute_batch(
            "CREATE TRIGGER fail_upd BEFORE UPDATE ON albums BEGIN SELECT RAISE(ABORT, 'injected'); END;",
        )
        .unwrap();
    let (out, lines) = run(&mut db, &lib, &id, &[], Some(&entry("Old", "New")));
    assert_eq!(
        out.message,
        "Cache migration failed during dir rename; rolled back"
    );
    assert!(base.join("lib/Old").is_dir());
    let r = has(
        &lines,
        Level::Error,
        FM,
        "execute_renames: cache migration failed; rolling back dir rename: ",
    );
    assert!(r.message.contains("injected"), "{}", r.message);
}

// the Python app: musaic/checks/filename_mismatch.py:710
// `log.exception("execute_renames: stage phase failed; rolling back")`.
#[test]
fn failed_staging_is_logged() {
    let tmp = tempfile::tempdir().unwrap();
    let base = musaic_testdata::canonical(tmp.path());
    let (mut db, lib, id) = library(&base, "Al", &[("1 a.flac", "1")]);
    let album = base.join("lib/Al");
    std::fs::set_permissions(&album, std::fs::Permissions::from_mode(0o555)).unwrap();
    let (out, lines) = run(&mut db, &lib, &id, &[entry("1 a.flac", "01 A.flac")], None);
    std::fs::set_permissions(&album, std::fs::Permissions::from_mode(0o755)).unwrap();
    if out.ok {
        panic!("staging into a read-only dir succeeded (running as root?)");
    }
    has(
        &lines,
        Level::Error,
        FM,
        "execute_renames: stage phase failed; rolling back: ",
    );
}

// the Python app: musaic/checks/filename_mismatch.py:618
// `log.exception("execute_renames: track-tag migration failed for %s → %s (continuing)", ...)`
// and musaic/services/apply.py:169
// `_log.exception("Failed to resync track-tag sidecar after rename for album %s", album_id)`
// (the sidecar step borrowed from apply.py).. Injected by
// dropping track_tags.
#[test]
fn failed_track_row_migration_is_logged() {
    let tmp = tempfile::tempdir().unwrap();
    let base = musaic_testdata::canonical(tmp.path());
    let (mut db, lib, id) = library(&base, "Al", &[("1 a.flac", "1")]);
    db.connection()
        .execute_batch("DROP TABLE track_tags;")
        .unwrap();
    let (out, lines) = run(&mut db, &lib, &id, &[entry("1 a.flac", "01 A.flac")], None);
    assert!(out.ok, "{}", out.message);
    has(
        &lines,
        Level::Error,
        FM,
        "execute_renames: track-tag migration failed for 1 a.flac \u{2192} 01 A.flac (continuing): ",
    );
    has(
        &lines,
        Level::Error,
        "musaic.services.apply",
        &format!("Failed to resync track-tag sidecar after rename for album {id}: "),
    );
}

// the Python app: musaic/checks/filename_mismatch.py:731
// `log.exception("execute_renames: post-rename rescan failed (continuing)")`
// and :649 `... post-dir-rename rescan failed (continuing)`.
// Injected with triggers that abort any write to albums after the renames.
#[test]
fn failed_rescans_are_logged() {
    let abort_albums = "CREATE TRIGGER fail_ins BEFORE INSERT ON albums BEGIN SELECT RAISE(ABORT, 'injected'); END;
         CREATE TRIGGER fail_upd2 BEFORE UPDATE ON albums BEGIN SELECT RAISE(ABORT, 'injected'); END;";

    // File rename.
    let tmp = tempfile::tempdir().unwrap();
    let base = musaic_testdata::canonical(tmp.path());
    let (mut db, lib, id) = library(&base, "Al", &[("1 a.flac", "1")]);
    db.connection().execute_batch(abort_albums).unwrap();
    let (out, lines) = run(&mut db, &lib, &id, &[entry("1 a.flac", "01 A.flac")], None);
    assert!(out.ok, "{}", out.message);
    let r = has(
        &lines,
        Level::Error,
        FM,
        "execute_renames: post-rename rescan failed (continuing): ",
    );
    assert!(r.message.contains("injected"), "{}", r.message);

    // Directory-only rename: the migration's UPDATE must pass, so the
    // triggers go in after it, through the rescan's INSERT only.
    let tmp = tempfile::tempdir().unwrap();
    let base = musaic_testdata::canonical(tmp.path());
    let (mut db, lib, id) = library(&base, "Old", &[("1 a.flac", "1")]);
    db.connection()
        .execute_batch(
            "CREATE TRIGGER fail_rescan BEFORE UPDATE OF track_count ON albums BEGIN SELECT RAISE(ABORT, 'injected'); END;
             CREATE TRIGGER fail_rescan_ins BEFORE INSERT ON albums BEGIN SELECT RAISE(ABORT, 'injected'); END;",
        )
        .unwrap();
    let (out, lines) = run(&mut db, &lib, &id, &[], Some(&entry("Old", "New")));
    assert!(out.ok, "{}", out.message);
    has(
        &lines,
        Level::Error,
        FM,
        "execute_renames: post-dir-rename rescan failed (continuing): ",
    );
}

// the Python app: musaic/checks/filename_mismatch.py:213
// `log.warning("filename-mismatch: couldn't open %s: %s", flac, e)`.
// The reason is this build's FLAC reader error, not mutagen's.
#[test]
fn unreadable_flac_in_plan_is_logged() {
    let tmp = tempfile::tempdir().unwrap();
    let base = musaic_testdata::canonical(tmp.path());
    let (db, _lib, id) = library(&base, "Al", &[("1 a.flac", "1")]);
    let bad = base.join("lib/Al/2 b.flac");
    std::fs::write(&bad, b"not a flac").unwrap();
    let (plan, lines) = capture(|| derive_plan(db.connection(), &id).unwrap());
    assert!(plan.unwrap().error.is_none());
    has(
        &lines,
        Level::Warning,
        FM,
        &format!("filename-mismatch: couldn't open {}: ", bad.display()),
    );
}

// the Python app: musaic/checks/filename_mismatch.py:419
// `log.exception("filename-mismatch: derive_plan error for %s", album_path)`.
// The plan says "see server log"; this is that line. Driven by a
// template whose `:c` spec overflows (Python's OverflowError
// "%c arg not in range(0x110000)", which this build's renderer reproduces).
#[test]
fn derive_plan_error_is_logged() {
    let tmp = tempfile::tempdir().unwrap();
    let base = musaic_testdata::canonical(tmp.path());
    let (db, _lib, id) = library(&base, "Al", &[("1 a.flac", "9999999")]);
    db.connection()
        .execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES ('filename_template_single_artist', '{tracknumber:c}')",
            [],
        )
        .unwrap();
    let (plan, lines) = capture(|| derive_plan(db.connection(), &id).unwrap());
    assert_eq!(
        plan.unwrap().error.as_deref(),
        Some("Couldn't re-derive plan; see server log")
    );
    let r = has(
        &lines,
        Level::Error,
        FM,
        "filename-mismatch: derive_plan error for ",
    );
    assert_eq!(
        r.message,
        format!(
            "filename-mismatch: derive_plan error for {}: %c arg not in range(0x110000)",
            base.join("lib/Al").display()
        )
    );
}

// the Python app: musaic/track_tags_sidecar.py:137
// `log.warning("Could not write tag sidecar %s: %s", p, e)`.
#[test]
fn sidecar_write_failure_is_logged() {
    let tmp = tempfile::tempdir().unwrap();
    let album = tmp.path().join("al");
    std::fs::create_dir(&album).unwrap();
    std::fs::set_permissions(&album, std::fs::Permissions::from_mode(0o555)).unwrap();
    let map = BTreeMap::from([("1 a.flac".to_owned(), vec!["x".to_owned()])]);
    let (ok, lines) = capture(|| write_sidecar(&album, &map));
    std::fs::set_permissions(&album, std::fs::Permissions::from_mode(0o755)).unwrap();
    if ok {
        panic!("wrote into a read-only dir (running as root?)");
    }
    has(
        &lines,
        Level::Warning,
        "musaic.track_tags_sidecar",
        &format!(
            "Could not write tag sidecar {}: ",
            album.join(".media-manager-tags.json").display()
        ),
    );
}
