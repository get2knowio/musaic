//! The `settings` and `database` operations with no HTTP in scope: events and
//! the job-queue wake come from the operation, refusals are typed, no secret
//! value appears in a result, and a restore guard or running work refuses what
//! it should with nothing changed.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use musaic_app::App;
use musaic_app::ops::OpError;
use musaic_app::ops::settings::{PrefValue, SettingsPatch};
use musaic_events::{Event, EventKind, Subscriber, Topic};

/// Every `SettingsChanged` the bus carries, as the names it lists.
fn record_settings_events(app: &App) -> Arc<Mutex<Vec<Vec<String>>>> {
    let seen: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
    let s2 = Arc::clone(&seen);
    app.bus().register(
        Topic::Kind(EventKind::SettingsChanged),
        Subscriber::from_fn(move |e| {
            if let Event::SettingsChanged { keys } = e {
                s2.lock().unwrap().push(keys.clone());
            }
            Ok(())
        }),
    );
    seen
}

fn settle(app: &App) {
    assert!(app.bus().wait_background_idle(Duration::from_secs(30)));
}

fn max_parallel(patch: i64) -> SettingsPatch {
    SettingsPatch {
        max_parallel: Some(patch),
        ..SettingsPatch::default()
    }
}

#[test]
fn patch_publishes_settings_changed_once_with_names_only() {
    let (app, _s) = common::app();
    let seen = record_settings_events(&app);
    let patch = SettingsPatch {
        max_parallel: Some(3),
        model: Some("some-model".into()),
        acoustid: Some("TOP-SECRET-ACOUSTID-KEY".into()),
        ..SettingsPatch::default()
    };
    let rec = app.ops().settings().patch(patch).unwrap();
    settle(&app);
    assert_eq!(rec.int("ai_max_parallel"), Some(3));
    assert_eq!(rec.str("ai_model_id"), Some("some-model"));
    let events = seen.lock().unwrap();
    assert_eq!(events.len(), 1, "one event for one patch: {events:?}");
    assert_eq!(
        events[0],
        ["ai_max_parallel", "ai_model_id", "acoustid_api_key"]
    );
    assert!(!format!("{events:?}").contains("TOP-SECRET"));
}

#[test]
fn a_patch_wakes_the_jobs_feed_and_a_refused_one_does_not() {
    let (app, _s) = common::app();
    let mut rx = app.subscribe_jobs();
    assert!(!rx.has_changed().unwrap());
    let err = app.ops().settings().patch(max_parallel(99)).unwrap_err();
    assert!(matches!(err, OpError::Rejected { .. }), "{err:?}");
    assert!(!rx.has_changed().unwrap(), "a refused patch wakes nothing");
    app.ops().settings().patch(max_parallel(2)).unwrap();
    assert!(rx.has_changed().unwrap(), "the queue is woken by the patch");
    rx.borrow_and_update();
    let paused = SettingsPatch {
        paused: Some(true),
        ..SettingsPatch::default()
    };
    app.ops().settings().patch(paused).unwrap();
    assert!(rx.has_changed().unwrap());
}

#[test]
fn an_invalid_value_is_rejected_and_changes_nothing() {
    let (app, _s) = common::app();
    let seen = record_settings_events(&app);
    let before = app.ops().settings().get().unwrap();
    for bad in [
        max_parallel(0),
        max_parallel(9),
        SettingsPatch {
            retention: Some(101),
            ..SettingsPatch::default()
        },
        SettingsPatch {
            interval: Some(-1.0),
            ..SettingsPatch::default()
        },
        SettingsPatch {
            provider: Some("not-a-provider".into()),
            ..SettingsPatch::default()
        },
    ] {
        match app.ops().settings().patch(bad) {
            Err(OpError::Rejected { message, .. }) => assert!(!message.is_empty()),
            other => panic!("{other:?}"),
        }
    }
    settle(&app);
    assert_eq!(app.ops().settings().get().unwrap(), before);
    assert!(seen.lock().unwrap().is_empty(), "nothing was written");
}

#[test]
fn a_patch_during_a_restore_is_busy_and_changes_nothing() {
    let (app, _s) = common::app();
    let before = app.ops().settings().get().unwrap();
    let restore = common::hold_restore(&app);
    // Busy, also for a value that is invalid: the guard comes first, as
    // `PATCH /api/settings` answers.
    for p in [max_parallel(3), max_parallel(99)] {
        let err = app.ops().settings().patch(p).unwrap_err();
        assert!(matches!(err, OpError::Busy { .. }), "{err:?}");
    }
    assert!(matches!(
        app.ops().settings().dismiss_acoustid_prompt(),
        Err(OpError::Busy { .. })
    ));
    let theme = [("theme".to_string(), PrefValue::Text("dark".into()))];
    assert!(matches!(
        app.ops().settings().set_ui_prefs(&theme),
        Err(OpError::Busy { .. })
    ));
    // Reads go on.
    assert_eq!(app.ops().settings().get().unwrap(), before);
    assert!(app.ops().settings().ui_prefs().unwrap().theme.is_none());
    drop(restore);
    assert!(app.ops().settings().patch(max_parallel(3)).is_ok());
}

#[test]
fn no_result_holds_a_secret_value() {
    let (app, s) = common::app();
    let secret = "SEKRET-KEY-VALUE-0123456789";
    let patch = SettingsPatch {
        ai_key: Some(secret.into()),
        acoustid: Some(secret.into()),
        firecrawl: Some(secret.into()),
        ..SettingsPatch::default()
    };
    let rec = app.ops().settings().patch(patch).unwrap();
    assert_eq!(rec.bool("acoustid_api_key_set"), Some(true));
    assert_eq!(rec.bool("ai_api_key_set"), Some(true));
    assert!(!format!("{rec:?}").contains(secret));
    assert!(!format!("{:?}", app.ops().settings().get().unwrap()).contains(secret));
    assert!(
        rec.iter().all(|(k, _)| !k.ends_with("_api_key")),
        "keys are reported only as set or not"
    );
    // Not in the database file either: the keys live in the secrets store.
    app.ops().database().info().unwrap();
    let db = std::fs::read(s.config.join("library.db")).unwrap();
    assert!(!db.windows(secret.len()).any(|w| w == secret.as_bytes()));
    // A refusal does not echo a value.
    let bad = SettingsPatch {
        provider: Some(secret.into()),
        ..SettingsPatch::default()
    };
    let err = app.ops().settings().patch(bad).unwrap_err();
    assert!(matches!(err, OpError::Rejected { .. }));
}

#[test]
fn the_acoustid_prompt_is_dismissed_once_and_published() {
    let (app, _s) = common::app();
    let seen = record_settings_events(&app);
    assert!(!app.ops().settings().acoustid_prompt().unwrap().dismissed);
    assert!(
        app.ops()
            .settings()
            .dismiss_acoustid_prompt()
            .unwrap()
            .dismissed
    );
    settle(&app);
    assert!(app.ops().settings().acoustid_prompt().unwrap().dismissed);
    assert_eq!(*seen.lock().unwrap(), [vec!["acoustid_prompt_dismissed"]]);
}

#[test]
fn ui_prefs_are_validated_before_the_guard_and_publish_when_written() {
    let (app, _s) = common::app();
    let seen = record_settings_events(&app);
    let ops = app.ops();
    let prefs = ops.settings().ui_prefs().unwrap();
    assert_eq!((prefs.theme, prefs.tile_columns), (None, None));

    let both = [
        ("theme".to_string(), PrefValue::Text("dark".into())),
        ("tile_columns".to_string(), PrefValue::Whole(5)),
    ];
    let set = ops.settings().set_ui_prefs(&both).unwrap();
    assert_eq!(set.theme.as_deref(), Some("dark"));
    assert_eq!(set.tile_columns, Some(5));
    // Null forgets one, a field left out is untouched; nothing written, no event.
    let forget = [("theme".to_string(), PrefValue::Null)];
    let set = ops.settings().set_ui_prefs(&forget).unwrap();
    assert_eq!((set.theme, set.tile_columns), (None, Some(5)));
    ops.settings().set_ui_prefs(&[]).unwrap();
    settle(&app);
    assert_eq!(
        *seen.lock().unwrap(),
        [vec!["ui_theme", "ui_tile_columns"], vec!["ui_theme"],]
    );

    // Meaning first: refused with the restore guard held too.
    let _restore = common::hold_restore(&app);
    for bad in [
        ("theme", PrefValue::Text("blue".into())),
        ("theme", PrefValue::Other),
        ("tile_columns", PrefValue::Whole(8)),
        ("tile_columns", PrefValue::Other),
        ("colour", PrefValue::Null),
    ] {
        let err = ops
            .settings()
            .set_ui_prefs(&[(bad.0.to_string(), bad.1.clone())])
            .unwrap_err();
        assert!(matches!(err, OpError::Rejected { .. }), "{bad:?}: {err:?}");
    }
    // The first bad value in order names the refusal.
    let two_bad = [
        ("tile_columns".to_string(), PrefValue::Whole(1)),
        ("colour".to_string(), PrefValue::Null),
    ];
    let err = ops.settings().set_ui_prefs(&two_bad).unwrap_err();
    assert!(err.message().starts_with("tile_columns must be"), "{err}");
    // A valid one is Busy.
    assert!(matches!(
        ops.settings().set_ui_prefs(&both),
        Err(OpError::Busy { .. })
    ));
}

#[test]
fn snapshots_can_be_created_listed_deleted_and_restored() {
    let (app, _s) = common::app();
    let ops = app.ops();
    let db = ops.database();
    assert!(db.snapshots().unwrap().is_empty());
    let made = db.create_snapshot().unwrap();
    assert!(made.filename.ends_with(".db") && made.size_bytes > 0);
    let list = db.snapshots().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].filename, made.filename);
    assert!(list[0].valid);

    // A change made after the snapshot is undone by restoring it.
    app.ops().settings().patch(max_parallel(4)).unwrap();
    let info = db.restore_snapshot(&made.filename).unwrap();
    assert!(!info.restored_at.is_empty());
    assert_eq!(
        app.ops().settings().get().unwrap().int("ai_max_parallel"),
        Some(1)
    );

    let gone = db.delete_snapshot(&made.filename).unwrap();
    assert!(gone.deleted);
    assert_eq!(gone.name, made.filename);
    assert!(db.snapshots().unwrap().is_empty());
    // A name that is not a snapshot's is refused, as is one that is not there.
    assert!(matches!(
        db.delete_snapshot("../library.db"),
        Err(OpError::Rejected { .. })
    ));
    assert!(matches!(
        db.restore_snapshot(&made.filename),
        Err(OpError::Rejected { .. })
    ));
}

#[test]
fn snapshots_disabled_is_a_conflict() {
    let (app, _s) = common::app();
    let off = SettingsPatch {
        retention: Some(0),
        ..SettingsPatch::default()
    };
    app.ops().settings().patch(off).unwrap();
    let err = app.ops().database().create_snapshot().unwrap_err();
    assert!(
        matches!(&err, OpError::Conflict { message, .. } if message.starts_with("Snapshotting is disabled")),
        "{err:?}"
    );
}

#[test]
fn a_restore_while_work_holds_the_gate_is_busy_and_work_is_refused_during_one() {
    let (app, _s) = common::app();
    let ops = app.ops();
    let db = ops.database();
    let made = db.create_snapshot().unwrap();
    let backup_dir = db.scratch().unwrap();
    let backup = db.backup_to(backup_dir.path()).unwrap();

    let work = app.gate().begin_work().unwrap();
    for r in [
        db.restore_snapshot(&made.filename),
        db.restore_from(&backup.path),
    ] {
        assert!(matches!(r, Err(OpError::Busy { .. })), "{r:?}");
    }
    assert!(matches!(db.restore_allowed(), Err(OpError::Busy { .. })));
    // Nothing was touched.
    assert_eq!(db.snapshots().unwrap().len(), 1);
    drop(work);
    assert!(db.restore_allowed().is_ok());

    let restore = common::hold_restore(&app);
    assert!(matches!(db.create_snapshot(), Err(OpError::Busy { .. })));
    assert!(matches!(
        db.delete_snapshot(&made.filename),
        Err(OpError::Busy { .. })
    ));
    // Reads go on.
    assert_eq!(db.snapshots().unwrap().len(), 1);
    assert!(db.info().unwrap().exists);
    drop(restore);
    assert!(db.delete_snapshot(&made.filename).unwrap().deleted);
}

#[test]
fn info_backup_and_restore_from_a_file() {
    let (app, s) = common::app();
    let ops = app.ops();
    let db = ops.database();
    let info = db.info().unwrap();
    assert!(info.exists && info.size_bytes > 0);
    assert_eq!(info.path, s.config.join("library.db"));
    assert_eq!(info.schema_version, Some(info.current_schema));

    let scratch = db.scratch().unwrap();
    let backup = db.backup_to(scratch.path()).unwrap();
    assert!(backup.path.starts_with(scratch.path()) && backup.path.is_file());
    assert!(backup.suggested_name.ends_with(".db"));

    app.ops().settings().patch(max_parallel(6)).unwrap();
    let restored = db.restore_from(&backup.path).unwrap();
    assert_eq!(restored.schema_version, info.current_schema);
    assert!(!restored.aside_paths.is_empty());
    assert_eq!(
        app.ops().settings().get().unwrap().int("ai_max_parallel"),
        Some(1)
    );
    drop(scratch);

    // A file that is not a database is refused and the live one stays.
    let junk = s.tmp.path().join("junk.db");
    std::fs::write(&junk, vec![7u8; 4096]).unwrap();
    assert!(matches!(
        db.restore_from(&junk),
        Err(OpError::Rejected { .. })
    ));
    assert!(matches!(
        db.restore_from(&s.tmp.path().join("missing.db")),
        Err(OpError::Rejected { .. })
    ));
    assert!(db.info().unwrap().exists);
    assert!(app.ops().settings().patch(max_parallel(2)).is_ok());
}

#[test]
fn a_one_call_backup_and_an_upload_restore_clean_up_after_themselves() {
    let (app, _s) = common::app();
    let ops = app.ops();
    let db = ops.database();
    let backup = db.backup().unwrap();
    let dir = backup.file.path.parent().unwrap().to_path_buf();
    assert!(backup.file.path.is_file());

    // The upload's directory is gone once the restore returns.
    let upload = db.scratch().unwrap();
    let up_dir = upload.path().to_path_buf();
    let file = up_dir.join("upload.db");
    std::fs::copy(&backup.file.path, &file).unwrap();
    db.restore_upload(upload, &file).unwrap();
    assert!(!up_dir.exists(), "the upload was removed");

    drop(backup);
    assert!(!dir.exists(), "the backup copy goes with its value");
}
