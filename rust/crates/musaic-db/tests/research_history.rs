//! `research_history`: the old job records schema v24 keeps, their link to
//! requests, the history-only records no agent ever sees, and how both move
//! with their album's id.

use musaic_db::Db;
use musaic_db::delegated_work::{self as queue, List};
use musaic_db::renames::{album_key_in_use, move_album_rows, rename_album_directory};
use musaic_db::research_history::{self as history, JobRow};
use rusqlite::{Connection, params};

const LIB: &str = "/music";

fn db() -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let (db, _) = Db::open_or_create(dir.path().join("library.db")).unwrap();
    db.connection()
        .execute(
            "INSERT INTO libraries (path, scanned_at) VALUES (?1, 'x')",
            [LIB],
        )
        .unwrap();
    (dir, db)
}

fn album(c: &Connection, id: &str) {
    c.execute(
        "INSERT INTO albums (id, library_path, path, dir_name) VALUES (?1, ?2, ?3, ?1)",
        params![id, LIB, format!("{LIB}/{id}")],
    )
    .unwrap();
}

fn job(c: &Connection, id: &str, album: &str, job_type: &str, status: &str, created: &str) {
    let row = JobRow {
        album_path: format!("{LIB}/{album}"),
        album_title: format!("Title {id}"),
        hint: format!("hint {id}"),
        acted_on_action: (status == "acted_on").then(|| "applied".into()),
        result_json: Some("not json {".into()),
        ..JobRow::seed(id, album, job_type, status, created)
    };
    history::import_legacy_job(c, &row).unwrap();
}

fn count(c: &Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}

/// Three records: two a request names, one of a type no request kind
/// matches (history-only).
fn seeded() -> (tempfile::TempDir, Db) {
    let (dir, db) = db();
    let c = db.connection();
    album(c, "a");
    album(c, "b");
    job(c, "j1", "a", "metadata", "queued", "2026-01-01T00:00:00Z");
    job(
        c,
        "j2",
        "a",
        "metadata+cover",
        "failed",
        "2026-01-02T00:00:00Z",
    );
    job(c, "j3", "b", "lyrics", "acted_on", "2026-01-03T00:00:00Z");
    (dir, db)
}

#[test]
fn get_and_list_show_records_as_stored_newest_first() {
    let (_d, db) = seeded();
    let c = db.connection();
    let j1 = history::get(c, "j1").unwrap().unwrap();
    assert_eq!(j1.album_title, "Title j1");
    assert_eq!(j1.hint, "hint j1");
    assert_eq!(j1.created_at, "2026-01-01T00:00:00Z");
    // An unreadable stored result is kept as it is.
    assert_eq!(j1.result_json.as_deref(), Some("not json {"));
    assert!(history::get(c, "nope").unwrap().is_none());

    let ids = |rows: Vec<JobRow>| rows.into_iter().map(|r| r.id).collect::<Vec<_>>();
    assert_eq!(
        ids(history::list(c, None, history::LIST_LIMIT).unwrap()),
        ["j3", "j2", "j1"]
    );
    assert_eq!(ids(history::list(c, None, 1).unwrap()), ["j3"]);
    assert_eq!(
        ids(history::list(c, Some(&["failed".into(), "queued".into()]), 10).unwrap()),
        ["j2", "j1"]
    );
    assert!(history::list(c, Some(&[]), 10).unwrap().is_empty());
    assert_eq!(
        ids(history::list_history_only(c, None, 10).unwrap()),
        ["j3"]
    );
}

#[test]
fn a_history_only_record_is_never_selected_by_any_agent_query() {
    let (_d, db) = seeded();
    let c = db.connection();
    // j1 -> one request, j2 -> two in one group, j3 -> none.
    assert_eq!(count(c, "SELECT COUNT(*) FROM delegated_work"), 3);
    assert_eq!(
        count(
            c,
            "SELECT COUNT(*) FROM delegated_work WHERE legacy_job_id = 'j3'"
        ),
        0
    );
    // Make every request agent-visible as far as the row allows, so the
    // only thing keeping j3 out is that it is not a request.
    c.execute(
        "UPDATE delegated_work SET base_revision = 'rev' WHERE state = 'queued'",
        [],
    )
    .unwrap();
    let listed = queue::list(
        c,
        List {
            principal_id: "agent",
            state: None,
            kind: None,
            batch_id: None,
            after: None,
            limit: 100,
        },
    )
    .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].legacy_job_id.as_deref(), Some("j1"));
    assert!(
        queue::desktop_list(c)
            .unwrap()
            .iter()
            .all(|w| w.legacy_job_id.as_deref() != Some("j3"))
    );
    // It blocks nothing.
    assert!(queue::unresolved_for_albums(c, &["b"]).unwrap().is_empty());
    assert_eq!(queue::unresolved_for_albums(c, &["a"]).unwrap().len(), 1);
}

#[test]
fn delete_and_delete_for_statuses_leave_linked_records_to_their_requests() {
    let (_d, db) = seeded();
    let c = db.connection();
    // Linked records are not history-only: a status sweep leaves them.
    assert_eq!(
        history::delete_for_statuses(c, &["failed".into(), "queued".into()]).unwrap(),
        0
    );
    assert_eq!(history::delete_for_statuses(c, &[]).unwrap(), 0);
    assert_eq!(
        history::delete_for_statuses(c, &["acted_on".into()]).unwrap(),
        1
    );
    assert!(history::get(c, "j3").unwrap().is_none());
    assert!(history::delete(c, "j1").unwrap());
    assert!(!history::delete(c, "j1").unwrap());
    // The record's requests are the caller's.
    assert_eq!(
        count(
            c,
            "SELECT COUNT(*) FROM delegated_work WHERE legacy_job_id = 'j1'"
        ),
        1
    );
}

#[test]
fn repoint_moves_every_record_of_an_album() {
    let (_d, db) = seeded();
    let c = db.connection();
    assert_eq!(history::repoint(c, "a", "z").unwrap(), 2);
    assert_eq!(history::repoint(c, "a", "z").unwrap(), 0);
    let j1 = history::get(c, "j1").unwrap().unwrap();
    assert_eq!(j1.album_id, "z");
    // A display copy: keeps the old text.
    assert_eq!(j1.album_path, format!("{LIB}/a"));
    assert_eq!(history::get(c, "j3").unwrap().unwrap().album_id, "b");
}

#[test]
fn import_twice_adds_nothing() {
    let (_d, db) = seeded();
    let c = db.connection();
    job(
        c,
        "j2",
        "a",
        "metadata+cover",
        "failed",
        "2026-01-02T00:00:00Z",
    );
    assert_eq!(count(c, "SELECT COUNT(*) FROM research_history"), 3);
    assert_eq!(count(c, "SELECT COUNT(*) FROM delegated_work"), 3);
}

#[test]
fn an_album_rename_moves_requests_and_history_in_its_transaction() {
    let (_d, mut db) = seeded();
    let moved = rename_album_directory(db.connection_mut(), "a", "a2", "/music/a2", "a2").unwrap();
    // Two old jobs (one with two requests): counted as two jobs.
    assert_eq!(moved.ai_jobs, 2);
    let c = db.connection();
    assert_eq!(
        count(
            c,
            "SELECT COUNT(*) FROM delegated_work WHERE album_id = 'a2'"
        ),
        3
    );
    assert_eq!(
        count(
            c,
            "SELECT COUNT(*) FROM research_history WHERE album_id = 'a2'"
        ),
        2
    );
    assert_eq!(
        count(
            c,
            "SELECT (SELECT COUNT(*) FROM delegated_work WHERE album_id = 'a')
                  + (SELECT COUNT(*) FROM research_history WHERE album_id = 'a')"
        ),
        0
    );

    // A rollback of the caller's transaction leaves everything where it was.
    {
        let tx = musaic_db::immediate(c).unwrap();
        let m = move_album_rows(&tx, "a2", "a3", "/music/a3", "a3").unwrap();
        assert_eq!(m.ai_jobs, 2);
        tx.rollback().unwrap();
    }
    assert_eq!(
        count(
            c,
            "SELECT COUNT(*) FROM delegated_work WHERE album_id = 'a2'"
        ),
        3
    );
    assert_eq!(
        count(
            c,
            "SELECT COUNT(*) FROM research_history WHERE album_id = 'a2'"
        ),
        2
    );
}

#[test]
fn a_native_request_counts_as_one_job_when_it_moves() {
    let (_d, mut db) = db();
    album(db.connection(), "a");
    queue::enqueue(
        db.connection(),
        &[queue::NewWork {
            id: "w",
            group_id: "g",
            kind: "cover.research",
            album_id: "a",
            base_revision: "rev",
            input_json: "{}",
            input_digest: "input",
            max_attempts: queue::DEFAULT_MAX_ATTEMPTS,
        }],
        1_000,
    )
    .unwrap();
    let moved = rename_album_directory(db.connection_mut(), "a", "b", "/music/b", "b").unwrap();
    assert_eq!(moved.ai_jobs, 1);
    assert_eq!(
        queue::get(db.connection(), "w").unwrap().unwrap().album_id,
        "b"
    );
}

#[test]
fn an_id_still_holding_research_is_in_use() {
    let (_d, db) = db();
    let c = db.connection();
    // No album row under these ids: only their research holds them.
    job(
        c,
        "j",
        "gone-history",
        "lyrics",
        "failed",
        "2026-01-01T00:00:00Z",
    );
    queue::enqueue(
        c,
        &[queue::NewWork {
            id: "w",
            group_id: "g",
            kind: "cover.research",
            album_id: "gone-request",
            base_revision: "rev",
            input_json: "{}",
            input_digest: "input",
            max_attempts: queue::DEFAULT_MAX_ATTEMPTS,
        }],
        1_000,
    )
    .unwrap();
    assert!(album_key_in_use(c, "gone-history", "/music/new1").unwrap());
    assert!(album_key_in_use(c, "gone-request", "/music/new2").unwrap());
    assert!(!album_key_in_use(c, "free", "/music/new3").unwrap());
}
