//! Case-only corrections of file and folder names, end to end: planned by
//! `derive_plan`, carried out by `execute_renames`, and checked against the
//! directory listing (the spelling on disk), not against `exists()`.
//!
//! These run on every volume. The temp dir's behaviour is probed at test
//! time (not assumed from the target OS): a case-insensitive volume (macOS
//! and Windows defaults) has one file under both spellings, a
//! case-sensitive one (Linux) has two names. The volume that does not keep a
//! new spelling at all is simulated in the unit tests of `rename.rs`.

use std::path::Path;

use musaic_db::{Db, albums, renames};
use musaic_fsops::plan::derive_plan;
use musaic_fsops::rename::{PlanEntry, RenameEnv, SkipReason, execute_renames};
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

/// Whether the volume holding `dir` treats `X` and `x` as one name.
fn case_insensitive(dir: &Path) -> bool {
    let p = dir.join("CaseProbe");
    std::fs::write(&p, b"").unwrap();
    let r = dir.join("caseprobe").exists();
    std::fs::remove_file(p).unwrap();
    r
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

/// One album `dir` under `<base>/lib` with `(file name, title)` tracks,
/// scanned into a copy of the empty v16 database.
fn library(base: &Path, dir: &str, files: &[(&str, &str)]) -> (Db, String, String) {
    let lib = base.join("lib");
    let album = lib.join(dir);
    std::fs::create_dir_all(&album).unwrap();
    for (i, (name, title)) in files.iter().enumerate() {
        let n = (i + 1).to_string();
        std::fs::write(
            album.join(name),
            flac_bytes(&[
                ("TRACKNUMBER", &n),
                ("TITLE", title),
                ("ALBUMARTIST", "ABBA"),
                ("ARTIST", "ABBA"),
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

fn env(lib: &str) -> RenameEnv<'_> {
    RenameEnv {
        library_dir: Some(lib),
        now: NOW,
        track_order: TrackOrder::Filename,
    }
}

/// The filename-mismatch plan proposes case-only renames of a file and of
/// the album folder (the template's `{albumartist}` is `ABBA`, the folder
/// says `abba`); executing it renames both, the album gets the id of the
/// new spelling (as any folder rename), and the plan is then empty.
#[test]
fn case_only_plan_is_proposed_executed_and_then_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let base = musaic_testdata::canonical(tmp.path());
    let (mut db, lib, old_id) = library(
        &base,
        "abba - Gold [FLAC]",
        &[("01 - Dancing queen.flac", "Dancing Queen")],
    );
    let plan = derive_plan(db.connection(), &old_id).unwrap().unwrap();
    assert_eq!(plan.renames.len(), 1, "{plan:?}");
    assert_eq!(plan.renames[0].filename, "01 - Dancing queen.flac");
    assert_eq!(plan.renames[0].expected, "01 - Dancing Queen.flac");
    assert_eq!(plan.renames[0].collides_with, None);
    let dr = plan.dir_rename.clone().expect("a case-only folder rename");
    assert_eq!(
        (dr.old.as_str(), dr.new.as_str()),
        ("abba - Gold [FLAC]", "ABBA - Gold [FLAC]")
    );

    let out = execute_renames(
        db.connection_mut(),
        &env(&lib),
        &old_id,
        &[entry("01 - Dancing queen.flac", "01 - Dancing Queen.flac")],
        Some(&entry(&dr.old, &dr.new)),
    );
    assert!(out.ok, "{}", out.message);
    assert_eq!(out.renamed, 1);
    let d = out.detail.as_ref().unwrap();
    assert!(d.dir_renamed);
    assert!(d.errors.is_empty(), "{:?}", d.errors);

    let lib_dir = base.join("lib");
    let new_dir = lib_dir.join("ABBA - Gold [FLAC]");
    assert_eq!(
        listing(&lib_dir),
        ["ABBA - Gold [FLAC]"],
        "no temp name left"
    );
    assert_eq!(listing(&new_dir), ["01 - Dancing Queen.flac"]);

    let new_id = musaic_core::album_id(&py_resolve(&new_dir).to_string_lossy());
    assert_ne!(new_id, old_id, "the id hashes the new spelling");
    assert_eq!(d.new_album_id, new_id);
    let ev = out.album_renamed.expect("AlbumRenamed");
    assert_eq!((ev.old_album_id, ev.new_album_id), (old_id, new_id.clone()));
    let loc = renames::album_location(db.connection(), &new_id)
        .unwrap()
        .expect("the row moved to the new id");
    assert_eq!(loc.path, new_dir.to_string_lossy());

    let again = derive_plan(db.connection(), &new_id).unwrap().unwrap();
    assert!(again.is_empty(), "{again:?}");
}

/// Sending the same case-only plan again is "already renamed" on both
/// kinds of volume (on an insensitive one the old spelling still opens the
/// file; the listing says it is no longer there).
#[test]
fn resubmitted_case_only_rename_is_already_renamed() {
    let tmp = tempfile::tempdir().unwrap();
    let base = musaic_testdata::canonical(tmp.path());
    let (mut db, lib, id) = library(&base, "Album", &[("01 - one.flac", "One")]);
    let plan = [entry("01 - one.flac", "01 - One.flac")];
    let first = execute_renames(db.connection_mut(), &env(&lib), &id, &plan, None);
    assert!(first.ok, "{}", first.message);
    assert_eq!(first.renamed, 1);
    let second = execute_renames(db.connection_mut(), &env(&lib), &id, &plan, None);
    assert!(second.ok, "{}", second.message);
    assert_eq!(second.renamed, 0);
    assert!(second.already_renamed, "{}", second.message);
    assert_eq!(listing(&base.join("lib/Album")), ["01 - One.flac"]);
}

/// A case-only rename keeps the file's bytes and its database rows
/// (track tags follow the new spelling).
#[test]
fn case_only_file_rename_keeps_contents_and_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let base = musaic_testdata::canonical(tmp.path());
    let (mut db, lib, id) = library(&base, "Album", &[("01 - one.flac", "One")]);
    musaic_db::track_tags::add_track_tag(db.connection_mut(), &id, "01 - one.flac", "Keep", NOW)
        .unwrap();
    let album = base.join("lib/Album");
    let before = std::fs::read(album.join("01 - one.flac")).unwrap();
    let out = execute_renames(
        db.connection_mut(),
        &env(&lib),
        &id,
        &[entry("01 - one.flac", "01 - One.flac")],
        None,
    );
    assert!(out.ok, "{}", out.message);
    assert_eq!(out.message, "1 file");
    let names: Vec<String> = listing(&album)
        .into_iter()
        .filter(|n| n.ends_with(".flac"))
        .collect();
    assert_eq!(names, ["01 - One.flac"]);
    assert_eq!(std::fs::read(album.join("01 - One.flac")).unwrap(), before);
    let ev = out.album_renamed.expect("AlbumRenamed(id, id)");
    assert_eq!(
        (ev.old_album_id.as_str(), ev.new_album_id.as_str()),
        (id.as_str(), id.as_str())
    );
    let tagged: Vec<String> = db
        .connection()
        .prepare("SELECT filename FROM track_tags WHERE album_id = ?1")
        .unwrap()
        .query_map([&id], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(tagged, ["01 - One.flac"]);
}

/// On a case-sensitive volume `One.flac` and `one.flac` can be two
/// different files: the rename is refused and neither is touched. (On an
/// insensitive volume the two cannot coexist; nothing to test.)
#[test]
fn a_different_file_under_the_other_case_is_never_overwritten() {
    let tmp = tempfile::tempdir().unwrap();
    let base = musaic_testdata::canonical(tmp.path());
    if case_insensitive(&base) {
        println!(
            "skipped: {}: the temp dir is case-insensitive",
            musaic_testdata::test_name()
        );
        return;
    }
    let (mut db, lib, id) = library(&base, "Album", &[("01 - one.flac", "One")]);
    let album = base.join("lib/Album");
    std::fs::write(album.join("01 - One.flac"), b"someone else's file").unwrap();
    let out = execute_renames(
        db.connection_mut(),
        &env(&lib),
        &id,
        &[entry("01 - one.flac", "01 - One.flac")],
        None,
    );
    assert!(!out.ok);
    assert!(
        out.message
            .starts_with("Refusing to overwrite an existing unrelated file"),
        "{}",
        out.message
    );
    assert_eq!(
        std::fs::read(album.join("01 - One.flac")).unwrap(),
        b"someone else's file"
    );
    assert!(album.join("01 - one.flac").is_file());

    // The same for a folder: `ALBUM` is another directory.
    std::fs::create_dir(base.join("lib/ALBUM")).unwrap();
    let out = execute_renames(
        db.connection_mut(),
        &env(&lib),
        &id,
        &[],
        Some(&entry("Album", "ALBUM")),
    );
    assert!(!out.ok);
    assert!(
        out.message
            .starts_with("Refusing to overwrite existing directory"),
        "{}",
        out.message
    );
    assert_eq!(listing(&base.join("lib")), ["ALBUM", "Album"]);
}

/// Two tracks whose rendered names differ only in case collide in the plan,
/// as `execute_renames` refuses the pair on every volume.
#[test]
fn targets_that_differ_only_in_case_collide_in_the_plan() {
    let tmp = tempfile::tempdir().unwrap();
    let base = musaic_testdata::canonical(tmp.path());
    let lib = base.join("lib");
    let album = lib.join("ABBA - Gold [FLAC]");
    std::fs::create_dir_all(&album).unwrap();
    for (name, title) in [("a.flac", "Same"), ("b.flac", "SAME")] {
        std::fs::write(
            album.join(name),
            flac_bytes(&[
                ("TRACKNUMBER", "1"),
                ("TITLE", title),
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
    let plan = derive_plan(db.connection(), &scanned[0].id)
        .unwrap()
        .unwrap();
    let got: Vec<(&str, Option<&str>)> = plan
        .renames
        .iter()
        .map(|r| (r.expected.as_str(), r.collides_with.as_deref()))
        .collect();
    assert_eq!(
        got,
        [("01 - Same.flac", None), ("01 - SAME.flac", Some("a.flac"))]
    );
}

/// A skipped source is still reported as missing, not as already renamed,
/// when neither spelling is there.
#[test]
fn a_missing_source_is_not_mistaken_for_a_case_only_rename() {
    let tmp = tempfile::tempdir().unwrap();
    let base = musaic_testdata::canonical(tmp.path());
    let (mut db, lib, id) = library(&base, "Album", &[("01 - one.flac", "One")]);
    let out = execute_renames(
        db.connection_mut(),
        &env(&lib),
        &id,
        &[entry("02 - two.flac", "02 - Two.flac")],
        None,
    );
    assert!(out.ok, "{}", out.message);
    assert_eq!(out.skipped.len(), 1);
    assert_eq!(out.skipped[0].reason, SkipReason::SourceMissing);
}

/// A case-only rename of a folder and a file whose names are near the
/// 255-byte limit: the descriptive temporary name would be too long, so
/// the short one is used, and both renames complete.
#[test]
fn case_only_rename_of_names_near_the_length_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let base = musaic_testdata::canonical(tmp.path());
    let dir = format!("abba - {} [FLAC]", "g".repeat(240));
    let file = format!("01 - {}.flac", "q".repeat(240));
    assert!(dir.len() > 245 && dir.len() <= 255 && file.len() <= 255);
    let lib_dir = base.join("lib");
    std::fs::create_dir_all(&lib_dir).unwrap();
    if std::fs::create_dir(lib_dir.join(&dir)).is_err() {
        println!(
            "skipped: {}: the temp dir does not take 250-byte names",
            musaic_testdata::test_name()
        );
        return;
    }
    std::fs::remove_dir(lib_dir.join(&dir)).unwrap();
    let (mut db, lib, id) = library(&base, &dir, &[(&file, "Q")]);
    let new_dir = dir.replacen("abba", "ABBA", 1);
    let new_file = file.replacen("01 - q", "01 - Q", 1);
    let out = execute_renames(
        db.connection_mut(),
        &env(&lib),
        &id,
        &[entry(&file, &new_file)],
        Some(&entry(&dir, &new_dir)),
    );
    assert!(out.ok, "{}", out.message);
    assert_eq!(out.renamed, 1);
    assert_eq!(listing(&lib_dir), std::slice::from_ref(&new_dir));
    assert_eq!(listing(&lib_dir.join(&new_dir)), [new_file]);
}

/// Two hard links `X.flac` and `one.flac` are the same file; on a
/// case-insensitive volume `x.flac` names it too. `x.flac` -> `one.flac`
/// is not a case-only rename, so it is not "already renamed" while
/// `X.flac` is still there (it is attempted, and refused without loss).
#[test]
fn a_hard_link_is_not_mistaken_for_an_already_done_case_only_rename() {
    let tmp = tempfile::tempdir().unwrap();
    let base = musaic_testdata::canonical(tmp.path());
    if !case_insensitive(&base) {
        println!(
            "skipped: {}: the temp dir is case-sensitive",
            musaic_testdata::test_name()
        );
        return;
    }
    let (mut db, lib, id) = library(&base, "Album", &[("one.flac", "One")]);
    let album = base.join("lib/Album");
    if std::fs::hard_link(album.join("one.flac"), album.join("X.flac")).is_err() {
        println!(
            "skipped: {}: the temp dir has no hard links",
            musaic_testdata::test_name()
        );
        return;
    }
    let out = execute_renames(
        db.connection_mut(),
        &env(&lib),
        &id,
        &[entry("x.flac", "one.flac")],
        None,
    );
    assert!(!out.already_renamed, "{}", out.message);
    assert!(
        out.skipped
            .iter()
            .all(|e| e.reason != SkipReason::AlreadyRenamed),
        "{:?}",
        out.skipped
    );
    assert_eq!(listing(&album), ["X.flac", "one.flac"]);
}
