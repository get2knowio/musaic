//! The retained event history (spec 005): which transitions of the work
//! state machine write a row, and what pruning and rotation leave.

use musaic_db::delegated_work::{self as queue, Claim, Completion, Lease, NewWork, Progress};
use musaic_db::delegation_events as events;
use musaic_db::{Db, open_connection};
use rusqlite::Connection;

const NOW: i64 = 1_000_000;
const LEASE: i64 = 5 * 60 * 1000;

fn db() -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let (db, _) = Db::open_or_create(dir.path().join("library.db")).unwrap();
    (dir, db)
}

fn item(id: &str) -> NewWork<'_> {
    NewWork {
        id,
        group_id: "group",
        kind: "metadata.identify",
        album_id: "album",
        base_revision: "album-rev",
        input_json: "{}",
        input_digest: "input",
        max_attempts: 10,
    }
}

fn claim<'a>(id: &'a str, key: &'a str, now: i64) -> Claim<'a> {
    Claim {
        work_id: id,
        principal_id: "agent",
        idempotency_key: key,
        request_digest: key,
        lease_token_hash: "token",
        now,
    }
}

fn lease<'a>(reply: &queue::ClaimRow, id: &'a str, now: i64) -> Lease<'a> {
    Lease {
        work_id: id,
        principal_id: "agent",
        claim_generation: reply.claim_generation,
        lease_token_hash: "token",
        now,
    }
}

fn completion() -> Completion<'static> {
    Completion {
        key: "complete",
        request_digest: "request",
        input_digest: "input",
        id: "result",
        digest: "result-digest",
        result_json: r#"{"outcome":"no_match"}"#,
        validation: "verified",
    }
}

fn history(conn: &Connection) -> Vec<(String, String, String)> {
    let epoch = queue::epoch(conn).unwrap();
    events::page(conn, &epoch, 0, &[], &[], 100)
        .unwrap()
        .into_iter()
        .map(|e| (e.work_id, e.event_type, e.state))
        .collect()
}

fn names(conn: &Connection) -> Vec<String> {
    history(conn)
        .into_iter()
        .map(|(_, t, s)| format!("{t}:{s}"))
        .collect()
}

#[test]
fn a_full_lifecycle_writes_one_event_per_change_of_state() {
    let (dir, db) = db();
    let conn = db.connection();
    queue::enqueue(conn, &[item("w")], NOW).unwrap();
    assert_eq!(names(conn), ["requested:queued"]);
    // A duplicate request joins the unresolved item and writes nothing.
    queue::enqueue(conn, &[item("w2")], NOW).unwrap();
    assert_eq!(names(conn).len(), 1);

    // Another connection claims it (separate connections, as the app does).
    let other = open_connection(dir.path().join("library.db")).unwrap();
    let first = queue::claim(&other, claim("w", "k1", NOW + 1))
        .unwrap()
        .unwrap();
    assert_eq!(names(conn), ["requested:queued", "updated:claimed"]);
    let heartbeat = |c: &Connection| -> Option<i64> {
        c.query_row(
            "SELECT last_heartbeat_at FROM work_attempts WHERE work_id='w' AND generation=?1",
            [first.claim_generation],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(heartbeat(conn), Some(NOW + 1));

    // A renewal changes the revision, not the state: no event, new heartbeat.
    queue::renew(conn, lease(&first, "w", NOW + 10))
        .unwrap()
        .unwrap();
    assert_eq!(names(conn).len(), 2);
    assert_eq!(heartbeat(conn), Some(NOW + 10));

    let progress = |sequence, now| {
        queue::progress(
            conn,
            lease(&first, "w", now),
            Progress {
                sequence,
                stage: "research",
                completed: None,
                total: None,
                message: None,
            },
        )
        .unwrap()
        .unwrap()
    };
    progress(1, NOW + 20);
    assert_eq!(names(conn).last().unwrap(), "updated:running");
    assert_eq!(heartbeat(conn), Some(NOW + 20));
    progress(2, NOW + 30);
    assert_eq!(names(conn).len(), 3, "running to running writes none");

    // A retryable failure puts it back to queued: requested again.
    let failed = queue::fail(conn, lease(&first, "w", NOW + 40), "net", "try later", true)
        .unwrap()
        .unwrap();
    assert_eq!(failed.state, "queued");
    assert_eq!(names(conn).last().unwrap(), "requested:queued");

    // Claim, release (back to queued), claim, expire (queued), claim, complete.
    let at = NOW + 1_000_000;
    let second = queue::claim(conn, claim("w", "k2", at)).unwrap().unwrap();
    queue::release(conn, lease(&second, "w", at + 1), "later")
        .unwrap()
        .unwrap();
    assert_eq!(names(conn).last().unwrap(), "requested:queued");
    let third = queue::claim(conn, claim("w", "k3", at + 2_000_000))
        .unwrap()
        .unwrap();
    queue::expire(conn, at + 2_000_000 + LEASE).unwrap();
    assert_eq!(names(conn).last().unwrap(), "requested:queued");
    let at = at + 4_000_000;
    let fourth = queue::claim(conn, claim("w", "k4", at)).unwrap().unwrap();
    assert_ne!(third.claim_generation, fourth.claim_generation);
    let done = queue::complete(conn, lease(&fourth, "w", at + 1), completion())
        .unwrap()
        .unwrap();
    assert_eq!(done.state, "awaiting_review");
    assert_eq!(names(conn).last().unwrap(), "updated:awaiting_review");

    // Review: reserving a result and finishing it.
    let count = names(conn).len();
    let reserved = queue::reserve_review(conn, "w", "result-digest", done.revision, at + 2)
        .unwrap()
        .unwrap();
    assert_eq!(names(conn).len(), count, "reserving keeps the state");
    queue::finish_review(conn, "w", reserved.revision, "applied", at + 3)
        .unwrap()
        .unwrap();
    assert_eq!(names(conn).last().unwrap(), "updated:applied");

    // Every row names its work and a state the work took, in order.
    let all = history(conn);
    assert!(all.iter().all(|(id, _, _)| id == "w"));
    let sequences: Vec<i64> = {
        let epoch = queue::epoch(conn).unwrap();
        events::page(conn, &epoch, 0, &[], &[], 100)
            .unwrap()
            .iter()
            .map(|e| e.sequence)
            .collect()
    };
    assert!(sequences.windows(2).all(|w| w[0] < w[1]));
}

#[test]
fn cancel_discard_and_retry_write_their_events() {
    let (_dir, db) = db();
    let conn = db.connection();
    let work = queue::enqueue(conn, &[item("c")], NOW).unwrap();
    let cancelled = queue::cancel_from_desktop(conn, "c", work[0].revision, NOW + 1)
        .unwrap()
        .unwrap();
    assert_eq!(cancelled.state, "cancelled");
    assert_eq!(names(conn), ["requested:queued", "updated:cancelled"]);
    let retried = queue::retry(conn, "c", cancelled.revision, NOW + 2)
        .unwrap()
        .unwrap();
    assert_eq!(retried.state, "queued");
    assert_eq!(names(conn).last().unwrap(), "requested:queued");
}

#[test]
fn an_event_and_its_change_commit_together() {
    let (_dir, db) = db();
    let conn = db.connection();
    queue::enqueue(conn, &[item("t")], NOW).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER no_events BEFORE INSERT ON delegation_events
         BEGIN SELECT RAISE(ABORT,'event write failed'); END;",
    )
    .unwrap();
    assert!(queue::claim(conn, claim("t", "kt", NOW + 1)).is_err());
    // The claim, its audit row and its notification rolled back with it.
    assert_eq!(queue::get(conn, "t").unwrap().unwrap().state, "queued");
    assert_eq!(names(conn), ["requested:queued"]);
}

#[test]
fn head_is_zero_without_audit_and_follows_the_audit_sequence() {
    let (_dir, db) = db();
    let conn = db.connection();
    let epoch = queue::epoch(conn).unwrap();
    let before = events::head(conn, &epoch).unwrap();
    queue::enqueue(conn, &[item("h")], NOW).unwrap();
    assert!(events::head(conn, &epoch).unwrap() > before);
    let row = events::oldest(conn, &epoch).unwrap().unwrap();
    assert_eq!(row.sequence, events::head(conn, &epoch).unwrap());
}

#[test]
fn pruning_removes_old_events_and_advances_the_watermark_only() {
    let (_dir, db) = db();
    let conn = db.connection();
    queue::enqueue(conn, &[item("p")], NOW).unwrap();
    let epoch = queue::epoch(conn).unwrap();
    let work_before = queue::get(conn, "p").unwrap().unwrap();
    let audit_before: i64 = conn
        .query_row("SELECT COUNT(*) FROM delegation_audit", [], |r| r.get(0))
        .unwrap();
    assert_eq!(events::pruned_through(conn).unwrap(), 0);
    assert_eq!(events::prune(conn, NOW).unwrap(), 0, "nothing older yet");
    assert_eq!(events::prune(conn, NOW + 1).unwrap(), 1);
    assert!(events::oldest(conn, &epoch).unwrap().is_none());
    assert!(events::pruned_through(conn).unwrap() > 0);
    assert_eq!(queue::get(conn, "p").unwrap().unwrap(), work_before);
    let audit_after: i64 = conn
        .query_row("SELECT COUNT(*) FROM delegation_audit", [], |r| r.get(0))
        .unwrap();
    assert_eq!(audit_before, audit_after);
}

#[test]
fn rotating_the_epoch_drops_the_old_history() {
    let (_dir, db) = db();
    let conn = db.connection();
    queue::enqueue(conn, &[item("r")], NOW).unwrap();
    let old = queue::epoch(conn).unwrap();
    assert_eq!(events::page(conn, &old, 0, &[], &[], 10).unwrap().len(), 1);
    let new = queue::rotate_epoch(conn, None, NOW + 5).unwrap();
    assert_ne!(old, new);
    assert!(
        events::page(conn, &old, 0, &[], &[], 10)
            .unwrap()
            .is_empty()
    );
    let left: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM delegation_events WHERE library_epoch<>?1",
            [&new],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(left, 0);
    assert!(events::pruned_through(conn).unwrap() > 0);
}

#[test]
fn filters_by_kind_and_state_and_age_floor() {
    let (_dir, db) = db();
    let conn = db.connection();
    let mut other = item("k2");
    other.kind = "cover.research";
    other.album_id = "album2";
    queue::enqueue(conn, &[item("k1")], NOW).unwrap();
    queue::enqueue(conn, &[other], NOW + 100).unwrap();
    let epoch = queue::epoch(conn).unwrap();
    assert_eq!(
        events::page(conn, &epoch, 0, &["cover.research"], &[], 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        events::page(conn, &epoch, 0, &[], &["claimed"], 10)
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        events::page(
            conn,
            &epoch,
            0,
            &["metadata.identify", "cover.research"],
            &["queued"],
            10
        )
        .unwrap()
        .len(),
        2
    );
    let first = events::oldest(conn, &epoch).unwrap().unwrap();
    assert_eq!(
        events::age_floor(conn, &epoch, NOW + 50).unwrap(),
        first.sequence
    );
    assert_eq!(events::age_floor(conn, &epoch, NOW).unwrap(), 0);
}

fn tables(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn a_fresh_database_has_the_v23_objects() {
    let (_dir, db) = db();
    let conn = db.connection();
    assert_eq!(db.schema_version().unwrap(), musaic_db::SCHEMA_VERSION);
    assert_eq!(musaic_db::SCHEMA_VERSION, 23);
    let t = tables(conn);
    for name in [
        "delegation_events",
        "delegation_event_state",
        "agent_host_observations",
    ] {
        assert!(t.contains(&name.to_string()), "{name}");
    }
    assert_eq!(events::pruned_through(conn).unwrap(), 0);
    let heartbeat: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('work_attempts') WHERE name='last_heartbeat_at'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(heartbeat, 1);
}

#[test]
fn a_v22_database_upgrades_in_place_and_reopening_is_a_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("library.db");
    {
        let (db, _) = Db::open_or_create(&file).unwrap();
        let conn = db.connection();
        queue::enqueue(conn, &[item("old")], NOW).unwrap();
        // Take the v23 additions away to leave what a v22 file looks like.
        conn.execute_batch(
            "DROP TABLE delegation_events; DROP TABLE delegation_event_state;
             DROP TABLE agent_host_observations;
             ALTER TABLE work_attempts DROP COLUMN last_heartbeat_at;
             UPDATE schema_version SET version=22;",
        )
        .unwrap();
    }
    let (db, _) = Db::open(&file).unwrap();
    assert_eq!(db.schema_version().unwrap(), 23);
    let conn = db.connection();
    assert!(tables(conn).contains(&"delegation_events".to_string()));
    // Work that existed before the upgrade is intact; new changes get events.
    assert_eq!(queue::get(conn, "old").unwrap().unwrap().state, "queued");
    queue::claim(conn, claim("old", "k", NOW + 1))
        .unwrap()
        .unwrap();
    assert_eq!(names(conn), ["updated:claimed"]);
    let before = tables(conn);
    drop(db);
    let (db, _) = Db::open(&file).unwrap();
    assert_eq!(db.schema_version().unwrap(), 23);
    assert_eq!(tables(db.connection()), before);
}

#[test]
fn an_album_change_and_a_discard_write_updated_events() {
    let (_dir, db) = db();
    let conn = db.connection();
    queue::enqueue(conn, &[item("s")], NOW).unwrap();
    queue::invalidate_album(conn, "album", NOW + 1).unwrap();
    assert_eq!(names(conn), ["requested:queued", "updated:cancelled"]);

    let mut review = item("d");
    review.album_id = "album-d";
    review.input_digest = "input-d";
    let work = queue::enqueue(conn, &[review], NOW + 2).unwrap();
    let at = NOW + 3;
    let first = queue::claim(conn, claim("d", "kd", at)).unwrap().unwrap();
    let done = queue::complete(
        conn,
        lease(&first, "d", at + 1),
        Completion {
            input_digest: "input-d",
            ..completion()
        },
    )
    .unwrap()
    .unwrap();
    assert_eq!(done.state, "awaiting_review");
    queue::discard(conn, "d", Some("result-digest"), done.revision, at + 2)
        .unwrap()
        .unwrap();
    assert_eq!(names(conn).last().unwrap(), "updated:discarded");
    assert_eq!(work[0].id, "d");
}

#[test]
fn host_observations_are_stored_per_pairing_and_removed_with_it() {
    use musaic_db::agent_hosts::{self as hosts, Observation};
    let (_dir, db) = db();
    let conn = db.connection();
    conn.execute(
        "INSERT INTO agent_pairings(id,name,scopes,secret_fingerprint,created_at)
         VALUES ('p1','One','[]','f','2026-11-23T00:00:00Z')",
        [],
    )
    .unwrap();
    let o = Observation {
        client_name: "Host".into(),
        client_version: "1.0".into(),
        profile: "2026-07-28".into(),
        tasks: true,
        skills: false,
        events: true,
    };
    assert!(hosts::get(conn, "p1").unwrap().is_none());
    assert!(
        hosts::upsert(conn, "p1", &o, 10).unwrap(),
        "first is a change"
    );
    assert!(!hosts::upsert(conn, "p1", &o, 20).unwrap(), "same values");
    assert_eq!(hosts::get(conn, "p1").unwrap().unwrap().observed_at, 20);
    let changed = Observation {
        skills: true,
        ..o.clone()
    };
    assert!(hosts::upsert(conn, "p1", &changed, 30).unwrap());
    assert_eq!(
        hosts::get(conn, "p1").unwrap().unwrap().observation,
        changed
    );
    // An unknown or revoked pairing stores nothing.
    assert!(!hosts::upsert(conn, "nope", &o, 1).unwrap());
    conn.execute("UPDATE agent_pairings SET revoked_at='x' WHERE id='p1'", [])
        .unwrap();
    assert!(!hosts::upsert(conn, "p1", &o, 40).unwrap());
    hosts::delete(conn, "p1").unwrap();
    assert!(hosts::get(conn, "p1").unwrap().is_none());
}
