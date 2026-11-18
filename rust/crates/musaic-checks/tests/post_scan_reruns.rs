//! Deliberate change: bus-driven reruns really
//! run the checks. There is no Python app output for this: in the Python app the
//! rerun subscribers set `in_post_scan_rerun` and then call the guarded
//! `run_post_scan_checks`, which returns at once
//! (testdata/golden/events/catalog.json, scenario
//! `composed_with_real_post_scan_guard`), so a fix's finding stays open until
//! the next scan. These tests assert the outcome the decision states, with
//! the real check runner behind musaic-events' catalog:
//! - An AlbumTagsChanged after a fix reruns the scan-time
//!   checks scoped to that album and the finding resolves;
//! - A publish from inside a sweep does not start another sweep
//!   (the rerun terminates after one pass);
//! - Inside a bulk loop the per-album reruns stand down and the one
//!   BulkBatchCompleted runs one library-wide sweep;
//! - AlbumRenamed runs filename-mismatch scoped to the new id.

mod common;

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use common::{Sandbox, setup};
use musaic_checks::base::Emit;
use musaic_checks::checks::inconsistent_artist_sort::{SortWrite, apply_cluster_sort};
use musaic_checks::{
    Check, CheckCtx, CheckEnv, CheckError, CheckMeta, CheckScope, ChecksSideEffects, Cost, FixKind,
    NoResolver, Registry, ScopeKind, SystemClock, run_check, run_post_scan_checks,
};
use musaic_db::findings::{all_findings, all_runs};
use musaic_events::{Bus, Event, EventKind, Subscriber, Topic, register_all};
use serde_json::json;

static PUBLISHER: CheckMeta = CheckMeta {
    id: "publisher",
    label: "Publisher",
    description: "",
    category: "metadata",
    cost: Cost::Cheap,
    runs_during_scan: true,
    fix_kind: FixKind::None,
    bulk_action_label: None,
};

/// A scan-time check that publishes an album event every time it runs, the
/// way a check writing data (e.g. an anchor) would.
struct Publisher(Arc<OnceLock<Arc<Bus>>>);

impl Check for Publisher {
    fn meta(&self) -> &CheckMeta {
        &PUBLISHER
    }
    fn run(&self, _c: &CheckCtx<'_>, _s: &CheckScope, _e: &mut Emit<'_>) -> Result<(), CheckError> {
        if let Some(bus) = self.0.get() {
            bus.publish(&Event::AlbumTagsChanged {
                album_id: "a1".into(),
                fields: vec!["title".into()],
            });
        }
        Ok(())
    }
}

struct World {
    sb: Sandbox,
    _db: musaic_db::Db,
    bus: Arc<Bus>,
    /// Every `ChecksRerunFinished` the bus delivered, in order.
    finished: Arc<Mutex<Vec<Event>>>,
}

fn world(registry: Registry, case: serde_json::Value) -> World {
    let (sb, db) = Sandbox::new();
    setup(&sb, db.connection(), &case);
    let bus = Arc::new(Bus::new());
    let finished = Arc::new(Mutex::new(Vec::new()));
    let sink = finished.clone();
    bus.register(
        Topic::Kind(EventKind::ChecksRerunFinished),
        Subscriber::from_fn(move |ev| {
            sink.lock().unwrap().push(ev.clone());
            Ok(())
        })
        .key("test_collect_finished"),
    );
    let publisher = bus.clone();
    let env = CheckEnv {
        registry,
        resolver: Arc::new(NoResolver),
        clock: Arc::new(SystemClock),
        notify: Arc::new(|| {}),
        publish: Arc::new(move |ev| publisher.publish(&ev)),
        conversion_logs: None,
    };
    let db_path = sb.db_path.clone();
    let lib = sb.lib_s.clone();
    let fx = ChecksSideEffects {
        env,
        // As the app opens them (`musaic_app::open_conn`): 30 s busy timeout.
        // rusqlite's own default is 5 s, and two reruns for one event write
        // at once on separate threads. SQLite's busy handler is not a queue,
        // so the one that loses is starved until its timeout while the other
        // makes many short commits, which a slow disk (a Windows runner) can
        // stretch past 5 s: "database is locked", and a rerun missing from
        // the recorded runs.
        open: Arc::new(move || {
            musaic_db::open_connection(&db_path).map_err(|e| e.to_string().into())
        }),
        library_dir: Arc::new(move || Some(lib.clone())),
        resolver_invalidate: Arc::new(|_| Ok(())),
        delete_thumb: Arc::new(|_| Ok(())),
    };
    register_all(&bus, Arc::new(fx), None);
    World {
        sb,
        _db: db,
        bus,
        finished,
    }
}

/// Wait for every background subscriber to finish. Returns as soon as they
/// have; the deadline only has to catch a hang, and must outlast the busy
/// timeout (30 s) a subscriber can legitimately spend waiting for the other's
/// writes, so that a stuck write shows up as its own error, not as this one.
fn idle(w: &World) {
    assert!(w.bus.wait_background_idle(Duration::from_secs(60)));
}

fn runs(w: &World) -> Vec<(String, String)> {
    all_runs(&w.sb.open())
        .unwrap()
        .into_iter()
        .map(|r| (r.check_id, r.scope))
        .collect()
}

fn gaps_case() -> serde_json::Value {
    json!({"albums": [
        {"id": "a1", "library_path": "<LIB>", "path": "<LIB>/A1", "dir_name": "A1",
         "track_numbers_json": "[1, 3]", "track_count": 2, "has_embedded_cover": 1},
        {"id": "a2", "library_path": "<LIB>", "path": "<LIB>/A2", "dir_name": "A2",
         "track_numbers_json": "[1, 3]", "track_count": 2, "has_embedded_cover": 1},
    ]})
}

/// Fix -> AlbumTagsChanged -> scoped rerun -> resolved;
/// the other album's finding is untouched.
#[test]
fn album_event_reruns_scan_checks_and_resolves_the_fixed_album() {
    let w = world(Registry::default(), gaps_case());
    let conn = w.sb.open();
    let env = CheckEnv::new(Arc::new(NoResolver));
    run_post_scan_checks(&conn, &env, &w.sb.lib_s, None).unwrap();
    let open_gaps = |c: &rusqlite::Connection| -> Vec<String> {
        all_findings(c)
            .unwrap()
            .into_iter()
            .filter(|f| f.check_id == "track-gaps" && f.resolved_at.is_none())
            .filter_map(|f| f.album_id)
            .collect()
    };
    assert_eq!(open_gaps(&conn), ["a1", "a2"]);
    conn.execute("UPDATE albums SET track_numbers_json = '[1, 2]'", [])
        .unwrap();
    let before = runs(&w).len();

    w.bus.publish(&Event::AlbumTagsChanged {
        album_id: "a1".into(),
        fields: vec!["tracknumber".into()],
    });
    idle(&w);

    assert_eq!(
        open_gaps(&conn),
        ["a2"],
        "a1 resolved by the bus rerun, a2 untouched"
    );
    let new: Vec<_> = runs(&w).into_iter().skip(before).collect();
    let scan_checks: Vec<&str> = Registry::default()
        .iter()
        .filter(|c| c.meta().runs_during_scan)
        .map(|c| c.meta().id)
        .collect();
    assert_eq!(
        new,
        scan_checks
            .iter()
            .map(|c| (c.to_string(), r#"["a1"]"#.to_string()))
            .collect::<Vec<_>>()
    );
    assert!(!musaic_events::in_post_scan_rerun());
}

/// A check that publishes from inside a sweep does not
/// start another sweep; the rerun terminates after one pass.
#[test]
fn publish_from_inside_a_sweep_terminates() {
    let bus_cell: Arc<OnceLock<Arc<Bus>>> = Arc::new(OnceLock::new());
    let reg = Registry::new(vec![Arc::new(Publisher(bus_cell.clone()))]);
    let w = world(reg, gaps_case());
    bus_cell.set(w.bus.clone()).ok().unwrap();

    w.bus
        .publish(&Event::LibraryScanCompleted { album_count: 2 });
    idle(&w);
    assert_eq!(runs(&w), [("publisher".to_string(), "all".to_string())]);

    w.bus.publish(&Event::AlbumTagsChanged {
        album_id: "a2".into(),
        fields: vec![],
    });
    idle(&w);
    assert_eq!(
        runs(&w),
        [
            ("publisher".to_string(), "all".to_string()),
            ("publisher".to_string(), r#"["a2"]"#.to_string())
        ]
    );
}

/// Per-album publishes inside a bulk apply stand down;
/// the single BulkBatchCompleted runs one library-wide sweep.
#[test]
fn bulk_apply_coalesces_into_one_library_wide_sweep() {
    let w = world(Registry::default(), gaps_case());
    let writes = vec![
        SortWrite {
            album_id: "a1".into(),
            album_level: true,
            track_filenames: vec![],
            sort_value: "X".into(),
        },
        SortWrite {
            album_id: "a2".into(),
            album_level: true,
            track_filenames: vec![],
            sort_value: "X".into(),
        },
    ];
    let seen_flags = Mutex::new(Vec::new());
    let n = apply_cluster_sort(&w.bus, &writes, |wr| {
        seen_flags
            .lock()
            .unwrap()
            .push(musaic_events::in_bulk_run());
        w.bus.publish(&Event::AlbumTagsChanged {
            album_id: wr.album_id.clone(),
            fields: vec!["album_artist_sort".into()],
        });
        true
    });
    idle(&w);
    assert_eq!(n, 2);
    assert_eq!(*seen_flags.lock().unwrap(), [true, true]);
    let r = runs(&w);
    assert_eq!(r.len(), 5, "one sweep of the five scan-time checks: {r:?}");
    assert!(r.iter().all(|(_, scope)| scope == "all"));
}

/// AlbumRenamed reruns the scan-time checks for the new
/// id and the filename-mismatch check scoped to it.
#[test]
fn rename_reruns_filename_mismatch_for_the_new_id() {
    let w = world(Registry::default(), gaps_case());
    w.bus.publish(&Event::AlbumRenamed {
        old_album_id: "old".into(),
        new_album_id: "a1".into(),
    });
    idle(&w);
    let mut r = runs(&w);
    r.sort();
    assert!(
        r.contains(&("filename-mismatch".to_string(), r#"["a1"]"#.to_string())),
        "{r:?}"
    );
    assert_eq!(
        r.iter().filter(|(_, s)| s == r#"["a1"]"#).count(),
        6,
        "{r:?}"
    );
}

/// The runner refuses before writing anything for an unknown check (RunError),
/// and a direct manual run outside any sweep sees no guard flag.
#[test]
fn manual_run_has_no_guard() {
    let w = world(Registry::default(), gaps_case());
    let conn = w.sb.open();
    let env = CheckEnv::new(Arc::new(NoResolver));
    assert!(run_check(&conn, &env, "nope", &w.sb.lib_s, ScopeKind::All, None).is_err());
    assert!(all_runs(&conn).unwrap().is_empty());
}

fn scan_check_ids() -> Vec<String> {
    Registry::default()
        .iter()
        .filter(|c| c.meta().runs_during_scan)
        .map(|c| c.meta().id.to_string())
        .collect()
}

/// A scoped rerun triggered by an album event ends with one
/// `ChecksRerunFinished` naming the checks it ran and the album, published
/// after the findings are written.
#[test]
fn album_rerun_publishes_one_finished_event_after_its_findings() {
    let w = world(Registry::default(), gaps_case());
    let conn = w.sb.open();
    w.bus.publish(&Event::AlbumTagsChanged {
        album_id: "a1".into(),
        fields: vec!["tracknumber".into()],
    });
    idle(&w);
    assert_eq!(
        *w.finished.lock().unwrap(),
        [Event::ChecksRerunFinished {
            checks: scan_check_ids(),
            failed: vec![],
            album_ids: vec!["a1".into()],
        }]
    );
    // The findings the event reports are in the database already.
    let gaps = all_findings(&conn)
        .unwrap()
        .into_iter()
        .filter(|f| f.check_id == "track-gaps" && f.album_id.as_deref() == Some("a1"))
        .count();
    assert_eq!(gaps, 1);
}

/// A library-wide rerun, and a bulk loop's single one, each publish one
/// event with no album list.
#[test]
fn library_wide_and_bulk_reruns_publish_one_event_each() {
    let w = world(Registry::default(), gaps_case());
    w.bus
        .publish(&Event::LibraryScanCompleted { album_count: 2 });
    idle(&w);
    let want = Event::ChecksRerunFinished {
        checks: scan_check_ids(),
        failed: vec![],
        album_ids: vec![],
    };
    assert_eq!(
        w.finished.lock().unwrap().as_slice(),
        std::slice::from_ref(&want)
    );

    musaic_events::run_bulk(&w.bus, true, || {
        for id in ["a1", "a2"] {
            w.bus.publish(&Event::AlbumTagsChanged {
                album_id: id.into(),
                fields: vec![],
            });
        }
    });
    idle(&w);
    assert_eq!(*w.finished.lock().unwrap(), [want.clone(), want]);
}

/// The event never starts a rerun: publishing it directly runs nothing, and
/// the event published at the end of a rerun does not start a second one.
#[test]
fn rerun_finished_event_starts_no_rerun() {
    let w = world(Registry::default(), gaps_case());
    w.bus.publish(&Event::ChecksRerunFinished {
        checks: scan_check_ids(),
        failed: vec![],
        album_ids: vec![],
    });
    idle(&w);
    assert!(runs(&w).is_empty(), "{:?}", runs(&w));

    w.bus
        .publish(&Event::LibraryScanCompleted { album_count: 2 });
    idle(&w);
    assert_eq!(runs(&w).len(), scan_check_ids().len(), "one sweep only");
    assert_eq!(
        w.finished.lock().unwrap().len(),
        2,
        "the direct one and the sweep's"
    );
}

static FAILING: CheckMeta = CheckMeta {
    id: "failing",
    label: "Failing",
    description: "",
    category: "metadata",
    cost: Cost::Cheap,
    runs_during_scan: true,
    fix_kind: FixKind::None,
    bulk_action_label: None,
};

struct Failing;

impl Check for Failing {
    fn meta(&self) -> &CheckMeta {
        &FAILING
    }
    fn run(&self, _: &CheckCtx<'_>, _: &CheckScope, _: &mut Emit<'_>) -> Result<(), CheckError> {
        Err(CheckError::Failed("boom".into()))
    }
}

/// A check that fails still wrote its run: the event names it in `failed`.
#[test]
fn failed_run_is_reported_in_failed() {
    let w = world(Registry::new(vec![Arc::new(Failing)]), gaps_case());
    w.bus
        .publish(&Event::LibraryScanCompleted { album_count: 2 });
    idle(&w);
    assert_eq!(
        *w.finished.lock().unwrap(),
        [Event::ChecksRerunFinished {
            checks: vec!["failing".into()],
            failed: vec!["failing".into()],
            album_ids: vec![],
        }]
    );
}

/// A batch in which no run started (here, no check to run) publishes nothing.
#[test]
fn a_batch_without_runs_publishes_nothing() {
    let w = world(Registry::new(vec![]), gaps_case());
    w.bus
        .publish(&Event::LibraryScanCompleted { album_count: 2 });
    idle(&w);
    assert!(w.finished.lock().unwrap().is_empty());
}
