//! Coalesced publishing of committed delegation changes (spec 006, research
//! D12): a thousand outbox rows reach the bus as a handful of
//! `AgentStateChanged(work)` events of at most 128 ids each, every one after
//! its rows were committed, and nothing is lost. The bulk run that defers its
//! publishing is covered in `ops_fixes.rs`
//! (`a_thousand_unsettled_albums_make_one_batch_of_a_thousand`).

mod common;

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use musaic_db::delegated_work::{self as db, NewWork};
use musaic_events::{AgentWhat, Event, EventKind, Subscriber, Topic};

#[test]
fn a_thousand_outbox_rows_are_a_handful_of_events_and_none_is_lost() {
    const N: usize = 1000;
    let (app, _s) = common::app();
    let path = app.db_path().to_path_buf();
    let conn = musaic_db::open_connection(&path).unwrap();
    let ids: Vec<String> = (0..N)
        .map(|i| format!("00000000-0000-4000-8000-{i:012}"))
        .collect();
    let albums: Vec<String> = (0..N).map(|i| format!("{i:016x}")).collect();
    for (id, album) in ids.iter().zip(&albums) {
        db::enqueue(
            &conn,
            &[NewWork {
                id,
                group_id: id,
                kind: "metadata.identify",
                album_id: album,
                base_revision: "rev",
                input_json: "{}",
                input_digest: id,
                max_attempts: 3,
            }],
            1_000,
        )
        .unwrap();
    }
    let seen: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
    let uncommitted = Arc::new(Mutex::new(0usize));
    let (sink, missing) = (seen.clone(), uncommitted.clone());
    app.bus().register(
        Topic::Kind(EventKind::AgentStateChanged),
        Subscriber::from_fn(move |event| {
            if let Event::AgentStateChanged {
                what: AgentWhat::Work,
                ids,
            } = event
            {
                // Published after the commit: another connection sees each row.
                let other = musaic_db::open_connection(&path).unwrap();
                for id in ids {
                    if db::get(&other, id).unwrap().is_none() {
                        *missing.lock().unwrap() += 1;
                    }
                }
                sink.lock().unwrap().push(ids.clone());
            }
            Ok(())
        })
        .key("coalesce-test")
        .name("coalesce-test"),
    );
    app.ops().agent().sweep_delegation();
    let events = seen.lock().unwrap().clone();
    assert_eq!(events.len(), N.div_ceil(128), "{}", events.len());
    assert!(events.iter().all(|ids| !ids.is_empty() && ids.len() <= 128));
    let all: BTreeSet<String> = events.concat().into_iter().collect();
    assert_eq!(all, ids.iter().cloned().collect::<BTreeSet<_>>());
    assert_eq!(*uncommitted.lock().unwrap(), 0);
    assert!(
        db::outbox(&conn, 10).unwrap().is_empty(),
        "every page acknowledged"
    );
    // Nothing new: nothing published.
    app.ops().agent().sweep_delegation();
    assert_eq!(seen.lock().unwrap().len(), events.len());
}
