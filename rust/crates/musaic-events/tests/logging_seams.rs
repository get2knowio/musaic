//! Log lines the Python app writes when a `library_events` subscriber fails
//! (the lower crates log through `musaic-log`): each catalog subscriber's own
//! `_log.exception(...)` on `musaic.services.event_subscribers`
//! (services/event_subscribers.py:64-261; the subscriber swallows the
//! error, so the bus sees none), and the bus's
//! `_log.exception("subscriber %s failed for event %s (continuing)", ...)`
//! on `musaic.library_events` (library_events.py:339) for anything else.
//!
//! Deliberate difference, said once: the Python app's `log.exception(msg)`
//! (ERROR plus a traceback) is ERROR `msg: <error text>` here.
//!
//! Not ported because unreachable: the producers' "library_events publish
//! failed for ..." lines (apply.py:253 and a dozen others). The Python app
//! guarded each `publish(...)` call; `Bus::publish` returns nothing and
//! catches every subscriber error and panic, so no publish can fail.

use std::sync::Arc;

use musaic_events::{Bus, Event, SideEffects, Subscriber, SubscriberError, Topic, register_all};
use musaic_log::{Level, Record, capture};

/// Every side effect fails.
struct Failing;

impl SideEffects for Failing {
    fn library_dir(&self) -> Option<String> {
        Some("/lib".into())
    }
    fn resolver_invalidate(&self, _: &str) -> Result<(), SubscriberError> {
        Err("resolver down (test)".into())
    }
    fn delete_thumb(&self, _: &str) -> Result<(), SubscriberError> {
        Err("thumbs down (test)".into())
    }
    fn run_post_scan_checks(&self, _: &str, _: Option<&[String]>) -> Result<(), SubscriberError> {
        Err("checks down (test)".into())
    }
    fn run_check_scoped(&self, _: &str, _: &str, _: &[String]) -> Result<(), SubscriberError> {
        Err("checks down (test)".into())
    }
    fn notify_findings_changed(&self) -> Result<(), SubscriberError> {
        Err("notify down (test)".into())
    }
}

/// The foreground lines of one publish, in order (background subscribers
/// log on their own threads, which `capture` does not see).
fn publish(ev: Event) -> Vec<(Level, String, String)> {
    let bus = Bus::new();
    register_all(&bus, Arc::new(Failing), None);
    let ((), lines) = capture(|| bus.publish(&ev));
    assert!(bus.wait_background_idle(std::time::Duration::from_secs(30)));
    lines
        .into_iter()
        .map(
            |Record {
                 level,
                 target,
                 message,
             }| (level, target, message),
        )
        .collect()
}

const SUBS: &str = "musaic.services.event_subscribers";

fn err(msg: &str) -> (Level, String, String) {
    (Level::Error, SUBS.to_string(), msg.to_string())
}

// the Python app: event_subscribers.py:64 `_resolver_invalidate failed for %s`,
// :89 `_thumb_drop failed for %s`, :261 `_findings_runner_notify failed
// (continuing)`. Each failure is logged and dispatch goes on.
#[test]
fn failing_inline_subscribers_are_logged() {
    assert_eq!(
        publish(Event::AlbumCoverChanged {
            album_id: "a1".into()
        }),
        // Order: at equal priority the concrete class's subscribers
        // run before the AlbumScopedEvent ones.
        vec![
            err("_thumb_drop failed for a1: thumbs down (test)"),
            err("_resolver_invalidate failed for a1: resolver down (test)"),
            err("_findings_runner_notify failed (continuing): notify down (test)"),
        ]
    );
}

// the Python app: event_subscribers.py:75 `_resolver_invalidate_for_rename failed
// (%s → %s)` and :98 `_thumb_drop_for_rename failed for %s`.
#[test]
fn failing_rename_subscribers_are_logged() {
    assert_eq!(
        publish(Event::AlbumRenamed {
            old_album_id: "old".into(),
            new_album_id: "new".into(),
        }),
        vec![
            err("_resolver_invalidate_for_rename failed (old \u{2192} new): resolver down (test)"),
            err("_thumb_drop_for_rename failed for old: thumbs down (test)"),
            err("_findings_runner_notify failed (continuing): notify down (test)"),
        ]
    );
}

// the Python app: event_subscribers.py:150 `_post_scan_rerun failed for %s
// (continuing)`, :176 `_post_scan_rerun_for_rename failed (%s → %s)`, :202
// `_post_scan_rerun_library_wide failed (continuing)`, :243
// `_filename_mismatch_rerun failed for %s (continuing)`. Those subscribers
// run in the background; the texts are checked on the message function the
// subscriber logs with.
#[test]
fn background_subscriber_messages() {
    use musaic_events::catalog::{Handler, failure_message};
    let renamed = Event::AlbumRenamed {
        old_album_id: "old".into(),
        new_album_id: "new".into(),
    };
    let tags = Event::AlbumTagsChanged {
        album_id: "a1".into(),
        fields: vec![],
    };
    assert_eq!(
        failure_message(Handler::PostScanRerun, &tags),
        "_post_scan_rerun failed for a1 (continuing)"
    );
    assert_eq!(
        failure_message(Handler::PostScanRerunForRename, &renamed),
        "_post_scan_rerun_for_rename failed (old \u{2192} new)"
    );
    assert_eq!(
        failure_message(
            Handler::PostScanRerunLibraryWide,
            &Event::BulkBatchCompleted
        ),
        "_post_scan_rerun_library_wide failed (continuing)"
    );
    assert_eq!(
        failure_message(Handler::FilenameMismatchRerun, &renamed),
        "_filename_mismatch_rerun failed for new (continuing)"
    );
}

// the Python app: library_events.py:339 `_log.exception("subscriber %s failed for
// event %s (continuing)", reg.name, type(event).__name__)`. A
// subscriber that raises (here: returns Err, or panics) is logged by the
// bus, and dispatch continues.
#[test]
fn failing_subscriber_is_logged_by_the_bus() {
    let bus = Bus::new();
    bus.register(
        Topic::Event,
        Subscriber::from_fn(|_| Err("intentional (test)".into()))
            .priority(100)
            .name("raiser"),
    );
    bus.register(
        Topic::Event,
        Subscriber::from_fn(|_| panic!("intentional panic (test)"))
            .priority(150)
            .name("panicker"),
    );
    let ((), lines) = capture(|| bus.publish(&Event::BulkBatchCompleted));
    let got: Vec<(Level, &str, &str)> = lines
        .iter()
        .map(|r| (r.level, r.target.as_str(), r.message.as_str()))
        .collect();
    assert_eq!(
        got,
        vec![
            (
                Level::Error,
                "musaic.library_events",
                "subscriber raiser failed for event BulkBatchCompleted (continuing): intentional (test)"
            ),
            (
                Level::Error,
                "musaic.library_events",
                "subscriber panicker failed for event BulkBatchCompleted (continuing): intentional panic (test)"
            ),
        ]
    );
}
