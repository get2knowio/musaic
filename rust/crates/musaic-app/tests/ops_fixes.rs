//! The `fixes` operations with no HTTP in scope: fix one finding and see the bus
//! event, the research request a fix falls through to (and the jobs feed it wakes without
//! being asked), quick-fix-all as a run with a sink, the per-check actions, the
//! multi-disc conversion, and the restore gate: every fix is `Busy` during a
//! restore, before the finding is looked up.

mod common;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use musaic_app::App;
use musaic_app::ops::checks::{ConversionPreview, FindingsQuery, PlanPreview};
use musaic_app::ops::fixes::{
    ConvertEvent, Listening, PlanEntry, QuickFixEnd, QuickFixEvent, QuickFixOutcome,
};
use musaic_app::ops::{OpError, Subject};
use musaic_services::error::Details;

const SHORT: Duration = Duration::from_millis(300);

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

/// A configured app whose library was scanned, with every check run once.
fn checked_with(
    more: impl FnOnce(musaic_app::AppBuilder) -> musaic_app::AppBuilder,
) -> (App, common::Scratch) {
    let (app, s) = common::app_quiet_pool(more);
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

fn checked() -> (App, common::Scratch) {
    checked_with(|b| b)
}

fn finding_ids(app: &App, check: &str) -> Vec<i64> {
    app.ops()
        .checks()
        .list_findings(&FindingsQuery {
            check_id: Some(check.into()),
            ..FindingsQuery::default()
        })
        .unwrap()
        .iter()
        .map(|f| f.id())
        .collect()
}

/// The names of the bus events published since the feed was taken.
fn names(feed: &musaic_app::ops::system::LibraryEvents) -> Vec<String> {
    let mut out = Vec::new();
    while let Some(e) = feed.next(SHORT) {
        out.push(e.name().to_string());
    }
    out
}

fn count(names: &[String], name: &str) -> usize {
    names.iter().filter(|n| n.as_str() == name).count()
}

/// Research requests made (one per job card: a group).
fn job_rows(app: &App) -> i64 {
    let conn = musaic_db::open_connection(app.db_path()).unwrap();
    conn.query_row(
        "SELECT COUNT(DISTINCT group_id) FROM delegated_work",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn a_fix_that_writes_publishes_its_event() {
    let (app, _s) = checked();
    let id = finding_ids(&app, "tags-out-of-sync")[0];
    let feed = app.ops().system().library_events();
    let r = app.ops().fixes().fix("tags-out-of-sync", id, "").unwrap();
    assert!(r.ok, "{r:?}");
    assert!(r.job_id.is_none());
    let seen = names(&feed);
    assert!(count(&seen, "AlbumTagsChanged") >= 1, "{seen:?}");
}

#[test]
fn a_fix_that_is_not_attempted_says_so_instead_of_failing() {
    let (app, _s) = checked();
    let id = finding_ids(&app, "inconsistent-artist-sort")[0];
    let r = app
        .ops()
        .fixes()
        .fix("inconsistent-artist-sort", id, "")
        .unwrap();
    assert!(!r.ok);
    assert!(r.message.contains("Resolve"), "{}", r.message);
    // A finding of another check, an unknown one and a check without a fix.
    match app.ops().fixes().fix("track-gaps", id, "") {
        Err(OpError::NotFound { what, .. }) => assert_eq!(what, Subject::Finding),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        app.ops().fixes().fix("track-gaps", 99_999, ""),
        Err(OpError::NotFound { .. })
    ));
}

#[test]
fn with_no_provider_a_cover_fix_queues_assistant_research() {
    let (app, _s) = checked();
    let jobs = app.subscribe_jobs();
    let id = finding_ids(&app, "no-cover-art")[0];
    let r = app.ops().fixes().fix("no-cover-art", id, "").unwrap();
    assert!(r.ok, "{r:?}");
    let group = r.job_id.unwrap();
    let items = app.ops().jobs().research_items(&group).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].kind, "cover.research");
    assert_eq!(job_rows(&app), 1);
    assert!(jobs.has_changed().unwrap());
}

#[test]
fn a_fix_that_enqueues_a_job_wakes_the_jobs_feed_unasked() {
    let (app, _s) = checked_with(|b| b);
    let mut jobs = app.subscribe_jobs();
    jobs.mark_unchanged();
    let id = finding_ids(&app, "no-cover-art")[0];
    let first = app.ops().fixes().fix("no-cover-art", id, "").unwrap();
    assert!(first.ok, "{first:?}");
    let job = first.job_id.clone().expect("a job id");
    assert_eq!(job_rows(&app), 1);
    assert!(jobs.has_changed().unwrap(), "the jobs feed was woken");
    jobs.mark_unchanged();
    // The album now has an unresolved request: a second Fix joins it (the
    // same id) and makes no second request.
    let second = app.ops().fixes().fix("no-cover-art", id, "").unwrap();
    assert!(second.ok, "{second:?}");
    assert_eq!(second.job_id.as_deref(), Some(job.as_str()));
    assert_eq!(job_rows(&app), 1);
    // One unresolved request per album, kind and input (FR-011): a Fix with
    // another hint asks something else, so it is a request of its own.
    let third = app
        .ops()
        .fixes()
        .fix("no-cover-art", id, "the 2012 reissue")
        .unwrap();
    assert!(third.ok, "{third:?}");
    assert_ne!(third.job_id.as_deref(), Some(job.as_str()));
    assert_eq!(job_rows(&app), 2);
}

#[test]
fn quick_fix_all_reports_start_progress_and_done_and_publishes_one_batch() {
    let (app, _s) = checked();
    let feed = app.ops().system().library_events();
    let mut seen: Vec<QuickFixEvent> = Vec::new();
    let end = app
        .ops()
        .fixes()
        .quick_fix_all("no-cover-art", false, &mut |e| {
            seen.push(e);
            Listening::Yes
        })
        .unwrap();
    assert!(
        matches!(seen[0], QuickFixEvent::Start { total: 1 }),
        "{seen:?}"
    );
    assert!(matches!(
        &seen[1],
        QuickFixEvent::Progress {
            outcome: QuickFixOutcome::Skipped,
            ..
        }
    ));
    let QuickFixEvent::Done(summary) = &seen[2] else {
        panic!("{seen:?}")
    };
    assert_eq!(seen.len(), 3);
    assert_eq!((summary.total, summary.skipped), (1, 1));
    assert_eq!(end, QuickFixEnd::Finished(summary.clone()));
    let published = names(&feed);
    assert_eq!(count(&published, "BulkBatchCompleted"), 1, "{published:?}");
    assert_eq!(app.gate().snapshot(), (0, false));
}

#[test]
fn quick_fix_all_publishes_its_batch_even_when_the_sink_goes_away() {
    let (app, _s) = checked();
    let findings = finding_ids(&app, "unidentified-album").len();
    assert!(findings > 2, "{findings}");
    let feed = app.ops().system().library_events();
    let mut calls = 0;
    let end = app
        .ops()
        .fixes()
        .quick_fix_all("unidentified-album", false, &mut |_| {
            calls += 1;
            Listening::Gone
        })
        .unwrap();
    assert_eq!(calls, 1, "not called again once it answered Gone");
    assert_eq!(end, QuickFixEnd::Stopped);
    let published = names(&feed);
    assert_eq!(count(&published, "BulkBatchCompleted"), 1, "{published:?}");
    assert_eq!(app.gate().snapshot(), (0, false));
}

#[test]
fn quick_fix_all_treats_a_panicking_sink_as_gone() {
    let (app, _s) = checked();
    let feed = app.ops().system().library_events();
    let mut calls = 0;
    let end = app
        .ops()
        .fixes()
        .quick_fix_all("unidentified-album", false, &mut |_| {
            calls += 1;
            panic!("sink failed");
        })
        .unwrap();
    assert_eq!((calls, end), (1, QuickFixEnd::Stopped));
    assert_eq!(count(&names(&feed), "BulkBatchCompleted"), 1);
}

#[test]
fn an_unknown_check_is_not_found_before_any_event() {
    let (app, _s) = common::app();
    let mut calls = 0;
    match app
        .ops()
        .fixes()
        .quick_fix_all("no-such-check", false, &mut |_| {
            calls += 1;
            Listening::Yes
        }) {
        Err(OpError::NotFound {
            what, id, message, ..
        }) => {
            assert_eq!(what, Subject::Check);
            assert_eq!(id, "no-such-check");
            assert_eq!(message, "Unknown check_id: 'no-such-check'");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(calls, 0);
    assert_eq!(app.gate().snapshot(), (0, false));
}

/// FR-030. The gate refuses writing work only while a restore runs, so a second
/// quick-fix-all started while one runs is admitted and runs to its end beside
/// it, as a second HTTP client's does: nothing is `Busy`, each run publishes its
/// own `BulkBatchCompleted`, and the gate is free when both are done.
#[test]
fn a_second_quick_fix_all_runs_beside_the_first() {
    let hold = Arc::new(AtomicBool::new(true));
    let entered = Arc::new(AtomicUsize::new(0));
    let (h, e) = (hold.clone(), entered.clone());
    let (app, _s) = checked_with(move |b| {
        b.quick_fix_pause(Arc::new(move || {
            e.fetch_add(1, Ordering::SeqCst);
            while h.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(2));
            }
        }))
    });
    struct Release(Arc<AtomicBool>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.store(false, Ordering::SeqCst);
        }
    }
    let _release = Release(hold.clone());
    let feed = app.ops().system().library_events();
    let first = {
        let app = app.clone();
        std::thread::spawn(move || {
            app.ops()
                .fixes()
                .quick_fix_all("unidentified-album", false, &mut |_| Listening::Yes)
        })
    };
    eventually("the first run between two findings", || {
        entered.load(Ordering::SeqCst) > 0
    });
    assert_eq!(app.gate().snapshot(), (1, false));
    let second = app
        .ops()
        .fixes()
        .quick_fix_all("no-cover-art", false, &mut |_| Listening::Yes);
    assert!(matches!(second, Ok(QuickFixEnd::Finished(_))), "{second:?}");
    assert_eq!(app.gate().snapshot(), (1, false), "the first still runs");
    hold.store(false, Ordering::SeqCst);
    let first = first.join().unwrap();
    assert!(matches!(first, Ok(QuickFixEnd::Finished(_))), "{first:?}");
    assert_eq!(app.gate().snapshot(), (0, false));
    assert_eq!(count(&names(&feed), "BulkBatchCompleted"), 2);
}

#[test]
fn the_filename_rename_applies_a_plan_and_publishes_the_rename() {
    let (app, _s) = checked();
    let finding = app
        .ops()
        .checks()
        .list_findings(&FindingsQuery {
            check_id: Some("filename-mismatch".into()),
            ..FindingsQuery::default()
        })
        .unwrap()
        .into_iter()
        .find(|f| f.summary().contains("Legacy Folder Name"))
        .expect("the legacy folder finding");
    let PlanPreview::Derived { plan, .. } = app
        .ops()
        .checks()
        .filename_plan_preview(finding.id())
        .unwrap()
    else {
        panic!("not derived")
    };
    let dir = plan.dir_rename.expect("a folder rename");
    // Neither a plan nor a folder rename is refused, as is a finding of another check.
    assert!(matches!(
        app.ops()
            .fixes()
            .apply_filename_rename(finding.id(), &[], None),
        Err(OpError::Rejected { .. })
    ));
    let other = finding_ids(&app, "duplicate-tracks")[0];
    assert!(matches!(
        app.ops().fixes().apply_filename_rename(other, &[], None),
        Err(OpError::NotFound { .. })
    ));
    let feed = app.ops().system().library_events();
    let out = app
        .ops()
        .fixes()
        .apply_filename_rename(
            finding.id(),
            &[],
            Some(&PlanEntry {
                old: dir.old,
                new: dir.new,
            }),
        )
        .unwrap();
    assert!(out.ok, "{out:?}");
    assert!(out.detail.is_some_and(|d| d.dir_renamed));
    assert!(count(&names(&feed), "AlbumRenamed") >= 1);
}

#[test]
fn a_duplicate_file_goes_to_the_trash_and_the_album_is_rescanned() {
    let (app, _s) = checked();
    let id = finding_ids(&app, "duplicate-tracks")[0];
    let preview = app.ops().checks().cluster_preview(id).unwrap();
    let album = preview.album_id().to_string();
    let name = preview.filenames()[0].to_string();
    // A name the finding does not list is refused, and nothing moves.
    assert!(matches!(
        app.ops()
            .fixes()
            .delete_duplicate_file(id, "not-a-duplicate.flac"),
        Err(OpError::Rejected { .. })
    ));
    let feed = app.ops().system().library_events();
    let done = app.ops().fixes().delete_duplicate_file(id, &name).unwrap();
    assert!(done.trashed_to.is_some(), "{done:?}");
    let path = std::path::PathBuf::from(app.ops().library().album(&album).unwrap().path());
    assert!(!path.join(&name).exists());
    assert!(count(&names(&feed), "AlbumTagsChanged") >= 1);
}

#[test]
fn an_artist_sort_value_is_written_across_the_cluster() {
    let (app, _s) = checked();
    let id = finding_ids(&app, "inconsistent-artist-sort")[0];
    assert!(matches!(
        app.ops().fixes().apply_cluster_sort(id, "  "),
        Err(OpError::Rejected { message, .. }) if message == "sort_value is required"
    ));
    let feed = app.ops().system().library_events();
    let r = app
        .ops()
        .fixes()
        .apply_cluster_sort(id, "Sorted, Artist")
        .unwrap();
    assert!(r.ok && r.applied >= 1, "{r:?}");
    assert_eq!(r.sort_value, "Sorted, Artist");
    assert!(
        r.message.starts_with("Wrote sort='Sorted, Artist' to "),
        "{}",
        r.message
    );
    let seen = names(&feed);
    assert!(count(&seen, "BulkBatchCompleted") >= 1, "{seen:?}");
}

#[test]
fn a_conversion_is_refused_when_the_plan_changed_and_carries_the_fresh_digest() {
    let (app, _s) = checked();
    let id = finding_ids(&app, "multi-disc-layout")[0];
    let ConversionPreview::Derived { preview, .. } =
        app.ops().checks().conversion_preview(id).unwrap()
    else {
        panic!("not derived")
    };
    let mut seen = Vec::new();
    let err = app
        .ops()
        .fixes()
        .convert_multi_disc(
            id,
            "stale",
            Some(&mut |e| {
                seen.push(e);
                Listening::Yes
            }),
        )
        .unwrap_err();
    // After `Started` (the guard is held), the refusal is the return value.
    assert_eq!(seen, [ConvertEvent::Started]);
    match err {
        OpError::Conflict {
            details: Some(Details::Refusal(r)),
            ..
        } => {
            assert_eq!(r.reason_code, "plan_changed");
            assert_eq!(r.digest.as_deref(), Some(preview.digest().as_str()));
        }
        other => panic!("{other:?}"),
    }
    // Unknown finding; and nothing to abandon.
    assert!(matches!(
        app.ops().fixes().convert_multi_disc(99_999, "x", None),
        Err(OpError::NotFound { .. })
    ));
    match app.ops().fixes().abandon_multi_disc(id).unwrap_err() {
        OpError::Conflict {
            details: Some(Details::Refusal(r)),
            ..
        } => assert_eq!(r.reason_code, "no_conversion"),
        other => panic!("{other:?}"),
    }
    assert_eq!(app.gate().snapshot(), (0, false));
}

#[test]
fn a_conversion_carries_out_the_plan_it_was_shown() {
    let (app, _s) = checked();
    let id = finding_ids(&app, "multi-disc-layout")[0];
    let ConversionPreview::Derived { preview, .. } =
        app.ops().checks().conversion_preview(id).unwrap()
    else {
        panic!("not derived")
    };
    let steps = Mutex::new(Vec::new());
    let feed = app.ops().system().library_events();
    let out = app
        .ops()
        .fixes()
        .convert_multi_disc(
            id,
            &preview.digest(),
            Some(&mut |e| {
                steps.lock().unwrap().push(e);
                Listening::Yes
            }),
        )
        .unwrap();
    assert!(out.ok(), "{}", out.user_message());
    let steps = steps.into_inner().unwrap();
    assert_eq!(steps[0], ConvertEvent::Started);
    assert!(
        steps[1..]
            .iter()
            .all(|e| matches!(e, ConvertEvent::Progress(_)))
    );
    assert!(count(&names(&feed), "BulkBatchCompleted") >= 1);
    assert_eq!(app.gate().snapshot(), (0, false));
}

#[test]
fn every_fix_is_busy_during_a_restore_even_for_something_unknown() {
    let (app, _s) = checked();
    let finding = finding_ids(&app, "tags-out-of-sync")[0];
    let _restore = common::hold_restore(&app);
    let busy = |r: Result<(), OpError>| assert!(matches!(r, Err(OpError::Busy { .. })), "{r:?}");
    busy(
        app.ops()
            .fixes()
            .fix("tags-out-of-sync", finding, "")
            .map(|_| ()),
    );
    busy(
        app.ops()
            .fixes()
            .fix("no-such-check", 99_999, "")
            .map(|_| ()),
    );
    let mut called = false;
    busy(
        app.ops()
            .fixes()
            .quick_fix_all("no-such-check", false, &mut |_| {
                called = true;
                Listening::Yes
            })
            .map(|_| ()),
    );
    busy(
        app.ops()
            .fixes()
            .quick_fix_all("tags-out-of-sync", false, &mut |_| {
                called = true;
                Listening::Yes
            })
            .map(|_| ()),
    );
    assert!(!called);
    busy(
        app.ops()
            .fixes()
            .apply_filename_rename(99_999, &[], None)
            .map(|_| ()),
    );
    busy(
        app.ops()
            .fixes()
            .delete_duplicate_file(99_999, "x")
            .map(|_| ()),
    );
    busy(
        app.ops()
            .fixes()
            .apply_cluster_sort(99_999, "x")
            .map(|_| ()),
    );
    busy(
        app.ops()
            .fixes()
            .convert_multi_disc(
                99_999,
                "x",
                Some(&mut |_| {
                    called = true;
                    Listening::Yes
                }),
            )
            .map(|_| ()),
    );
    busy(app.ops().fixes().abandon_multi_disc(99_999).map(|_| ()));
    assert!(!called, "a refusal before the sink is called");
}

// ---------- bulk research (spec 006) ----------

mod bulk {
    use super::*;
    use musaic_events::{AgentWhat, Event, EventKind, Subscriber, Topic};

    /// Every `AgentStateChanged(work)` id list published from now on.
    pub fn work_events(app: &App) -> Arc<Mutex<Vec<Vec<String>>>> {
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
            .key("bulk-research-work-events")
            .name("bulk-research-work-events"),
        );
        seen
    }

    pub fn run(app: &App, check: &str, research: bool) -> (Vec<QuickFixEvent>, QuickFixEnd) {
        let mut seen = Vec::new();
        let end = app
            .ops()
            .fixes()
            .quick_fix_all(check, research, &mut |e| {
                seen.push(e);
                Listening::Yes
            })
            .unwrap();
        (seen, end)
    }

    pub fn rows(app: &App, sql: &str) -> Vec<(String, Option<String>)> {
        let conn = musaic_db::open_connection(app.db_path()).unwrap();
        let mut stmt = conn.prepare(sql).unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    pub fn summary(seen: &[QuickFixEvent]) -> musaic_app::ops::fixes::QuickFixSummary {
        match seen.last() {
            Some(QuickFixEvent::Done(s)) => s.clone(),
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn bulk_research_sends_the_one_unsettled_album_in_one_batch() {
    let (app, _s) = checked();
    let feed = app.ops().system().library_events();
    let (seen, end) = bulk::run(&app, "no-cover-art", true);
    assert!(matches!(
        &seen[1],
        QuickFixEvent::Progress {
            outcome: QuickFixOutcome::SentForResearch,
            ..
        }
    ));
    let s = bulk::summary(&seen);
    assert_eq!(end, QuickFixEnd::Finished(s.clone()));
    let r = s.research.clone().expect("research counts");
    assert_eq!(
        (s.total, r.sent_for_research, r.already_requested),
        (1, 1, 0)
    );
    let batch = r.batch_id.clone().expect("a batch");
    let rows = bulk::rows(&app, "SELECT kind, batch_id FROM delegated_work");
    assert_eq!(rows, vec![("cover.research".into(), Some(batch.clone()))]);
    let batches = app.ops().jobs().research_batches().unwrap();
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].batch_id, batch);
    assert_eq!(batches[0].check_id, "no-cover-art");
    assert!(batches[0].finished_at.is_some(), "the run reached its end");
    assert!(!batches[0].unfinished);
    assert_eq!((batches[0].size, batches[0].counts.queued), (1, 1));
    // One card per batch, not per album: the jobs list leaves it out.
    assert!(app.ops().jobs().list(None).unwrap().is_empty());
    let published = names(&feed);
    assert_eq!(count(&published, "BulkBatchCompleted"), 1, "{published:?}");
    // No assistant is paired: the request waits all the same.
    assert_eq!(app.ops().jobs().delegated_work().unwrap().len(), 1);
}

#[test]
fn bulk_research_off_makes_no_batch_and_keeps_the_resolver_only_summary() {
    let (app, _s) = checked();
    let (seen, _) = bulk::run(&app, "no-cover-art", false);
    let s = bulk::summary(&seen);
    assert!(s.research.is_none());
    assert_eq!((s.total, s.skipped), (1, 1));
    assert_eq!(job_rows(&app), 0);
    assert!(app.ops().jobs().research_batches().unwrap().is_empty());
}

#[test]
fn bulk_research_skips_missing_albums_and_counts_an_existing_request() {
    let (app, _s) = checked();
    let ids = finding_ids(&app, "unidentified-album");
    assert!(ids.len() > 2);
    let conn = musaic_db::open_connection(app.db_path()).unwrap();
    let album_of = |fid: i64| -> String {
        conn.query_row(
            "SELECT album_id FROM check_findings WHERE id=?1",
            [fid],
            |r| r.get(0),
        )
        .unwrap()
    };
    let missing = album_of(ids[0]);
    let requested = album_of(ids[1]);
    conn.execute(
        "UPDATE albums SET missing_since='2026-01-01T00:00:00Z' WHERE id=?1",
        [&missing],
    )
    .unwrap();
    // A request made on its own before the run.
    let single = app
        .ops()
        .fixes()
        .fix("unidentified-album", ids[1], "")
        .unwrap();
    assert!(single.ok, "{single:?}");
    let (seen, _) = bulk::run(&app, "unidentified-album", true);
    let s = bulk::summary(&seen);
    let r = s.research.clone().unwrap();
    assert_eq!(s.not_applicable, 1, "{seen:?}");
    assert_eq!(r.already_requested, 1, "{seen:?}");
    assert_eq!(
        r.sent_for_research,
        ids.len() - 2 - s.fixed - s.still_open - s.skipped - s.errors
    );
    let rows = bulk::rows(
        &app,
        "SELECT album_id, batch_id FROM delegated_work ORDER BY created_at, id",
    );
    assert!(rows.iter().all(|(album, _)| *album != missing), "{rows:?}");
    // The existing request was not moved into the batch.
    let existing: Vec<_> = rows.iter().filter(|(a, _)| *a == requested).collect();
    assert_eq!(existing.len(), 1);
    assert_eq!(existing[0].1, None);
    assert!(
        rows.iter()
            .filter(|(a, _)| *a != requested)
            .all(|(_, b)| b == &r.batch_id)
    );
}

#[test]
fn a_bulk_research_run_stopped_part_way_keeps_what_it_made_as_unfinished() {
    let (app, _s) = checked();
    assert!(finding_ids(&app, "unidentified-album").len() > 2);
    let mut progress = 0;
    let end = app
        .ops()
        .fixes()
        .quick_fix_all("unidentified-album", true, &mut |e| {
            if matches!(e, QuickFixEvent::Progress { .. }) {
                progress += 1;
                if progress == 2 {
                    return Listening::Gone;
                }
            }
            Listening::Yes
        })
        .unwrap();
    assert_eq!(end, QuickFixEnd::Stopped);
    let made = job_rows(&app);
    assert!((1..=2).contains(&made), "{made}");
    let batches = app.ops().jobs().research_batches().unwrap();
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].finished_at, None);
    assert!(batches[0].unfinished, "no run holds it any more");
    assert_eq!(batches[0].size, made);
}

#[test]
fn a_thousand_unsettled_albums_make_one_batch_of_a_thousand() {
    const N: usize = 1000;
    let (app, s) = checked();
    let lib = s.tmp.path().join("library");
    common::clone_finding_albums(&app, &lib, "no-cover-art", N - 1);
    let work = bulk::work_events(&app);
    let feed = app.ops().system().library_events();
    // Offline every MusicBrainz request fails, and a resolution reached
    // that way is never memoized (the next call asks again), so the memo hit
    // of the request's second resolve is not observable here; the run's
    // shape and scale are.
    let (seen, _) = bulk::run(&app, "no-cover-art", true);
    let s = bulk::summary(&seen);
    let r = s.research.clone().unwrap();
    assert_eq!((s.total, r.sent_for_research), (N, N), "{s:?}");
    let batch = r.batch_id.unwrap();
    let rows = bulk::rows(&app, "SELECT album_id, batch_id FROM delegated_work");
    assert_eq!(rows.len(), N);
    assert!(
        rows.iter()
            .all(|(_, b)| b.as_deref() == Some(batch.as_str()))
    );
    let summary = app.ops().jobs().research_batch(&batch).unwrap();
    assert_eq!(summary.summary.size, N as i64);
    assert_eq!(summary.items.len(), N);
    // Deferred publishing: a handful of relay events, not one per request.
    let events = work.lock().unwrap().clone();
    assert!(events.len() <= 9, "{} work events", events.len());
    assert!(events.iter().all(|ids| ids.len() <= 128));
    assert_eq!(events.iter().map(Vec::len).sum::<usize>(), N);
    assert_eq!(count(&names(&feed), "BulkBatchCompleted"), 1);
}
