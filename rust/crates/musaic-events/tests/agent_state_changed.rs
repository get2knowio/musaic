//! `AgentStateChanged` reports a change of agent state; like
//! `ChecksRerunFinished` it must never cause a rerun, wake the check sweep or
//! notify findings, while the relay still carries it to the UI (T018).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use musaic_events::{AgentWhat, Bus, Event, Relay, SideEffects, SubscriberError, register_all};

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
fn an_agent_state_change_starts_nothing_and_reaches_the_relay() {
    let bus = Bus::new();
    let relay = Relay::new();
    let fx = Arc::new(Counting::default());
    register_all(&bus, fx.clone(), Some(&relay));
    let conn = relay.connect();

    for what in [
        AgentWhat::Pairings,
        AgentWhat::Approvals,
        AgentWhat::Activity,
        AgentWhat::Changes,
    ] {
        bus.publish(&Event::AgentStateChanged {
            what,
            ids: vec!["p_1".into()],
        });
    }
    assert!(bus.wait_background_idle(Duration::from_secs(60)));
    assert_eq!(fx.calls.load(Ordering::SeqCst), 0, "no side effect ran");
    assert_eq!(
        fx.wakes.load(Ordering::SeqCst),
        0,
        "the sweep was not woken"
    );
    for want in ["pairings", "approvals", "activity", "changes"] {
        let msg = conn.recv_timeout(Duration::from_secs(30)).expect("relayed");
        assert_eq!(&*msg.name, "AgentStateChanged");
        assert_eq!(
            &*msg.data,
            format!(r#"{{"what": "{want}", "ids": ["p_1"]}}"#)
        );
    }
    assert!(conn.try_recv().is_none());

    bus.publish(&Event::LibraryScanCompleted { album_count: 1 });
    assert!(bus.wait_background_idle(Duration::from_secs(60)));
    assert_eq!(
        fx.wakes.load(Ordering::SeqCst),
        1,
        "other events still wake it"
    );
}

#[test]
fn the_event_round_trips_through_its_class_name() {
    use musaic_events::{ArgValue, EventKind};
    let ev = Event::from_fields(
        "AgentStateChanged",
        [
            ("what", ArgValue::Str("approvals".into())),
            ("ids", ArgValue::StrList(vec!["ap_1".into()])),
        ],
    )
    .unwrap();
    assert_eq!(ev.kind(), EventKind::AgentStateChanged);
    assert_eq!(ev.album_id(), None);
    assert!(
        Event::from_fields(
            "AgentStateChanged",
            [("what", ArgValue::Str("nope".into()))]
        )
        .is_err()
    );
    assert!(!EventKind::AgentStateChanged.is_album_scoped());
}
