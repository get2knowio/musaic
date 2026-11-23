use std::sync::Arc;

use musaic_services::ScanStatus;
use musaic_services::scan::{CancelScan, ScanDeps, ScanService, StartScan};

#[test]
fn a_completed_scan_handle_keeps_its_result_and_cannot_cancel_the_next_scan() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("library");
    std::fs::create_dir(&root).unwrap();
    let db = dir.path().join("library.db");
    musaic_db::Db::open_or_create(&db).unwrap();
    let deps = ScanDeps {
        open_db: Arc::new(move || musaic_db::open_connection(&db).map_err(|e| e.to_string())),
        bus: Arc::new(musaic_events::Bus::new()),
        clock: Arc::new(|| "2026-11-22T19:08:44Z".into()),
        track_order: Default::default(),
        after_scan: None,
        on_album: None,
        on_worker_done: None,
        log: None,
    };
    let service = ScanService::new();
    let (first, first_run) =
        service.start_observed_scan(root.to_str().unwrap(), false, deps.clone());
    let StartScan::Started(_, first) = first else {
        panic!("first scan starts")
    };
    first.run();
    let finished = first_run.state();
    assert_eq!(finished.status, ScanStatus::Done);

    let (second, second_run) = service.start_observed_scan(root.to_str().unwrap(), true, deps);
    let StartScan::Started(_, second) = second else {
        panic!("second scan starts")
    };
    assert_eq!(first_run.state(), finished);
    assert!(matches!(first_run.cancel(), CancelScan::NotRunning(state) if state == finished));
    assert!(!second_run.state().cancel_requested);
    second.run();
    assert_eq!(second_run.state().status, ScanStatus::Done);
}
