//! A merge of several album rows into one (a multi-disc set gathered into
//! one folder) is reported as one `AlbumRenamed(old, survivor)` per merged
//! row, under a bulk run, and no event type of its own. This checks that
//! the whole catalog handles several old ids naming one new id: each dead
//! id's thumbnail and resolver memo are dropped, the survivor's memo is
//! dropped, nothing reruns per event inside the bulk run, one library-wide
//! rerun follows the batch, and the relay carries every event to the UI
//! (which drops the dead ids' caches and refetches the survivor's).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use musaic_events::{Bus, Event, Relay, SideEffects, SubscriberError, register_all, run_bulk};

#[derive(Default)]
struct Recording {
    calls: Mutex<Vec<String>>,
}

impl Recording {
    fn hit(&self, s: String) -> Result<(), SubscriberError> {
        self.calls.lock().unwrap().push(s);
        Ok(())
    }

    fn calls(&self) -> Vec<String> {
        let mut v = self.calls.lock().unwrap().clone();
        v.sort();
        v
    }
}

impl SideEffects for Recording {
    fn library_dir(&self) -> Option<String> {
        Some("/lib".into())
    }
    fn resolver_invalidate(&self, id: &str) -> Result<(), SubscriberError> {
        self.hit(format!("resolver {id}"))
    }
    fn delete_thumb(&self, id: &str) -> Result<(), SubscriberError> {
        self.hit(format!("thumb {id}"))
    }
    fn run_post_scan_checks(&self, _: &str, ids: Option<&[String]>) -> Result<(), SubscriberError> {
        self.hit(format!("rerun {ids:?}"))
    }
    fn run_check_scoped(
        &self,
        check: &str,
        _: &str,
        ids: &[String],
    ) -> Result<(), SubscriberError> {
        self.hit(format!("check {check} {ids:?}"))
    }
    fn notify_findings_changed(&self) -> Result<(), SubscriberError> {
        Ok(())
    }
}

fn renamed(old: &str, new: &str) -> Event {
    Event::AlbumRenamed {
        old_album_id: old.into(),
        new_album_id: new.into(),
    }
}

#[test]
fn several_old_ids_renamed_to_one_survivor_inside_a_bulk_run() {
    let bus = Bus::new();
    let relay = Relay::new();
    let fx = Arc::new(Recording::default());
    register_all(&bus, fx.clone(), Some(&relay));
    let conn = relay.connect();

    run_bulk(&bus, true, || {
        bus.publish(&renamed("x1", "t"));
        bus.publish(&renamed("x2", "t"));
    });
    assert!(bus.wait_background_idle(Duration::from_secs(60)));

    assert_eq!(
        fx.calls(),
        [
            "rerun None",
            "resolver t",
            "resolver t",
            "resolver x1",
            "resolver x2",
            "thumb x1",
            "thumb x2",
        ],
        "the dead ids' thumbnails and memos dropped, the survivor's memo dropped, \
         one library-wide rerun after the batch and none per event"
    );
    let mut relayed = Vec::new();
    while let Some(m) = conn.recv_timeout(Duration::from_secs(5)) {
        relayed.push(format!("{} {}", m.name, m.data));
        if &*m.name == "BulkBatchCompleted" {
            break;
        }
    }
    assert_eq!(
        relayed,
        [
            r#"AlbumRenamed {"old_album_id": "x1", "new_album_id": "t"}"#,
            r#"AlbumRenamed {"old_album_id": "x2", "new_album_id": "t"}"#,
            "BulkBatchCompleted {}",
        ]
    );
}

#[test]
fn outside_a_bulk_run_each_event_reruns_the_survivor() {
    let bus = Bus::new();
    let fx = Arc::new(Recording::default());
    register_all(&bus, fx.clone(), None);
    bus.publish(&renamed("x1", "t"));
    bus.publish(&renamed("x2", "t"));
    assert!(bus.wait_background_idle(Duration::from_secs(60)));
    let calls = fx.calls();
    // Harmless but repeated: a caller merging several rows runs under a
    // bulk run so the survivor is checked once.
    assert_eq!(
        calls.iter().filter(|c| *c == "rerun Some([\"t\"])").count(),
        2
    );
    assert_eq!(
        calls
            .iter()
            .filter(|c| *c == "check filename-mismatch [\"t\"]")
            .count(),
        2
    );
    assert!(!calls.iter().any(|c| c.contains("x1\"]") || c == "thumb t"));
}
