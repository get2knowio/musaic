//! The `library` operations with no HTTP in scope: albums, tracks and covers as
//! typed values, and the preconditions each operation owns (the guard before the
//! work, nothing changed when refused).

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use musaic_app::App;
use musaic_app::ops::OpError;
use musaic_app::ops::Subject;
use musaic_app::ops::library::{CoverImage, CoverSize};
use musaic_events::{Event, EventKind, Subscriber, Topic};

fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for {what}");
}

/// A configured app whose library was scanned and saved.
fn scanned() -> (App, common::Scratch) {
    let (app, s) = common::app_quiet_pool(|b| b);
    common::configure(&app, &s);
    let n = app.ops().scan().subscribe(false).unwrap().count();
    assert!(n > 2);
    eventually("the work guard released", || {
        app.gate().snapshot() == (0, false)
    });
    (app, s)
}

#[test]
fn albums_tracks_and_covers_are_typed_values() {
    let (app, _s) = scanned();
    let list = app.ops().library().list_albums(false).unwrap();
    assert!(list.count() > 1);
    assert!(!list.scanned_at.is_empty());
    let first = &list.albums[0];
    assert!(!first.id().is_empty());
    assert!(!first.display_artist(true).is_empty());
    assert!(!first.is_missing());

    let one = app.ops().library().album(first.id()).unwrap();
    assert_eq!(one.id(), first.id());
    assert_eq!(one.path(), first.path());

    let tracks = app.ops().library().tracks(first.id()).unwrap();
    assert_eq!(tracks.album_id, first.id());
    assert!(!tracks.tracks.is_empty());
    assert!(tracks.tracks[0].file_name().ends_with(".flac"));
    assert!(tracks.tracks[0].error().is_none());

    // Some album in the synthetic library has a cover; one without is NotFound.
    let mut served = 0;
    for a in &list.albums {
        match app.ops().library().cover(a.id(), CoverSize::Full) {
            Ok(CoverImage::File(p)) => {
                assert!(p.is_file());
                served += 1;
            }
            Ok(CoverImage::Embedded { data, mime }) => {
                assert!(!data.is_empty() && mime.starts_with("image/"));
                served += 1;
            }
            Ok(other) => panic!("a full cover is not {other:?}"),
            Err(OpError::NotFound {
                what: Subject::Cover,
                ..
            }) => {}
            Err(e) => panic!("{e:?}"),
        }
    }
    assert!(served > 0, "the library has covers");
    let thumb = app.ops().library().cover(first.id(), CoverSize::Thumb);
    if let Ok(CoverImage::Thumbnail(p)) = thumb {
        assert!(p.is_file());
    }
}

#[test]
fn an_unknown_album_id_is_not_found_everywhere() {
    let (app, _s) = scanned();
    let ops = app.ops();
    let lib = ops.library();
    let nf = |e: OpError| {
        matches!(
            e,
            OpError::NotFound {
                what: Subject::Album,
                ..
            }
        )
    };
    assert!(nf(lib.album("0000000000000000").unwrap_err()));
    assert!(nf(lib.tracks("0000000000000000").unwrap_err()));
    assert!(nf(lib
        .cover("0000000000000000", CoverSize::Thumb)
        .unwrap_err()));
    assert!(nf(lib.remove_album("0000000000000000").unwrap_err()));
    assert!(nf(lib.clear_mb_anchor("0000000000000000").unwrap_err()));
    assert!(nf(lib
        .resolution("0000000000000000", false, "")
        .unwrap_err()));
}

#[test]
fn reads_without_a_library_are_no_library() {
    let (app, _s) = common::app();
    let ops = app.ops();
    let lib = ops.library();
    assert!(matches!(
        lib.list_albums(false),
        Err(OpError::NoLibrary { .. })
    ));
    assert!(matches!(lib.album("x"), Err(OpError::NoLibrary { .. })));
    assert!(matches!(lib.tracks("x"), Err(OpError::NoLibrary { .. })));
    assert_eq!(lib.config().library_dir, None);
    assert!(!lib.config().exists);
}

#[test]
fn set_library_dir_during_a_restore_is_busy_even_for_a_bad_folder() {
    let (app, s) = common::app();
    let bad = s.tmp.path().join("no-such-folder");
    let bad = bad.to_str().unwrap();
    let refused = |e: OpError| matches!(&e, OpError::Rejected { message, .. } if message.starts_with("Not a directory: "));
    assert!(refused(
        app.ops().library().set_library_dir(bad).unwrap_err()
    ));
    let restore = common::hold_restore(&app);
    // The guard comes before the folder check, as `POST /api/config` has
    // always answered (a deviation from FR-008, kept for byte-identical HTTP).
    let err = app.ops().library().set_library_dir(bad).unwrap_err();
    assert!(matches!(err, OpError::Busy { .. }), "{err:?}");
    // A good folder during a restore is Busy, and nothing is stored.
    let good = s.tmp.path().join("music");
    std::fs::create_dir(&good).unwrap();
    let err = app
        .ops()
        .library()
        .set_library_dir(good.to_str().unwrap())
        .unwrap_err();
    assert!(matches!(err, OpError::Busy { .. }), "{err:?}");
    assert_eq!(app.ops().library().config().library_dir, None);
    drop(restore);
    let choice = app
        .ops()
        .library()
        .set_library_dir(good.to_str().unwrap())
        .unwrap();
    assert!(choice.library_dir.is_some());
    assert!(app.ops().library().config().exists);
}

#[test]
fn remove_album_during_a_restore_is_busy_and_the_row_stays() {
    let (app, _s) = scanned();
    let id = app.ops().library().list_albums(false).unwrap().albums[0]
        .id()
        .to_string();
    let restore = common::hold_restore(&app);
    let err = app.ops().library().remove_album(&id).unwrap_err();
    assert!(matches!(err, OpError::Busy { .. }), "{err:?}");
    assert!(
        app.ops()
            .library()
            .list_albums(false)
            .unwrap()
            .albums
            .iter()
            .any(|a| a.id() == id),
        "the row is still there"
    );
    // The other writers are refused too, and so is a refreshing list.
    assert!(matches!(
        app.ops().library().clear_mb_anchor(&id),
        Err(OpError::Busy { .. })
    ));
    assert!(matches!(
        app.ops().library().resolution(&id, false, ""),
        Err(OpError::Busy { .. })
    ));
    assert!(matches!(
        app.ops().library().list_albums(true),
        Err(OpError::Busy { .. })
    ));
    // A plain list reads on.
    assert!(app.ops().library().list_albums(false).is_ok());
    drop(restore);
}

#[test]
fn remove_album_when_free_publishes_album_deleted_unasked() {
    let (app, _s) = scanned();
    let id = app.ops().library().list_albums(false).unwrap().albums[0]
        .id()
        .to_string();
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let s2 = Arc::clone(&seen);
    app.bus().register(
        Topic::Kind(EventKind::AlbumDeleted),
        Subscriber::from_fn(move |e| {
            if let Event::AlbumDeleted { album_id } = e {
                s2.lock().unwrap().push(album_id.clone());
            }
            Ok(())
        }),
    );
    let removed = app.ops().library().remove_album(&id).unwrap();
    assert_eq!(removed.album_id, id);
    assert!(app.bus().wait_background_idle(Duration::from_secs(30)));
    assert_eq!(seen.lock().unwrap().as_slice(), std::slice::from_ref(&id));
    assert!(
        !app.ops()
            .library()
            .list_albums(false)
            .unwrap()
            .albums
            .iter()
            .any(|a| a.id() == id)
    );
    // Gone from the library: removing it again is NotFound.
    assert!(matches!(
        app.ops().library().remove_album(&id),
        Err(OpError::NotFound { .. })
    ));
}

#[test]
fn clear_mb_anchor_publishes_the_tags_change() {
    let (app, _s) = scanned();
    let id = app.ops().library().list_albums(false).unwrap().albums[0]
        .id()
        .to_string();
    let seen: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
    let s2 = Arc::clone(&seen);
    app.bus().register(
        Topic::Kind(EventKind::AlbumTagsChanged),
        Subscriber::from_fn(move |e| {
            if let Event::AlbumTagsChanged { fields, .. } = e {
                s2.lock().unwrap().push(fields.clone());
            }
            Ok(())
        }),
    );
    let cleared = app.ops().library().clear_mb_anchor(&id).unwrap();
    assert_eq!(cleared.blocked_mbid, None, "no anchor to block");
    assert!(app.bus().wait_background_idle(Duration::from_secs(30)));
    assert_eq!(
        *seen.lock().unwrap(),
        [vec!["mb_release_group_mbid".to_string()]]
    );
}
