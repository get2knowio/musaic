//! The `checks` operations with no HTTP in scope: list and read, run one and read
//! typed findings, ignore and unignore, the three previews re-derived from the
//! current state, the canonical recording, the sweep and the check-runs feed.
//! A run publishes `ChecksRerunFinished` for the library-events feed; a restore
//! running makes the writing operations `Busy` before they look at anything.

mod common;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use musaic_app::App;
use musaic_app::ops::checks::{
    CheckList, CheckRunsState, ConversionPreview, FindingsQuery, PlanPreview,
};
use musaic_app::ops::{OpError, Subject};

const SHORT: Duration = Duration::from_millis(500);

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
fn checked() -> (App, common::Scratch) {
    let (app, s) = common::app_quiet_pool(|b| b);
    scan_and_run(&app, &s);
    (app, s)
}

fn scan_and_run(app: &App, s: &common::Scratch) {
    common::configure(app, s);
    let n = app.ops().scan().subscribe(false).unwrap().count();
    assert!(n > 2);
    eventually("the work guard released", || {
        app.gate().snapshot() == (0, false)
    });
    common::wait_for_fingerprints(app);
    for c in app.ops().checks().list().unwrap() {
        let run = app
            .ops()
            .checks()
            .run(c.check_id(), "all", None)
            .expect("the check runs");
        assert_eq!(run.status, "completed", "{}", c.check_id());
    }
}

fn findings_of(app: &App, check: &str) -> Vec<musaic_app::ops::record::FindingRecord> {
    app.ops()
        .checks()
        .list_findings(&FindingsQuery {
            check_id: Some(check.into()),
            ..FindingsQuery::default()
        })
        .unwrap()
}

fn album_path(app: &App, album_id: &str) -> PathBuf {
    PathBuf::from(app.ops().library().album(album_id).unwrap().path())
}

#[test]
fn list_and_get_read_typed_records() {
    let (app, _s) = common::app();
    let checks = app.ops().checks().list().unwrap();
    assert!(checks.len() >= 8, "{} checks", checks.len());
    let first = checks[0].check_id().to_string();
    assert!(checks.iter().all(|c| c.open_count() == 0));
    assert!(checks.iter().all(|c| c.last_run_status().is_none()));
    let detail = app.ops().checks().get(&first).unwrap();
    assert_eq!(detail.check_id(), first);
    assert_eq!((detail.run_count(), detail.finding_count()), (0, 0));
}

#[test]
fn an_unknown_check_is_not_found() {
    let (app, _s) = common::app();
    match app.ops().checks().get("no-such-check") {
        Err(OpError::NotFound { what, id, message }) => {
            assert_eq!(what, Subject::Check);
            assert_eq!(id, "no-such-check");
            assert_eq!(message, "Unknown check_id: 'no-such-check'");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_run_records_typed_findings() {
    let (app, _s) = checked();
    let found = findings_of(&app, "filename-mismatch");
    assert!(!found.is_empty());
    let run = app
        .ops()
        .checks()
        .run("filename-mismatch", "all", None)
        .unwrap();
    assert_eq!(run.check_id, "filename-mismatch");
    assert_eq!(run.issue_count as usize, found.len());
    assert!(found.iter().all(|f| f.check_id() == "filename-mismatch"));
    assert!(found.iter().all(|f| f.album_id().is_some()));
    assert!(found.iter().all(|f| f.ignored_at().is_none()));
    let detail = app.ops().checks().get("filename-mismatch").unwrap();
    assert_eq!(detail.finding_count(), found.len());
    assert!(detail.run_count() >= 2);
    let listed = app.ops().checks().list().unwrap();
    let mine = listed
        .iter()
        .find(|c| c.check_id() == "filename-mismatch")
        .unwrap();
    assert_eq!(mine.open_count() as usize, found.len());
    assert_eq!(mine.last_run_status(), Some("completed"));
}

#[test]
fn a_run_over_named_albums_is_limited_to_them() {
    let (app, _s) = checked();
    let found = findings_of(&app, "filename-mismatch");
    let album = found[0].album_id().unwrap().to_string();
    let run = app
        .ops()
        .checks()
        .run("filename-mismatch", "albums", Some(&[album]))
        .unwrap();
    assert_eq!(run.status, "completed");
    assert!(run.issue_count >= 1);
}

#[test]
fn a_run_that_cannot_start_is_refused_in_the_routes_order() {
    let (app, s) = common::app();
    // No library yet: scope first, then the library.
    assert!(matches!(
        app.ops().checks().run("no-such-check", "bogus", None),
        Err(OpError::Rejected { message, .. }) if message == "scope must be 'all' or 'albums'"
    ));
    assert!(matches!(
        app.ops().checks().run("no-such-check", "incremental", None),
        Err(OpError::Rejected { .. })
    ));
    assert!(matches!(
        app.ops().checks().run("no-such-check", "all", None),
        Err(OpError::NoLibrary { message, .. }) if message == "No library configured"
    ));
    common::configure(&app, &s);
    // An unknown check and an albums scope without ids are refused, not 404.
    match app.ops().checks().run("no-such-check", "all", None) {
        Err(OpError::Rejected { message, .. }) => {
            assert_eq!(message, "Unknown check_id: 'no-such-check'");
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        app.ops().checks().run("filename-mismatch", "albums", None),
        Err(OpError::Rejected { .. })
    ));
}

#[test]
fn a_run_during_a_restore_is_busy_before_anything_is_looked_at() {
    let (app, s) = checked();
    let _restore = common::hold_restore(&app);
    for scope in ["all", "bogus"] {
        assert!(
            matches!(
                app.ops().checks().run("filename-mismatch", scope, None),
                Err(OpError::Busy { .. })
            ),
            "{scope}"
        );
    }
    let id = findings_of(&app, "filename-mismatch")[0].id();
    assert!(matches!(
        app.ops()
            .checks()
            .ignore_finding("filename-mismatch", id, None),
        Err(OpError::Busy { .. })
    ));
    assert!(matches!(
        app.ops().checks().unignore_finding("filename-mismatch", id),
        Err(OpError::Busy { .. })
    ));
    // Reads answer while a restore runs.
    assert!(app.ops().checks().list().is_ok());
    assert!(app.ops().checks().filename_plan_preview(id).is_ok());
    drop(s);
}

#[test]
fn a_run_publishes_checks_rerun_finished_for_the_library_events_feed() {
    let (app, _s) = checked();
    let events = app.ops().system().library_events();
    app.ops()
        .checks()
        .run("filename-mismatch", "all", None)
        .unwrap();
    let mut seen = None;
    while let Some(e) = events.next(SHORT) {
        if e.name() == "ChecksRerunFinished" {
            seen = Some(e);
            break;
        }
    }
    let e = seen.expect("ChecksRerunFinished reached the feed");
    assert!(e.payload_json().contains("filename-mismatch"));
}

#[test]
fn ignore_then_unignore_a_finding() {
    let (app, _s) = checked();
    let before = findings_of(&app, "filename-mismatch");
    let id = before[0].id();
    // A finding of another check is not this check's.
    match app.ops().checks().ignore_finding("track-gaps", id, None) {
        Err(OpError::NotFound { what, .. }) => assert_eq!(what, Subject::Finding),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        app.ops()
            .checks()
            .ignore_finding("filename-mismatch", 99_999, None),
        Err(OpError::NotFound { .. })
    ));
    let ignored = app
        .ops()
        .checks()
        .ignore_finding("filename-mismatch", id, Some("on purpose"))
        .unwrap()
        .expect("the finding");
    assert_eq!(ignored.id(), id);
    assert!(ignored.ignored_at().is_some());
    assert_eq!(
        findings_of(&app, "filename-mismatch").len(),
        before.len() - 1
    );
    let back = app
        .ops()
        .checks()
        .unignore_finding("filename-mismatch", id)
        .unwrap()
        .expect("the finding");
    assert!(back.ignored_at().is_none());
    assert_eq!(findings_of(&app, "filename-mismatch").len(), before.len());
}

#[test]
fn the_plan_preview_re_derives_from_the_current_state() {
    let (app, _s) = checked();
    let finding = findings_of(&app, "filename-mismatch")
        .into_iter()
        .find(|f| f.summary().contains("Legacy Folder Name"))
        .expect("the legacy folder finding");
    let album = finding.album_id().unwrap().to_string();
    let first = app
        .ops()
        .checks()
        .filename_plan_preview(finding.id())
        .unwrap();
    match &first {
        PlanPreview::Derived { album_id, plan } => {
            assert_eq!(album_id, &album);
            assert!(plan.dir_rename.is_some());
            assert!(!plan.album_missing);
        }
        other => panic!("{other:?}"),
    }
    // The folder goes away between two calls.
    let path = album_path(&app, &album);
    std::fs::rename(&path, path.with_file_name("moved away")).unwrap();
    let second = app
        .ops()
        .checks()
        .filename_plan_preview(finding.id())
        .unwrap();
    assert_ne!(first, second);
    match second {
        PlanPreview::Derived { plan, .. } => assert!(plan.album_missing),
        other => panic!("{other:?}"),
    }
    // A finding of another check is not a plan.
    let other = findings_of(&app, "duplicate-tracks")[0].id();
    assert!(matches!(
        app.ops().checks().filename_plan_preview(other),
        Err(OpError::NotFound { .. })
    ));
}

#[test]
fn the_cluster_preview_re_derives_from_the_current_state() {
    let (app, _s) = checked();
    let finding = findings_of(&app, "duplicate-tracks")
        .into_iter()
        .next()
        .expect("a duplicate");
    let first = app.ops().checks().cluster_preview(finding.id()).unwrap();
    assert_eq!(first.album_id(), finding.album_id().unwrap());
    assert!(!first.is_empty());
    assert!(first.removed_filenames().is_empty());
    let gone = first.filenames()[0].to_string();
    // Its fingerprint row goes between two calls.
    let conn = musaic_db::open_connection(app.db_path()).unwrap();
    conn.execute(
        "DELETE FROM track_fingerprints WHERE album_id = ?1 AND filename = ?2",
        rusqlite::params![first.album_id(), gone],
    )
    .unwrap();
    let second = app.ops().checks().cluster_preview(finding.id()).unwrap();
    assert_ne!(first, second);
    assert!(second.removed_filenames().contains(&gone.as_str()));
    let other = findings_of(&app, "filename-mismatch")[0].id();
    assert!(matches!(
        app.ops().checks().cluster_preview(other),
        Err(OpError::NotFound { .. })
    ));
}

#[test]
fn the_conversion_preview_re_derives_from_the_current_state() {
    let (app, _s) = checked();
    let finding = findings_of(&app, "multi-disc-layout")
        .into_iter()
        .next()
        .expect("a set");
    let first = app.ops().checks().conversion_preview(finding.id()).unwrap();
    let ConversionPreview::Derived {
        finding_id,
        records_dir,
        preview,
        ..
    } = &first
    else {
        panic!("{first:?}")
    };
    assert_eq!(*finding_id, finding.id());
    assert!(records_dir.is_some());
    assert!(preview.members.len() >= 2);
    // Another disc's folder goes away between two calls.
    let other = preview
        .members
        .iter()
        .find(|m| m.album_id != preview.album_id)
        .expect("another disc");
    std::fs::remove_dir_all(album_path(&app, &other.album_id)).unwrap();
    let second = app.ops().checks().conversion_preview(finding.id()).unwrap();
    assert_ne!(first, second);
    let wrong = findings_of(&app, "filename-mismatch")[0].id();
    assert!(matches!(
        app.ops().checks().conversion_preview(wrong),
        Err(OpError::NotFound { .. })
    ));
}

#[test]
fn the_canonical_recording_is_an_answer_with_a_reason_not_an_error() {
    let (app, _s) = checked();
    let finding = findings_of(&app, "duplicate-tracks")
        .into_iter()
        .next()
        .expect("a duplicate");
    let c = app
        .ops()
        .checks()
        .duplicate_canonical(finding.id())
        .unwrap();
    assert!(c.title.is_none() && c.artist.is_none());
    let reason = c.reason.expect("no title has a reason");
    assert!(
        ["no_mbid", "unreachable"].contains(&reason.code),
        "{reason:?}"
    );
    let other = findings_of(&app, "filename-mismatch")[0].id();
    assert!(matches!(
        app.ops().checks().duplicate_canonical(other),
        Err(OpError::NotFound {
            what: Subject::Finding,
            ..
        })
    ));
}

#[test]
fn the_sweep_reports_idle_without_a_sweep() {
    let (app, _s) = common::app();
    let s = app.ops().checks().sweep_status();
    assert_eq!(s, app.sweeper().state());
}

fn listed(state: &CheckRunsState) -> &[musaic_app::ops::record::CheckSummaryRecord] {
    match &state.checks {
        CheckList::Listed(l) => l,
        other => panic!("{other:?}"),
    }
}

fn last_status(state: &CheckRunsState, check: &str) -> Option<String> {
    listed(state)
        .iter()
        .find(|c| c.check_id() == check)
        .and_then(|c| c.last_run_status().map(str::to_string))
}

#[test]
fn the_feed_starts_with_the_checks_and_the_sweep() {
    let (app, _s) = common::app();
    let mut feed = app.ops().checks().runs_feed();
    let first = feed.next(SHORT).expect("the first value");
    assert_eq!(
        listed(&first).len(),
        app.ops().checks().list().unwrap().len()
    );
    assert_eq!(first.sweep, Some(app.sweeper().state()));
    assert!(
        feed.next(Duration::from_millis(40)).is_none(),
        "nothing changed"
    );
}

#[test]
fn the_feed_yields_the_running_state_on_a_run_start_and_a_value_on_its_end() {
    let (app, s) = common::app();
    common::configure(&app, &s);
    let mut feed = app.ops().checks().runs_feed();
    feed.next(SHORT).unwrap();
    // What a run does first: a `running` row, announced.
    let conn = musaic_db::open_connection(app.db_path()).unwrap();
    let run_id =
        musaic_db::findings::insert_run(&conn, "track-gaps", &app.now_iso(), "/lib", "all")
            .unwrap();
    (app.0.checks_env.notify)();
    let started = feed.next(SHORT).expect("a value on the start");
    assert_eq!(
        last_status(&started, "track-gaps").as_deref(),
        Some("running")
    );
    assert_eq!(started.sweep, None, "the sweep did not change");
    // And its end.
    musaic_db::findings::complete_run(&conn, run_id, &app.now_iso(), 0, 1).unwrap();
    (app.0.checks_env.notify)();
    let ended = feed.next(SHORT).expect("a value on the end");
    assert_eq!(
        last_status(&ended, "track-gaps").as_deref(),
        Some("completed")
    );
    // A real run announces itself the same way.
    app.ops().checks().run("track-gaps", "all", None).unwrap();
    let mut last = None;
    while let Some(v) = feed.next(Duration::from_millis(100)) {
        last = Some(v);
    }
    let last = last.expect("values from the run");
    assert_eq!(
        last_status(&last, "track-gaps").as_deref(),
        Some("completed")
    );
}

#[test]
fn the_feed_yields_a_value_after_an_ignore_and_after_an_unignore() {
    let (app, _s) = checked();
    let id = findings_of(&app, "track-gaps")[0].id();
    let open = |v: &CheckRunsState| {
        listed(v)
            .iter()
            .find(|c| c.check_id() == "track-gaps")
            .unwrap()
            .open_count()
    };
    let mut feed = app.ops().checks().runs_feed();
    let base = open(&feed.next(SHORT).unwrap());
    app.ops()
        .checks()
        .ignore_finding("track-gaps", id, None)
        .unwrap();
    let after_ignore = feed.next(SHORT).expect("a value after the ignore");
    assert_eq!(open(&after_ignore), base - 1);
    app.ops()
        .checks()
        .unignore_finding("track-gaps", id)
        .unwrap();
    let after_unignore = feed.next(SHORT).expect("a value after the unignore");
    assert_eq!(open(&after_unignore), base);
}

#[test]
fn a_change_of_the_sweep_alone_is_a_value_without_the_checks() {
    let (app, _s) = common::app();
    let mut feed = app.ops().checks().runs_feed();
    feed.next(SHORT).unwrap();
    app.sweeper().changed().set();
    let v = feed.next(SHORT).expect("a value on the sweep's change");
    assert_eq!(v.checks, CheckList::Unchanged);
    assert_eq!(v.sweep, Some(app.sweeper().state()));
}

#[test]
fn the_feed_carries_the_sweeps_progress() {
    let (app, s) = common::app_quiet_pool(|b| {
        b.check_sweep(true)
            .subscribers(true)
            .sweep_network(false)
            .sweep_settle(Duration::from_millis(50))
    });
    let mut feed = app.ops().checks().runs_feed();
    feed.next(SHORT).unwrap();
    common::configure(&app, &s);
    app.ops().scan().subscribe(false).unwrap().count();
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut progressed = false;
    while Instant::now() < deadline && !progressed {
        if let Some(v) = feed.next(Duration::from_millis(200)) {
            progressed = v.sweep.is_some_and(|s| s.total > 0);
        }
    }
    assert!(progressed, "the sweep's progress reached the feed");
}

#[test]
fn the_feed_ends_when_the_application_shuts_down() {
    let (app, _s) = common::app();
    let mut feed = app.ops().checks().runs_feed();
    feed.next(SHORT).unwrap();
    app.begin_shutdown();
    assert!(feed.next(SHORT).is_none());
}
