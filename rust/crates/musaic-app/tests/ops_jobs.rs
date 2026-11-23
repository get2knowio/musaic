//! The `jobs` operations with no HTTP in scope: list and read, enqueue for a
//! scanned album and see the jobs feed yield a new list, the album's active job
//! as a typed conflict (nothing woken), acted-on, retry, delete and bulk delete,
//! the resume, the provider list, and a restore guard or an unknown id refusing
//! with nothing changed.

mod common;

use std::time::Duration;

use musaic_app::App;
use musaic_app::ops::error::Details;
use musaic_app::ops::jobs::{Job, JobsList};
use musaic_app::ops::{Feed, OpError, Subject};
use musaic_db::ai_jobs;

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

/// A job row written straight to the database, in `status`.
fn seed(app: &App, id: &str, album: &str, status: &str) {
    let mut conn = musaic_db::open_connection(app.db_path()).unwrap();
    ai_jobs::enqueue(
        &mut conn,
        &ai_jobs::NewJob {
            album_id: album,
            album_path: "/p",
            album_title: "T",
            job_type: "metadata",
            hint: "",
            attach_cover: true,
        },
        || id.to_string(),
        NOW,
    )
    .unwrap()
    .unwrap();
    match status {
        "queued" => {}
        "running" => assert!(ai_jobs::mark_running(&conn, id, NOW).unwrap()),
        "completed" => ai_jobs::mark_completed(&conn, id, "{\"ok\":true}", NOW).unwrap(),
        "failed" => ai_jobs::mark_failed(&conn, id, "boom", None, NOW).unwrap(),
        other => panic!("{other}"),
    }
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
    assert_eq!(jobs.list(None).unwrap().len(), 2);
    // An empty list of statuses lists every status; values are matched as given.
    assert_eq!(jobs.list(Some(&[])).unwrap().len(), 2);
    let failed = jobs.list(Some(&["failed".to_string()])).unwrap();
    assert_eq!(ids(&failed), ["j2"]);
    assert!(
        jobs.list(Some(&[" failed".to_string()]))
            .unwrap()
            .is_empty()
    );
    assert_eq!(jobs.get("j1").unwrap().status, "queued");
    match jobs.get("nope") {
        Err(OpError::NotFound { what, id, .. }) => {
            assert_eq!((what, id.as_str()), (Subject::Job, "nope"));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(jobs.active_for_album("a1").unwrap().unwrap().id, "j1");
    // A failed job is not active.
    assert!(jobs.active_for_album("a2").unwrap().is_none());
    let q = jobs.queue_state().unwrap();
    assert_eq!((q.paused, q.running, q.queued), (false, 0, 1));
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
fn acted_on_applies_to_completed_jobs_and_wakes_the_feed() {
    let (app, _s) = common::app();
    seed(&app, "j1", "a1", "completed");
    seed(&app, "j2", "a2", "queued");
    let ops = app.ops();
    let jobs = ops.jobs();
    let mut feed = jobs.feed();
    next_list(&mut feed);
    assert!(matches!(
        jobs.mark_acted_on("nope", "applied"),
        Err(OpError::NotFound { .. })
    ));
    // Status before action, as the route checks them.
    assert!(matches!(
        jobs.mark_acted_on("j2", "maybe"),
        Err(OpError::Conflict { .. })
    ));
    assert!(matches!(
        jobs.mark_acted_on("j1", "maybe"),
        Err(OpError::Rejected { .. })
    ));
    assert!(feed.next(SHORT).is_none(), "refusals wake nobody");
    let job = jobs.mark_acted_on("j1", "applied").unwrap();
    assert_eq!(job.status, "acted_on");
    assert_eq!(job.acted_on_action.as_deref(), Some("applied"));
    assert_eq!(next_list(&mut feed).jobs.len(), 2);
}

#[test]
fn retry_requeues_a_failed_job_and_names_an_other_active_job() {
    let (app, _s) = common::app();
    seed(&app, "j1", "a1", "failed");
    seed(&app, "j2", "a2", "queued");
    let ops = app.ops();
    let jobs = ops.jobs();
    assert!(matches!(jobs.retry("nope"), Err(OpError::NotFound { .. })));
    assert!(matches!(
        jobs.retry("j2"),
        Err(OpError::Conflict { details: None, .. })
    ));
    // A second job for a1 makes a1's retry conflict, naming it.
    seed(&app, "j3", "a1", "queued");
    match jobs.retry("j1") {
        Err(OpError::Conflict {
            details: Some(Details::ExistingJob { job_id, status }),
            ..
        }) => assert_eq!((job_id.as_str(), status.as_str()), ("j3", "queued")),
        other => panic!("{other:?}"),
    }
    jobs.delete("j3").unwrap();
    let mut feed = jobs.feed();
    next_list(&mut feed);
    let job = jobs.retry("j1").unwrap();
    assert_eq!(job.status, "queued");
    assert!(job.error.is_none());
    assert_eq!(next_list(&mut feed).queue_state.queued, 2);
}

#[test]
fn delete_and_delete_many_refuse_what_runs_and_wake_the_feed_only_for_a_change() {
    let (app, _s) = common::app();
    seed(&app, "j1", "a1", "running");
    seed(&app, "j2", "a2", "queued");
    seed(&app, "j3", "a3", "failed");
    let ops = app.ops();
    let jobs = ops.jobs();
    let mut feed = jobs.feed();
    next_list(&mut feed);
    assert!(matches!(jobs.delete("j1"), Err(OpError::Conflict { .. })));
    assert!(matches!(jobs.delete("nope"), Err(OpError::NotFound { .. })));
    let s = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
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
    assert!(matches!(
        jobs.delete_many(&s(&["queued", "running"])),
        Err(OpError::Conflict { .. })
    ));
    assert_eq!(jobs.list(None).unwrap().len(), 3);
    // Nothing deleted by any of those, nothing woken, and a bulk delete that
    // matches no row wakes nobody either.
    let none = jobs.delete_many(&s(&["completed"])).unwrap();
    assert_eq!(none.count, 0);
    assert!(feed.next(SHORT).is_none());
    let two = jobs.delete_many(&s(&[" queued ", "failed", ""])).unwrap();
    assert_eq!((two.count, two.statuses), (2, s(&["queued", "failed"])));
    assert_eq!(ids(&next_list(&mut feed).jobs), ["j1"]);
    assert_eq!(app.ops().jobs().list(None).unwrap().len(), 1);
}

#[test]
fn delete_one_returns_its_id_and_wakes_the_feed() {
    let (app, _s) = common::app();
    seed(&app, "j1", "a1", "queued");
    let mut feed = app.ops().jobs().feed();
    next_list(&mut feed);
    assert_eq!(app.ops().jobs().delete("j1").unwrap().id, "j1");
    assert!(next_list(&mut feed).jobs.is_empty());
    assert!(matches!(
        app.ops().jobs().get("j1"),
        Err(OpError::NotFound { .. })
    ));
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
    busy(jobs.mark_acted_on("nope", "bogus").map(drop));
    busy(jobs.retry("j1").map(drop));
    busy(jobs.delete("nope").map(drop));
    busy(jobs.delete_many(&[]).map(drop));
    busy(jobs.resume_queue().map(drop));
    // Reads still answer.
    assert_eq!(jobs.get("j1").unwrap().status, "failed");
    assert!(feed.next(SHORT).is_none());
}

#[test]
fn resume_clears_the_pause_and_wakes_the_feed() {
    let (app, _s) = common::app();
    {
        let conn = musaic_db::open_connection(app.db_path()).unwrap();
        musaic_ai::jobs::set_queue_paused(&conn, true).unwrap();
    }
    let mut feed = app.ops().jobs().feed();
    assert!(next_list(&mut feed).queue_state.paused);
    let q = app.ops().jobs().resume_queue().unwrap();
    assert!(!q.paused);
    assert_eq!(q.max_parallel, 1);
    assert!(!next_list(&mut feed).queue_state.paused);
}

#[test]
fn providers_list_the_selection_and_what_this_build_has() {
    let (app, _s) = common::app();
    let p = app.ops().jobs().providers().unwrap();
    assert_eq!(p.selected.provider_id, "claude");
    assert_eq!(p.selected.model_id, "claude-sonnet-4-6");
    assert!(!p.selected.api_key_set);
    assert_eq!(p.providers.len(), 1);
    assert!(!p.providers[0].ready, "no model backend in this build");
}

#[test]
fn the_feed_ends_when_the_application_shuts_down() {
    let (app, _s) = common::app();
    let mut feed = app.ops().jobs().feed();
    next_list(&mut feed);
    app.begin_shutdown();
    assert!(feed.next(SHORT).is_none());
}
