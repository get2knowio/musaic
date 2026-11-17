//! Changing the library folder: a bad choice is refused and the folder that
//! was in effect stays.

use musaic_core::config::ConfigDir;
use musaic_services::apply::HttpError;
use musaic_services::library_ops;

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

fn set(w: &World, dir: &str) -> Result<Option<String>, Answer> {
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
    w.stored = set(&w, good.to_str().unwrap()).unwrap().unwrap();
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
