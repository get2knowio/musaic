//! `ChecksRerunFinished` reports a rerun; it must never cause one. Every
//! subscriber the catalog registers for an event runs for it, so this is
//! checked against the whole catalog: no rerun, no check sweep wake, no
//! findings notification (the runs notified the checks stream themselves),
//! while the relay still carries it to the UI.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use musaic_events::{Bus, Event, Relay, SideEffects, SubscriberError, register_all};

#[derive(Default)]
struct Counting {
    calls: AtomicUsize,
    wakes: AtomicUsize,
}

impl Counting {
    fn hit(&self) -> Result<(), SubscriberError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

impl SideEffects for Counting {
    fn library_dir(&self) -> Option<String> {
        Some("/lib".into())
    }
    fn resolver_invalidate(&self, _: &str) -> Result<(), SubscriberError> {
        self.hit()
    }
    fn delete_thumb(&self, _: &str) -> Result<(), SubscriberError> {
        self.hit()
    }
    fn run_post_scan_checks(&self, _: &str, _: Option<&[String]>) -> Result<(), SubscriberError> {
        self.hit()
    }
    fn run_check_scoped(&self, _: &str, _: &str, _: &[String]) -> Result<(), SubscriberError> {
        self.hit()
    }
    fn notify_findings_changed(&self) -> Result<(), SubscriberError> {
        self.hit()
    }
    fn check_sweep_wake(&self, _: &Event) -> Result<(), SubscriberError> {
        self.wakes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[test]
fn a_finished_rerun_starts_nothing_and_still_reaches_the_relay() {
    let bus = Bus::new();
    let relay = Relay::new();
    let fx = Arc::new(Counting::default());
    register_all(&bus, fx.clone(), Some(&relay));
    let conn = relay.connect();

    let ev = Event::ChecksRerunFinished {
        checks: vec!["track-gaps".into()],
        failed: vec![],
        album_ids: vec!["a1".into()],
    };
    bus.publish(&ev);
    assert!(bus.wait_background_idle(Duration::from_secs(60)));

    assert_eq!(fx.calls.load(Ordering::SeqCst), 0, "no side effect ran");
    assert_eq!(
        fx.wakes.load(Ordering::SeqCst),
        0,
        "the sweep was not woken"
    );
    let msg = conn.recv_timeout(Duration::from_secs(30)).expect("relayed");
    assert_eq!(&*msg.name, "ChecksRerunFinished");
    assert_eq!(
        &*msg.data,
        r#"{"checks": ["track-gaps"], "failed": [], "album_ids": ["a1"]}"#
    );
    assert!(conn.try_recv().is_none(), "relayed once");

    // The same catalog does wake the sweep for any other event.
    bus.publish(&Event::LibraryScanCompleted { album_count: 1 });
    assert!(bus.wait_background_idle(Duration::from_secs(60)));
    assert_eq!(fx.wakes.load(Ordering::SeqCst), 1);
}
