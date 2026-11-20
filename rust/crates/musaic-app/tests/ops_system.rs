//! The `system` operations with no HTTP in scope: health, the pool's state and
//! feed, and the library events feed.

mod common;

use std::time::Duration;

use musaic_app::ops::system::{Health, PoolState};
use musaic_events::Event;
use musaic_services::fingerprint_pool::PoolStatus;

const SHORT: Duration = Duration::from_millis(500);

fn running(completed: usize) -> PoolState {
    PoolState {
        status: PoolStatus::Running,
        library_dir: Some("/lib".into()),
        total: 10,
        completed,
        ..PoolState::default()
    }
}

#[test]
fn health_and_pool_status_are_typed_values() {
    let (app, _s) = common::app();
    assert_eq!(app.ops().system().health(), Health::Ok);
    assert_eq!(app.ops().system().pool_status(), PoolState::default());
}

#[test]
fn pool_feed_yields_the_current_state_then_a_value_after_a_change() {
    let (app, _s) = common::app();
    let mut feed = app.ops().system().pool_feed();
    assert_eq!(feed.next(SHORT), Some(PoolState::default()));
    assert_eq!(
        feed.next(Duration::from_millis(40)),
        None,
        "nothing changed"
    );
    app.pool().set_state(running(3));
    app.pool().changed().set();
    assert_eq!(feed.next(SHORT), Some(running(3)));
}

#[test]
fn pool_feed_coalesces_changes_between_reads() {
    let (app, _s) = common::app();
    let mut feed = app.ops().system().pool_feed();
    feed.next(SHORT);
    for n in 1..=3 {
        app.pool().set_state(running(n));
        app.pool().changed().set();
    }
    assert_eq!(feed.next(SHORT), Some(running(3)));
    assert_eq!(feed.next(Duration::from_millis(40)), None);
}

#[test]
fn library_events_yields_a_typed_event_after_a_bus_publish() {
    let (app, _s) = common::app();
    let events = app.ops().system().library_events();
    app.bus().publish(&Event::AlbumCoverChanged {
        album_id: "abc".into(),
    });
    let e = events.next(SHORT).expect("an event");
    assert_eq!(e.name(), "AlbumCoverChanged");
    assert!(e.payload_json().contains("abc"));
}

#[test]
fn a_slow_library_events_reader_does_not_block_another() {
    let (app, _s) = common::app();
    let slow = app.ops().system().library_events();
    let fast = app.ops().system().library_events();
    for i in 0..300 {
        app.bus().publish(&Event::AlbumCoverChanged {
            album_id: format!("a{i}"),
        });
        assert!(fast.next(SHORT).is_some(), "fast reader sees event {i}");
    }
    assert!(slow.dropped() > 0, "the unread queue dropped its oldest");
    assert!(slow.next(SHORT).is_some());
}

#[test]
fn every_feed_ends_after_shutdown() {
    let (app, _s) = common::app();
    let mut pool = app.ops().system().pool_feed();
    let events = app.ops().system().library_events();
    assert!(pool.next(SHORT).is_some());
    app.begin_shutdown();
    assert_eq!(pool.next(SHORT), None);
    assert!(events.next(SHORT).is_none());
}

#[test]
fn a_blocked_next_wakes_on_a_change_from_another_thread() {
    let (app, _s) = common::app();
    let mut feed = app.ops().system().pool_feed();
    feed.next(SHORT);
    let pool = app.clone();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        pool.pool().set_state(running(7));
        pool.pool().changed().set();
    });
    let start = std::time::Instant::now();
    assert_eq!(feed.next(Duration::from_secs(20)), Some(running(7)));
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "woken by the change, not the timeout"
    );
    t.join().unwrap();
}
