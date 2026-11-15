//! The `ai_jobs` table rules of musaic/jobs.py, on a fresh database
//! (the HTTP replay of testdata/golden/ai/http.json in musaic-api pins the
//! same rules against the Python app's recorded rows; these pin the SQL layer
//! directly, plus the one deliberate tightening).

use musaic_db::ai_jobs::{self, EnqueueRefused, NewJob};
use rusqlite::Connection;

fn db() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("library.db");
    musaic_db::Db::open_or_create(&path).unwrap();
    (tmp, path)
}

fn new<'a>(album: &'a str, job_type: &'static str) -> NewJob<'a> {
    NewJob {
        album_id: album,
        album_path: "/p",
        album_title: "",
        job_type,
        hint: "",
        attach_cover: true,
    }
}

fn enqueue(c: &mut Connection, album: &str, id: &str, now: &str) -> Result<String, EnqueueRefused> {
    ai_jobs::enqueue(c, &new(album, "metadata"), || id.to_string(), now)
        .unwrap()
        .map(|j| j.id)
}

// jobs.py:34-38, 152-162: an unknown type is refused; a queued,
// running or completed job blocks a second one for the album (its id and
// status reported); failed and acted_on do not.
#[test]
fn one_active_job_per_album() {
    let (_t, p) = db();
    let mut c = musaic_db::open_connection(&p).unwrap();
    assert_eq!(
        ai_jobs::enqueue(&mut c, &new("a", "lyrics"), || unreachable!(), "t").unwrap(),
        Err(EnqueueRefused::UnknownJobType("lyrics".into()))
    );
    enqueue(&mut c, "a", "j1", "2026-01-01T00:00:01Z").unwrap();
    for status in ["queued", "running", "completed"] {
        c.execute("UPDATE ai_jobs SET status = ?1 WHERE id = 'j1'", [status])
            .unwrap();
        let mut called = false;
        let r = ai_jobs::enqueue(
            &mut c,
            &new("a", "cover"),
            || {
                called = true;
                "x".into()
            },
            "t",
        )
        .unwrap();
        assert_eq!(
            r,
            Err(EnqueueRefused::Conflict {
                existing_job_id: "j1".into(),
                status: status.into()
            })
        );
        assert!(!called, "no id is made for a refused enqueue");
    }
    for (status, id) in [("failed", "j2"), ("acted_on", "j3")] {
        c.execute("UPDATE ai_jobs SET status = ?1", [status])
            .unwrap();
        enqueue(&mut c, "a", id, "2026-01-01T00:00:02Z").unwrap();
        c.execute("UPDATE ai_jobs SET status = 'failed' WHERE id = ?1", [id])
            .unwrap();
    }
}

// jobs.py:289-296, 348-369: mark_running moves only a queued row;
// retry requeues only a failed row (error and times cleared, created_at
// bumped, the failure excerpt kept); delete by status.
#[test]
fn transitions() {
    let (_t, p) = db();
    let mut c = musaic_db::open_connection(&p).unwrap();
    enqueue(&mut c, "a", "j1", "2026-01-01T00:00:01Z").unwrap();
    assert!(ai_jobs::mark_running(&c, "j1", "2026-01-01T00:00:02Z").unwrap());
    assert!(!ai_jobs::mark_running(&c, "j1", "2026-01-01T00:00:03Z").unwrap());
    assert_eq!(ai_jobs::retry_failed(&mut c, "j1", "t").unwrap(), None);
    ai_jobs::mark_failed(
        &c,
        "j1",
        "boom",
        Some("{\"failure_excerpt\": \"x\"}"),
        "2026-01-01T00:00:04Z",
    )
    .unwrap();
    let j = ai_jobs::retry_failed(&mut c, "j1", "2026-01-01T00:00:05Z")
        .unwrap()
        .unwrap();
    assert_eq!(j.status, "queued");
    assert_eq!(j.error, None);
    assert_eq!(j.started_at, None);
    assert_eq!(j.completed_at, None);
    assert_eq!(j.created_at, "2026-01-01T00:00:05Z");
    assert_eq!(
        j.result_json.as_deref(),
        Some("{\"failure_excerpt\": \"x\"}")
    );
    assert_eq!(ai_jobs::retry_failed(&mut c, "nope", "t").unwrap(), None);
    assert!(!ai_jobs::mark_acted_on(&c, "j1", "maybe", "t").unwrap());
    assert_eq!(
        ai_jobs::delete_jobs_by_status(&c, &["queued".into()]).unwrap(),
        1
    );
    assert_eq!(ai_jobs::delete_jobs_by_status(&c, &[]).unwrap(), 0);
}

// Concurrency, deliberate tightening: the active-job check and the
// insert are one IMMEDIATE transaction, so of many concurrent enqueues for
// one album exactly one succeeds (the Python app checks outside its insert
// transaction, jobs.py:155-185).
#[test]
fn concurrent_enqueues_for_one_album_admit_one() {
    let (_t, p) = db();
    let handles: Vec<_> = (0..8)
        .map(|i| {
            let p = p.clone();
            std::thread::spawn(move || {
                let mut c = musaic_db::open_connection(&p).unwrap();
                enqueue(&mut c, "a", &format!("j{i}"), "2026-01-01T00:00:01Z").is_ok()
            })
        })
        .collect();
    let ok = handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .filter(|x| *x)
        .count();
    assert_eq!(ok, 1);
    let c = musaic_db::open_connection(&p).unwrap();
    assert_eq!(ai_jobs::count_queued(&c).unwrap(), 1);
}
