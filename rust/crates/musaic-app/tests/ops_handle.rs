//! The empty surface: `App::ops()` reaches every group, and a `Feed` yields the
//! current value first and ends on shutdown.

mod common;

use std::time::Duration;

use musaic_app::ops::{Feed, OpError};
use tokio::sync::watch;

#[test]
fn ops_reaches_every_group() {
    let (app, _scratch) = common::app();
    let ops = app.ops();
    let _ = (
        ops.system(),
        ops.library(),
        ops.scan(),
        ops.settings(),
        ops.database(),
        ops.tags(),
        ops.analyze(),
        ops.apply(),
        ops.checks(),
        ops.fixes(),
        ops.jobs(),
    );
}

#[test]
fn a_held_restore_refuses_work_as_busy() {
    let (app, _scratch) = common::app();
    let _guard = common::hold_restore(&app);
    let err: OpError = app.gate().begin_work().unwrap_err().into();
    assert!(matches!(err, OpError::Busy { .. }));
}

#[test]
fn feed_yields_current_then_changes_then_ends_on_shutdown() {
    let (app, _scratch) = common::app();
    let (tx, rx) = watch::channel(1u64);
    let mut feed = Feed::new(rx, app.shutdown_signal());
    assert_eq!(feed.next(Duration::from_millis(50)), Some(1));
    assert_eq!(
        feed.next(Duration::from_millis(30)),
        None,
        "no change, timeout"
    );
    tx.send_replace(2);
    assert_eq!(feed.next(Duration::from_millis(500)), Some(2));
    app.begin_shutdown();
    assert_eq!(feed.next(Duration::from_millis(500)), None);
}
