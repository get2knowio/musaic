//! The `jobs` operations with no HTTP in scope: list and read over the
//! single research store (requests and the history of old jobs), enqueue for
//! a scanned album and see the jobs feed yield a new list, acted-on and retry
//! (no longer acting on anything), delete and Clear by status (research D13),
//! the resume, the provider list, and a restore guard or an unknown id
//! refusing with nothing changed.

mod common;

use std::time::Duration;

use musaic_app::App;
use musaic_app::ops::jobs::{Job, JobsList};
use musaic_app::ops::{Feed, OpError, Subject};
use musaic_db::research_history::{self, JobRow};

const SHORT: Duration = Duration::from_millis(300);
const NOW: &str = "2026-01-01T00:00:00Z";

/// An app whose library was scanned: the albums a job can be enqueued for.
fn scanned() -> (App, common::Scratch, Vec<String>) {
    let (app, s) = common::app_quiet_pool(|b| b);
    common::configure(&app, &s);
    let albums = app.ops().scan().subscribe(false).unwrap().count();
    assert!(albums > 2);
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while app.gate().snapshot() != (0, false) {
        assert!(std::time::Instant::now() < deadline, "the scan's guard");
        std::thread::sleep(Duration::from_millis(5));
    }
    let ids = app
        .ops()
        .library()
        .list_albums(false)
        .unwrap()
        .albums
        .iter()
        .map(|a| a.id().to_string())
        .collect();
    (app, s, ids)
}

/// An old job as the v24 step folds it: its history record and, for a known
/// type, the requests its status maps to. `status` may be `acted_on:applied`
/// or `acted_on:discarded`.
fn seed_typed(app: &App, id: &str, album: &str, job_type: &str, status: &str) {
    let conn = musaic_db::open_connection(app.db_path()).unwrap();
    let (status, action) = match status.split_once(':') {
        Some((s, a)) => (s, Some(a.to_string())),
        None => (status, None),
    };
    let mut job = JobRow::seed(id, album, job_type, status, NOW);
    job.album_title = "T".into();
    job.acted_on_action = action;
    let tx = musaic_db::immediate(&conn).unwrap();
    research_history::import_legacy_job(&tx, &job).unwrap();
    tx.commit().unwrap();
}

fn seed(app: &App, id: &str, album: &str, status: &str) {
    seed_typed(app, id, album, "metadata", status);
}

fn count(app: &App, sql: &str) -> i64 {
    let conn = musaic_db::open_connection(app.db_path()).unwrap();
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

fn next_list(feed: &mut Feed<Result<JobsList, OpError>>) -> JobsList {
    feed.next(Duration::from_secs(5))
        .expect("a value")
        .expect("readable")
}

fn ids(jobs: &[Job]) -> Vec<&str> {
    jobs.iter().map(|j| j.id.as_str()).collect()
}

#[test]
fn list_get_and_the_active_job_read_what_is_stored() {
    let (app, _s) = common::app();
    let ops = app.ops();
    let jobs = ops.jobs();
    assert!(jobs.list(None).unwrap().is_empty());
    seed(&app, "j1", "a1", "queued");
    seed(&app, "j2", "a2", "failed");
    // A type no request kind matches is a history-only record, listed as stored.
    seed_typed(&app, "h1", "a3", "lyrics", "completed");
    assert_eq!(jobs.list(None).unwrap().len(), 3);
    // An empty list of statuses lists every status; values are matched as given.
    assert_eq!(jobs.list(Some(&[])).unwrap().len(), 3);
    let failed = jobs.list(Some(&["failed".to_string()])).unwrap();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].album_id, "a2");
    assert!(
        jobs.list(Some(&[" failed".to_string()]))
            .unwrap()
            .is_empty()
    );
    // An old job's id still answers, as its research group.
    let j1 = jobs.get("j1").unwrap();
    assert_eq!(
        (j1.status.as_str(), j1.album_title.as_str()),
        ("queued", "T")
    );
    assert_eq!(jobs.get(&j1.id).unwrap(), j1);
    let h1 = jobs.get("h1").unwrap();
    assert_eq!(
        (h1.id.as_str(), h1.job_type.as_str(), h1.status.as_str()),
        ("h1", "lyrics", "completed")
    );
    match jobs.get("nope") {
        Err(OpError::NotFound { what, id, .. }) => {
            assert_eq!((what, id.as_str()), (Subject::Job, "nope"));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(jobs.active_for_album("a1").unwrap().unwrap().id, j1.id);
    // A failed job is not active, and a history-only record never is.
    assert!(jobs.active_for_album("a2").unwrap().is_none());
    assert!(jobs.active_for_album("a3").unwrap().is_none());
    let q = jobs.queue_state().unwrap();
    assert_eq!(
        (q.paused, q.running, q.queued, q.clearable),
        (false, 0, 1, 0)
    );
}

#[test]
fn enqueue_for_a_scanned_album_wakes_the_jobs_feed_with_a_new_list() {
    let (app, _s, albums) = scanned();
    let mut feed = app.ops().jobs().feed();
    let first = next_list(&mut feed);
    assert!(first.jobs.is_empty());
    assert!(feed.next(SHORT).is_none(), "nothing changed yet");
    let job = app
        .ops()
        .jobs()
        .enqueue(&albums[0], "metadata", "a hint", false)
        .unwrap();
    assert_eq!(job.status, "queued");
    assert_eq!(job.album_id, albums[0]);
    assert_eq!(job.hint, "a hint");
    assert!(!job.attach_cover);
    assert!(
        !job.album_title.is_empty(),
        "the album's title is on the job"
    );
    let list = next_list(&mut feed);
    assert_eq!(ids(&list.jobs), [job.id.as_str()]);
    assert_eq!(list.queue_state.queued, 1);
}

#[test]
fn repeated_research_joins_the_existing_request_and_distinct_kinds_can_coexist() {
    let (app, _s, albums) = scanned();
    let ops = app.ops();
    let jobs = ops.jobs();
    let first = jobs.enqueue(&albums[0], "metadata", "", true).unwrap();
    let repeated = jobs.enqueue(&albums[0], "metadata", "", true).unwrap();
    assert_eq!(first.id, repeated.id);
    let cover = jobs.enqueue(&albums[0], "cover", "", true).unwrap();
    assert_ne!(cover.id, first.id);
    assert_eq!(jobs.list(None).unwrap().len(), 2);
}

#[test]
fn enqueue_checks_in_todays_order_and_changes_nothing_when_it_refuses() {
    let (app, _s, albums) = scanned();
    let ops = app.ops();
    let jobs = ops.jobs();
    // The job type comes before the album.
    assert!(matches!(
        jobs.enqueue("nope", "lyrics", "", true),
        Err(OpError::Rejected { .. })
    ));
    match jobs.enqueue("nope", "metadata", "", true) {
        Err(OpError::NotFound { what, message, .. }) => {
            assert_eq!(what, Subject::Album);
            assert_eq!(message, "Album 'nope' not in cache; rescan first");
        }
        other => panic!("{other:?}"),
    }
    // A restore running is refused before either, even for a bad job type.
    let _restore = common::hold_restore(&app);
    for t in ["lyrics", "metadata"] {
        assert!(matches!(
            jobs.enqueue(&albums[0], t, "", true),
            Err(OpError::Busy { .. })
        ));
    }
    drop(_restore);
    assert!(jobs.list(None).unwrap().is_empty());
}

#[test]
fn clear_counts_and_removes_requests_and_history_only_records() {
    let (app, _s) = common::app();
    seed(&app, "j1", "a1", "running");
    seed(&app, "j2", "a2", "failed");
    seed(&app, "j3", "a3", "acted_on:applied");
    seed(&app, "j4", "a4", "acted_on:discarded");
    seed(&app, "j6", "a6", "completed");
    // A combined job with one applied and one failed request.
    seed_typed(&app, "j5", "a5", "metadata+cover", "acted_on:applied");
    {
        let conn = musaic_db::open_connection(app.db_path()).unwrap();
        conn.execute(
            "UPDATE delegated_work SET state='failed' WHERE legacy_job_id='j5' AND kind='cover.research'",
            [],
        )
        .unwrap();
        // A stored result goes with its request.
        conn.execute(
            "INSERT INTO work_results(id,work_id,input_digest,digest,result_json,validation,created_at)
             SELECT 'r3', id, input_digest, 'd', '{}', 'verified', 0 FROM delegated_work
             WHERE legacy_job_id='j3'",
            [],
        )
        .unwrap();
    }
    seed_typed(&app, "h1", "a7", "lyrics", "acted_on:applied");
    seed_typed(&app, "h2", "a7", "lyrics", "failed");
    seed_typed(&app, "h3", "a7", "lyrics", "weird");
    seed_typed(&app, "h4", "a7", "lyrics", "queued");
    let albums_before = count(&app, "SELECT count(*) FROM albums");
    let ops = app.ops();
    let jobs = ops.jobs();
    let mut feed = jobs.feed();
    next_list(&mut feed);
    let s = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();

    // Refusals change nothing and wake nobody.
    assert!(matches!(jobs.delete("j1"), Err(OpError::Conflict { .. })));
    assert!(matches!(jobs.delete("j6"), Err(OpError::Conflict { .. })));
    assert!(matches!(jobs.delete("nope"), Err(OpError::NotFound { .. })));
    assert!(matches!(
        jobs.delete_many(&s(&["", "  "])),
        Err(OpError::Rejected { .. })
    ));
    match jobs.delete_many(&s(&["queued", "bogus"])) {
        Err(OpError::Rejected { message, .. }) => {
            assert_eq!(message, "Unknown status: ['bogus']");
        }
        other => panic!("{other:?}"),
    }
    for unresolved in ["running", "queued", "completed"] {
        assert!(
            matches!(
                jobs.delete_many(&s(&["failed", unresolved])),
                Err(OpError::Conflict { .. })
            ),
            "{unresolved}"
        );
    }
    assert!(feed.next(SHORT).is_none());
    let requests = count(&app, "SELECT count(*) FROM delegated_work");
    assert_eq!(requests, 7);

    // Clear: three applied or discarded requests (one of j5's two) and one
    // history-only acted-on record; the count is what the button showed.
    let clearable = jobs.queue_state().unwrap().clearable;
    assert_eq!(clearable, 4);
    let cleared = jobs.delete_many(&s(&[" acted_on ", ""])).unwrap();
    assert_eq!(
        (cleared.count as i64, cleared.statuses),
        (clearable, s(&["acted_on"]))
    );
    assert_eq!(jobs.queue_state().unwrap().clearable, 0);
    next_list(&mut feed);
    assert_eq!(count(&app, "SELECT count(*) FROM delegated_work"), 4);
    assert_eq!(count(&app, "SELECT count(*) FROM work_results"), 0);
    let conn = musaic_db::open_connection(app.db_path()).unwrap();
    for (id, kept) in [
        ("j3", false),
        ("j4", false),
        ("h1", false),
        ("j5", true),
        ("h2", true),
        ("h3", true),
        ("h4", true),
    ] {
        assert_eq!(
            research_history::get(&conn, id).unwrap().is_some(),
            kept,
            "{id}"
        );
    }
    let j5 = jobs.get("j5").unwrap();
    assert_eq!(
        (j5.job_type.as_str(), j5.status.as_str()),
        ("cover", "failed")
    );

    // Nothing left to clear: nothing woken.
    assert_eq!(jobs.delete_many(&s(&["acted_on"])).unwrap().count, 0);
    assert!(feed.next(SHORT).is_none());

    // Failed: j2 and j5's failed request, and the failed or unsupported
    // history-only records.
    let failed = jobs.delete_many(&s(&["failed"])).unwrap();
    assert_eq!(failed.count, 4);
    next_list(&mut feed);
    for (id, kept) in [
        ("j2", false),
        ("j5", false),
        ("h2", false),
        ("h3", false),
        ("h4", true),
    ] {
        assert_eq!(
            research_history::get(&conn, id).unwrap().is_some(),
            kept,
            "{id}"
        );
    }
    let mut left: Vec<String> = jobs
        .list(None)
        .unwrap()
        .into_iter()
        .map(|j| format!("{}:{}", j.album_id, j.status))
        .collect();
    left.sort();
    assert_eq!(left, ["a1:queued", "a6:completed", "a7:queued"]);
    assert_eq!(count(&app, "SELECT count(*) FROM albums"), albums_before);
}

#[test]
fn delete_one_removes_a_finished_card_or_a_history_record_and_wakes_the_feed() {
    let (app, _s) = common::app();
    seed(&app, "j1", "a1", "failed");
    seed(&app, "j2", "a2", "acted_on:applied");
    seed_typed(&app, "h1", "a3", "lyrics", "failed");
    let ops = app.ops();
    let jobs = ops.jobs();
    let mut feed = jobs.feed();
    next_list(&mut feed);
    // By the old job's id, by the group id, and a history-only record.
    let group = jobs.get("j2").unwrap().id;
    for id in ["j1", group.as_str(), "h1"] {
        assert_eq!(jobs.delete(id).unwrap().id, id);
        next_list(&mut feed);
        assert!(
            matches!(jobs.get(id), Err(OpError::NotFound { .. })),
            "{id}"
        );
    }
    assert!(jobs.list(None).unwrap().is_empty());
    assert_eq!(count(&app, "SELECT count(*) FROM delegated_work"), 0);
    assert_eq!(count(&app, "SELECT count(*) FROM research_history"), 0);
}

#[test]
fn a_restore_refuses_every_writer_with_nothing_changed() {
    let (app, _s) = common::app();
    seed(&app, "j1", "a1", "failed");
    let mut feed = app.ops().jobs().feed();
    next_list(&mut feed);
    let _restore = common::hold_restore(&app);
    let ops = app.ops();
    let jobs = ops.jobs();
    let busy = |r: Result<(), OpError>| assert!(matches!(r, Err(OpError::Busy { .. })), "{r:?}");
    busy(jobs.enqueue("a", "bogus", "", true).map(drop));
    busy(jobs.delete("nope").map(drop));
    busy(jobs.delete_many(&[]).map(drop));
    // Reads still answer.
    assert_eq!(jobs.get("j1").unwrap().status, "failed");
    assert!(feed.next(SHORT).is_none());
}

// The queue state keeps its shape with fixed values (no pause, one at a
// time), whatever an old database stored for the retired settings.
#[test]
fn the_queue_state_is_fixed_whatever_was_stored() {
    let (app, _s) = common::app();
    {
        let conn = musaic_db::open_connection(app.db_path()).unwrap();
        musaic_db::settings::set_raw(&conn, "ai_queue_paused", "1").unwrap();
        musaic_db::settings::set_raw(&conn, "ai_max_parallel", "4").unwrap();
    }
    let q = app.ops().jobs().queue_state().unwrap();
    assert!(!q.paused);
    assert_eq!(q.max_parallel, 1);
    let mut feed = app.ops().jobs().feed();
    let state = next_list(&mut feed).queue_state;
    assert!(!state.paused);
    assert_eq!(state.max_parallel, 1);
}

#[test]
fn the_feed_ends_when_the_application_shuts_down() {
    let (app, _s) = common::app();
    let mut feed = app.ops().jobs().feed();
    next_list(&mut feed);
    app.begin_shutdown();
    assert!(feed.next(SHORT).is_none());
}
