//! What a user is shown when a rename is refused or fails: a stable
//! `reason`, a plain `user_message`, and the technical `message` unchanged.
//!
//! The failures are real ones in a temp dir: a target that exists, a missing
//! folder, a read-only folder (unix, not when running as root). The
//! volume-specific classes (disk full, a name too long for the volume, a
//! file in use) cannot be produced portably; their mapping from the error
//! is unit-tested in `musaic-core` (`rename_reason`), and the sentence each
//! one gets is asserted here through that table.

use std::path::Path;

use musaic_core::rename_reason::Reason;
use musaic_db::{Db, albums};
use musaic_fsops::rename::{PlanEntry, RenameEnv, RenameOutcome, execute_renames};
use musaic_scan::{TrackOrder, py_resolve, scan_library};

const NOW: &str = "2026-01-01T00:00:00Z";

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

/// One album folder `abba - x` under `<base>/lib` with the given files,
/// scanned into a copy of the empty v16 database.
fn library(base: &Path, files: &[&str]) -> (Db, String, String) {
    let lib = base.join("lib");
    let album = lib.join("abba - x");
    std::fs::create_dir_all(&album).unwrap();
    for (i, name) in files.iter().enumerate() {
        std::fs::write(
            album.join(name),
            flac_bytes(&[
                ("TRACKNUMBER", &(i + 1).to_string()),
                ("TITLE", name),
                ("ALBUMARTIST", "ABBA"),
                ("ALBUM", "Gold"),
            ]),
        )
        .unwrap();
    }
    let db_path = base.join("library.db");
    std::fs::copy(musaic_testdata::public("db/v16-empty.db"), &db_path).unwrap();
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
) -> RenameOutcome {
    let env = RenameEnv {
        library_dir: Some(lib),
        now: NOW,
        track_order: TrackOrder::Filename,
    };
    execute_renames(db.connection_mut(), &env, id, plan, dir)
}

/// The user-facing text carries nothing from the system or a repr.
fn assert_plain(user: &str) {
    for raw in [
        "os error",
        "Permission denied",
        "No such file",
        "Couldn't rename directory",
        "Rename staging failed",
        "Refusing",
    ] {
        assert!(!user.contains(raw), "{raw:?} in {user}");
    }
    let words = user.replace("n't", "nt").replace("'s ", "s ");
    assert!(!words.contains('\''), "{user}");
}

fn reason_of(out: &RenameOutcome) -> &'static str {
    out.reason.expect("a reason").code()
}

#[test]
fn an_existing_target_file() {
    let tmp = tempfile::tempdir().unwrap();
    let base = musaic_testdata::canonical(tmp.path());
    let (mut db, lib, id) = library(&base, &["a.flac", "b.flac"]);
    let out = run(&mut db, &lib, &id, &[entry("a.flac", "b.flac")], None);
    assert!(!out.ok);
    assert_eq!(reason_of(&out), "already_exists");
    assert_eq!(
        out.user_message.as_deref(),
        Some(
            "A file named \u{201c}b.flac\u{201d} is already in the folder, so nothing was \
             renamed. Rename or move that file, then try again."
        )
    );
    // The technical text is unchanged.
    assert_eq!(
        out.message,
        "Refusing to overwrite an existing unrelated file: b.flac"
    );
}

#[test]
fn an_existing_target_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let base = musaic_testdata::canonical(tmp.path());
    let (mut db, lib, id) = library(&base, &["a.flac"]);
    std::fs::create_dir(base.join("lib/other")).unwrap();
    let out = run(&mut db, &lib, &id, &[], Some(&entry("abba - x", "other")));
    assert_eq!(reason_of(&out), "already_exists");
    let user = out.user_message.unwrap();
    assert!(
        user.starts_with("A folder named \u{201c}other\u{201d} already exists"),
        "{user}"
    );
    assert_plain(&user);
    assert_eq!(
        out.message,
        "Refusing to overwrite existing directory: other"
    );
}

#[test]
fn a_name_with_a_separator() {
    let tmp = tempfile::tempdir().unwrap();
    let base = musaic_testdata::canonical(tmp.path());
    let (mut db, lib, id) = library(&base, &["a.flac"]);
    let out = run(&mut db, &lib, &id, &[entry("a.flac", "x/y.flac")], None);
    assert_eq!(reason_of(&out), "name_not_allowed");
    let user = out.user_message.unwrap();
    assert!(
        user.contains("\u{201c}x/y.flac\u{201d} can't be used as a file name"),
        "{user}"
    );
    assert_plain(&user);
    assert!(
        out.message
            .starts_with("Refusing rename target with path separator: 'x/y.flac'")
    );

    let out = run(&mut db, &lib, &id, &[], Some(&entry("abba - x", "a\\b")));
    assert_eq!(reason_of(&out), "name_not_allowed");
    assert_plain(&out.user_message.unwrap());
}

#[test]
fn two_files_onto_one_name() {
    let tmp = tempfile::tempdir().unwrap();
    let base = musaic_testdata::canonical(tmp.path());
    let (mut db, lib, id) = library(&base, &["a.flac", "b.flac"]);
    let out = run(
        &mut db,
        &lib,
        &id,
        &[entry("a.flac", "c.flac"), entry("b.flac", "C.flac")],
        None,
    );
    assert_eq!(reason_of(&out), "duplicate_target");
    let user = out.user_message.unwrap();
    assert!(
        user.starts_with("More than one file would be renamed to"),
        "{user}"
    );
    assert_plain(&user);
}

#[test]
fn a_missing_album_folder_and_an_unknown_album() {
    let tmp = tempfile::tempdir().unwrap();
    let base = musaic_testdata::canonical(tmp.path());
    let (mut db, lib, id) = library(&base, &["a.flac"]);

    let out = run(
        &mut db,
        &lib,
        "0123456789abcdef",
        &[entry("a.flac", "b.flac")],
        None,
    );
    assert_eq!(reason_of(&out), "album_not_found");
    assert_eq!(out.message, "Album not found");
    assert_plain(&out.user_message.unwrap());

    std::fs::remove_dir_all(base.join("lib/abba - x")).unwrap();
    let out = run(&mut db, &lib, &id, &[entry("a.flac", "b.flac")], None);
    assert_eq!(reason_of(&out), "folder_missing");
    let user = out.user_message.unwrap();
    assert!(!user.contains("abba - x"), "no path in: {user}");
    assert_plain(&user);
    assert!(out.message.starts_with("Album directory missing: "));

    let env = RenameEnv {
        library_dir: None,
        now: NOW,
        track_order: TrackOrder::Filename,
    };
    let out = execute_renames(db.connection_mut(), &env, &id, &[], None);
    assert_eq!(reason_of(&out), "no_library");
    assert_eq!(out.message, "No library configured");
}

#[test]
fn a_file_that_is_not_in_the_folder_is_left_alone_and_said_so() {
    let tmp = tempfile::tempdir().unwrap();
    let base = musaic_testdata::canonical(tmp.path());
    let (mut db, lib, id) = library(&base, &["a.flac"]);
    let out = run(
        &mut db,
        &lib,
        &id,
        &[entry("a.flac", "z.flac"), entry("gone.flac", "g.flac")],
        None,
    );
    assert!(out.ok, "{}", out.message);
    assert_eq!(reason_of(&out), "not_found");
    assert_eq!(
        out.user_message.as_deref(),
        Some(
            "Renamed 1 file. \u{201c}gone.flac\u{201d} wasn't in the folder, so it was left alone."
        )
    );
    // The technical text keeps its quoted form.
    assert_eq!(out.message, "1 file; 1 not found, skipped: 'gone.flac'");
}

/// A folder the user cannot write to: the rename fails at the operating
/// system, whose text differs per platform. Unix only (a Windows folder
/// cannot be made read-only for renames the same way), and not as root,
/// who can write anywhere.
#[cfg(unix)]
mod read_only {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn set_mode(p: &Path, mode: u32) {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// True when `dir` really refuses new entries (false for root).
    fn refuses_writes(dir: &Path) -> bool {
        std::fs::File::create(dir.join(".probe")).is_err()
    }

    #[test]
    fn files_in_a_read_only_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let base = musaic_testdata::canonical(tmp.path());
        let (mut db, lib, id) = library(&base, &["a.flac"]);
        let album = base.join("lib/abba - x");
        set_mode(&album, 0o555);
        if !refuses_writes(&album) {
            set_mode(&album, 0o755);
            eprintln!("skipped: the process can write to a read-only folder (root)");
            return;
        }
        let out = run(&mut db, &lib, &id, &[entry("a.flac", "b.flac")], None);
        set_mode(&album, 0o755);
        assert!(!out.ok);
        assert_eq!(reason_of(&out), "permission_denied");
        assert_eq!(
            out.user_message.as_deref(),
            Some(
                "\u{201c}a.flac\u{201d} couldn't be renamed because Musaic isn't allowed to \
                 change it. Check that you can change files in this folder, then try again."
            )
        );
        // The technical text still has the system's wording, for the log.
        assert!(
            out.message
                .starts_with("Rename staging failed for 'a.flac': "),
            "{}",
            out.message
        );
        assert!(album.join("a.flac").is_file());
    }

    #[test]
    fn a_folder_in_a_read_only_library() {
        let tmp = tempfile::tempdir().unwrap();
        let base = musaic_testdata::canonical(tmp.path());
        let (mut db, lib, id) = library(&base, &["a.flac"]);
        let libdir = base.join("lib");
        set_mode(&libdir, 0o555);
        if !refuses_writes(&libdir) {
            set_mode(&libdir, 0o755);
            eprintln!("skipped: the process can write to a read-only folder (root)");
            return;
        }
        let out = run(
            &mut db,
            &lib,
            &id,
            &[],
            Some(&entry("abba - x", "ABBA - Gold")),
        );
        set_mode(&libdir, 0o755);
        assert!(!out.ok);
        assert_eq!(reason_of(&out), "permission_denied");
        let user = out.user_message.unwrap();
        assert!(
            user.starts_with(
                "The album folder couldn't be renamed because Musaic isn't allowed to change it."
            ),
            "{user}"
        );
        assert_plain(&user);
        assert!(
            out.message.starts_with("Couldn't rename directory: "),
            "{}",
            out.message
        );
        assert!(libdir.join("abba - x").is_dir());
    }
}

/// Every reason has a cause and an advice that are plain text.
#[test]
fn every_reason_reads_plainly() {
    use Reason::*;
    for r in [
        PermissionDenied,
        InUse,
        DiskFull,
        NameTooLong,
        ReadOnly,
        NotFound,
        NameNotAllowed,
        AlreadyExists,
        NotKept,
        BatchStopped,
        DuplicateTarget,
        NoLibrary,
        AlbumNotFound,
        AlbumMissing,
        FolderMissing,
        LibraryUpdateFailed,
        Unverified,
        Other,
    ] {
        let s = musaic_core::rename_reason::couldnt_rename("It", r);
        assert!(s.ends_with('.'), "{s}");
        assert!(!r.code().is_empty() && r.code() == r.code().to_lowercase());
        assert_plain(&s);
    }
}
