//! Research batches through the `jobs` operations (spec 006): list, detail
//! and cancel of the batch a bulk Fix all made, its absence once Clear leaves
//! it no request, Missing albums and an epoch rotation, the album's active
//! request, the delegated work list the Library reads, and the cancel of one
//! job card's research (FR-036).

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use musaic_app::App;
use musaic_app::ops::OpError;
use musaic_app::ops::checks::FindingsQuery;
use musaic_app::ops::fixes::{Listening, QuickFixEvent};
use musaic_db::delegated_work as queue;
use musaic_events::{AgentWhat, Event, EventKind, Subscriber, Topic};

fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for {what}");
}

fn checked() -> (App, common::Scratch) {
    let (app, s) = common::app_quiet_pool(|b| b);
    common::configure(&app, &s);
    app.ops().scan().subscribe(false).unwrap().count();
    eventually("the work guard released", || {
        app.gate().snapshot() == (0, false)
    });
    common::wait_for_fingerprints(&app);
    for c in app.ops().checks().list().unwrap() {
        app.ops().checks().run(c.check_id(), "all", None).unwrap();
    }
    (app, s)
}

/// Bulk research over unidentified albums: the batch id.
fn bulk(app: &App) -> String {
    let n = app
        .ops()
        .checks()
        .list_findings(&FindingsQuery {
            check_id: Some("unidentified-album".into()),
            ..FindingsQuery::default()
        })
        .unwrap()
        .len();
    assert!(n > 2, "{n}");
    let mut batch = None;
    app.ops()
        .fixes()
        .quick_fix_all("unidentified-album", true, &mut |e| {
            if let QuickFixEvent::Done(s) = &e {
                batch = s.research.as_ref().and_then(|r| r.batch_id.clone());
            }
            Listening::Yes
        })
        .unwrap();
    batch.expect("a batch")
}

fn conn(app: &App) -> rusqlite::Connection {
    musaic_db::open_connection(app.db_path()).unwrap()
}

fn batch_ids(app: &App, batch: &str) -> Vec<String> {
    let c = conn(app);
    let mut stmt = c
        .prepare("SELECT id FROM delegated_work WHERE batch_id=?1 ORDER BY created_at, id")
        .unwrap();
    stmt.query_map([batch], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn set_state(app: &App, id: &str, state: &str) {
    conn(app)
        .execute(
            "UPDATE delegated_work SET state=?2 WHERE id=?1",
            [id, state],
        )
        .unwrap();
}

fn work_events(app: &App) -> Arc<Mutex<Vec<Vec<String>>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    app.bus().register(
        Topic::Kind(EventKind::AgentStateChanged),
        Subscriber::from_fn(move |event| {
            if let Event::AgentStateChanged {
                what: AgentWhat::Work,
                ids,
            } = event
            {
                sink.lock().unwrap().push(ids.clone());
            }
            Ok(())
        })
        .key("batches-work-events")
        .name("batches-work-events"),
    );
    seen
}

#[test]
fn a_batch_is_listed_read_and_cancelled_leaving_review_alone() {
    let (app, _s) = checked();
    let batch = bulk(&app);
    let ids = batch_ids(&app, &batch);
    let n = ids.len() as i64;
    assert!(n >= 2);
    set_state(&app, &ids[0], "awaiting_review");

    let ops = app.ops();
    let jobs = ops.jobs();
    let list = jobs.research_batches().unwrap();
    assert_eq!(list.len(), 1);
    let summary = &list[0];
    assert_eq!(summary.check_id, "unidentified-album");
    assert_eq!(summary.size, n);
    assert_eq!(
        (summary.counts.queued, summary.counts.awaiting_review),
        (n - 1, 1)
    );
    assert!(!summary.unfinished);
    let detail = jobs.research_batch(&batch).unwrap();
    assert_eq!(detail.items.len(), ids.len());
    assert!(
        detail
            .items
            .iter()
            .all(|i| i.batch_id.as_deref() == Some(batch.as_str()))
    );

    let events = work_events(&app);
    let done = jobs.cancel_research_batch(&batch).unwrap();
    assert_eq!(done.cancelled as i64, n - 1);
    assert_eq!(
        (
            done.summary.counts.cancelled,
            done.summary.counts.awaiting_review
        ),
        (n - 1, 1)
    );
    assert_eq!(
        queue::get(&conn(&app), &ids[0]).unwrap().unwrap().state,
        "awaiting_review"
    );
    let published = events.lock().unwrap().clone();
    assert_eq!(published.len(), 1, "one page, one event: {published:?}");
    assert_eq!(published[0].len() as i64, n - 1);
    // Nothing left to cancel is still an answer.
    assert_eq!(jobs.cancel_research_batch(&batch).unwrap().cancelled, 0);
    assert!(matches!(
        jobs.research_batch("00000000-0000-4000-8000-000000000000"),
        Err(OpError::NotFound { .. })
    ));
    assert!(matches!(
        jobs.cancel_research_batch("00000000-0000-4000-8000-000000000000"),
        Err(OpError::NotFound { .. })
    ));
    let _guard = common::hold_restore(&app);
    assert!(matches!(
        jobs.cancel_research_batch(&batch),
        Err(OpError::Busy { .. })
    ));
}

#[test]
fn a_batch_is_gone_once_clear_leaves_it_no_request() {
    let (app, _s) = checked();
    let batch = bulk(&app);
    let ops = app.ops();
    let jobs = ops.jobs();
    jobs.cancel_research_batch(&batch).unwrap();
    let n = batch_ids(&app, &batch).len();
    let cleared = jobs.delete_many(&["failed".to_string()]).unwrap();
    assert_eq!(cleared.count, n);
    assert!(jobs.research_batches().unwrap().is_empty());
    assert!(matches!(
        jobs.research_batch(&batch),
        Err(OpError::NotFound { .. })
    ));
    let left: i64 = conn(&app)
        .query_row("SELECT count(*) FROM work_batches", [], |r| r.get(0))
        .unwrap();
    assert_eq!(left, 0);
}

#[test]
fn missing_albums_and_an_epoch_rotation_keep_the_batch_listed() {
    let (app, _s) = checked();
    let batch = bulk(&app);
    let ids = batch_ids(&app, &batch);
    let c = conn(&app);
    let album = queue::get(&c, &ids[0]).unwrap().unwrap().album_id;
    c.execute(
        "UPDATE albums SET missing_since='2026-01-01T00:00:00Z' WHERE id=?1",
        [&album],
    )
    .unwrap();
    let detail = app.ops().jobs().research_batch(&batch).unwrap();
    assert!(detail.items.iter().any(|i| i.album_id == album));

    queue::rotate_epoch(&c, Some("/another/library"), 1_900_000_000_000).unwrap();
    let list = app.ops().jobs().research_batches().unwrap();
    assert_eq!(list.len(), 1, "a batch of an earlier epoch is still listed");
    assert_eq!(list[0].counts.cancelled, ids.len() as i64);
    assert_eq!(list[0].counts.queued, 0);
}

#[test]
fn the_album_panel_and_the_library_see_batched_requests() {
    let (app, _s) = checked();
    let batch = bulk(&app);
    let ids = batch_ids(&app, &batch);
    set_state(&app, &ids[0], "awaiting_review");
    let c = conn(&app);
    let first = queue::get(&c, &ids[0]).unwrap().unwrap();
    let second = queue::get(&c, &ids[1]).unwrap().unwrap();

    let ops = app.ops();
    let jobs = ops.jobs();
    let active = jobs
        .active_for_album(&second.album_id)
        .unwrap()
        .expect("active");
    assert_eq!(active.id, second.group_id);
    assert_eq!(active.status, "queued");
    let active = jobs
        .active_for_album(&first.album_id)
        .unwrap()
        .expect("active");
    assert_eq!(active.status, "completed");

    // T062a: what the Library derives its pills and its review filter from.
    let work = jobs.delegated_work().unwrap();
    assert_eq!(work.len(), ids.len());
    let reviewed = work.iter().find(|i| i.work_id == ids[0]).unwrap();
    assert_eq!(reviewed.album_id, first.album_id);
    assert_eq!(reviewed.kind, first.kind);
    assert_eq!(reviewed.state, "awaiting_review");
    assert_eq!(reviewed.group_id, first.group_id);
    assert_eq!(reviewed.batch_id.as_deref(), Some(batch.as_str()));
    let json = serde_json::to_value(reviewed).unwrap();
    for key in ["workId", "groupId", "batchId", "albumId", "kind", "state"] {
        assert!(json.get(key).is_some(), "{key}: {json}");
    }
    // The jobs list is cards; the batch is not one of them.
    assert!(jobs.list(None).unwrap().is_empty());
}

#[test]
fn cancelling_a_card_cancels_its_waiting_and_held_research_only() {
    let (app, _s) = checked();
    let album = app.ops().library().list_albums(false).unwrap().albums[0]
        .id()
        .to_string();
    let ops = app.ops();
    let jobs = ops.jobs();
    let job = jobs.enqueue(&album, "metadata+cover", "", true).unwrap();
    let c = conn(&app);
    let items = musaic_db::research_jobs::group(&c, &job.id).unwrap();
    assert_eq!(items.len(), 2, "{items:?}");
    for i in &items {
        assert_eq!(i.state, "queued", "{i:?}");
    }
    // One held by an assistant.
    let claimed = queue::claim(
        &c,
        queue::Claim {
            work_id: &items[1].id,
            principal_id: "assistant",
            idempotency_key: "claim-0001",
            request_digest: "d",
            lease_token_hash: "h",
            now: app
                .now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64,
        },
    )
    .unwrap()
    .unwrap();
    assert_eq!(claimed.work_id, items[1].id);

    let events = work_events(&app);
    let done = jobs.cancel_research_group(&job.id).unwrap();
    assert_eq!((done.id.as_str(), done.cancelled), (job.id.as_str(), 2));
    for i in &items {
        assert_eq!(queue::get(&c, &i.id).unwrap().unwrap().state, "cancelled");
    }
    assert_eq!(events.lock().unwrap().len(), 1);
    // Nothing waiting or held any more.
    assert!(matches!(
        jobs.cancel_research_group(&job.id),
        Err(OpError::Conflict { .. })
    ));
    assert!(matches!(
        jobs.cancel_research_group("no-such-job"),
        Err(OpError::NotFound { .. })
    ));
}

#[test]
fn cancelling_a_card_leaves_a_proposal_awaiting_review() {
    let (app, _s) = checked();
    let albums = app.ops().library().list_albums(false).unwrap().albums;
    let ops = app.ops();
    let jobs = ops.jobs();
    let job = jobs
        .enqueue(albums[1].id(), "metadata+cover", "", true)
        .unwrap();
    let c = conn(&app);
    let items = musaic_db::research_jobs::group(&c, &job.id).unwrap();
    set_state(&app, &items[0].id, "awaiting_review");
    let done = jobs.cancel_research_group(&job.id).unwrap();
    assert_eq!(done.cancelled, 1);
    assert_eq!(
        queue::get(&c, &items[0].id).unwrap().unwrap().state,
        "awaiting_review"
    );
    assert_eq!(
        queue::get(&c, &items[1].id).unwrap().unwrap().state,
        "cancelled"
    );
    // An old job's history record has nothing to cancel.
    let mut old = musaic_db::research_history::JobRow::seed(
        "old-1",
        albums[2].id(),
        "lyrics",
        "completed",
        "2026-01-01T00:00:00Z",
    );
    old.album_title = "T".into();
    let tx = musaic_db::immediate(&c).unwrap();
    musaic_db::research_history::import_legacy_job(&tx, &old).unwrap();
    tx.commit().unwrap();
    assert!(matches!(
        jobs.cancel_research_group("old-1"),
        Err(OpError::Conflict { .. })
    ));
    let _guard = common::hold_restore(&app);
    assert!(matches!(
        jobs.cancel_research_group(&job.id),
        Err(OpError::Busy { .. })
    ));
}
