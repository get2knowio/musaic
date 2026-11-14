//! Deliberate change: bus-driven post-scan reruns really run the
//! checks. The rerun subscribers no longer set `in_post_scan_rerun`; the
//! runner guard (`run_post_scan_guarded`) does, for the sweep's duration.
//!
//! Evidence: testdata/golden/events/catalog-rerun-guard.json, recorded from the
//! Python app. Per scenario it holds `python_app` (the Python app as-is) and
//! `fixed` (the Python app with the fix applied by monkeypatch), both with the REAL `run_post_scan_checks` guard and only
//! the check registry / `run_check` stubbed. The untouched Python app record
//! (catalog.json) is compared in golden_events.rs with its own enumerated
//! delta.

mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use common::{Fx, Trace, canon, golden, replay_catalog, rerun_guard_delta_for_catalog_json};
use musaic_events::{Bus, Event, Relay, SideEffects, context, register_all};
use serde_json::{Value, json};

fn sorted(v: &Value) -> Vec<String> {
    let mut x: Vec<String> = v.as_array().unwrap().iter().map(Value::to_string).collect();
    x.sort();
    x
}

/// Multiset difference a − b (b must be contained in a).
fn minus(a: &[String], b: &[String]) -> Vec<String> {
    let mut rest = a.to_vec();
    for x in b {
        let i = rest
            .iter()
            .position(|y| y == x)
            .unwrap_or_else(|| panic!("Python app entry missing from fixed trace: {x}"));
        rest.remove(i);
    }
    rest
}

/// Rust equals the patched Python app in every
/// scenario, including the two termination scenarios.
#[test]
fn rust_matches_patched_python_for_every_scenario() {
    let g = golden("catalog-rerun-guard.json");
    assert_eq!(
        g["patched_subscribers"],
        json!([
            "_post_scan_rerun",
            "_post_scan_rerun_for_rename",
            "_post_scan_rerun_library_wide"
        ])
    );
    for (name, sc) in g["scenarios"].as_object().unwrap() {
        let publishes = sc
            .get("check_publishes")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        assert_eq!(
            replay_catalog(sc, true, publishes),
            canon(&sc["fixed"]),
            "scenario {name}"
        );
    }
}

/// Delta, enumerated: per scenario, how many background calls the
/// fix adds to the Python app's trace. Every scenario not listed here (and the
/// ones listed with 0) must be unchanged.
const ADDED: &[(&str, usize)] = &[
    ("each_event_AlbumTagsChanged", 1),
    ("each_event_AlbumCoverChanged", 1),
    ("each_event_AlbumRenamed", 1),
    ("each_event_AlbumMbAnchorSet", 1),
    ("each_event_TrackFingerprinted", 1),
    ("each_event_AlbumDeleted", 1),
    ("each_event_AlbumMarkedMissing", 1),
    ("each_event_LibraryScanCompleted", 1),
    ("each_event_BulkBatchCompleted", 1),
    ("file_only_rename", 1),
    ("bulk_dispatcher_coalesces", 1),
    ("bulk_flag_inherited_by_worker_thread", 1),
    ("register_all_twice", 9),
    ("target_raises_rename_old", 2),
    ("composed_with_real_post_scan_guard", 4),
    ("rerun_check_publishes_terminates", 6),
    ("rerun_check_publishes_bg_republish_terminates", 6),
    // Suppressed by a guard flag, or no library: unchanged.
    ("in_post_scan_rerun_suppresses_reruns", 0),
    ("in_bulk_run_suppresses_reruns", 0),
    ("bulk_dispatcher_no_publish", 0),
    ("no_library_configured", 0),
];

fn bg(entry: Value) -> String {
    entry.to_string()
}

/// The full added entries of the two termination scenarios: one sweep per
/// original event, plus the sync subscribers of the event the check
/// publishes (they run on the rerun thread) — and no second sweep.
fn termination_delta(name: &str) -> Vec<Value> {
    let rec = |target: &str, extra: Value, thread: &str| {
        let mut v = json!({"target": target, "rerun": true, "bulk": false,
                           "thread": format!("library_events:{thread}")});
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        v
    };
    let run_check = |ids: Value, thread: &str| {
        let scope = if ids.is_null() { "all" } else { "albums" };
        rec(
            "run_check",
            json!({"check_id": "a", "library_path": "/lib", "scope_kind": scope, "album_ids": ids}),
            thread,
        )
    };
    match name {
        "rerun_check_publishes_terminates" => vec![
            run_check(json!(["a1"]), "_post_scan_rerun"),
            rec(
                "resolver_invalidate",
                json!({"album_id": "a1"}),
                "_post_scan_rerun",
            ),
            rec("notify_findings_changed", json!({}), "_post_scan_rerun"),
            run_check(Value::Null, "_post_scan_rerun_library_wide"),
            rec(
                "resolver_invalidate",
                json!({"album_id": "lib"}),
                "_post_scan_rerun_library_wide",
            ),
            rec(
                "notify_findings_changed",
                json!({}),
                "_post_scan_rerun_library_wide",
            ),
        ],
        "rerun_check_publishes_bg_republish_terminates" => vec![
            run_check(json!(["a1"]), "_post_scan_rerun"),
            rec(
                "resolver_invalidate",
                json!({"album_id": "a1"}),
                "_post_scan_rerun",
            ),
            rec("notify_findings_changed", json!({}), "_post_scan_rerun"),
            json!({"sub": "bg_republisher", "event": "AlbumMbAnchorSet", "action": "republish",
                   "rerun": true, "bulk": false, "thread": "library_events:bg_republisher"}),
            rec(
                "resolver_invalidate",
                json!({"album_id": "from-bg"}),
                "bg_republisher",
            ),
            rec("notify_findings_changed", json!({}), "bg_republisher"),
        ],
        _ => unreachable!(),
    }
}

/// The delta between the Python app and the patched the Python app is exactly the
/// enumerated one: sync traces identical everywhere; in background,
/// the Python app's entries all survive and the only additions are one
/// `run_check("a")` per post-scan-rerun subscriber call the stub-level
/// the Python app record (catalog.json) shows, the composed scenario's four, and
/// the two termination scenarios' listed entries.
#[test]
fn delta_from_python_is_exactly_enumerated() {
    let g = golden("catalog-rerun-guard.json");
    let stub = golden("catalog.json");
    let scenarios = g["scenarios"].as_object().unwrap();
    let table: BTreeMap<&str, usize> = ADDED.iter().copied().collect();
    assert_eq!(
        scenarios.keys().map(String::as_str).collect::<Vec<_>>(),
        table.keys().copied().collect::<Vec<_>>(),
        "every scenario must be in the enumerated table"
    );
    for (name, sc) in scenarios {
        let (o, f) = (&sc["python_app"], &sc["fixed"]);
        assert_eq!(o["sync"], f["sync"], "{name}: sync trace changed");
        let added = minus(&sorted(&f["background"]), &sorted(&o["background"]));
        assert_eq!(added.len(), table[name.as_str()], "{name}: {added:#?}");
        // The Python app itself never runs check "a" from the bus.
        assert!(
            !o["background"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["check_id"] == "a"),
            "{name}"
        );
        let mut want: Vec<String> = if name.starts_with("rerun_check_publishes") {
            termination_delta(name).into_iter().map(bg).collect()
        } else if let Some(d) = rerun_guard_delta_for_catalog_json(name) {
            d.into_iter().map(bg).collect()
        } else {
            // One run_check("a") for each stub-level run_post_scan_checks call.
            stub["scenarios"][name]["trace"]["background"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|e| e["target"] == "run_post_scan_checks")
                .map(|e| {
                    let scope = if e["album_ids"].is_null() {
                        "all"
                    } else {
                        "albums"
                    };
                    bg(json!({"target": "run_check", "check_id": "a",
                              "library_path": e["library_dir"], "scope_kind": scope,
                              "album_ids": e["album_ids"], "rerun": e["rerun"],
                              "bulk": e["bulk"], "thread": e["thread"]}))
                })
                .collect()
        };
        want.sort();
        assert_eq!(added, want, "{name}");
    }
}

fn counting_fx(trace: Arc<Trace>, bus: &Bus) -> Arc<dyn SideEffects> {
    Arc::new(Fx {
        trace,
        library_dir: Some("/lib".into()),
        real_post_scan: true,
        check_publishes: Some(bus.clone()),
    })
}

fn sweeps(trace: &Trace) -> usize {
    trace.to_value()["background"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["check_id"] == "a")
        .count()
}

/// Termination: a rerun whose check publishes an album event from
/// inside the sweep runs exactly one sweep per external event — also when a
/// background subscriber spawned by that inner publish republishes again
/// from its own thread.
#[test]
fn rerun_whose_checks_publish_terminates_after_one_pass() {
    let trace = Arc::new(Trace::default());
    let bus = Bus::new();
    register_all(&bus, counting_fx(trace.clone(), &bus), Some(&Relay::new()));
    let tags = |id: &str| Event::AlbumTagsChanged {
        album_id: id.into(),
        fields: vec![],
    };
    bus.register(
        musaic_events::Topic::Kind(musaic_events::EventKind::AlbumMbAnchorSet),
        musaic_events::Subscriber::from_fn({
            let bus = bus.clone();
            move |_| {
                bus.publish(&Event::AlbumTagsChanged {
                    album_id: "from-bg".into(),
                    fields: vec![],
                });
                Ok(())
            }
        })
        .background(true)
        .name("bg_republisher"),
    );
    for i in 0..5 {
        bus.publish(&tags(&format!("a{i}")));
    }
    assert!(
        bus.wait_background_idle(Duration::from_secs(5)),
        "event cycle did not terminate"
    );
    assert_eq!(sweeps(&trace), 5);
    bus.reset();
}

/// LOAD-BEARING under the fix: a rerun in flight on another
/// thread does not suppress an unrelated publish's rerun.
#[test]
fn unrelated_concurrent_rerun_still_runs_with_fix() {
    let trace = Arc::new(Trace::default());
    let bus = Bus::new();
    let fx = Arc::new(Fx {
        trace: trace.clone(),
        library_dir: Some("/lib".into()),
        real_post_scan: true,
        check_publishes: None,
    });
    register_all(&bus, fx, None);
    let (held_tx, held_rx) = mpsc::channel();
    let (rel_tx, rel_rx) = mpsc::channel::<()>();
    let a = std::thread::spawn(move || {
        musaic_events::run_post_scan_guarded(|| {
            held_tx.send(()).unwrap();
            let _ = rel_rx.recv_timeout(Duration::from_secs(2));
        })
    });
    held_rx.recv().unwrap();
    assert!(!context::in_post_scan_rerun());
    let b = bus.clone();
    std::thread::spawn(move || {
        b.publish(&Event::AlbumDeleted {
            album_id: "x".into(),
        })
    })
    .join()
    .unwrap();
    assert!(bus.wait_background_idle(Duration::from_secs(5)));
    rel_tx.send(()).unwrap();
    a.join().unwrap();
    assert_eq!(sweeps(&trace), 1);
}
