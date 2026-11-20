//! The `fixes` operations with no HTTP in scope: fix one finding and see the bus
//! event, the AI job a fix falls through to (and the jobs feed it wakes without
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

fn job_rows(app: &App) -> i64 {
    let conn = musaic_db::open_connection(app.db_path()).unwrap();
    conn.query_row("SELECT COUNT(*) FROM ai_jobs", [], |r| r.get(0))
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
fn with_no_model_backend_a_fix_that_needs_one_enqueues_nothing() {
    let (app, _s) = checked();
    let jobs = app.subscribe_jobs();
    let id = finding_ids(&app, "no-cover-art")[0];
    let r = app.ops().fixes().fix("no-cover-art", id, "").unwrap();
    assert!(!r.ok, "{r:?}");
    assert!(r.job_id.is_none());
    assert_eq!(job_rows(&app), 0);
    assert!(!jobs.has_changed().unwrap(), "no job, no wake-up");
}

#[test]
fn a_fix_that_enqueues_a_job_wakes_the_jobs_feed_unasked() {
    let (app, _s) = checked_with(|b| {
        b.ai_provider(Arc::new(musaic_ai::provider::replay::ReplayProvider::new(
            "claude",
        )))
    });
    let mut jobs = app.subscribe_jobs();
    jobs.mark_unchanged();
    let id = finding_ids(&app, "no-cover-art")[0];
    let first = app.ops().fixes().fix("no-cover-art", id, "").unwrap();
    assert!(first.ok, "{first:?}");
    let job = first.job_id.clone().expect("a job id");
    assert_eq!(job_rows(&app), 1);
    assert!(jobs.has_changed().unwrap(), "the jobs feed was woken");
    jobs.mark_unchanged();
    // The album now has an active job: the answer is the conflict, not an error,
    // and nothing was enqueued, so nothing wakes the feed.
    let second = app.ops().fixes().fix("no-cover-art", id, "").unwrap();
    assert!(!second.ok);
    assert_eq!(second.job_id.as_deref(), Some(job.as_str()));
    assert!(
        second.message.starts_with("Already has an active job ("),
        "{}",
        second.message
    );
    assert_eq!(job_rows(&app), 1);
    assert!(!jobs.has_changed().unwrap());
}

#[test]
fn quick_fix_all_reports_start_progress_and_done_and_publishes_one_batch() {
    let (app, _s) = checked();
    let feed = app.ops().system().library_events();
    let mut seen: Vec<QuickFixEvent> = Vec::new();
    let end = app
        .ops()
        .fixes()
        .quick_fix_all("no-cover-art", &mut |e| {
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
    assert_eq!(end, QuickFixEnd::Finished(*summary));
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
        .quick_fix_all("unidentified-album", &mut |_| {
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
        .quick_fix_all("unidentified-album", &mut |_| {
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
    match app.ops().fixes().quick_fix_all("no-such-check", &mut |_| {
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
                .quick_fix_all("unidentified-album", &mut |_| Listening::Yes)
        })
    };
    eventually("the first run between two findings", || {
        entered.load(Ordering::SeqCst) > 0
    });
    assert_eq!(app.gate().snapshot(), (1, false));
    let second = app
        .ops()
        .fixes()
        .quick_fix_all("no-cover-art", &mut |_| Listening::Yes);
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
            .quick_fix_all("no-such-check", &mut |_| {
                called = true;
                Listening::Yes
            })
            .map(|_| ()),
    );
    busy(
        app.ops()
            .fixes()
            .quick_fix_all("tags-out-of-sync", &mut |_| {
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
