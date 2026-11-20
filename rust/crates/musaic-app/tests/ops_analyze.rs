//! The `analyze` operation with no HTTP in scope: the run-with-sink shape. A
//! refusal comes back before the sink is called once; a run calls the sink in
//! order and ends with exactly one terminal event, the work guard held until
//! after it; a sink that is gone is not called again and the run still finishes
//! and publishes.

mod common;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use musaic_app::App;
use musaic_app::ops::analyze::{AnalysisKind, AnalyzeEvent, AnalyzeOutcome, FileResult};
use musaic_app::ops::{Listening, OpError, Subject};
use musaic_events::{Event, EventKind, Subscriber, Topic};
use musaic_services::analyze::Analyzer;

/// Measurements that need no audio.
struct Fixed;

impl Analyzer for Fixed {
    fn bpm(&self, _: &Path) -> Result<f64, String> {
        Ok(120.0)
    }
    fn loudness(&self, _: &Path) -> Result<(f64, f64), String> {
        Ok((-14.0, 0.5))
    }
}

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

/// A configured app (stub analyzer) whose library was scanned and saved.
fn scanned() -> (App, common::Scratch) {
    let (app, s) = common::app_quiet_pool(|b| b.analyzer(Arc::new(Fixed)));
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

/// `(album id, fields)` of every `AlbumTagsChanged`.
type Changes = Arc<Mutex<Vec<(String, Vec<String>)>>>;

fn record_tag_changes(app: &App) -> Changes {
    let seen: Changes = Arc::default();
    let s2 = Arc::clone(&seen);
    app.bus().register(
        Topic::Kind(EventKind::AlbumTagsChanged),
        Subscriber::from_fn(move |e| {
            if let Event::AlbumTagsChanged { album_id, fields } = e {
                s2.lock().unwrap().push((album_id.clone(), fields.clone()));
            }
            Ok(())
        }),
    );
    seen
}

#[test]
fn a_refusal_is_returned_before_the_sink_is_called() {
    let (app, _s) = scanned();
    let (id, path, _n) = an_album(&app);
    let calls = std::cell::Cell::new(0);
    let mut sink = |_: AnalyzeEvent| {
        calls.set(calls.get() + 1);
        Listening::Yes
    };

    // Unknown album.
    let err = app
        .ops()
        .analyze()
        .run("0000000000000000", AnalysisKind::Bpm, &mut sink)
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

    // Busy: a restore holds the gate; nothing is read or written.
    let restore = common::hold_restore(&app);
    let err = app
        .ops()
        .analyze()
        .run(&id, AnalysisKind::Bpm, &mut sink)
        .unwrap_err();
    assert!(matches!(err, OpError::Busy { .. }), "{err:?}");
    // The guard comes before the album lookup, as the route always did.
    let err = app
        .ops()
        .analyze()
        .run("0000000000000000", AnalysisKind::Bpm, &mut sink)
        .unwrap_err();
    assert!(matches!(err, OpError::Busy { .. }), "{err:?}");
    drop(restore);

    // A folder that cannot be listed.
    std::fs::remove_dir_all(&path).unwrap();
    let err = app
        .ops()
        .analyze()
        .run(&id, AnalysisKind::Bpm, &mut sink)
        .unwrap_err();
    assert!(
        matches!(&err, OpError::Conflict { message, .. } if message.contains("cannot be reached")),
        "{err:?}"
    );

    // A Missing album.
    app.ops().scan().subscribe(true).unwrap().count();
    eventually("the work guard released", || {
        app.gate().snapshot() == (0, false)
    });
    assert!(app.ops().library().album(&id).unwrap().is_missing());
    let err = app
        .ops()
        .analyze()
        .run(&id, AnalysisKind::ReplayGain, &mut sink)
        .unwrap_err();
    assert!(
        matches!(&err, OpError::Conflict { message, .. } if message.starts_with("Album is missing from disk")),
        "{err:?}"
    );

    assert_eq!(calls.get(), 0, "no refusal reached the sink");
    assert_eq!(app.gate().snapshot(), (0, false), "the guard was released");

    // No library folder.
    let (bare, _s2) = common::app();
    let err = bare
        .ops()
        .analyze()
        .run("x", AnalysisKind::Bpm, &mut sink)
        .unwrap_err();
    assert!(matches!(err, OpError::NoLibrary { .. }), "{err:?}");
    assert_eq!(calls.get(), 0);
}

#[test]
fn a_run_reports_in_order_and_ends_with_one_terminal_event() {
    let (app, _s) = scanned();
    let (id, _path, n) = an_album(&app);
    let changes = record_tag_changes(&app);
    let mut events: Vec<AnalyzeEvent> = Vec::new();
    // The work guard is held while the sink is called, the terminal one too.
    let mut held_at_each = Vec::new();
    let outcome = app
        .ops()
        .analyze()
        .run(&id, AnalysisKind::ReplayGain, &mut |e| {
            held_at_each.push(app.gate().snapshot().0);
            events.push(e);
            Listening::Yes
        })
        .unwrap();
    assert_eq!(app.gate().snapshot(), (0, false), "released afterwards");
    assert!(held_at_each.iter().all(|&a| a == 1), "{held_at_each:?}");

    assert!(
        matches!(&events[0], AnalyzeEvent::Started { what: AnalysisKind::ReplayGain, file_count } if *file_count == n),
        "{:?}",
        events[0]
    );
    let terminal: Vec<_> = events.iter().filter(|e| e.is_terminal()).collect();
    assert_eq!(terminal.len(), 1);
    let AnalyzeEvent::Done(summary) = events.last().unwrap() else {
        panic!(
            "the last event is not the terminal one: {:?}",
            events.last()
        );
    };
    assert_eq!(summary.analyzed_count, n);
    assert_eq!(summary.errors, 0);
    assert_eq!(outcome, AnalyzeOutcome::Finished(summary.clone()));

    // Per file, started before done, in file order; then the album's gain, then
    // the rescanned album, then done.
    let mut shape = String::new();
    for e in &events {
        shape.push(match e {
            AnalyzeEvent::Started { .. } => 'S',
            AnalyzeEvent::FileStarted { .. } => 'f',
            AnalyzeEvent::FileDone { .. } => 'd',
            AnalyzeEvent::AlbumDone { .. } => 'A',
            AnalyzeEvent::UpdatedAlbum(_) => 'U',
            AnalyzeEvent::Done(_) => 'D',
        });
    }
    assert_eq!(shape, format!("S{}AUD", "fd".repeat(n)));
    for (i, e) in events.iter().filter_map(file_done).enumerate() {
        assert_eq!(e.0, i);
        assert!(matches!(e.1, FileResult::ReplayGain { track_gain, .. } if !track_gain.is_empty()));
    }

    // The bus event is published once, by the operation, before the end.
    assert!(app.bus().wait_background_idle(Duration::from_secs(30)));
    assert_eq!(
        *changes.lock().unwrap(),
        [(id.clone(), vec!["replaygain".to_string()])]
    );
}

fn file_done(e: &AnalyzeEvent) -> Option<(usize, &FileResult)> {
    match e {
        AnalyzeEvent::FileDone { index, result, .. } => Some((*index, result)),
        _ => None,
    }
}

#[test]
fn a_gone_sink_is_not_called_again_and_the_run_still_completes() {
    let (app, _s) = scanned();
    let (id, _path, n) = an_album(&app);
    let changes = record_tag_changes(&app);
    let mut calls = 0;
    let outcome = app
        .ops()
        .analyze()
        .run(&id, AnalysisKind::Bpm, &mut |_| {
            calls += 1;
            Listening::Gone
        })
        .unwrap();
    assert_eq!(calls, 1, "not called after it said it was gone");
    // Every file was still analysed and the album's event still published.
    let AnalyzeOutcome::Finished(summary) = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(summary.what, AnalysisKind::Bpm);
    assert_eq!(summary.analyzed_count, n);
    assert_eq!(summary.errors, 0);
    assert!(app.bus().wait_background_idle(Duration::from_secs(30)));
    assert_eq!(
        *changes.lock().unwrap(),
        [(id.clone(), vec!["bpm".to_string()])]
    );
    assert_eq!(app.gate().snapshot(), (0, false));
}

#[test]
fn a_prepared_analysis_dropped_unrun_releases_the_guard() {
    let (app, _s) = scanned();
    let (id, _, _) = an_album(&app);
    let run = app.ops().analyze().prepare(&id, AnalysisKind::Bpm).unwrap();
    assert!(matches!(
        app.ops().database().restore_allowed(),
        Err(OpError::Busy { .. })
    ));
    drop(run);
    assert!(app.ops().database().restore_allowed().is_ok());
    // The refusals are the prepare step's.
    assert!(matches!(
        app.ops()
            .analyze()
            .prepare("0000000000000000", AnalysisKind::Bpm),
        Err(OpError::NotFound { .. })
    ));
}
