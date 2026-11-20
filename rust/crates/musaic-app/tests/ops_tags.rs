//! The `tags` operations with no HTTP in scope: tag state as typed values, the
//! preconditions each write owns (the guard before the work, nothing changed
//! when refused, a Missing album refused), and the vocabulary.

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use musaic_app::App;
use musaic_app::ops::OpError;
use musaic_app::ops::Subject;
use musaic_app::ops::tags::AlbumTags;
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

/// An album that has tracks: its id, folder and file names.
fn an_album(app: &App) -> (String, String, Vec<String>) {
    for a in app.ops().library().list_albums(false).unwrap().albums {
        let tracks = app.ops().library().tracks(a.id()).unwrap().tracks;
        let files: Vec<String> = tracks
            .iter()
            .filter(|t| t.error().is_none())
            .map(|t| t.file_name().to_string())
            .collect();
        if files.len() >= 2 {
            return (a.id().to_string(), a.path().to_string(), files);
        }
    }
    panic!("no album with two tracks");
}

/// Every `AlbumTagsChanged` the bus carries.
fn record_events(app: &App) -> Arc<Mutex<Vec<Event>>> {
    let seen: Arc<Mutex<Vec<Event>>> = Arc::default();
    let s2 = Arc::clone(&seen);
    app.bus().register(
        Topic::Kind(EventKind::AlbumTagsChanged),
        Subscriber::from_fn(move |e| {
            s2.lock().unwrap().push(e.clone());
            Ok(())
        }),
    );
    seen
}

fn tag_names(t: &AlbumTags, file: &str) -> Vec<String> {
    t.track(file)
        .map(|s| s.iter().map(str::to_string).collect())
        .unwrap_or_default()
}

#[test]
fn tags_are_written_read_back_as_tag_sets_and_listed() {
    let (app, _s) = scanned();
    let (id, _path, files) = an_album(&app);
    let events = record_events(&app);
    let ops = app.ops();
    let tags = ops.tags();

    assert!(tags.album_tags(&id).unwrap().tracks.is_empty());
    let all = tags.add_album_tag(&id, "  Favourite ", None).unwrap();
    assert_eq!(all.album_id, id);
    assert!(all.album_tags.contains("Favourite"), "the tag is stripped");
    for f in &files {
        assert_eq!(tag_names(&all, f), ["Favourite"], "{f}");
    }

    let one = tags.add_track_tag(&id, &files[0], "Live").unwrap();
    assert_eq!(tag_names(&one, &files[0]), ["Favourite", "Live"]);
    assert_eq!(tag_names(&one, &files[1]), ["Favourite"]);
    assert!(!one.album_tags.contains("Live"), "not every track has it");
    assert_eq!(tags.album_tags(&id).unwrap(), one);

    let counts = tags.list_tags().unwrap();
    assert!(
        counts
            .tags
            .iter()
            .any(|(t, n)| t == "Favourite" && *n == files.len() as i64)
    );
    let live = tags.tracks_for_tag("Live").unwrap();
    assert_eq!(live.tag, "Live");
    assert_eq!(live.tracks.len(), 1);
    assert_eq!(live.tracks[0].album_id, id);
    assert_eq!(live.tracks[0].file_name, files[0]);

    let gone = tags.remove_track_tag(&id, &files[0], "Live").unwrap();
    assert_eq!(tag_names(&gone, &files[0]), ["Favourite"]);
    let none = tags.remove_album_tag(&id, "Favourite").unwrap();
    assert!(none.tracks.is_empty() && none.album_tags.is_empty());

    let replaced = tags
        .replace_album_tags(
            &id,
            &[(files[1].clone(), vec!["Only".into(), "Two".into()])],
        )
        .unwrap();
    assert_eq!(tag_names(&replaced, &files[1]), ["Only", "Two"]);

    // Tag writes publish no `AlbumTagsChanged` today (the recorded behaviour has no event).
    assert!(app.bus().wait_background_idle(Duration::from_secs(30)));
    assert!(events.lock().unwrap().is_empty(), "{:?}", events.lock());
}

#[test]
fn a_missing_album_refuses_writes_and_still_reads() {
    let (app, _s) = scanned();
    let (id, path, files) = an_album(&app);
    app.ops()
        .tags()
        .add_track_tag(&id, &files[0], "Kept")
        .unwrap();
    std::fs::remove_dir_all(&path).unwrap();
    app.ops().scan().subscribe(true).unwrap().count();
    eventually("the work guard released", || {
        app.gate().snapshot() == (0, false)
    });
    assert!(app.ops().library().album(&id).unwrap().is_missing());

    let ops = app.ops();
    let tags = ops.tags();
    let read = tags.album_tags(&id).unwrap();
    assert_eq!(tag_names(&read, &files[0]), ["Kept"]);
    let refused = |e: OpError| matches!(&e, OpError::Conflict { message, .. } if message.starts_with("Album is missing from disk"));
    assert!(refused(tags.add_album_tag(&id, "New", None).unwrap_err()));
    assert!(refused(
        tags.add_track_tag(&id, &files[0], "New").unwrap_err()
    ));
    assert!(refused(tags.remove_album_tag(&id, "Kept").unwrap_err()));
    assert!(refused(
        tags.remove_track_tag(&id, &files[0], "Kept").unwrap_err()
    ));
    assert!(refused(tags.replace_album_tags(&id, &[]).unwrap_err()));
    assert_eq!(tags.album_tags(&id).unwrap(), read, "nothing changed");
}

#[test]
fn a_write_during_a_restore_is_busy_with_nothing_changed() {
    let (app, _s) = scanned();
    let (id, _path, files) = an_album(&app);
    let ops = app.ops();
    let tags = ops.tags();
    let before = tags.add_track_tag(&id, &files[0], "Before").unwrap();
    tags.add_vocabulary_entry("Voc", None, None, None).unwrap();
    let vocab_before = tags.list_vocabulary().unwrap();

    let restore = common::hold_restore(&app);
    let busy = |e: OpError| matches!(e, OpError::Busy { .. });
    // The guard comes first: a blank tag or an unknown album is still Busy,
    // as the routes always answered.
    assert!(busy(tags.add_album_tag(&id, "x", None).unwrap_err()));
    assert!(busy(tags.add_album_tag(&id, " ", None).unwrap_err()));
    assert!(busy(tags.add_album_tag("nope", "x", None).unwrap_err()));
    assert!(busy(tags.add_track_tag(&id, &files[0], "x").unwrap_err()));
    assert!(busy(tags.remove_album_tag(&id, "Before").unwrap_err()));
    assert!(busy(
        tags.remove_track_tag(&id, &files[0], "Before").unwrap_err()
    ));
    assert!(busy(tags.replace_album_tags(&id, &[]).unwrap_err()));
    assert!(busy(
        tags.add_vocabulary_entry("x", None, None, None)
            .unwrap_err()
    ));
    assert!(busy(tags.promote_orphan_tag("Before").unwrap_err()));
    assert!(busy(
        tags.patch_vocabulary_entry("Voc", Some("d"), None, None)
            .unwrap_err()
    ));
    assert!(busy(tags.rename_vocabulary_entry("Voc", "V2").unwrap_err()));
    assert!(busy(
        tags.merge_vocabulary_entry("Voc", "Before").unwrap_err()
    ));
    assert!(busy(tags.delete_vocabulary_entry("Voc", true).unwrap_err()));
    // Reads answer during a restore.
    assert_eq!(tags.album_tags(&id).unwrap(), before);
    assert_eq!(tags.list_vocabulary().unwrap(), vocab_before);
    drop(restore);
    assert_eq!(tags.album_tags(&id).unwrap(), before, "nothing changed");
}

#[test]
fn refusals_are_typed_in_the_routes_order() {
    let (app, _s) = common::app();
    let ops = app.ops();
    let tags = ops.tags();
    // No library: the album lookup says so.
    assert!(matches!(
        tags.album_tags("x"),
        Err(OpError::NoLibrary { .. })
    ));
    assert!(matches!(
        tags.add_album_tag("x", "t", None),
        Err(OpError::NoLibrary { .. })
    ));

    let (app, _s) = scanned();
    let (id, _path, files) = an_album(&app);
    let ops = app.ops();
    let tags = ops.tags();
    // The album is looked up before the tag is checked.
    let err = tags
        .add_album_tag("0000000000000000", " ", None)
        .unwrap_err();
    assert!(
        matches!(
            err,
            OpError::NotFound {
                what: Subject::Album,
                ..
            }
        ),
        "{err:?}"
    );
    assert!(matches!(
        tags.add_album_tag(&id, "  ", None).unwrap_err(),
        OpError::Rejected { message, .. } if message == "tag is required"
    ));
    let err = tags.add_track_tag(&id, "../x.flac", "t").unwrap_err();
    assert!(
        matches!(&err, OpError::Rejected { message, .. } if message.contains("is not a FLAC file of this album")),
        "{err:?}"
    );
    assert!(matches!(
        tags.add_album_tag(&id, "t", Some(&["nope.flac".to_string()]))
            .unwrap_err(),
        OpError::Rejected { .. }
    ));
    assert!(matches!(
        tags.tracks_for_tag(" ").unwrap_err(),
        OpError::Rejected { .. }
    ));
    assert!(tags.album_tags(&id).unwrap().tracks.is_empty());
    assert!(files.len() >= 2);
}

#[test]
fn the_vocabulary_is_listed_edited_and_refused_as_the_routes_do() {
    let (app, _s) = scanned();
    let (id, _path, files) = an_album(&app);
    let ops = app.ops();
    let tags = ops.tags();

    let v = tags
        .add_vocabulary_entry(" Mood ", Some("how it feels"), Some("#abc"), Some(2))
        .unwrap();
    assert_eq!(v.entries.len(), 1);
    assert_eq!(v.entries[0].tag, "Mood");
    assert_eq!(v.entries[0].description, "how it feels");
    assert_eq!(v.entries[0].color.as_deref(), Some("#abc"));
    assert_eq!(v.entries[0].sort_order, 2);
    assert!(matches!(
        tags.add_vocabulary_entry(" ", None, None, None)
            .unwrap_err(),
        OpError::Rejected { .. }
    ));

    tags.add_track_tag(&id, &files[0], "Stray").unwrap();
    let v = tags.list_vocabulary().unwrap();
    assert!(v.orphans.iter().any(|(t, n)| t == "Stray" && *n == 1));
    let v = tags.promote_orphan_tag("Stray").unwrap();
    assert!(v.entries.iter().any(|e| e.tag == "Stray" && e.count == 1));
    assert!(v.orphans.is_empty());

    // A name another entry has is a Conflict (rename); a refused merge is Rejected.
    assert!(matches!(
        tags.rename_vocabulary_entry("Mood", "Stray").unwrap_err(),
        OpError::Conflict { .. }
    ));
    assert!(matches!(
        tags.merge_vocabulary_entry("Mood", "Missing").unwrap_err(),
        OpError::Rejected { .. }
    ));
    let renamed = tags.rename_vocabulary_entry("Stray", "Wandering").unwrap();
    assert!(renamed.renamed.vocab);
    assert_eq!(renamed.renamed.tag_rows, 1);
    assert_eq!(renamed.renamed.albums, std::slice::from_ref(&id));
    assert!(renamed.renamed.sidecars_not_written.is_empty());
    assert!(
        tags.album_tags(&id)
            .unwrap()
            .track(&files[0])
            .unwrap()
            .contains("Wandering")
    );

    let merged = tags.merge_vocabulary_entry("Wandering", "Mood").unwrap();
    assert_eq!(merged.merged.moved, 1);
    assert!(
        tags.album_tags(&id)
            .unwrap()
            .track(&files[0])
            .unwrap()
            .contains("Mood")
    );
    let patched = tags
        .patch_vocabulary_entry("Mood", Some("changed"), None, None)
        .unwrap();
    assert_eq!(patched.entries[0].description, "changed");

    let deleted = tags.delete_vocabulary_entry("Mood", true).unwrap();
    assert!(deleted.deleted.vocab);
    assert_eq!(deleted.deleted.tag_rows, 1);
    assert!(deleted.vocabulary.entries.is_empty());
    assert!(tags.list_tags().unwrap().tags.is_empty());
}
