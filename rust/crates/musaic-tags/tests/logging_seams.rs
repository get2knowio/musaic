//! Log lines the Python app's `musaic.tags` writes on its failure paths
//! (the lower crates log through `musaic-log`). Each test drives
//! the failure and checks level, target and the message text, which is
//! the Python app's format string filled with this build's values. Where the Python app
//! interpolates a Python exception (`%s` of `e`), the Rust text differs
//! (io::Error / decoder wording) and only the fixed part is asserted.

// Unix only: these tests drive failures through Unix permission bits
// (std::os::unix::fs::PermissionsExt), which Windows does not have.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use musaic_log::{Level, Record, capture};
use musaic_tags::{
    Event, RenameFailure, apply_to_album_iter, normalize_to_jpeg, save_cover_to_album_dir,
};

/// Minimal FLAC (STREAMINFO + VORBIS_COMMENT + 1 KiB PADDING, no audio
/// frames): the recipe of the fsops golden generator's `flac_bytes` plus
/// padding, so a same-length tag edit is written in place.
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
    out.push(0x04);
    out.extend_from_slice(&(vc.len() as u32).to_be_bytes()[1..]);
    out.extend_from_slice(&vc);
    out.push(0x81);
    out.extend_from_slice(&1024u32.to_be_bytes()[1..]);
    out.extend_from_slice(&[0; 1024]);
    out
}

fn only(lines: &[Record], level: Level, prefix: &str) -> Record {
    let hits: Vec<&Record> = lines
        .iter()
        .filter(|r| r.message.starts_with(prefix))
        .collect();
    assert_eq!(hits.len(), 1, "{lines:?}");
    assert_eq!(hits[0].level, level, "{lines:?}");
    assert_eq!(hits[0].target, "musaic.tags");
    hits[0].clone()
}

/// The `Done` event's rename failures.
fn rename_failures(events: &[Event]) -> Vec<RenameFailure> {
    match events.last() {
        Some(Event::Done {
            rename_failures, ..
        }) => rename_failures.clone(),
        other => panic!("last event is not done: {other:?}"),
    }
}

fn renumber(album: &Path) -> Vec<Event> {
    let mut events = Vec::new();
    apply_to_album_iter(
        album,
        &Vec::new(),
        &vec![(
            "01 a.flac".to_owned(),
            vec![("tracknumber".to_owned(), Some("2".to_owned()))],
        )],
        &Default::default(),
        None,
        false,
        &mut |e| events.push(e),
    )
    .unwrap();
    events
}

// the Python app: musaic/tags.py:425
// `log.warning("cover JPEG normalization failed: %s — keeping original bytes", e)`.
// The reason is this build's ("cannot identify image file"), Pillow's
// adds the BytesIO repr.
#[test]
fn undecodable_cover_logs_normalization_failure() {
    let (out, lines) = capture(|| normalize_to_jpeg(b"not an image", 95));
    assert_eq!(out, b"not an image");
    let r = only(&lines, Level::Warning, "cover JPEG normalization failed: ");
    assert!(
        r.message.ends_with(" \u{2014} keeping original bytes"),
        "{}",
        r.message
    );
}

// the Python app: musaic/tags.py:445
// `log.warning("save_cover_to_album_dir: not a directory: %s", album_path)`.
#[test]
fn cover_sidecar_into_missing_dir_logs() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("gone");
    let (out, lines) = capture(|| save_cover_to_album_dir(&missing, b"x"));
    assert!(out.is_none());
    let r = only(
        &lines,
        Level::Warning,
        "save_cover_to_album_dir: not a directory: ",
    );
    assert_eq!(
        r.message,
        format!(
            "save_cover_to_album_dir: not a directory: {}",
            missing.display()
        )
    );
}

// the Python app: musaic/tags.py:453 `log.warning("failed to write %s: %s", target, e)`.
// `e` is io::Error's text here, OSError's repr in the Python app.
#[test]
fn cover_sidecar_write_failure_logs() {
    let tmp = tempfile::tempdir().unwrap();
    let album = tmp.path().join("album");
    std::fs::create_dir(&album).unwrap();
    std::fs::set_permissions(&album, std::fs::Permissions::from_mode(0o555)).unwrap();
    let (out, lines) = capture(|| save_cover_to_album_dir(&album, b"x"));
    std::fs::set_permissions(&album, std::fs::Permissions::from_mode(0o755)).unwrap();
    if out.is_some() {
        panic!("wrote into a read-only dir (running as root?)");
    }
    only(
        &lines,
        Level::Warning,
        &format!("failed to write {}: ", album.join("cover.jpg").display()),
    );
}

// the Python app: musaic/tags.py:277
// `log.warning("rename target exists, rolling back: %s", final)`.
#[test]
fn rename_onto_existing_file_logs_rollback() {
    let tmp = tempfile::tempdir().unwrap();
    let album = tmp.path().join("album");
    std::fs::create_dir(&album).unwrap();
    std::fs::write(album.join("01 a.flac"), flac_bytes(&[("TRACKNUMBER", "1")])).unwrap();
    std::fs::write(album.join("02 a.flac"), flac_bytes(&[("TRACKNUMBER", "2")])).unwrap();
    let (events, lines) = capture(|| renumber(&album));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::FileRenamed { .. })),
        "{events:?}"
    );
    assert!(album.join("01 a.flac").is_file());
    // Deliberate addition: the file that kept its name is in the outcome.
    assert_eq!(
        rename_failures(&events),
        [RenameFailure {
            filename: "01 a.flac".into(),
            new_filename: "02 a.flac".into(),
            error: "02 a.flac already exists".into(),
        }]
    );
    let r = only(
        &lines,
        Level::Warning,
        "rename target exists, rolling back: ",
    );
    assert_eq!(
        r.message,
        format!(
            "rename target exists, rolling back: {}",
            album.join("02 a.flac").display()
        )
    );
}

// the Python app: musaic/tags.py:290
// `log.exception("rename batch failed; attempting rollback")`.
// Deliberate difference: ERROR with ": <error>" appended instead of a
// Python traceback. Driven by a read-only album dir: the tag write is in
// place, the staging rename is refused. The new TRACKNUMBER stays written
// (rolling a tag write back could fail the same way); deliberate change:
// the `done` event names the file that kept its old name, where the
// Python app only logged the batch failure.
#[test]
fn failed_rename_batch_logs_error() {
    let tmp = tempfile::tempdir().unwrap();
    let album = tmp.path().join("album");
    std::fs::create_dir(&album).unwrap();
    std::fs::write(album.join("01 a.flac"), flac_bytes(&[("TRACKNUMBER", "1")])).unwrap();
    std::fs::set_permissions(&album, std::fs::Permissions::from_mode(0o555)).unwrap();
    let (events, lines) = capture(|| renumber(&album));
    std::fs::set_permissions(&album, std::fs::Permissions::from_mode(0o755)).unwrap();
    let saved = events.iter().any(|e| match e {
        Event::FileDone(c) => c.error.is_none(),
        _ => false,
    });
    assert!(saved, "the in-place tag write must succeed: {events:?}");
    only(
        &lines,
        Level::Error,
        "rename batch failed; attempting rollback: ",
    );
    assert!(album.join("01 a.flac").is_file());
    let failures = rename_failures(&events);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert_eq!(
        (
            failures[0].filename.as_str(),
            failures[0].new_filename.as_str()
        ),
        ("01 a.flac", "02 a.flac")
    );
    assert!(!failures[0].error.is_empty());
}

// The same batch failure part-way through: the first file is staged, the
// second's temporary name is taken by a non-empty directory, which a file
// cannot be moved onto. The staged file is moved back, and both files are
// named in the `done` event.
#[test]
fn failed_rename_batch_names_every_file_left_unrenamed() {
    let tmp = tempfile::tempdir().unwrap();
    let album = tmp.path().join("album");
    std::fs::create_dir(&album).unwrap();
    let long = "02 b.flac".to_string();
    let blocker = album.join(".02 b.flac.renaming.tmp");
    std::fs::create_dir(&blocker).unwrap();
    std::fs::write(blocker.join("keep"), b"").unwrap();
    std::fs::write(album.join("01 a.flac"), flac_bytes(&[("TRACKNUMBER", "1")])).unwrap();
    std::fs::write(album.join(&long), flac_bytes(&[("TRACKNUMBER", "2")])).unwrap();
    let mut events = Vec::new();
    let ((), lines) = capture(|| {
        apply_to_album_iter(
            &album,
            &Vec::new(),
            &vec![
                (
                    "01 a.flac".to_owned(),
                    vec![("tracknumber".to_owned(), Some("3".to_owned()))],
                ),
                (
                    long.clone(),
                    vec![("tracknumber".to_owned(), Some("4".to_owned()))],
                ),
            ],
            &Default::default(),
            None,
            false,
            &mut |e| events.push(e),
        )
        .unwrap()
    });
    only(
        &lines,
        Level::Error,
        "rename batch failed; attempting rollback: ",
    );
    let mut names: Vec<String> = std::fs::read_dir(&album)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            ".02 b.flac.renaming.tmp".to_owned(),
            "01 a.flac".to_owned(),
            long.clone()
        ]
    );
    let failures = rename_failures(&events);
    let got: Vec<(&str, &str)> = failures
        .iter()
        .map(|f| (f.filename.as_str(), f.new_filename.as_str()))
        .collect();
    assert_eq!(
        got,
        [("01 a.flac", "03 a.flac"), (long.as_str(), "04 b.flac")]
    );
    assert_eq!(
        failures[0].error,
        format!("not renamed: the batch stopped at {long}")
    );
}
