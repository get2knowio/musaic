//! Changing the library folder: a bad choice is refused and the folder that
//! was in effect stays.

use musaic_core::config::ConfigDir;
use musaic_services::apply::HttpError;
use musaic_services::library_ops::{self, LibraryChoice};

/// The error a handler would answer with: the status of an `HttpError`, 500
/// for anything else.
#[derive(Debug)]
struct Answer(u16);

impl From<HttpError> for Answer {
    fn from(e: HttpError) -> Self {
        Answer(e.status)
    }
}
impl From<musaic_db::Error> for Answer {
    fn from(_: musaic_db::Error) -> Self {
        Answer(500)
    }
}

struct World {
    tmp: tempfile::TempDir,
    cfg: ConfigDir,
    db_path: std::path::PathBuf,
    stored: String,
}

fn set(w: &World, dir: &str) -> Result<LibraryChoice, Answer> {
    let db_path = w.db_path.clone();
    library_ops::set_library_dir::<Answer>(
        &w.cfg,
        None,
        dir,
        move || Ok(musaic_db::open_connection(&db_path)?),
        |_| Answer(500),
    )
}

fn setup() -> World {
    let tmp = tempfile::tempdir().unwrap();
    let good = tmp.path().join("music");
    std::fs::create_dir(&good).unwrap();
    let db_path = tmp.path().join("library.db");
    drop(musaic_db::Db::open_or_create(&db_path).unwrap());
    let mut w = World {
        cfg: ConfigDir::new(tmp.path().join("config")),
        tmp,
        db_path,
        stored: String::new(),
    };
    w.stored = set(&w, good.to_str().unwrap())
        .unwrap()
        .library_dir
        .unwrap();
    w
}

fn still_stored(w: &World) {
    assert_eq!(w.cfg.load().library_dir.as_deref(), Some(w.stored.as_str()));
}

#[test]
fn a_missing_path_is_refused_and_the_old_folder_stays() {
    let w = setup();
    let e = set(&w, w.tmp.path().join("nope").to_str().unwrap()).unwrap_err();
    assert_eq!(e.0, 400);
    still_stored(&w);
}

#[test]
fn a_file_is_refused_and_the_old_folder_stays() {
    let w = setup();
    let file = w.tmp.path().join("a.flac");
    std::fs::write(&file, b"x").unwrap();
    assert_eq!(set(&w, file.to_str().unwrap()).unwrap_err().0, 400);
    still_stored(&w);
}

#[cfg(unix)]
#[test]
fn an_unreadable_folder_is_refused_and_the_old_folder_stays() {
    use std::os::unix::fs::PermissionsExt;
    let w = setup();
    let locked = w.tmp.path().join("locked");
    std::fs::create_dir(&locked).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    // A process that can list it anyway (root) cannot show the refusal.
    let can_list = std::fs::read_dir(&locked).is_ok();
    let result = set(&w, locked.to_str().unwrap());
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    if can_list {
        return;
    }
    assert_eq!(result.unwrap_err().0, 400);
    still_stored(&w);
}

#[test]
fn an_already_configured_folder_that_vanished_is_left_alone() {
    let w = setup();
    std::fs::remove_dir(w.tmp.path().join("music")).unwrap();
    // A refused change touches nothing, and reading the setting back does
    // not clear it; the scan decides what a missing folder means.
    assert!(set(&w, w.tmp.path().join("other").to_str().unwrap()).is_err());
    still_stored(&w);
    assert!(library_ops::configured_library(w.cfg.load().library_dir).is_err());
    still_stored(&w);
}

/// An album row under `lib`, the way a scan leaves it.
fn seed_album(w: &World, lib: &str, id: &str) {
    let conn = musaic_db::open_connection(&w.db_path).unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO libraries(path, scanned_at) VALUES (?1, 'x')",
        [lib],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO albums(id, library_path, path) VALUES (?1, ?2, ?3)",
        [id, lib, &format!("{lib}/{id}")],
    )
    .unwrap();
}

fn albums_under(w: &World, lib: &str) -> i64 {
    let conn = musaic_db::open_connection(&w.db_path).unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM albums WHERE library_path = ?1",
        [lib],
        |r| r.get(0),
    )
    .unwrap()
}

fn upper(w: &World) -> String {
    w.tmp.path().join("MUSIC").to_str().unwrap().to_owned()
}

/// A volume that treats `music` and `MUSIC` as one folder.
#[cfg(any(target_os = "macos", windows))]
fn insensitive(w: &World) -> bool {
    std::path::Path::new(&upper(w)).is_dir()
}

#[cfg(target_os = "macos")]
#[test]
fn a_case_variant_of_the_library_keeps_the_stored_spelling() {
    let w = setup();
    seed_album(&w, &w.stored, "a1");
    if !insensitive(&w) {
        return; // a case-sensitive volume: a different folder
    }
    let got = set(&w, &upper(&w)).unwrap();
    assert_eq!(got.library_dir.as_deref(), Some(w.stored.as_str()));
    assert!(got.kept_stored_spelling);
    still_stored(&w);
    assert_eq!(albums_under(&w, &w.stored), 1);
}

#[cfg(target_os = "macos")]
#[test]
fn a_variant_of_an_earlier_library_returns_to_its_spelling() {
    let w = setup();
    seed_album(&w, &w.stored, "a1");
    let other = w.tmp.path().join("other");
    std::fs::create_dir(&other).unwrap();
    set(&w, other.to_str().unwrap()).unwrap();
    if !insensitive(&w) {
        return;
    }
    let got = set(&w, &upper(&w)).unwrap();
    assert_eq!(got.library_dir.as_deref(), Some(w.stored.as_str()));
    assert!(got.kept_stored_spelling);
    assert_eq!(albums_under(&w, &w.stored), 1);
}

/// The resolved path is the OS's own spelling there, so a typed case never
/// reaches the stored string.
#[cfg(windows)]
#[test]
fn a_case_variant_of_the_library_resolves_to_the_stored_spelling() {
    let w = setup();
    seed_album(&w, &w.stored, "a1");
    if !insensitive(&w) {
        return;
    }
    let got = set(&w, &upper(&w)).unwrap();
    assert_eq!(got.library_dir.as_deref(), Some(w.stored.as_str()));
    still_stored(&w);
    assert_eq!(albums_under(&w, &w.stored), 1);
}

#[cfg(target_os = "linux")]
#[test]
fn folders_differing_only_in_case_are_separate_libraries() {
    let w = setup();
    seed_album(&w, &w.stored, "a1");
    std::fs::create_dir(w.tmp.path().join("MUSIC")).unwrap();
    let got = set(&w, &upper(&w)).unwrap();
    assert!(!got.kept_stored_spelling);
    assert_eq!(got.library_dir.as_deref(), Some(upper(&w).as_str()));
    assert_eq!(albums_under(&w, &w.stored), 1);
    assert_eq!(albums_under(&w, &upper(&w)), 0);
}

/// `resolve_path` follows links, so a link to the library stores the real
/// folder, which is the string already stored: nothing is respelled.
#[cfg(unix)]
#[test]
fn a_symlink_to_the_library_resolves_to_the_stored_string() {
    let w = setup();
    seed_album(&w, &w.stored, "a1");
    let link = w.tmp.path().join("link");
    std::os::unix::fs::symlink(w.tmp.path().join("music"), &link).unwrap();
    let got = set(&w, link.to_str().unwrap()).unwrap();
    assert_eq!(got.library_dir.as_deref(), Some(w.stored.as_str()));
    assert!(!got.kept_stored_spelling);
    assert_eq!(albums_under(&w, &w.stored), 1);
}

#[test]
fn a_known_library_that_no_longer_opens_is_not_matched() {
    let w = setup();
    let gone = w.tmp.path().join("gone");
    seed_album(&w, gone.to_str().unwrap(), "g1");
    let other = w.tmp.path().join("other");
    std::fs::create_dir(&other).unwrap();
    let got = set(&w, other.to_str().unwrap()).unwrap();
    assert!(!got.kept_stored_spelling);
    assert_ne!(got.library_dir.as_deref(), Some(w.stored.as_str()));
}
