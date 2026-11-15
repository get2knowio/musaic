//! A save whose new metadata does not fit the old region rewrites the file
//! (temp sibling, fsync, rename). What the filesystem keeps about the old
//! file must survive that, or be reported (see `musaic_tags::preserve`):
//!
//! * extended attributes and odd permission bits are carried over;
//! * the creation date is carried over (macOS), the modification time is
//!   the time of the save;
//! * the group (and, as root, the owner) is carried over;
//! * a hard link is broken, never shifted in place, and that is reported;
//! * a symlink keeps pointing at the rewritten file;
//! * a temp file left by an earlier crash lends none of its attributes.
//!
//! A filesystem that refuses extended attributes makes the attribute
//! checks skip with a printed reason (the "not supported" policy itself is
//! tested against a simulated filesystem in `preserve.rs`).

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use musaic_tags::{Event, FlacFile, SaveMode, SaveReport};

const ALBUM: &str = "Aurora Fields - First Light [FLAC]";
const TRACK: &str = "01 - Dawn Chorus.flac";

/// A temp album holding a copy of one synthetic track.
fn fixture() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let src = musaic_testdata::synthetic_library().join(ALBUM).join(TRACK);
    let path = tmp.path().join(TRACK);
    fs::copy(src, &path).unwrap();
    (tmp, path)
}

/// A comment far larger than any padding: the save must rewrite the file.
fn big_comment() -> String {
    "Recorded in an invented hall. ".repeat(1000)
}

fn grow(path: &Path) -> SaveReport {
    let mut f = FlacFile::open(path).unwrap();
    f.tags_mut().set("comment", &big_comment());
    let report = f.save().unwrap();
    assert_eq!(
        report.mode,
        SaveMode::Replaced,
        "the save must rewrite the file"
    );
    report
}

fn comment(path: &Path) -> Option<String> {
    FlacFile::open(path)
        .unwrap()
        .tags()
        .and_then(|t| t.first("comment"))
        .map(str::to_string)
}

fn temp_name(path: &Path) -> PathBuf {
    path.with_file_name(format!(
        ".{}.musaic-tags.tmp",
        path.file_name().unwrap().to_string_lossy()
    ))
}

/// Attributes a user or the OS might have put on a music file.
fn sample_xattrs() -> Vec<(&'static str, Vec<u8>)> {
    let mut v = vec![
        ("user.musaic.test.colour", b"teal".to_vec()),
        ("user.musaic.test.binary", vec![0, 1, 2, 0xfe, 0xff]),
    ];
    if cfg!(target_vendor = "apple") {
        // Finder tags, Finder info (exactly 32 bytes) and a quarantine flag.
        v.push((
            "com.apple.metadata:_kMDItemUserTags",
            b"bplist00\xa1\x01VBlue\n4".to_vec(),
        ));
        let mut finder = vec![0u8; 32];
        finder[9] = 0x0c; // a colour label
        v.push(("com.apple.FinderInfo", finder));
        v.push(("com.apple.quarantine", b"0081;00000000;Test;".to_vec()));
    }
    v
}

/// Sets every sample attribute, or returns why this filesystem cannot hold
/// them.
fn set_xattrs(path: &Path) -> Result<(), String> {
    for (name, value) in sample_xattrs() {
        if let Err(e) = xattr::set(path, name, &value) {
            return Err(format!(
                "cannot set extended attribute {name} in {}: {e}",
                path.parent().unwrap().display()
            ));
        }
    }
    Ok(())
}

#[test]
fn extended_attributes_and_permissions_survive_a_rewrite() {
    let (_tmp, path) = fixture();
    let xattrs_ok = match set_xattrs(&path) {
        Ok(()) => true,
        Err(why) => {
            println!("skipped (extended attributes only): {why}");
            false
        }
    };
    // Odd bits: owner read/write, group write only, others write + execute.
    fs::set_permissions(&path, fs::Permissions::from_mode(0o623)).unwrap();
    // FAT and some shares have no permission bits to keep.
    let mode_ok = fs::metadata(&path).unwrap().permissions().mode() & 0o7777 == 0o623;
    if !mode_ok {
        println!("skipped (permissions only): this filesystem does not keep permission bits");
    }
    let ino = fs::metadata(&path).unwrap().ino();

    let report = grow(&path);

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let meta = fs::metadata(&path).unwrap();
    assert_ne!(meta.ino(), ino, "the file was rewritten");
    if mode_ok {
        assert_eq!(meta.permissions().mode() & 0o7777, 0o623, "permissions");
    }
    assert_eq!(comment(&path), Some(big_comment()));
    if xattrs_ok {
        for (name, value) in sample_xattrs() {
            assert_eq!(
                xattr::get(&path, name).unwrap(),
                Some(value),
                "extended attribute {name}"
            );
        }
    }
    assert!(!temp_name(&path).exists(), "temp file left behind");
}

/// The creation date ("Date Created" in Finder) is the original's; the
/// modification time is the save's, so the library scan sees the edit.
#[test]
fn creation_date_is_kept_and_modification_time_is_the_saves() {
    let (_tmp, path) = fixture();
    let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000); // 2001
    let times = fs::FileTimes::new().set_modified(old);
    #[cfg(target_os = "macos")]
    let times = {
        use std::os::macos::fs::FileTimesExt;
        times.set_created(old)
    };
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(times)
        .unwrap();
    let before_save = SystemTime::now() - Duration::from_secs(5);

    let report = grow(&path);

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let meta = fs::metadata(&path).unwrap();
    assert!(meta.modified().unwrap() > before_save, "modification time");
    if cfg!(target_os = "macos") {
        assert_eq!(meta.created().unwrap(), old, "creation date");
    } else {
        println!("note: creation date not settable on this platform; not checked");
    }
}

/// The group is kept when the user may set it (a group they belong to
/// other than the one a new file gets); as root, the owner too.
#[test]
fn owner_and_group_survive_a_rewrite() {
    let (tmp, path) = fixture();
    let id = |arg: &str| -> Vec<u32> {
        let out = std::process::Command::new("id").arg(arg).output().unwrap();
        String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .filter_map(|s| s.parse().ok())
            .collect()
    };
    let root = id("-u") == [0];
    // The group a file created in this directory gets.
    let probe = tmp.path().join("probe");
    fs::write(&probe, b"").unwrap();
    let default_gid = fs::metadata(&probe).unwrap().gid();
    let other = if root {
        Some(4243)
    } else {
        id("-G").into_iter().find(|g| *g != default_gid)
    };
    let Some(gid) = other else {
        println!("skipped: the user belongs to no group other than {default_gid}");
        return;
    };
    let uid = root.then_some(4242);
    if let Err(e) = std::os::unix::fs::chown(&path, uid, Some(gid)) {
        println!("skipped: cannot give the test file group {gid}: {e}");
        return;
    }
    let want = fs::metadata(&path).unwrap();

    let report = grow(&path);

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let meta = fs::metadata(&path).unwrap();
    assert_eq!(meta.gid(), want.gid(), "group");
    assert_eq!(meta.uid(), want.uid(), "owner");
}

/// Breaking the link loses nothing: the other name keeps the complete old
/// file. Shifting the audio in place to keep the link could destroy both on
/// a crash. The break is reported, never silent.
#[test]
fn a_hard_link_is_broken_and_reported() {
    let (tmp, path) = fixture();
    let other = tmp.path().join("elsewhere.flac");
    if let Err(e) = fs::hard_link(&path, &other) {
        println!("skipped: no hard links on this filesystem: {e}");
        return;
    }
    let old_bytes = fs::read(&path).unwrap();

    let report = grow(&path);

    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(
        report.warnings[0].contains("1 other hard link"),
        "{:?}",
        report.warnings
    );
    assert_eq!(comment(&path), Some(big_comment()));
    assert_eq!(
        fs::read(&other).unwrap(),
        old_bytes,
        "the other name is intact"
    );
    assert_eq!(fs::metadata(&path).unwrap().nlink(), 1);
}

/// The warning reaches the per-file apply event.
#[test]
fn apply_reports_the_warning_on_the_file_it_concerns() {
    let tmp = tempfile::tempdir().unwrap();
    let album = tmp.path().join(ALBUM);
    fs::create_dir(&album).unwrap();
    let src = musaic_testdata::synthetic_library().join(ALBUM);
    for e in fs::read_dir(&src).unwrap().flatten() {
        if e.path().extension().is_some_and(|x| x == "flac") {
            fs::copy(e.path(), album.join(e.file_name())).unwrap();
        }
    }
    if let Err(e) = fs::hard_link(album.join(TRACK), tmp.path().join("elsewhere.flac")) {
        println!("skipped: no hard links on this filesystem: {e}");
        return;
    }
    let mut done = Vec::new();
    musaic_tags::apply_to_album_iter(
        &album,
        &vec![("genre".to_string(), Some(big_comment()))],
        &Vec::new(),
        None,
        false,
        &mut |e| {
            if let Event::FileDone(c) = e {
                done.push(c);
            }
        },
    )
    .unwrap();
    assert!(done.len() > 1);
    for c in &done {
        assert!(c.saved && c.error.is_none(), "{c:?}");
        if c.filename == TRACK {
            assert_eq!(c.warnings.len(), 1, "{c:?}");
        } else {
            assert!(c.warnings.is_empty(), "{c:?}");
        }
    }
}

/// Renaming over a symlink would turn it into a regular file and leave its
/// target with the old tags.
#[test]
fn a_symlink_keeps_pointing_at_the_rewritten_file() {
    let (tmp, path) = fixture();
    let link = tmp.path().join("link.flac");
    if let Err(e) = std::os::unix::fs::symlink(&path, &link) {
        println!("skipped: no symlinks on this filesystem: {e}");
        return;
    }

    let report = grow(&link);

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_link(&link).unwrap(), path);
    assert_eq!(comment(&path), Some(big_comment()));
    assert!(!temp_name(&link).exists() && !temp_name(&path).exists());
}

/// A temp file a crash left behind is removed, not reused: reusing it would
/// carry its stale attributes into the new file.
#[test]
fn a_stale_temp_file_lends_none_of_its_attributes() {
    let (_tmp, path) = fixture();
    let stale = temp_name(&path);
    fs::write(&stale, b"left by a crash").unwrap();
    if let Err(e) = xattr::set(&stale, "user.musaic.test.stale", b"1") {
        println!("skipped: cannot set extended attributes here: {e}");
        return;
    }

    grow(&path);

    assert_eq!(xattr::get(&path, "user.musaic.test.stale").unwrap(), None);
    assert!(!stale.exists());
}
