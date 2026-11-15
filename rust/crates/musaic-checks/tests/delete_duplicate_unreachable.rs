//! "Delete this duplicate file" with the album
//! directory unreachable is refused with 409 and nothing is deleted.
//! The Python app (musaic/api/health.py:531-575) treated an unreachable directory
//! as "file already gone", dropped the fingerprint row, and its rescan then
//! deleted the album row. The scenario-level delta from the Python app's record
//! (directory renamed away) is in golden_scenarios.rs
//! (`duplicates_delete_file_unreachable`); these cover what the Python app
//! golden cannot reach: an unreadable directory, a path that is a file, and
//! the precondition on its own.

// Unix only: these tests drive failures through Unix permission bits
// (std::os::unix::fs::PermissionsExt), which Windows does not have.
#![cfg(unix)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use common::{Sandbox, setup};
use musaic_checks::checks::duplicate_tracks::{
    HttpRefusal, require_reachable_album_dir, trash_duplicate_file,
};
use musaic_db::findings::{get_finding, upsert_finding};
use rusqlite::Connection;
use serde_json::Value as J;

/// The Python app-recorded setup of the unreachable scenario (album a1 "Dupes"
/// with 01.flac / 02.flac and their fingerprint rows), plus its finding.
fn rig() -> (Sandbox, musaic_db::Db, i64) {
    let g = common::golden("scenarios.json");
    let sc: J = g["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "duplicates-delete-file-unreachable")
        .unwrap()
        .clone();
    let (sb, db) = Sandbox::new();
    setup(&sb, db.connection(), &sc);
    let id = upsert_finding(
        db.connection(),
        "duplicate-tracks",
        Some("a1"),
        "warn",
        "Duplicate audio: \"x\" (2 copies)",
        Some(r#"{"filenames": ["01.flac", "02.flac"]}"#),
        "2026-01-01T00:00:00Z",
    )
    .unwrap();
    (sb, db, id)
}

fn fingerprint_files(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT filename FROM track_fingerprints ORDER BY filename")
        .unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn delete(sb: &Sandbox, conn: &Connection, id: i64, name: &str) -> Result<String, HttpRefusal> {
    let f = get_finding(conn, id).unwrap();
    trash_duplicate_file(
        conn,
        &sb.lib_s,
        &sb.root.join("trash"),
        f.as_ref(),
        name,
        || "stamp".into(),
    )
    .map(|t| t.message)
}

fn running_as_root(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok()
}

/// The precondition the future endpoint calls first. 409 for a
/// directory that does not exist, a path that is a file, and a directory
/// that cannot be listed; the canonical directory otherwise.
#[test]
fn reachable_directory_precondition() {
    let (sb, _db, _) = rig();
    let dupes = sb.lib.join("Dupes");
    assert_eq!(
        require_reachable_album_dir(&dupes).unwrap(),
        dupes.canonicalize().unwrap()
    );
    for p in [sb.lib.join("Gone"), dupes.join("01.flac")] {
        let e = require_reachable_album_dir(&p).unwrap_err();
        assert_eq!(e.status, 409, "{p:?}");
        assert!(e.detail.contains("nothing was deleted"), "{}", e.detail);
    }
    std::fs::set_permissions(&dupes, std::fs::Permissions::from_mode(0o000)).unwrap();
    let r = require_reachable_album_dir(&dupes);
    let root = running_as_root(&dupes);
    std::fs::set_permissions(&dupes, std::fs::Permissions::from_mode(0o755)).unwrap();
    if root {
        eprintln!("running as root: an unreadable directory cannot be simulated");
        return;
    }
    assert_eq!(r.unwrap_err().status, 409);
}

/// An album directory that exists but cannot be read (a permission
/// flicker, a share that lost its credentials). The file lookup fails with
/// "permission denied", not "not found". Before the decision this crate
/// took that for "already gone" and dropped the fingerprint row; now: 409,
/// file in place, both fingerprint rows kept, nothing in the trash.
#[test]
fn unreadable_album_directory_is_refused_and_nothing_is_deleted() {
    let (sb, db, id) = rig();
    let conn = db.connection();
    let dupes = sb.lib.join("Dupes");
    std::fs::set_permissions(&dupes, std::fs::Permissions::from_mode(0o000)).unwrap();
    let r = delete(&sb, conn, id, "02.flac");
    let root = running_as_root(&dupes);
    std::fs::set_permissions(&dupes, std::fs::Permissions::from_mode(0o755)).unwrap();
    if root {
        eprintln!("running as root: an unreadable directory cannot be simulated");
        return;
    }
    assert_eq!(r.unwrap_err().status, 409);
    assert!(dupes.join("02.flac").is_file());
    assert_eq!(fingerprint_files(conn), ["01.flac", "02.flac"]);
    assert!(!sb.root.join("trash").exists());
}

/// The album path now names a file (the directory was replaced):
/// 409, fingerprint rows kept.
#[test]
fn album_path_that_is_not_a_directory_is_refused() {
    let (sb, db, id) = rig();
    let conn = db.connection();
    let dupes = sb.lib.join("Dupes");
    std::fs::rename(&dupes, sb.lib.join("Dupes.moved")).unwrap();
    std::fs::write(&dupes, b"not a directory").unwrap();
    assert_eq!(delete(&sb, conn, id, "02.flac").unwrap_err().status, 409);
    assert_eq!(fingerprint_files(conn), ["01.flac", "02.flac"]);
}

/// The Python app's idempotent cleanup still works (health.py:531-560): the
/// directory is there, the file is not, so its fingerprint row is dropped
/// and the click reports "already gone".
#[test]
fn file_already_gone_with_directory_present_is_cleaned_up() {
    let (sb, db, id) = rig();
    let conn = db.connection();
    std::fs::remove_file(sb.lib.join("Dupes/02.flac")).unwrap();
    let msg = delete(&sb, conn, id, "02.flac").unwrap();
    assert!(msg.starts_with("02.flac was already gone"), "{msg}");
    assert_eq!(fingerprint_files(conn), ["01.flac"]);
}

/// With the directory reachable, only "not found" is the already-gone path;
/// any other failure to look the file up is a 500 and deletes nothing (the
/// the Python app caught only `FileNotFoundError`; `01.flac/inner` raises
/// `NotADirectoryError` there too).
#[test]
fn other_lookup_failures_are_errors_not_already_gone() {
    let (sb, db, _) = rig();
    let conn = db.connection();
    let id = upsert_finding(
        conn,
        "duplicate-tracks",
        Some("a1"),
        "warn",
        "Duplicate audio: \"y\" (2 copies)",
        Some(r#"{"filenames": ["01.flac/inner", "02.flac"]}"#),
        "2026-01-01T00:00:00Z",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO track_fingerprints (album_id, filename, fingerprint, duration_s, last_computed_at) \
         VALUES ('a1', '01.flac/inner', 'AQAA', 1.0, 't')",
        [],
    )
    .unwrap();
    assert_eq!(
        delete(&sb, conn, id, "01.flac/inner").unwrap_err().status,
        500
    );
    assert_eq!(
        fingerprint_files(conn),
        ["01.flac", "01.flac/inner", "02.flac"]
    );
}
