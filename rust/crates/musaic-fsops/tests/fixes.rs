//! Deliberate changes on paths the Python app golden cannot
//! reach without fault injection. Each test names the Python app behaviour it
//! departs from; scenario-level deltas live in golden_scenarios.rs.

// Unix only: these tests drive failures through Unix permission bits
// (std::os::unix::fs::PermissionsExt), which Windows does not have.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use musaic_db::{Db, albums};
use musaic_fsops::rename::{PlanEntry, RenameEnv, execute_renames};
use musaic_fsops::trash::send_to_trash;
use musaic_scan::{TrackOrder, py_resolve, scan_library};

const NOW: &str = "2026-01-01T00:00:00Z";

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

fn library(base: &Path, dir: &str, files: &[(&str, &str)]) -> (Db, String, String) {
    let lib = base.join("lib");
    let album = lib.join(dir);
    std::fs::create_dir_all(&album).unwrap();
    for (name, title) in files {
        std::fs::write(
            album.join(name),
            flac_bytes(&[("TRACKNUMBER", "1"), ("TITLE", title), ("ALBUM", "X")]),
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

/// An I/O failure AFTER the directory rename (staging cannot create
/// its temporary name: the renamed dir is read-only) must still report the
/// directory rename and carry AlbumRenamed(old, new). The Python app's staging
/// failure returned only {ok: false, message, renamed: 0}
/// (filename_mismatch.py:709-722), hiding the committed dir rename.
#[test]
fn failure_after_dir_rename_reports_the_rename() {
    let tmp = tempfile::tempdir().unwrap();
    let base = musaic_testdata::canonical(tmp.path());
    // The scenario needs a platform that renames a read-only directory (the
    // rename only writes to the parent). Some systems refuse it; the dir
    // rename then fails first and there is nothing to test here.
    let probe = base.join("probe");
    std::fs::create_dir(&probe).unwrap();
    std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o555)).unwrap();
    if std::fs::rename(&probe, base.join("probe2")).is_err() {
        std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o755)).unwrap();
        println!("skipped: this system does not rename a read-only directory");
        return;
    }
    let (mut db, lib_s, old_id) = library(&base, "Old", &[("1 a.flac", "A")]);
    let album = base.join("lib/Old");
    std::fs::set_permissions(&album, std::fs::Permissions::from_mode(0o555)).unwrap();
    let env = RenameEnv {
        library_dir: Some(&lib_s),
        now: NOW,
        track_order: TrackOrder::Filename,
    };
    let out = execute_renames(
        db.connection_mut(),
        &env,
        &old_id,
        &[entry("1 a.flac", "01 - A.flac")],
        Some(&entry("Old", "New")),
    );
    let new_dir = base.join("lib/New");
    std::fs::set_permissions(&new_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    if out.ok {
        panic!("staging into a read-only dir succeeded (running as root?)");
    }
    assert!(
        out.message.starts_with("Rename staging failed"),
        "{}",
        out.message
    );
    let d = out.detail.expect("detail must report the dir rename");
    assert!(d.dir_renamed);
    let new_id = musaic_core::album_id(&py_resolve(&new_dir).to_string_lossy());
    assert_eq!(d.new_album_id, new_id);
    let ev = out.album_renamed.expect("AlbumRenamed must be returned");
    assert_eq!((ev.old_album_id, ev.new_album_id), (old_id, new_id.clone()));
    // The file is untouched in the renamed dir; the DB row follows the dir.
    assert!(new_dir.join("1 a.flac").is_file());
    let path: String = db
        .connection()
        .query_row("SELECT path FROM albums WHERE id = ?1", [&new_id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(path, new_dir.to_string_lossy());
}

/// A leftover temporary name from an interrupted run is never
/// clobbered by staging (the Python app's `old_p.rename(tmp)` replaced it), and
/// the case-only rename still completes.
#[test]
fn staging_never_clobbers_a_leftover_temp_file() {
    let tmp = tempfile::tempdir().unwrap();
    let base = musaic_testdata::canonical(tmp.path());
    let (mut db, lib_s, id) = library(&base, "Album", &[("01 - one.flac", "One")]);
    let album = base.join("lib/Album");
    std::fs::write(album.join(".01 - one.flac.renaming.tmp"), b"leftover").unwrap();
    let env = RenameEnv {
        library_dir: Some(&lib_s),
        now: NOW,
        track_order: TrackOrder::Filename,
    };
    let before = std::fs::read(album.join("01 - one.flac")).unwrap();
    let out = execute_renames(
        db.connection_mut(),
        &env,
        &id,
        &[entry("01 - one.flac", "01 - One.flac")],
        None,
    );
    assert!(out.ok, "{}", out.message);
    assert_eq!(
        std::fs::read(album.join(".01 - one.flac.renaming.tmp")).unwrap(),
        b"leftover"
    );
    let names: Vec<String> = std::fs::read_dir(&album)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".flac"))
        .collect();
    assert_eq!(names, ["01 - One.flac"]);
    assert_eq!(std::fs::read(album.join("01 - One.flac")).unwrap(), before);
}

/// Names already in the trash (including ones a concurrent trash
/// just created) are skipped, never replaced: dup.flac, dup__S.flac and
/// dup__S-2.flac exist, so the next copy lands at dup__S-3.flac.
#[test]
fn trash_skips_every_existing_name() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path();
    let trash = base.join("trash");
    let sub = trash.join("Album");
    std::fs::create_dir_all(&sub).unwrap();
    let stamp = "2026-05-05T16-30-46";
    for (n, text) in [
        ("dup.flac", "1"),
        (&*format!("dup__{stamp}.flac"), "2"),
        (&*format!("dup__{stamp}-2.flac"), "3"),
    ] {
        std::fs::write(sub.join(n), text).unwrap();
    }
    let src_dir = base.join("Album");
    std::fs::create_dir(&src_dir).unwrap();
    std::fs::write(src_dir.join("dup.flac"), "4").unwrap();
    let got = send_to_trash(&trash, &src_dir.join("dup.flac"), None, || {
        stamp.to_string()
    })
    .unwrap();
    assert_eq!(got, sub.join(format!("dup__{stamp}-3.flac")));
    for (n, text) in [
        ("dup.flac", "1"),
        (&*format!("dup__{stamp}.flac"), "2"),
        (&*format!("dup__{stamp}-2.flac"), "3"),
        (&*format!("dup__{stamp}-3.flac"), "4"),
    ] {
        assert_eq!(std::fs::read_to_string(sub.join(n)).unwrap(), text, "{n}");
    }
    assert!(!src_dir.join("dup.flac").exists());
}

/// (remove-after-claim order): when no trash name can be
/// claimed (the trash sub-directory is read-only) the move fails and the
/// source is still there, unchanged: it is removed only after its new name
/// exists.
#[test]
fn trash_keeps_the_source_when_no_name_can_be_claimed() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path();
    let trash = base.join("trash");
    let sub = trash.join("Album");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o555)).unwrap();
    let src_dir = base.join("Album");
    std::fs::create_dir(&src_dir).unwrap();
    std::fs::write(src_dir.join("keep.flac"), "payload").unwrap();
    let got = send_to_trash(&trash, &src_dir.join("keep.flac"), None, || {
        "2026-05-05T16-30-47".to_string()
    });
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).unwrap();
    if got.is_ok() {
        panic!("claimed a name in a read-only dir (running as root?)");
    }
    assert_eq!(
        std::fs::read_to_string(src_dir.join("keep.flac")).unwrap(),
        "payload"
    );
    assert_eq!(std::fs::read_dir(&sub).unwrap().count(), 0);
}

fn env(lib: &str) -> RenameEnv<'_> {
    RenameEnv {
        library_dir: Some(lib),
        now: NOW,
        track_order: TrackOrder::Filename,
    }
}

fn flac_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with(".media-manager"))
        .collect();
    names.sort();
    names
}

/// A Missing album is refused before anything is touched, even when its
/// folder is reachable again (rule 6: scans observe, users decide; a rescan
/// clears Missing first). The Python app went ahead.
#[test]
fn missing_album_is_not_renamed() {
    let tmp = tempfile::tempdir().unwrap();
    let base = musaic_testdata::canonical(tmp.path());
    let (mut db, lib_s, id) = library(&base, "Old", &[("1 a.flac", "A")]);
    albums::mark_album_missing(db.connection(), &id, "2026-02-01T00:00:00Z").unwrap();
    let out = execute_renames(
        db.connection_mut(),
        &env(&lib_s),
        &id,
        &[entry("1 a.flac", "01 - A.flac")],
        Some(&entry("Old", "New")),
    );
    assert!(!out.ok);
    assert_eq!(
        out.message,
        "Album is missing from disk (missing since 2026-02-01T00:00:00Z); nothing was renamed"
    );
    assert!(out.album_renamed.is_none());
    assert_eq!(flac_names(&base.join("lib/Old")), ["1 a.flac"]);
    assert!(!base.join("lib/New").exists());
}

/// The post-rename upsert keeps the album in its own library. The Python
/// app upserted under the configured library dir, which moved the row
/// (and created a `libraries` row) when the setting had changed since the
/// album was scanned.
#[test]
fn rescan_keeps_the_rows_own_library() {
    let tmp = tempfile::tempdir().unwrap();
    let base = musaic_testdata::canonical(tmp.path());
    let (mut db, lib_s, id) = library(&base, "Al", &[("1 a.flac", "A")]);
    let other = base.join("elsewhere");
    std::fs::create_dir(&other).unwrap();
    let other_s = py_resolve(&other).to_string_lossy().into_owned();
    let out = execute_renames(
        db.connection_mut(),
        &env(&other_s),
        &id,
        &[entry("1 a.flac", "01 - A.flac")],
        None,
    );
    assert!(out.ok, "{}", out.message);
    let conn = db.connection();
    let (lib_of_row, sample): (String, String) = conn
        .query_row(
            "SELECT library_path, sample_track FROM albums WHERE id = ?1",
            [&id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(lib_of_row, lib_s);
    assert!(sample.ends_with("01 - A.flac"), "{sample}");
    let libraries: Vec<String> = conn
        .prepare("SELECT path FROM libraries ORDER BY path")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(libraries, [lib_s]);
}

/// Entries left alone are returned and summed up in the message, not only
/// logged: one source gone, one already renamed by an earlier call.
#[test]
fn skipped_entries_reach_the_outcome() {
    use musaic_fsops::rename::SkipReason;
    let tmp = tempfile::tempdir().unwrap();
    let base = musaic_testdata::canonical(tmp.path());
    let (mut db, lib_s, id) = library(&base, "Al", &[("1 a.flac", "A"), ("02 - B.flac", "B")]);
    let out = execute_renames(
        db.connection_mut(),
        &env(&lib_s),
        &id,
        &[
            entry("1 a.flac", "01 - A.flac"),
            entry("2 b.flac", "02 - B.flac"),
            entry("3 c.flac", "03 - C.flac"),
        ],
        None,
    );
    assert!(out.ok, "{}", out.message);
    assert_eq!(out.renamed, 1);
    assert_eq!(
        out.message,
        "1 file; 1 not found, skipped: '3 c.flac'; 1 already renamed"
    );
    let got: Vec<(&str, SkipReason)> = out
        .skipped
        .iter()
        .map(|e| (e.old.as_str(), e.reason))
        .collect();
    assert_eq!(
        got,
        [
            ("2 b.flac", SkipReason::AlreadyRenamed),
            ("3 c.flac", SkipReason::SourceMissing)
        ]
    );
    // Only skips: still "Nothing to rename", with the reason.
    let out = execute_renames(
        db.connection_mut(),
        &env(&lib_s),
        &id,
        &[entry("3 c.flac", "03 - C.flac")],
        None,
    );
    assert_eq!(
        out.message,
        "Nothing to rename; 1 not found, skipped: '3 c.flac'"
    );
}

/// Staging of the second file fails after the first was staged (every
/// temporary name it could take is in use: its own name is near the
/// 255-byte limit, so it takes the short `.musaic-<n>.tmp` names, all
/// occupied here). The first file is moved back; nothing is renamed and
/// the message names the file.
#[test]
fn staging_failure_mid_batch_restores_every_file() {
    let tmp = tempfile::tempdir().unwrap();
    let base = musaic_testdata::canonical(tmp.path());
    let long = format!("2 {}.flac", "b".repeat(243));
    assert_eq!(long.len(), 250);
    let (mut db, lib_s, id) = library(&base, "Al", &[("1 a.flac", "A"), (&long, "B")]);
    let album = base.join("lib/Al");
    for n in 1..=1000 {
        std::fs::write(album.join(format!(".musaic-{n}.tmp")), b"").unwrap();
    }
    let before = flac_names(&album);
    let out = execute_renames(
        db.connection_mut(),
        &env(&lib_s),
        &id,
        &[
            entry("1 a.flac", "01 - A.flac"),
            entry(&long, "02 - B.flac"),
        ],
        None,
    );
    assert!(!out.ok);
    assert_eq!(out.renamed, 0);
    assert!(
        out.message
            .starts_with(&format!("Rename staging failed for '{long}': ")),
        "{}",
        out.message
    );
    assert!(
        out.message.ends_with("; no file was renamed"),
        "{}",
        out.message
    );
    assert_eq!(flac_names(&album), before);
}

/// The second pass cannot give a file its new name (too long for the
/// filesystem; the preflight cannot see that): that file goes back to its
/// old name, the others are renamed, and the message names the one that
/// was not.
#[test]
fn second_pass_failure_names_the_file_and_restores_it() {
    let tmp = tempfile::tempdir().unwrap();
    let base = musaic_testdata::canonical(tmp.path());
    let (mut db, lib_s, id) = library(&base, "Al", &[("1 a.flac", "A"), ("2 b.flac", "B")]);
    let album = base.join("lib/Al");
    let too_long = format!("02 - {}.flac", "B".repeat(300));
    let out = execute_renames(
        db.connection_mut(),
        &env(&lib_s),
        &id,
        &[
            entry("1 a.flac", "01 - A.flac"),
            entry("2 b.flac", &too_long),
        ],
        None,
    );
    assert!(!out.ok);
    assert_eq!(out.renamed, 1);
    assert!(
        out.message
            .starts_with("1 file; 1 error(s): not renamed '2 b.flac'"),
        "{}",
        out.message
    );
    let d = out.detail.unwrap();
    assert_eq!(d.errors.len(), 1, "{:?}", d.errors);
    assert!(
        d.errors[0].starts_with("2 b.flac \u{2192} 02 - "),
        "{:?}",
        d.errors
    );
    assert_eq!(flac_names(&album), ["01 - A.flac", "2 b.flac"]);
    // Files renamed: the event still goes out so the UI refreshes.
    assert!(out.album_renamed.is_some());
}
