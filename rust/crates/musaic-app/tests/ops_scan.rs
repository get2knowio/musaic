//! The `scan` operations with no HTTP in scope: a scan of the synthetic library
//! followed through typed events, replay for a second subscriber, cancel, and
//! the order of the guard and the library check.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use musaic_app::App;
use musaic_app::ops::OpError;
use musaic_app::ops::scan::{CancelOutcome, ScanEvent, ScanStatus};
use musaic_events::{Event, Subscriber, Topic};
use musaic_services::gate::Busy;

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

/// The scan thread drops its guard just after the terminal event.
fn guard_released(app: &App) {
    eventually("the work guard released", || {
        app.gate().snapshot() == (0, false)
    });
}

/// An app whose scans park in each album while `park` is set.
fn parking_app() -> (App, common::Scratch, Arc<AtomicBool>) {
    let park = Arc::new(AtomicBool::new(true));
    let p2 = Arc::clone(&park);
    let (app, s) = common::app_quiet_pool(|b| {
        b.on_scan_album(Arc::new(move |_album, _fresh| {
            let deadline = Instant::now() + Duration::from_secs(30);
            while p2.load(Ordering::SeqCst) {
                assert!(Instant::now() < deadline, "the scan was never released");
                std::thread::sleep(Duration::from_millis(2));
            }
        }))
    });
    (app, s, park)
}

fn terminals(events: &[ScanEvent]) -> usize {
    events.iter().filter(|e| e.is_terminal()).count()
}

#[test]
fn a_scan_yields_start_then_albums_then_exactly_one_terminal_event() {
    let (app, s) = common::app_quiet_pool(|b| b);
    common::configure(&app, &s);
    let events: Vec<ScanEvent> = app.ops().scan().subscribe(false).unwrap().collect();
    assert!(
        matches!(events.first(), Some(ScanEvent::Start { .. })),
        "{events:?}"
    );
    assert!(events.iter().any(|e| matches!(e, ScanEvent::Album { .. })));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ScanEvent::Progress { .. }))
    );
    assert_eq!(terminals(&events), 1);
    assert!(
        matches!(events.last(), Some(ScanEvent::Done(d)) if d.count > 1),
        "the terminal event is last: {:?}",
        events.last()
    );
    assert_eq!(app.ops().scan().status().status, ScanStatus::Done);
    guard_released(&app);
    // The scan saved its snapshot: the library lists its albums now.
    let list = app.ops().library().list_albums(false).unwrap();
    assert!(list.count() > 1);
}

#[test]
fn a_second_subscriber_replays_the_running_scan_from_its_first_event() {
    let (app, s, park) = parking_app();
    common::configure(&app, &s);
    let first = app.ops().scan().subscribe(false).unwrap();
    eventually("the first album", || app.ops().scan().status().scanned >= 1);
    // A scan runs: this one joins it rather than starting another.
    let second = app.ops().scan().subscribe(false).unwrap();
    park.store(false, Ordering::SeqCst);
    let a: Vec<ScanEvent> = first.collect();
    let b: Vec<ScanEvent> = second.collect();
    assert_eq!(a, b, "both see the same scan, from its start");
    assert!(matches!(b.first(), Some(ScanEvent::Start { .. })));
    assert_eq!(terminals(&b), 1);
    guard_released(&app);
}

#[test]
fn cancel_during_a_scan_yields_cancelling_then_cancelled_and_saves_nothing() {
    let (app, s, park) = parking_app();
    common::configure(&app, &s);
    let sub = app.ops().scan().subscribe(false).unwrap();
    eventually("the first album", || app.ops().scan().status().scanned >= 1);
    let ends = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let e2 = Arc::clone(&ends);
    app.bus().register(
        Topic::Event,
        Subscriber::from_fn(move |ev| {
            if let Event::LibraryScanStopped { reason, .. } = ev {
                e2.lock().unwrap().push(reason.clone());
            }
            Ok(())
        }),
    );
    assert!(matches!(
        app.ops().scan().cancel(),
        CancelOutcome::Requested(_)
    ));
    assert_eq!(app.ops().scan().status().status_str(), "cancelling");
    park.store(false, Ordering::SeqCst);
    let events: Vec<ScanEvent> = sub.collect();
    assert_eq!(terminals(&events), 1);
    assert!(
        matches!(events.last(), Some(ScanEvent::Cancelled(_))),
        "{events:?}"
    );
    let at = |f: fn(&ScanEvent) -> bool| events.iter().position(f).expect("event present");
    assert!(
        at(|e| matches!(e, ScanEvent::Cancelling { .. })) < events.len() - 1,
        "cancelling comes before the terminal event"
    );
    assert_eq!(app.ops().scan().status().status, ScanStatus::Cancelled);
    guard_released(&app);
    eventually("the stop published", || !ends.lock().unwrap().is_empty());
    assert_eq!(*ends.lock().unwrap(), ["cancelled"]);
    // No snapshot was written.
    let list = app.ops().library().list_albums(false).unwrap();
    assert_eq!(list.count(), 0);
    assert_eq!(list.scanned_at, "");
}

#[test]
fn cancel_works_while_a_restore_guard_is_held() {
    let (app, _s) = common::app();
    let _restore = common::hold_restore(&app);
    assert!(matches!(
        app.ops().scan().cancel(),
        CancelOutcome::NotRunning(_)
    ));
}

#[test]
fn subscribe_without_a_library_is_no_library() {
    let (app, _s) = common::app();
    let err = app.ops().scan().subscribe(false).err().unwrap();
    assert!(matches!(err, OpError::NoLibrary { .. }), "{err:?}");
}

#[test]
fn a_missing_library_folder_is_no_library() {
    let (app, s) = common::app();
    let lib = common::configure(&app, &s);
    std::fs::remove_dir_all(lib).unwrap();
    let err = app.ops().scan().subscribe(false).err().unwrap();
    assert!(matches!(err, OpError::NoLibrary { .. }), "{err:?}");
}

#[test]
fn scan_operations_during_a_restore_are_busy_before_the_library_is_looked_at() {
    // No library is configured: Busy is still the answer, as the stream route's.
    let (app, _s) = common::app();
    let _restore = common::hold_restore(&app);
    let busy = |e: OpError| {
        matches!(
            e,
            OpError::Busy {
                reason: Busy::Restoring,
                ..
            }
        )
    };
    assert!(busy(app.ops().scan().subscribe(false).err().unwrap()));
    assert!(busy(app.ops().scan().start(false).unwrap_err()));
    assert!(busy(app.ops().scan().rescan().unwrap_err()));
}

#[test]
fn rescan_checks_the_library_after_the_guard_and_starts_a_forced_scan() {
    let (app, s) = common::app_quiet_pool(|b| b);
    let err = app.ops().scan().rescan().unwrap_err();
    assert!(matches!(err, OpError::NoLibrary { .. }), "{err:?}");
    common::configure(&app, &s);
    let started = app.ops().scan().rescan().unwrap();
    assert_eq!(
        Some(started.library_dir),
        app.ops().library().config().library_dir
    );
    let events: Vec<ScanEvent> = app.ops().scan().subscribe(false).unwrap().collect();
    assert!(
        matches!(events.first(), Some(ScanEvent::Start { force: true, .. })),
        "{events:?}"
    );
    guard_released(&app);
}
