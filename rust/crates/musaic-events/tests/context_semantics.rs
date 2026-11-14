//! Rust-specific mechanics behind the Python app contracts that the golden
//! replays cannot reach on their own: panics as "raise", per-task flag
//! isolation on one thread, explicit inheritance, cancellation.

mod common;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use common::{block_on, yield_once};
use musaic_events::{
    Bus, Event, FailureKind, Subscriber, Topic, context, run_bulk, run_bulk_async,
    run_post_scan_guarded,
};

fn tags(id: &str) -> Event {
    Event::AlbumTagsChanged {
        album_id: id.into(),
        fields: vec![],
    }
}

/// The flag resets when the guarded body panics
/// (test_sync_flag_resets_even_if_check_raises, panic edition).
#[test]
fn flag_restored_after_panic() {
    let r = catch_unwind(|| run_post_scan_guarded(|| panic!("boom")));
    assert!(r.is_err());
    assert!(!context::in_post_scan_rerun());
    let r = catch_unwind(|| context::with_bulk_run(|| panic!("boom")));
    assert!(r.is_err());
    assert!(!context::in_bulk_run());
}

/// A panicking subscriber is isolated like a raising one; the failure
/// hook sees errors and panics from sync and background subscribers.
#[test]
fn panicking_subscribers_are_isolated_and_reported() {
    let bus = Bus::new();
    let failures = Arc::new(Mutex::new(Vec::new()));
    let f2 = failures.clone();
    bus.set_failure_hook(Some(Arc::new(
        move |name: &str, ev: &str, k: &FailureKind| {
            let kind = match k {
                FailureKind::Error(_) => "error",
                FailureKind::Panic(_) => "panic",
                FailureKind::SpawnFailed(_) => "spawn",
            };
            f2.lock().unwrap().push(format!("{name}:{ev}:{kind}"));
        },
    )));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s2 = seen.clone();
    bus.register(
        Topic::Event,
        Subscriber::from_fn(|_| panic!("intentional"))
            .priority(100)
            .name("p"),
    );
    bus.register(
        Topic::Event,
        Subscriber::from_fn(|_| Err("e".into()))
            .priority(150)
            .name("e"),
    );
    bus.register(
        Topic::Event,
        Subscriber::from_fn(|_| panic!("bg"))
            .background(true)
            .name("bg"),
    );
    bus.register(
        Topic::Event,
        Subscriber::from_fn(move |_| {
            s2.lock().unwrap().push("ok");
            Ok(())
        })
        .priority(300),
    );
    bus.publish(&tags("a"));
    assert!(bus.wait_background_idle(std::time::Duration::from_secs(5)));
    assert_eq!(*seen.lock().unwrap(), vec!["ok"]);
    let mut f = failures.lock().unwrap().clone();
    f.sort();
    assert_eq!(
        f,
        vec![
            "bg:AlbumTagsChanged:panic",
            "e:AlbumTagsChanged:error",
            "p:AlbumTagsChanged:panic"
        ]
    );
}

/// LOAD-BEARING async edition: two tasks interleaved on ONE
/// thread; the one holding the flag across an await must not leak it to
/// the other (a bare thread-local would).
#[test]
fn scoped_flag_does_not_leak_between_tasks_on_one_thread() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s1 = seen.clone();
    let s2 = seen.clone();
    let mut a = Box::pin(context::scope_post_scan_rerun(async move {
        s1.lock()
            .unwrap()
            .push(("a-before", context::in_post_scan_rerun()));
        yield_once().await;
        s1.lock()
            .unwrap()
            .push(("a-after", context::in_post_scan_rerun()));
    }));
    let mut b = Box::pin(async move {
        s2.lock()
            .unwrap()
            .push(("b-before", context::in_post_scan_rerun()));
        yield_once().await;
        s2.lock()
            .unwrap()
            .push(("b-after", context::in_post_scan_rerun()));
    });
    let mut cx = Context::from_waker(Waker::noop());
    assert!(a.as_mut().poll(&mut cx).is_pending());
    assert!(b.as_mut().poll(&mut cx).is_pending());
    assert!(
        !context::in_post_scan_rerun(),
        "flag leaked out of the poll"
    );
    assert!(a.as_mut().poll(&mut cx).is_ready());
    assert!(b.as_mut().poll(&mut cx).is_ready());
    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            ("a-before", true),
            ("b-before", false),
            ("a-after", true),
            ("b-after", false)
        ]
    );
}

/// `inherit` carries the caller's flags into a future polled elsewhere
/// (the `tokio::spawn` case); without it a new thread sees the default.
#[test]
fn inherit_carries_flags_to_another_thread() {
    let (inherited, plain) = context::with_post_scan_rerun(|| {
        let fut = context::inherit(async { context::in_post_scan_rerun() });
        let plain = async { context::in_post_scan_rerun() };
        let h = std::thread::spawn(move || (block_on(fut), block_on(plain)));
        h.join().unwrap()
    });
    assert!(inherited);
    assert!(!plain);
}

/// Teardown: the batch event is published after the flag is reset,
/// even if the batch panics (Python's `finally`).
#[test]
fn run_bulk_publishes_after_reset_even_on_panic() {
    let bus = Bus::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s2 = seen.clone();
    bus.register(
        Topic::Event,
        Subscriber::from_fn(move |e| {
            s2.lock().unwrap().push((e.name(), context::in_bulk_run()));
            Ok(())
        }),
    );
    let r = catch_unwind(AssertUnwindSafe(|| {
        run_bulk(&bus, true, || {
            bus.publish(&tags("a"));
            panic!("mid-batch");
        })
    }));
    assert!(r.is_err());
    assert_eq!(
        *seen.lock().unwrap(),
        vec![("AlbumTagsChanged", true), ("BulkBatchCompleted", false)]
    );
}

/// Async bulk: flag set while polled, batch event on completion and
/// on cancellation (future dropped mid-way = client disconnect).
#[test]
fn run_bulk_async_publishes_on_completion_and_cancellation() {
    let bus = Bus::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s2 = seen.clone();
    bus.register(
        Topic::Event,
        Subscriber::from_fn(move |e| {
            s2.lock().unwrap().push((e.name(), context::in_bulk_run()));
            Ok(())
        }),
    );
    block_on(run_bulk_async(&bus, true, async {
        bus.publish(&tags("a"));
        yield_once().await;
        bus.publish(&tags("b"));
    }));
    {
        let mut fut = Box::pin(run_bulk_async(&bus, true, async {
            bus.publish(&tags("c"));
            std::future::pending::<()>().await;
        }));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(matches!(fut.as_mut().poll(&mut cx), Poll::Pending));
        assert!(!context::in_bulk_run());
    } // dropped: cancelled
    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            ("AlbumTagsChanged", true),
            ("AlbumTagsChanged", true),
            ("BulkBatchCompleted", false),
            ("AlbumTagsChanged", true),
            ("BulkBatchCompleted", false),
        ]
    );
}

/// A dropped connection unregisters itself.
#[test]
fn dropping_connection_unregisters() {
    let relay = musaic_events::Relay::new();
    let a = relay.connect();
    let b = relay.connect();
    assert_eq!(relay.subscriber_count(), 2);
    drop(a);
    assert_eq!(relay.subscriber_count(), 1);
    drop(b);
    assert_eq!(relay.subscriber_count(), 0);
}
