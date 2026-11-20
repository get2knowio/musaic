//! The `apply` operations with no HTTP in scope: the run-with-sink shape. A
//! refusal comes back before the sink is called once; a run calls the sink in
//! order with the work guard held, publishes its bus events itself, and a sink
//! that is gone is not called again while the run still finishes.

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use musaic_app::App;
use musaic_app::ops::apply::{
    ApplyChanges, ApplyEvent, ApplyOutcome, MAX_COVER_UPLOAD_BYTES, RemovalFault, TagEvent,
};
use musaic_app::ops::{Listening, OpError, Subject};
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

/// An album with at least two tracks: its id, folder and track count.
fn an_album(app: &App) -> (String, String, usize) {
    for a in app.ops().library().list_albums(false).unwrap().albums {
        let n = app
            .ops()
            .library()
            .tracks(a.id())
            .unwrap()
            .tracks
            .iter()
            .filter(|t| t.error().is_none())
            .count();
        if n >= 2 {
            return (a.id().to_string(), a.path().to_string(), n);
        }
    }
    panic!("no album with two tracks");
}

/// What the bus carried: `("tags" | "cover", album id, fields)`.
type Seen = Arc<Mutex<Vec<(&'static str, String, Vec<String>)>>>;

fn record_bus(app: &App) -> Seen {
    let seen: Seen = Arc::default();
    for kind in [EventKind::AlbumTagsChanged, EventKind::AlbumCoverChanged] {
        let s2 = Arc::clone(&seen);
        app.bus().register(
            Topic::Kind(kind),
            Subscriber::from_fn(move |e| {
                match e {
                    Event::AlbumTagsChanged { album_id, fields } => {
                        s2.lock()
                            .unwrap()
                            .push(("tags", album_id.clone(), fields.clone()))
                    }
                    Event::AlbumCoverChanged { album_id } => {
                        s2.lock().unwrap().push(("cover", album_id.clone(), vec![]))
                    }
                    _ => {}
                }
                Ok(())
            }),
        );
    }
    seen
}

fn year(y: &str) -> ApplyChanges {
    ApplyChanges {
        suggestions: vec![("year".into(), y.into())],
        ..Default::default()
    }
}

/// A real JPEG from the synthetic library.
fn a_jpeg() -> Vec<u8> {
    let p =
        musaic_testdata::synthetic_library().join("Various Artists - Night Mix [FLAC]/cover.jpg");
    std::fs::read(p).expect("the synthetic library has a cover")
}

fn nothing(calls: &std::cell::Cell<usize>) -> impl FnMut(ApplyEvent) -> Listening + '_ {
    move |_| {
        calls.set(calls.get() + 1);
        Listening::Yes
    }
}

#[test]
fn a_run_reports_in_order_and_publishes_its_events_itself() {
    let (app, _s) = scanned();
    let (id, _path, n) = an_album(&app);
    let seen = record_bus(&app);
    let mut events = Vec::new();
    // The work guard is held while the sink is called, the terminal event too.
    let mut held = Vec::new();
    let outcome = app
        .ops()
        .apply()
        .apply_changes(&id, &year("2031"), &mut |e| {
            held.push(app.gate().snapshot().0);
            events.push(e);
            Listening::Yes
        })
        .unwrap();
    assert_eq!(outcome, ApplyOutcome::Finished);
    assert_eq!(app.gate().snapshot(), (0, false), "released afterwards");
    assert!(held.iter().all(|&h| h == 1), "{held:?}");

    assert!(
        matches!(&events[0], ApplyEvent::Tags(TagEvent::Start { file_count, .. }) if *file_count == n)
    );
    let terminal: Vec<_> = events.iter().filter(|e| e.is_terminal()).collect();
    assert_eq!(terminal.len(), 1);
    assert!(
        matches!(events.last(), Some(ApplyEvent::UpdatedAlbum(a)) if a.id() == id),
        "{:?}",
        events.last()
    );
    // Start, a started / done pair per file, Done, then the updated album.
    let mut shape = String::new();
    for e in &events {
        shape.push(match e {
            ApplyEvent::Tags(TagEvent::Start { .. }) => 'S',
            ApplyEvent::Tags(TagEvent::FileStarted { .. }) => 'f',
            ApplyEvent::Tags(TagEvent::FileDone(_)) => 'd',
            ApplyEvent::Tags(TagEvent::Done { .. }) => 'D',
            ApplyEvent::UpdatedAlbum(_) => 'U',
            other => panic!("unexpected {other:?}"),
        });
    }
    assert_eq!(shape, format!("S{}DU", "fd".repeat(n)));

    // Published by the operation, once, with no cover event.
    assert!(app.bus().wait_background_idle(Duration::from_secs(30)));
    assert_eq!(
        *seen.lock().unwrap(),
        [("tags", id.clone(), vec!["year".to_string()])]
    );
}

#[test]
fn a_cover_changes_art_and_publishes_both_events() {
    let (app, _s) = scanned();
    let (id, path, _n) = an_album(&app);
    let seen = record_bus(&app);
    let mut events = Vec::new();
    let outcome = app
        .ops()
        .apply()
        .upload_cover(&id, Some("IMAGE/JPEG"), a_jpeg(), &mut |e| {
            events.push(e);
            Listening::Yes
        })
        .unwrap();
    assert_eq!(outcome, ApplyOutcome::Finished);
    assert!(matches!(&events[0], ApplyEvent::CoverDownloaded { mime, .. } if mime == "image/jpeg"));
    assert!(
        events.iter().any(
            |e| matches!(e, ApplyEvent::CoverSaved { filename, .. } if filename == "cover.jpg")
        )
    );
    assert!(matches!(events.last(), Some(ApplyEvent::UpdatedAlbum(_))));
    assert!(std::path::Path::new(&path).join("cover.jpg").is_file());
    assert!(app.bus().wait_background_idle(Duration::from_secs(30)));
    let seen = seen.lock().unwrap();
    assert!(seen.iter().any(|s| s.0 == "tags" && s.1 == id), "{seen:?}");
    assert!(seen.iter().any(|s| s.0 == "cover" && s.1 == id), "{seen:?}");
}

#[test]
fn a_dry_run_ends_with_its_done_and_publishes_nothing() {
    let (app, _s) = scanned();
    let (id, _path, _n) = an_album(&app);
    let seen = record_bus(&app);
    let mut events = Vec::new();
    let mut changes = year("2031");
    changes.dry_run = true;
    app.ops()
        .apply()
        .apply_changes(&id, &changes, &mut |e| {
            events.push(e);
            Listening::Yes
        })
        .unwrap();
    assert!(
        matches!(
            events.last(),
            Some(ApplyEvent::Tags(TagEvent::Done { dry_run: true, .. }))
        ),
        "{:?}",
        events.last()
    );
    assert_eq!(events.iter().filter(|e| e.is_terminal()).count(), 1);
    assert!(app.bus().wait_background_idle(Duration::from_secs(30)));
    assert!(seen.lock().unwrap().is_empty());
}

#[test]
fn a_refusal_is_returned_before_the_sink_is_called() {
    let (app, _s) = scanned();
    let (id, path, _n) = an_album(&app);
    let calls = std::cell::Cell::new(0);
    let mut sink = nothing(&calls);
    let apply = app.ops();
    let apply = apply.apply();

    // Unknown album.
    let err = apply
        .apply_changes("0000000000000000", &year("2031"), &mut sink)
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

    // A removal that cannot be honoured comes first, before the guard.
    let restore = common::hold_restore(&app);
    let bad = ApplyChanges {
        clear: vec!["nope".into()],
        ..Default::default()
    };
    let err = apply.apply_changes(&id, &bad, &mut sink).unwrap_err();
    assert!(matches!(err, OpError::InvalidInput { .. }), "{err:?}");

    // Busy: a restore holds the gate; a dry run is refused too, and the guard
    // comes before the album lookup.
    let mut dry = year("2031");
    dry.dry_run = true;
    for (album, changes) in [(&id[..], &dry), ("0000000000000000", &dry)] {
        let err = apply.apply_changes(album, changes, &mut sink).unwrap_err();
        assert!(matches!(err, OpError::Busy { .. }), "{err:?}");
    }
    // An upload is Busy before its content is looked at.
    let err = apply
        .upload_cover(&id, Some("text/plain"), vec![1], &mut sink)
        .unwrap_err();
    assert!(matches!(err, OpError::Busy { .. }), "{err:?}");
    drop(restore);

    // An upload's own refusals: type, empty, size.
    let err = apply
        .upload_cover(&id, Some("text/plain"), vec![1], &mut sink)
        .unwrap_err();
    assert!(
        matches!(&err, OpError::Rejected { message, .. } if message.starts_with("Unsupported file type")),
        "{err:?}"
    );
    let err = apply
        .upload_cover(&id, Some("image/png"), Vec::new(), &mut sink)
        .unwrap_err();
    assert!(
        matches!(&err, OpError::Rejected { message, .. } if message == "Empty file"),
        "{err:?}"
    );
    let err = apply
        .upload_cover(
            &id,
            Some("image/png"),
            vec![0; MAX_COVER_UPLOAD_BYTES + 1],
            &mut sink,
        )
        .unwrap_err();
    assert!(
        matches!(err, OpError::TooLarge { limit, .. } if limit == MAX_COVER_UPLOAD_BYTES as u64),
        "{err:?}"
    );

    // A folder that is gone makes the album Missing after a rescan: refused as
    // the apply pipeline always did.
    std::fs::remove_dir_all(&path).unwrap();
    app.ops().scan().subscribe(true).unwrap().count();
    eventually("the work guard released", || {
        app.gate().snapshot() == (0, false)
    });
    assert!(app.ops().library().album(&id).unwrap().is_missing());
    for err in [
        apply
            .apply_changes(&id, &year("2031"), &mut sink)
            .unwrap_err(),
        apply
            .upload_cover(&id, Some("image/jpeg"), a_jpeg(), &mut sink)
            .unwrap_err(),
    ] {
        assert!(
            matches!(&err, OpError::Conflict { message, .. } if message.starts_with("Album is missing from disk")),
            "{err:?}"
        );
    }

    assert_eq!(calls.get(), 0, "no refusal reached the sink");
    assert_eq!(app.gate().snapshot(), (0, false), "the guard was released");

    // No library folder.
    let (bare, _s2) = common::app();
    let err = bare
        .ops()
        .apply()
        .apply_changes("x", &year("2031"), &mut sink)
        .unwrap_err();
    assert!(matches!(err, OpError::NoLibrary { .. }), "{err:?}");
    assert_eq!(calls.get(), 0);
}

#[test]
fn removal_problems_name_the_entry_and_the_fault() {
    let c = ApplyChanges {
        suggestions: vec![("genre".into(), "Folk".into())],
        clear: vec!["genre".into(), "bogus".into()],
        track_clear: vec![("01.flac".into(), vec!["tracknumber".into()])],
        ..Default::default()
    };
    let p = c.removal_problems();
    assert_eq!(p.len(), 3);
    assert_eq!(p[0].fault, RemovalFault::AlsoSuggested);
    assert!(
        matches!(&p[1].fault, RemovalFault::Unknown { allowed } if allowed.contains(&"genre".to_string()))
    );
    assert_eq!(p[2].fault, RemovalFault::SetOnly);
    assert!(ApplyChanges::default().removal_problems().is_empty());
}

#[test]
fn a_gone_sink_is_not_called_again_and_the_run_still_completes() {
    let (app, _s) = scanned();
    let (id, _path, _n) = an_album(&app);
    let seen = record_bus(&app);
    let mut calls = 0;
    let outcome = app
        .ops()
        .apply()
        .apply_changes(&id, &year("2032"), &mut |_| {
            calls += 1;
            Listening::Gone
        })
        .unwrap();
    assert_eq!(calls, 1, "not called after it said it was gone");
    assert_eq!(outcome, ApplyOutcome::Stopped);
    // Every file was still written and the album's event still published.
    assert!(app.bus().wait_background_idle(Duration::from_secs(30)));
    assert_eq!(
        *seen.lock().unwrap(),
        [("tags", id.clone(), vec!["year".to_string()])]
    );
    let album = app.ops().library().album(&id).unwrap();
    assert!(
        album
            .iter()
            .any(|(k, v)| k == "year" && v.as_str() == Some("2032")),
        "the row was updated"
    );
    assert_eq!(app.gate().snapshot(), (0, false));
}

#[test]
fn a_panicking_sink_counts_as_gone() {
    let (app, _s) = scanned();
    let (id, _path, _n) = an_album(&app);
    let seen = record_bus(&app);
    let mut calls = 0;
    app.ops()
        .apply()
        .apply_changes(&id, &year("2033"), &mut |_| {
            calls += 1;
            panic!("the sink broke");
        })
        .unwrap();
    assert_eq!(calls, 1);
    assert!(app.bus().wait_background_idle(Duration::from_secs(30)));
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[test]
fn the_previews_refuse_what_they_cannot_show() {
    let (app, _s) = scanned();
    let (id, _path, _n) = an_album(&app);
    let apply = app.ops();
    let apply = apply.apply();
    let err = apply.job_cover_preview("nope").unwrap_err();
    assert!(
        matches!(&err, OpError::NotFound { what: Subject::Job, message, .. } if message == "Job not found"),
        "{err:?}"
    );
    let err = apply.album_cover_preview("0000000000000000").unwrap_err();
    assert!(
        matches!(&err, OpError::NotFound { what: Subject::Album, message, .. } if message == "Album not found"),
        "{err:?}"
    );
    // An album with no MusicBrainz anchor has no cover to propose.
    let err = apply.album_cover_preview(&id).unwrap_err();
    assert!(
        matches!(&err, OpError::NotFound { what: Subject::Cover, message, .. } if message == "This album has no MusicBrainz cover to preview"),
        "{err:?}"
    );
}

#[test]
fn a_prepared_run_holds_the_guard_and_writes_nothing_until_it_runs() {
    let (app, _s) = scanned();
    let (id, _, _) = an_album(&app);
    let seen = record_bus(&app);

    // Dropped unrun (a face whose caller left): the guard goes, nothing is written.
    let run = app
        .ops()
        .apply()
        .prepare_changes(&id, &year("1971"))
        .unwrap();
    assert!(matches!(
        app.ops().database().restore_allowed(),
        Err(OpError::Busy { .. })
    ));
    drop(run);
    assert!(app.ops().database().restore_allowed().is_ok());
    std::thread::sleep(Duration::from_millis(50));
    assert!(seen.lock().unwrap().is_empty(), "nothing was written");

    // Run: the same as the one-call form.
    let calls = std::cell::Cell::new(0);
    let outcome = app
        .ops()
        .apply()
        .prepare_changes(&id, &year("1971"))
        .unwrap()
        .run(&mut nothing(&calls))
        .unwrap();
    assert_eq!(outcome, ApplyOutcome::Finished);
    assert!(calls.get() > 0);
    eventually("the tags event", || {
        seen.lock()
            .unwrap()
            .iter()
            .any(|(k, a, _)| *k == "tags" && *a == id)
    });
    assert!(app.ops().database().restore_allowed().is_ok());
}
