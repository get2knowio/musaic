//! Marking a job acted-on reads the clock once, so the job's `acted_on_at`
//! and the album's `metadata_ai_acted_on_at` are the same instant.

use std::cell::Cell;

use musaic_db::ai_jobs::{self, NewJob};
use musaic_services::host_log::HostLog;

struct Quiet;

impl HostLog for Quiet {
    fn info(&self, _: &str, _: &str) {}
    fn warn(&self, _: &str, msg: &str) {
        panic!("unexpected warning: {msg}");
    }
    fn error(&self, _: &str, msg: &str) {
        panic!("unexpected error: {msg}");
    }
}

#[test]
fn acting_on_a_job_reads_the_clock_once() {
    let tmp = tempfile::tempdir().unwrap();
    musaic_db::Db::open_or_create(tmp.path().join("library.db")).unwrap();
    let mut conn = musaic_db::open_connection(tmp.path().join("library.db")).unwrap();
    ai_jobs::enqueue(
        &mut conn,
        &NewJob {
            album_id: "a",
            album_path: "/p",
            album_title: "",
            job_type: "metadata",
            hint: "",
            attach_cover: true,
        },
        || "j1".to_string(),
        "2026-01-01T00:00:00Z",
    )
    .unwrap()
    .unwrap();
    ai_jobs::mark_completed(&conn, "j1", "{}", "2026-01-01T00:00:01Z").unwrap();

    // Every read of the clock is a different second.
    let reads = Cell::new(0u32);
    let clock = || {
        reads.set(reads.get() + 1);
        format!("2026-01-01T00:00:{:02}Z", 10 + reads.get())
    };
    let notified = Cell::new(0u32);
    let job =
        musaic_services::ai_jobs::mark_acted_on(&conn, "j1", "applied", &clock, &Quiet, &|| {
            notified.set(notified.get() + 1)
        })
        .unwrap();

    assert_eq!(reads.get(), 1, "the clock is read once per acted-on");
    assert_eq!(job.acted_on_at.as_deref(), Some("2026-01-01T00:00:11Z"));
    assert_eq!(notified.get(), 1);
}
