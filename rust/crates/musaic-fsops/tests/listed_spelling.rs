//! After a folder or file rename the album's id, its stored path and its
//! file rows are the ones a full scan derives from the directory listing,
//! so the next scan finds the same album: nothing is marked Missing, no
//! row is added, and fingerprints, track tags and findings stay with it.
//!
//! The volume that stores names decomposed is simulated in the unit tests
//! of `rename.rs`; the `#[ignore]`d test at the end runs the same rename
//! on a real HFS+ disk image on macOS (an explicit CI step runs it).

use std::path::{Path, PathBuf};

use musaic_core::album_id;
use musaic_db::{Db, albums, renames};
use musaic_fsops::rename::{PlanEntry, RenameEnv, execute_renames};
use musaic_scan::{TrackOrder, py_resolve, scan_library};
use rusqlite::Connection;

const NOW: &str = "2026-01-01T00:00:00Z";

/// Same recipe as the golden generator's `flac_bytes` (metadata only).
fn flac_bytes(title: &str) -> Vec<u8> {
    let mut si = Vec::new();
    si.extend_from_slice(&[0x10, 0, 0x10, 0, 0, 0, 0, 0, 0, 0]);
    let packed: u64 = (44100u64 << 44) | (1 << 41) | (15 << 36) | 44100;
    si.extend_from_slice(&packed.to_be_bytes());
    si.extend_from_slice(&[0; 16]);
    let mut vc = Vec::new();
    vc.extend_from_slice(&4u32.to_le_bytes());
    vc.extend_from_slice(b"test");
    let tags = [("TRACKNUMBER", "1"), ("TITLE", title), ("ALBUM", "X")];
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

/// The names `dir` lists, sorted.
fn listing(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// A library at `lib` with the album folders `dirs`, each holding
/// `01 - one.flac`, scanned into a copy of the empty v16 database at `db`.
fn library(lib: &Path, db: &Path, dirs: &[&str]) -> (Db, String) {
    for dir in dirs {
        let album = lib.join(dir);
        std::fs::create_dir_all(&album).unwrap();
        std::fs::write(album.join("01 - one.flac"), flac_bytes("One")).unwrap();
    }
    std::fs::copy(musaic_testdata::public("db/v16-empty.db"), db).unwrap();
    let (mut db, _) = Db::open(db).unwrap();
    let lib_s = py_resolve(lib).to_string_lossy().into_owned();
    let scanned = scan_library(lib, TrackOrder::Filename);
    albums::save_snapshot(db.connection_mut(), &lib_s, NOW, NOW, &scanned).unwrap();
    (db, lib_s)
}

fn id_of(dir: &Path) -> String {
    album_id(&py_resolve(dir).to_string_lossy())
}

fn entry(old: &str, new: &str) -> PlanEntry {
    PlanEntry {
        old: old.into(),
        new: new.into(),
    }
}

/// One fingerprint, one track tag and one finding for `file` of `id`.
fn add_rows(conn: &Connection, id: &str, file: &str) {
    conn.execute(
        "INSERT INTO track_fingerprints (album_id, filename, fingerprint, duration_s, \
         last_computed_at) VALUES (?1, ?2, 'AQAA', 1.0, ?3)",
        [id, file, NOW],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO track_tags (album_id, filename, tag, created_at) \
         VALUES (?1, ?2, 'keep', ?3)",
        [id, file, NOW],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO check_findings (check_id, album_id, summary, first_seen_at, \
         last_seen_at) VALUES ('missing_cover', ?1, 'no cover', ?2, ?2)",
        [id, NOW],
    )
    .unwrap();
}

/// `(table, album_id, filename)` of every per-album row, sorted.
fn rows(conn: &Connection) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for (table, file) in [
        ("track_fingerprints", "filename"),
        ("track_tags", "filename"),
        ("check_findings", "''"),
    ] {
        let mut stmt = conn
            .prepare(&format!("SELECT album_id, {file} FROM {table}"))
            .unwrap();
        let found = stmt
            .query_map([], |r| Ok((table.to_string(), r.get(0)?, r.get(1)?)))
            .unwrap();
        out.extend(found.map(Result::unwrap));
    }
    out.sort();
    out
}

/// The rows [`add_rows`] made, under album `id` and file `file`.
fn rows_of(id: &str, file: &str) -> Vec<(String, String, String)> {
    let mut v = vec![
        ("check_findings".to_string(), id.to_string(), String::new()),
        (
            "track_fingerprints".to_string(),
            id.to_string(),
            file.to_string(),
        ),
        ("track_tags".to_string(), id.to_string(), file.to_string()),
    ];
    v.sort();
    v
}

/// Rename the folder `old_dir` under `lib` to `new_dir` and its file to
/// `new_file`, then run a full scan. Asserts that the stored id and path
/// are the ones the scan derives, that the scan marks nothing Missing and
/// adds no row, and that the rows follow the listed file name. Returns the
/// album's folder as the scan found it.
fn rename_then_scan(
    lib: &Path,
    db: &mut Db,
    lib_s: &str,
    old_dir: &str,
    new_dir: &str,
    new_file: &str,
) -> PathBuf {
    let old_id = id_of(&lib.join(old_dir));
    add_rows(db.connection(), &old_id, "01 - one.flac");
    let before: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM albums", [], |r| r.get(0))
        .unwrap();
    let out = execute_renames(
        db.connection_mut(),
        &RenameEnv {
            library_dir: Some(lib_s),
            now: NOW,
            track_order: TrackOrder::Filename,
        },
        &old_id,
        &[entry("01 - one.flac", new_file)],
        Some(&entry(old_dir, new_dir)),
    );
    assert!(out.ok, "{}", out.message);
    let ev = out.album_renamed.expect("AlbumRenamed");
    let detail = out.detail.expect("detail");

    let scanned = scan_library(lib, TrackOrder::Filename);
    let found = scanned
        .iter()
        .find(|a| a.id == ev.new_album_id)
        .unwrap_or_else(|| panic!("no scanned album has id {}: {scanned:?}", ev.new_album_id));
    assert_eq!(detail.new_album_id, found.id);
    let stored = renames::album_location(db.connection(), &found.id)
        .unwrap()
        .expect("a row under the scanned id");
    assert_eq!(stored.path, found.path);
    let album = PathBuf::from(&found.path);
    let files: Vec<String> = listing(&album)
        .into_iter()
        .filter(|n| n.ends_with(".flac"))
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(rows(db.connection()), rows_of(&found.id, &files[0]));

    let snap = albums::save_snapshot(db.connection_mut(), lib_s, NOW, NOW, &scanned).unwrap();
    assert!(snap.newly_missing.is_empty(), "{snap:?}");
    assert!(snap.inserted.is_empty(), "{snap:?}");
    let after: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM albums", [], |r| r.get(0))
        .unwrap();
    assert_eq!(after, before);
    assert_eq!(rows(db.connection()), rows_of(&found.id, &files[0]));
    album
}

/// On the volume the tests run on, a rename to a name with an accented
/// letter gives the album the id a full scan derives, and keeps its rows.
#[test]
fn a_renamed_album_is_the_one_the_next_scan_finds() {
    let tmp = tempfile::tempdir().unwrap();
    let base = musaic_testdata::canonical(tmp.path());
    let lib = base.join("lib");
    let (mut db, lib_s) = library(&lib, &base.join("library.db"), &["abba - x"]);
    let album = rename_then_scan(
        &lib,
        &mut db,
        &lib_s,
        "abba - x",
        "Caf\u{e9} - x",
        "01 - Caf\u{e9}.flac",
    );
    // A volume that keeps names as given (APFS, ext4, NTFS) records the
    // spelling asked for: the same id as before this read-back existed.
    if listing(&lib) == ["Caf\u{e9} - x"] {
        assert_eq!(album, lib.join("Caf\u{e9} - x"));
        assert_eq!(id_of(&album), id_of(&lib.join("Caf\u{e9} - x")));
    }
}

/// On Linux an NFC and an NFD name are two names: a rename to the NFC
/// spelling beside an NFD sibling keeps both folders as they are, and
/// neither album takes the other's spelling or id.
#[cfg(target_os = "linux")]
#[test]
fn nfc_and_nfd_siblings_stay_distinct() {
    let tmp = tempfile::tempdir().unwrap();
    let base = musaic_testdata::canonical(tmp.path());
    let lib = base.join("lib");
    let nfc = "Caf\u{e9} - x";
    let nfd = "Cafe\u{301} - x";
    let (mut db, lib_s) = library(&lib, &base.join("library.db"), &["abba - x", nfd]);
    let sibling = id_of(&lib.join(nfd));
    let album = rename_then_scan(&lib, &mut db, &lib_s, "abba - x", nfc, "01 - two.flac");
    assert_eq!(album, lib.join(nfc));
    let mut want = vec![nfc.to_string(), nfd.to_string()];
    want.sort();
    assert_eq!(listing(&lib), want);
    assert_ne!(id_of(&album), sibling);
    let row = renames::album_location(db.connection(), &sibling)
        .unwrap()
        .expect("the sibling keeps its row");
    assert_eq!(row.path, lib.join(nfd).to_string_lossy());
    assert_eq!(row.missing_since, None);
}

/// On Windows a case-only rename records the spelling the OS lists.
#[cfg(windows)]
#[test]
fn a_case_only_rename_gets_the_spelling_windows_lists() {
    let tmp = tempfile::tempdir().unwrap();
    let base = musaic_testdata::canonical(tmp.path());
    let lib = base.join("lib");
    let (mut db, lib_s) = library(&lib, &base.join("library.db"), &["abba - x"]);
    let album = rename_then_scan(
        &lib,
        &mut db,
        &lib_s,
        "abba - x",
        "ABBA - x",
        "01 - One.flac",
    );
    assert_eq!(listing(&lib), ["ABBA - x"]);
    assert_eq!(album, lib.join("ABBA - x"));
    assert!(listing(&album).contains(&"01 - One.flac".to_string()));
}

/// An HFS+ disk image mounted under a temp dir, detached when dropped.
#[cfg(target_os = "macos")]
struct HfsVolume {
    mount: PathBuf,
    _dir: tempfile::TempDir,
}

#[cfg(target_os = "macos")]
impl HfsVolume {
    fn new() -> HfsVolume {
        use std::process::Command;
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("volume.dmg");
        let mount = dir.path().join("mnt");
        std::fs::create_dir(&mount).unwrap();
        let run = |cmd: &mut Command| {
            let out = cmd.output().expect("hdiutil");
            assert!(
                out.status.success(),
                "{cmd:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(Command::new("hdiutil")
            .args(["create", "-quiet", "-fs", "HFS+", "-size", "20m"])
            .args(["-volname", "musaic-test"])
            .arg(&image));
        run(Command::new("hdiutil")
            .args(["attach", "-quiet", "-nobrowse", "-mountpoint"])
            .arg(&mount)
            .arg(&image));
        HfsVolume { mount, _dir: dir }
    }
}

#[cfg(target_os = "macos")]
impl Drop for HfsVolume {
    fn drop(&mut self) {
        use std::process::Command;
        let detach = |force: bool| {
            let mut cmd = Command::new("hdiutil");
            cmd.args(["detach", "-quiet"]).arg(&self.mount);
            if force {
                cmd.arg("-force");
            }
            cmd.status().is_ok_and(|s| s.success())
        };
        if !detach(false) && !detach(true) {
            eprintln!("could not detach {}", self.mount.display());
        }
    }
}

/// A real HFS+ volume stores names decomposed: the folder and the file
/// renamed to NFC names are listed in NFD, and the stored id, path and
/// rows follow the listing. Needs `hdiutil` (no root); run by an explicit
/// CI step on macOS.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "mounts an HFS+ disk image; run with --ignored on macOS"]
fn an_hfs_plus_volume_gets_the_listed_spelling() {
    let volume = HfsVolume::new();
    let tmp = tempfile::tempdir().unwrap();
    let lib = musaic_testdata::canonical(&volume.mount).join("lib");
    let (mut db, lib_s) = library(&lib, &tmp.path().join("library.db"), &["abba - x"]);
    let album = rename_then_scan(
        &lib,
        &mut db,
        &lib_s,
        "abba - x",
        "Caf\u{e9} - x",
        "01 - Caf\u{e9}.flac",
    );
    assert_eq!(listing(&lib), ["Cafe\u{301} - x"]);
    assert_eq!(album, lib.join("Cafe\u{301} - x"));
    assert!(listing(&album).contains(&"01 - Cafe\u{301}.flac".to_string()));
    drop(db);
}
