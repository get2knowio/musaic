//! Replays the scenarios recorded from the Python app
//! (testdata/golden/events/*.json)
//! and compares traces.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use common::{Fx, Interp, Trace, canon, ev, event_from, golden, replay_catalog};
use musaic_events::{
    ArgValue, Bus, Event, EventKind, FromFieldsError, Relay, RelayMessage, SideEffects, Topic,
    ready_frame, register_all, sse_frame,
};
use serde_json::{Value, json};

fn run_dispatch_scenario(name: &str) {
    let g = golden("dispatch.json");
    let sc = &g["scenarios"][name];
    assert!(sc.is_object(), "no scenario {name} in dispatch.json");
    let trace = Arc::new(Trace::default());
    let interp = Interp::new(Bus::new(), trace.clone());
    interp.steps(sc["script"].as_array().unwrap());
    interp.wait();
    interp.bus.reset();
    assert_eq!(trace.to_value(), canon(&sc["trace"]), "scenario {name}");
}

macro_rules! dispatch_tests {
    ($($(#[$m:meta])* $name:ident),* $(,)?) => {
        $( $(#[$m])* #[test] fn $name() { run_dispatch_scenario(stringify!($name)); } )*
    };
}

// Dispatch order, MRO fan-out, dedupe, unregister, isolation.
dispatch_tests!(
    /// Priority order (test_subscribers_run_in_priority_order).
    priority_order,
    /// Ties keep registration order.
    same_priority_registration_order,
    /// (test_base_and_subclass_subscribers_interleave_by_priority).
    base_and_subclass_interleave,
    /// Ties across classes: subclass registrations before base ones (MRO order).
    mro_tie_order,
    /// (test_subscriber_for_base_class_catches_subclass_events).
    base_class_catches_subclasses,
    /// Subscribing to `Event` = all events.
    event_base_catches_all,
    /// A concrete-class subscriber sees only that class.
    exact_class_only,
    /// Negative and large priorities.
    negative_priority_and_large,
    /// Isolation (test_subscriber_exception_does_not_block_other_subscribers).
    raising_subscriber_isolated,
    /// Isolation for background subscribers.
    raising_background_subscriber,
    /// (test_register_dedupes_by_callback_identity).
    dedupe_same_callback,
    /// Dedupe is per topic: same callback on two matching topics runs twice.
    same_callback_two_topics,
    /// (test_unregister_removes_only_the_matching_callback) + unknown/wrong-topic no-ops.
    unregister,
    /// (test_no_subscribers_is_a_no_op).
    no_subscribers,
);

// Recursion guard, inheritance into background subscribers, no leaks.
dispatch_tests!(
    /// LOAD-BEARING (test_in_flight_rerun_short_circuits_nested_publish).
    nested_republish_guard_sync,
    /// LOAD-BEARING with a background rerun subscriber.
    nested_republish_guard_background,
    /// A set flag is visible to sync subscribers, then gone.
    flag_visible_to_sync_subscriber,
    /// LOAD-BEARING (test_background_subscriber_inherits_in_flight_flag), both flags.
    background_inherits_flags,
    /// The background snapshot is taken at publish time.
    background_snapshot_at_publish,
    /// LOAD-BEARING (test_concurrent_unrelated_rerun_is_not_suppressed).
    concurrent_unrelated_not_suppressed,
    /// A fresh thread starts from the default context.
    fresh_thread_does_not_inherit,
    /// (test_contextvar_resets_after_subscriber_raises).
    flag_resets_after_subscriber_raises,
    /// test_post_scan_guard::test_sync_sets_and_resets_flag.
    post_scan_sync_sets_and_resets,
    /// Test_post_scan_guard::test_sync_nested_call_short_circuits.
    post_scan_sync_nested_short_circuits,
    /// Test_post_scan_guard::test_sync_flag_resets_even_if_check_raises.
    post_scan_sync_resets_after_raise,
    /// Test_post_scan_guard::test_sync_publish_from_inside_check_does_not_spawn_subscriber_rerun.
    post_scan_sync_publish_inside_skips_bg_rerun,
    /// Nested-call guard when the caller is already inside a rerun.
    post_scan_skipped_when_flag_already_set,
    /// Test_post_scan_guard::test_async_sets_and_resets_flag.
    post_scan_async_sets_and_resets,
    /// Test_post_scan_guard::test_async_nested_call_short_circuits.
    post_scan_async_nested_short_circuits,
    /// Async reset after a raising check.
    post_scan_async_resets_after_raise,
    /// Async: background subscriber spawned inside the rerun inherits the flag.
    post_scan_async_publish_inside_skips_bg_rerun,
);

#[test]
fn every_golden_dispatch_scenario_is_replayed() {
    // Guards against the generator gaining a scenario no test replays.
    let g = golden("dispatch.json");
    let src = include_str!("golden_events.rs");
    for name in g["scenarios"].as_object().unwrap().keys() {
        assert!(
            src.contains(&format!("    {name},")),
            "scenario {name} not replayed"
        );
    }
}

// ------------------------------------------------------------------

/// Relay payload: event name + dataclass fields in order, tuples as
/// lists, `json.dumps` defaults (separators, ensure_ascii), SSE framing.
/// None-valued cases are unrepresentable in Rust and must be refused.
#[test]
fn payloads_match_python_bytes() {
    let g = golden("payloads.json");
    let mut checked = 0;
    for case in g["payloads"].as_array().unwrap() {
        let id = case["case"].as_str().unwrap();
        let parsed = event_from(&case["event"]);
        if case["rust_representable"].as_bool().unwrap() {
            let e = parsed.unwrap_or_else(|err| panic!("{id}: {err}"));
            assert_eq!(e.name(), case["name"].as_str().unwrap(), "{id}");
            assert_eq!(e.payload_json(), case["data"].as_str().unwrap(), "{id}");
            assert_eq!(
                sse_frame(e.name(), &e.payload_json()),
                case["frame"].as_str().unwrap(),
                "{id}"
            );
            assert_eq!(
                RelayMessage::from_event(&e).frame(),
                case["frame"].as_str().unwrap(),
                "{id}"
            );
            checked += 1;
        } else {
            assert!(
                matches!(parsed, Err(FromFieldsError::WrongType(_))),
                "{id}: {parsed:?}"
            );
        }
    }
    assert!(checked >= 20);
    assert_eq!(ready_frame(), g["ready_frame"].as_str().unwrap());
}

/// Deliberate additions the Python app does not have, all plain `Event`s
/// (not album-scoped), like `LibraryScanCompleted`; nothing in the catalog
/// subscribes to them by name, so only the `Event`-wide subscribers
/// (findings notify, check sweep wake, SSE relay) are registered for them:
///
/// * `LibraryScanStopped` (issue #193): a scan that ended without saving;
/// * `SettingsChanged` (issue #187): a settings write, with the names of the
///   settings written, for the background check sweep;
/// * `ChecksRerunFinished` (issue #204): a batch of check runs finished and
///   its findings are written. The findings notify and the sweep wake ignore
///   it, so only the relay acts on it.
///
/// Every other kind must match the recording.
const ADDED_KINDS: &[EventKind] = &[
    EventKind::LibraryScanStopped,
    EventKind::SettingsChanged,
    EventKind::ChecksRerunFinished,
];

/// Deliberate addition (issue #187): the background check sweep's
/// subscriber, on every event, between the findings notify and the relay.
/// Every other registration must match the recording.
const ADDED_SLOTS: &[&str] = &["_check_sweep_wake"];

/// `infos` without [`ADDED_SLOTS`], after checking the added slot is there
/// exactly where the catalog puts it for an `Event`-wide topic.
fn without_added_slots(mut v: Value, event_wide: bool) -> Value {
    let a = v.as_array_mut().unwrap();
    let before = a.len();
    a.retain(|r| !ADDED_SLOTS.contains(&r["name"].as_str().unwrap()));
    if event_wide {
        assert_eq!(before, a.len() + ADDED_SLOTS.len(), "added slot missing");
    }
    v
}

fn recorded_kinds() -> impl Iterator<Item = EventKind> {
    EventKind::ALL
        .into_iter()
        .filter(|k| !ADDED_KINDS.contains(k))
}

/// The class hierarchy (MRO) each event fans out through.
#[test]
fn mro_matches_python() {
    let g = golden("payloads.json");
    for kind in recorded_kinds() {
        let names: Vec<&str> = kind.mro().iter().map(|t| t.name()).collect();
        let want: Vec<&str> = g["mro"][kind.name()]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(names, want, "{kind:?}");
        assert_eq!(kind.is_album_scoped(), want.contains(&"AlbumScopedEvent"));
    }
    assert_eq!(
        g["mro"].as_object().unwrap().len(),
        EventKind::ALL.len() - ADDED_KINDS.len()
    );
    // The added kinds: absent from the recording, shaped like
    // LibraryScanCompleted.
    for added in ADDED_KINDS {
        assert!(g["mro"].get(added.name()).is_none());
        let names: Vec<&str> = added.mro().iter().map(|t| t.name()).collect();
        assert_eq!(names, [added.name(), "Event"]);
        assert!(!added.is_album_scoped());
    }
}

/// `ChecksRerunFinished`'s relay frame (check and album ids only) and its
/// keyword constructor.
#[test]
fn checks_rerun_finished_payload_and_fields() {
    let e = Event::ChecksRerunFinished {
        checks: vec!["filename-mismatch".into(), "track-gaps".into()],
        failed: vec!["track-gaps".into()],
        album_ids: vec![],
    };
    assert_eq!(
        RelayMessage::from_event(&e).frame(),
        "event: ChecksRerunFinished\ndata: {\"checks\": [\"filename-mismatch\", \"track-gaps\"], \"failed\": [\"track-gaps\"], \"album_ids\": []}\n\n"
    );
    assert_eq!(
        Event::from_fields("ChecksRerunFinished", []),
        Ok(Event::ChecksRerunFinished {
            checks: vec![],
            failed: vec![],
            album_ids: vec![]
        })
    );
}

/// `SettingsChanged`'s relay frame (setting names only) and its keyword
/// constructor.
#[test]
fn settings_changed_payload_and_fields() {
    let e = Event::SettingsChanged {
        keys: vec![
            "filename_template_single_artist".into(),
            "acoustid_api_key".into(),
        ],
    };
    assert_eq!(
        RelayMessage::from_event(&e).frame(),
        "event: SettingsChanged\ndata: {\"keys\": [\"filename_template_single_artist\", \"acoustid_api_key\"]}\n\n"
    );
    assert_eq!(
        Event::from_fields(
            "SettingsChanged",
            [(
                "keys",
                ArgValue::StrList(vec![
                    "filename_template_single_artist".into(),
                    "acoustid_api_key".into()
                ])
            )],
        ),
        Ok(e)
    );
    assert_eq!(
        Event::from_fields("SettingsChanged", []),
        Ok(Event::SettingsChanged { keys: vec![] })
    );
}

/// The added event's relay frame and its keyword constructor.
#[test]
fn library_scan_stopped_payload_and_fields() {
    let e = Event::LibraryScanStopped {
        reason: "cancelled".into(),
        album_count: 3,
    };
    assert_eq!(
        RelayMessage::from_event(&e).frame(),
        "event: LibraryScanStopped\ndata: {\"reason\": \"cancelled\", \"album_count\": 3}\n\n"
    );
    assert_eq!(
        Event::from_fields(
            "LibraryScanStopped",
            [
                ("reason", ArgValue::Str("cancelled".into())),
                ("album_count", ArgValue::Int(3)),
            ],
        ),
        Ok(e)
    );
    assert_eq!(
        Event::from_fields(
            "LibraryScanStopped",
            [("reason", ArgValue::Str("error".into()))]
        ),
        Err(FromFieldsError::Missing("album_count"))
    );
}

// ------------------------------------------------------------------ catalog

fn info_json(infos: Vec<musaic_events::RegistrationInfo>) -> Value {
    Value::Array(
        infos
            .into_iter()
            .map(|r| json!({"name": r.name, "priority": r.priority, "background": r.background}))
            .collect(),
    )
}

fn all_topics() -> Vec<Topic> {
    let mut t = vec![Topic::Event, Topic::AlbumScoped];
    t.extend(EventKind::ALL.map(Topic::Kind));
    t
}

/// The registry `register_all()` leaves behind and the effective
/// per-event dispatch plan (e.g. AlbumTagsChanged has no thumb drop;
/// AlbumCoverChanged's thumb drop runs before resolver invalidate).
#[test]
fn catalog_registry_and_dispatch_plan_match_python() {
    let g = golden("catalog.json");
    let bus = Bus::new();
    let relay = Relay::new();
    let fx: Arc<dyn SideEffects> = Arc::new(Fx {
        trace: Arc::new(Trace::default()),
        library_dir: Some("/lib".into()),
        real_post_scan: false,
        check_publishes: None,
    });
    register_all(&bus, fx, Some(&relay));
    let mut registry = BTreeMap::new();
    for t in all_topics() {
        let regs = bus.registrations(t);
        if !regs.is_empty() {
            registry.insert(
                t.name().to_owned(),
                without_added_slots(info_json(regs), t == Topic::Event),
            );
        }
    }
    assert_eq!(serde_json::to_value(registry).unwrap(), g["registry"]);
    for kind in recorded_kinds() {
        assert_eq!(
            without_added_slots(info_json(bus.dispatch_plan(kind)), true),
            g["dispatch_plan"][kind.name()],
            "{kind:?}"
        );
    }
    // The added kinds (ADDED_KINDS): only the Event-wide subscribers, i.e.
    // what BulkBatchCompleted gets minus its library-wide rerun. No check
    // rerun follows a scan that saved nothing, nor a settings write.
    let mut want = g["dispatch_plan"]["BulkBatchCompleted"].clone();
    want.as_array_mut()
        .unwrap()
        .retain(|r| !r["name"].as_str().unwrap().starts_with("_post_scan_rerun"));
    assert_eq!(want.as_array().unwrap().len(), 2, "{want}");
    for added in ADDED_KINDS {
        assert_eq!(
            without_added_slots(info_json(bus.dispatch_plan(*added)), true),
            want,
            "{added:?}"
        );
    }
    // Where the sweep's slot runs: after the findings notify, before the
    // relay, inline.
    let plan = info_json(bus.dispatch_plan(EventKind::AlbumTagsChanged));
    let names: Vec<&str> = plan
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    let at = names
        .iter()
        .position(|n| *n == "_check_sweep_wake")
        .unwrap();
    assert_eq!(names[at - 1], "_findings_runner_notify");
    assert_eq!(names[at + 1], musaic_events::relay::RELAY_NAME);
    assert_eq!(plan[at]["background"], json!(false));
}

fn run_catalog_scenario(name: &str) {
    let g = golden("catalog.json");
    let sc = &g["scenarios"][name];
    assert!(sc.is_object(), "no scenario {name} in catalog.json");
    let real = sc
        .get("real_post_scan")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let got = replay_catalog(sc, real, false);
    let mut want = canon(&sc["trace"]);
    if let Some(delta) = common::rerun_guard_delta_for_catalog_json(name) {
        // Deliberate change: the Python app's trace plus exactly these
        // background calls (bus-driven reruns now run check "a").
        want["background"].as_array_mut().unwrap().extend(delta);
        want = canon(&want);
    }
    assert_eq!(got, want, "scenario {name}");
}

macro_rules! catalog_tests {
    ($($(#[$m:meta])* $fn_name:ident => $scenario:literal),* $(,)?) => {
        $( $(#[$m])* #[test] fn $fn_name() { run_catalog_scenario($scenario); } )*
    };
}

catalog_tests!(
    /// Tags: invalidate, scoped rerun, notify; no thumb drop (test_tags_changed_does_not_drop_thumb).
    catalog_tags_changed => "each_event_AlbumTagsChanged",
    /// Cover: thumb drop + invalidate + rerun.
    catalog_cover_changed => "each_event_AlbumCoverChanged",
    /// Rename: both ids invalidated, old thumb, rerun + filename-mismatch on new id.
    catalog_renamed => "each_event_AlbumRenamed",
    /// Anchor set.
    catalog_anchor_set => "each_event_AlbumMbAnchorSet",
    /// Track fingerprinted.
    catalog_fingerprinted => "each_event_TrackFingerprinted",
    /// Delete drops the thumb.
    catalog_deleted => "each_event_AlbumDeleted",
    /// Marked missing.
    catalog_marked_missing => "each_event_AlbumMarkedMissing",
    /// Library-wide rerun on scan completion.
    catalog_scan_completed => "each_event_LibraryScanCompleted",
    /// Library-wide rerun on BulkBatchCompleted.
    catalog_bulk_completed => "each_event_BulkBatchCompleted",
    /// Old==new file-only rename.
    catalog_file_only_rename => "file_only_rename",
    /// Every rerun subscriber skips inside a post-scan rerun.
    catalog_in_post_scan_rerun_suppresses => "in_post_scan_rerun_suppresses_reruns",
    /// Every rerun subscriber skips inside a bulk run (incl. BulkBatchCompleted itself).
    catalog_in_bulk_run_suppresses => "in_bulk_run_suppresses_reruns",
    /// N applies under the bulk flag -> one library-wide rerun after reset.
    catalog_bulk_dispatcher_coalesces => "bulk_dispatcher_coalesces",
    /// Teardown: no BulkBatchCompleted when the dispatcher had nothing to do.
    catalog_bulk_dispatcher_no_publish => "bulk_dispatcher_no_publish",
    /// Nested bulk scope still resets before the batch event.
    catalog_bulk_nested_scope => "bulk_flag_inherited_by_worker_thread",
    /// No library configured -> no reruns.
    catalog_no_library => "no_library_configured",
    /// A failing target is swallowed; rename's second invalidate is skipped.
    catalog_target_raises => "target_raises_rename_old",
    /// Second register_all is a no-op.
    catalog_register_all_twice => "register_all_twice",
    /// Python app quirk: subscriber-set flag makes the guarded runner a no-op.
    catalog_composed_with_post_scan_guard => "composed_with_real_post_scan_guard",
);

#[test]
fn every_golden_catalog_scenario_is_replayed() {
    let g = golden("catalog.json");
    let src = include_str!("golden_events.rs");
    for name in g["scenarios"].as_object().unwrap().keys() {
        assert!(
            src.contains(&format!("=> \"{name}\"")),
            "scenario {name} not replayed"
        );
    }
}

// ------------------------------------------------------------------ relay

fn drain(c: &musaic_events::Connection) -> Value {
    let mut out = vec![];
    while let Some(m) = c.try_recv() {
        out.push(json!([&*m.name, &*m.data]));
    }
    Value::Array(out)
}

/// Publish 300 into a 256 queue: the newest 256 remain, in order.
#[test]
fn relay_overflow_drops_oldest() {
    let g = golden("relay.json");
    assert_eq!(
        g["queue_maxsize"].as_u64().unwrap() as usize,
        musaic_events::QUEUE_MAXSIZE
    );
    let bus = Bus::new();
    let relay = Relay::new();
    relay.register_on(&bus);
    let conn = relay.connect();
    let case = &g["overflow_300_into_256"];
    for i in 0..case["published"].as_u64().unwrap() {
        bus.publish(&ev(&json!({"class": "AlbumTagsChanged", "args": {"album_id": format!("a{i:03}"), "fields": ["n"]}})));
    }
    assert_eq!(conn.dropped(), 300 - 256);
    assert_eq!(drain(&conn), case["remaining"]);
}

/// `_safe_put` at small capacities (test_safe_put_drops_oldest_when_queue_is_full).
#[test]
fn relay_safe_put_cases() {
    let g = golden("relay.json");
    let relay = Relay::new();
    for case in g["safe_put_cases"].as_array().unwrap() {
        let conn = relay.connect_with_capacity(case["capacity"].as_u64().unwrap() as usize);
        for i in 0..case["prefill"].as_u64().unwrap() {
            conn.push(RelayMessage::new(&format!("old{i}"), "{}"));
        }
        for i in 0..case["put"].as_u64().unwrap() {
            conn.push(RelayMessage::new(
                &format!("new{i}"),
                &format!("{{\"i\": {i}}}"),
            ));
        }
        assert_eq!(drain(&conn), case["remaining"], "{case}");
    }
}

/// Fan-out to every connection; an unregistered (dropped) one gets nothing
/// (test_multiple_connections_each_receive_every_event, test_unregister_connection_stops_delivery).
#[test]
fn relay_fan_out_and_unregister() {
    let g = golden("relay.json");
    let case = &g["fan_out_and_unregister"];
    let bus = Bus::new();
    let relay = Relay::new();
    relay.register_on(&bus);
    let q1 = relay.connect_with_capacity(8);
    let q2 = relay.connect_with_capacity(8);
    let q3 = relay.connect_with_capacity(8);
    drop(q2);
    assert_eq!(
        relay.subscriber_count() as u64,
        case["subscriber_count"].as_u64().unwrap()
    );
    let all = golden("dispatch.json")["scenarios"]["event_base_catches_all"]["script"].clone();
    for step in all
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["op"] == "publish")
    {
        bus.publish(&ev(&step["event"]));
    }
    assert_eq!(drain(&q1), case["q1"]);
    assert_eq!(drain(&q3), case["q3"]);
    assert_eq!(case["q2_unregistered"], json!([]));
}

/// Teardown: a dead consumer ahead of a live one does not starve it
/// (test_dead_loop_does_not_starve_healthy_subscribers).
#[test]
fn relay_dead_consumer_does_not_starve() {
    let g = golden("relay.json");
    let case = &g["dead_consumer_first"];
    let bus = Bus::new();
    let relay = Relay::new();
    relay.register_on(&bus);
    relay.push_dead_entry_for_tests();
    let live = relay.connect_with_capacity(8);
    bus.publish(&ev(
        &json!({"class": "AlbumTagsChanged", "args": {"album_id": "should-still-reach-live"}}),
    ));
    assert_eq!(drain(&live), case["live"]);
}

/// Concurrency: publish from another thread reaches a blocked consumer
/// (test_publish_from_background_thread_reaches_queue).
#[test]
fn relay_cross_thread_publish() {
    let g = golden("relay.json");
    let bus = Bus::new();
    let relay = Relay::new();
    relay.register_on(&bus);
    let conn = relay.connect_with_capacity(8);
    let b2 = bus.clone();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        b2.publish(&ev(
            &json!({"class": "AlbumTagsChanged", "args": {"album_id": "from-thread"}}),
        ));
    });
    let m = conn
        .recv_timeout(Duration::from_secs(2))
        .expect("cross-thread publish never arrived");
    t.join().unwrap();
    assert_eq!(
        json!([&*m.name, &*m.data]),
        g["cross_thread_publish"]["received"]
    );
}

/// Async consumer woken by a publish from a plain thread.
#[test]
fn relay_async_recv_woken_cross_thread() {
    let g = golden("relay.json");
    let bus = Bus::new();
    let relay = Relay::new();
    relay.register_on(&bus);
    let mut conn = relay.connect_with_capacity(8);
    let b2 = bus.clone();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        b2.publish(&ev(
            &json!({"class": "AlbumTagsChanged", "args": {"album_id": "from-thread"}}),
        ));
    });
    let m = common::block_on(conn.recv());
    t.join().unwrap();
    assert_eq!(
        json!([&*m.name, &*m.data]),
        g["cross_thread_publish"]["received"]
    );
}

/// Relay registration: `Event`, priority 400, foreground;
/// idempotent; preserves publish order.
#[test]
fn relay_registration_and_order() {
    let g = golden("relay.json");
    let bus = Bus::new();
    let relay = Relay::new();
    relay.register_on(&bus);
    relay.register_on(&bus);
    let regs: Vec<Value> = bus
        .registrations(Topic::Event)
        .into_iter()
        .map(|r| json!({"topic": "Event", "name": r.name, "priority": r.priority, "background": r.background}))
        .collect();
    assert_eq!(Value::Array(regs), g["registration"]);
    let conn = relay.connect_with_capacity(8);
    bus.publish(&ev(
        &json!({"class": "AlbumRenamed", "args": {"old_album_id": "o", "new_album_id": "n"}}),
    ));
    bus.publish(&ev(&json!({"class": "BulkBatchCompleted", "args": {}})));
    assert_eq!(drain(&conn), g["order_preserved"]["received"]);
}
