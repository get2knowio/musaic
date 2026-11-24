use std::sync::Barrier;

use musaic_db::delegated_work::{
    self as queue, Claim, Completion, Lease, List, NewWork, Progress, Refusal,
};
use musaic_db::{Db, Opened, open_connection};
use rusqlite::{Connection, params};

#[path = "support/delegation_schema.rs"]
mod delegation_schema;

const NOW: i64 = 10_000;
const OLD_EPOCH: &str = "old";
const CLAIM_MS: i64 = 5 * 60 * 1000;

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
        max_attempts: queue::DEFAULT_MAX_ATTEMPTS,
    }
}

fn claim<'a>(id: &'a str, principal: &'a str, now: i64) -> Claim<'a> {
    Claim {
        work_id: id,
        principal_id: principal,
        idempotency_key: id,
        request_digest: "claim",
        lease_token_hash: principal,
        now,
    }
}

fn lease<'a>(reply: &queue::ClaimRow, id: &'a str, principal: &'a str, now: i64) -> Lease<'a> {
    Lease {
        work_id: id,
        principal_id: principal,
        claim_generation: reply.claim_generation,
        lease_token_hash: principal,
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

fn count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn upgrades_every_legacy_state_grouped_once_and_retains_raw_history() {
    let (dir, db) = db();
    let file = dir.path().join("library.db");
    let conn = db.connection();
    // The v23 and v24 tables arrived after this upgrade path; a v21 file
    // has none, and it still has the old job table v24 drops.
    conn.execute_batch(
        "DROP TABLE delegation_events; DROP TABLE delegation_event_state;
         DROP TABLE agent_host_observations; DROP TABLE research_history;",
    )
    .unwrap();
    for (_, name, _) in delegation_schema::OBJECTS
        .iter()
        .rev()
        .filter(|(ty, _, _)| *ty == "table")
    {
        conn.execute(&format!("DROP TABLE {name}"), []).unwrap();
    }
    conn.execute_batch(
        "DROP TABLE work_batches;
         CREATE TABLE ai_jobs (
             id TEXT PRIMARY KEY, album_id TEXT NOT NULL, album_path TEXT NOT NULL,
             album_title TEXT, job_type TEXT NOT NULL, hint TEXT NOT NULL DEFAULT '',
             status TEXT NOT NULL, result_json TEXT, error TEXT, acted_on_action TEXT,
             created_at TEXT NOT NULL, started_at TEXT, completed_at TEXT, acted_on_at TEXT,
             attach_cover INTEGER NOT NULL DEFAULT 1);",
    )
    .unwrap();
    conn.execute("UPDATE schema_version SET version=21", [])
        .unwrap();
    let legacy = [
        ("queued", "metadata", None, "queued"),
        ("running", "cover", None, "queued"),
        ("completed", "metadata+cover", None, "awaiting_review"),
        ("failed", "cover", None, "failed"),
        ("acted_on", "metadata", Some("applied"), "applied"),
        ("acted_on", "metadata", Some("discarded"), "discarded"),
    ];
    let created = "2026-11-22T14:00:00Z";
    let updated = "2026-11-22T14:01:00Z";
    for (id, (state, kind, action, _)) in legacy.iter().enumerate() {
        conn.execute(
            r#"INSERT INTO ai_jobs(id,album_id,album_path,job_type,status,acted_on_action,
              result_json,created_at,started_at,completed_at,acted_on_at)
             VALUES (?1,'album','/old/alb',?2,?3,?4,'{"old":true}',?5,?6,?6,?6)"#,
            params![id.to_string(), kind, state, action, created, updated],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO agent_pairings(id,name,scopes,secret_fingerprint,created_at)
         VALUES ('old-pairing','Previous assistant','[]','fingerprint',?1)",
        [created],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO agent_changes(id,pairing_id,proposal_id,album_id,created_at,state,before,outcome,after)
         VALUES ('old-change','old-pairing','old-proposal','album',?1,'applied','{}','{}','{}')",
        [created],
    ).unwrap();
    // Crash after migrating every row but before committing its schema version.
    conn.execute_batch(
        "CREATE TRIGGER migration_interrupt BEFORE UPDATE ON schema_version
         WHEN NEW.version=22 BEGIN SELECT RAISE(ABORT,'interrupted upgrade'); END;",
    )
    .unwrap();
    drop(db);
    assert!(
        Db::open(&file)
            .err()
            .unwrap()
            .to_string()
            .contains("interrupted upgrade")
    );
    let old = Connection::open(&file).unwrap();
    assert_eq!(count(&old, "ai_jobs"), 6);
    assert_eq!(
        old.query_row("SELECT version FROM schema_version", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        21
    );
    assert_eq!(
        old.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name='delegated_work'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    old.execute_batch("DROP TRIGGER migration_interrupt;")
        .unwrap();
    drop(old);

    let (db, outcome) = Db::open(&file).unwrap();
    assert_eq!(
        outcome,
        Opened::Migrated {
            from: 21,
            backup: None
        }
    );
    let conn = db.connection();
    let change = musaic_db::agent::get_change(conn, "old-change")
        .unwrap()
        .unwrap();
    assert_eq!(change.pairing_id.as_deref(), Some("old-pairing"));
    assert_eq!(change.after.as_deref(), Some("{}"));
    let rows = queue::desktop_list(conn).unwrap();
    assert_eq!(rows.len(), 7);
    for (id, (_, _, _, expected)) in legacy.iter().enumerate() {
        let id = id.to_string();
        let child: Vec<_> = rows
            .iter()
            .filter(|w| w.legacy_job_id.as_deref() == Some(&id))
            .collect();
        assert_eq!(child.len(), if id == "2" { 2 } else { 1 });
        assert!(child.iter().all(|w| w.state == *expected
            && w.group_id == child[0].group_id
            && w.created_at == 1_795_356_000_000
            && w.updated_at == 1_795_356_060_000));
        assert_eq!(
            musaic_db::research_history::get(conn, &id)
                .unwrap()
                .unwrap()
                .result_json
                .as_deref(),
            Some(r#"{"old":true}"#)
        );
    }
    let group: Vec<_> = rows
        .iter()
        .filter(|w| w.legacy_job_id.as_deref() == Some("2"))
        .collect();
    assert_ne!(group[0].kind, group[1].kind);
    assert!(group.iter().all(|w| w.result_id.is_none()));
    assert_eq!(count(conn, "delegation_audit"), 7);
    assert_eq!(queue::outbox(conn, 20).unwrap().len(), 7);
    drop(db);
    let (reopened, outcome) = Db::open(&file).unwrap();
    assert_eq!(outcome, Opened::Current);
    assert_eq!(queue::desktop_list(reopened.connection()).unwrap(), rows);
}

#[test]
fn concurrent_claims_and_restarts_preserve_fencing_and_replay() {
    let (dir, db) = db();
    let file = dir.path().join("library.db");
    let conn = db.connection();
    queue::enqueue(conn, &[item("work")], NOW).unwrap();
    let duplicates = queue::enqueue(conn, &[item("duplicate")], NOW).unwrap();
    assert_eq!(duplicates[0].id, "work");
    let barrier = Barrier::new(2);
    let (a, b) = std::thread::scope(|s| {
        let race = |principal| {
            let conn = open_connection(&file).unwrap();
            barrier.wait();
            queue::claim(&conn, claim("work", principal, NOW)).unwrap()
        };
        let first = s.spawn(move || race("alpha"));
        let second = s.spawn(move || race("beta"));
        (first.join().unwrap(), second.join().unwrap())
    });
    let (winner, loser, first) = match (a, b) {
        (Ok(a), Err(Refusal::AlreadyClaimed)) => ("alpha", "beta", a),
        (Err(Refusal::AlreadyClaimed), Ok(b)) => ("beta", "alpha", b),
        other => panic!("unexpected claim outcomes {other:?}"),
    };
    assert_eq!(first.lease_expires_at, NOW + CLAIM_MS);
    assert!(
        queue::list(
            conn,
            List {
                principal_id: loser,
                state: None,
                kind: None,
                batch_id: None,
                after: None,
                limit: 10
            }
        )
        .unwrap()
        .is_empty()
    );
    assert_eq!(
        queue::claim(conn, claim("work", winner, NOW + 1))
            .unwrap()
            .unwrap(),
        first
    );
    assert_eq!(
        queue::renew(conn, lease(&first, "work", loser, NOW + 1)).unwrap(),
        Err(Refusal::NotFound)
    );
    drop(db);
    let (db, _) = Db::open(&file).unwrap();
    let conn = db.connection();
    let old = lease(&first, "work", winner, NOW + CLAIM_MS);
    assert_eq!(queue::renew(conn, old).unwrap(), Err(Refusal::LeaseExpired));
    assert_eq!(queue::expire(conn, NOW + CLAIM_MS).unwrap(), ["work"]);
    assert_eq!(queue::renew(conn, old).unwrap(), Err(Refusal::LeaseExpired));
    let second = queue::claim(conn, claim("work", loser, NOW + CLAIM_MS))
        .unwrap()
        .unwrap();
    assert!(second.claim_generation > first.claim_generation);
    assert_eq!(
        queue::complete(conn, old, completion()).unwrap(),
        Err(Refusal::StaleGeneration)
    );
    let current = lease(&second, "work", loser, NOW + CLAIM_MS + 1);
    let running = queue::progress(
        conn,
        current,
        Progress {
            sequence: 3,
            stage: "research",
            completed: Some(2),
            total: Some(3),
            message: None,
        },
    )
    .unwrap()
    .unwrap();
    assert_eq!(running.state, "running");
    assert_eq!(
        queue::progress(
            conn,
            current,
            Progress {
                sequence: 2,
                stage: "old",
                completed: None,
                total: None,
                message: None
            }
        )
        .unwrap()
        .unwrap(),
        running
    );
    let outcome = queue::complete(conn, current, completion())
        .unwrap()
        .unwrap();
    assert_eq!(outcome.state, "awaiting_review");
    assert!(outcome.lease_expires_at.is_none());
    drop(db);
    let (db, _) = Db::open(&file).unwrap();
    let conn = db.connection();
    assert_eq!(
        queue::complete(conn, current, completion())
            .unwrap()
            .unwrap(),
        outcome
    );
    assert_eq!(
        queue::complete(
            conn,
            current,
            Completion {
                request_digest: "changed",
                ..completion()
            }
        )
        .unwrap(),
        Err(Refusal::IdempotencyConflict)
    );
    assert_eq!(count(conn, "work_results"), 1);
    assert_eq!(
        queue::enqueue(conn, &[item("third")], NOW).unwrap()[0].id,
        "work"
    );
}

#[test]
fn failed_writes_rollback_the_whole_transition_and_unsent_outbox() {
    let (_dir, db) = db();
    let c = db.connection();
    c.execute_batch(
        "CREATE TRIGGER abort_outbox BEFORE INSERT ON delegation_outbox
      BEGIN SELECT RAISE(ABORT,'crash after audit'); END;",
    )
    .unwrap();
    assert!(queue::enqueue(c, &[item("one")], NOW).is_err());
    for table in ["delegated_work", "delegation_audit", "delegation_outbox"] {
        assert_eq!(count(c, table), 0);
    }
    c.execute_batch("DROP TRIGGER abort_outbox;").unwrap();
    queue::enqueue(c, &[item("one")], NOW).unwrap();
    c.execute_batch(
        "CREATE TRIGGER abort_replay BEFORE INSERT ON work_replays
      BEGIN SELECT RAISE(ABORT,'crash before replay'); END;",
    )
    .unwrap();
    assert!(queue::claim(c, claim("one", "owner", NOW)).is_err());
    assert_eq!(queue::get(c, "one").unwrap().unwrap().state, "queued");
    assert_eq!(count(c, "work_attempts"), 0);
    assert_eq!(count(c, "delegation_outbox"), 1);
    c.execute_batch("DROP TRIGGER abort_replay;").unwrap();
    let reply = queue::claim(c, claim("one", "owner", NOW))
        .unwrap()
        .unwrap();
    c.execute_batch(
        "CREATE TRIGGER abort_replay BEFORE INSERT ON work_replays WHEN NEW.operation='complete'
      BEGIN SELECT RAISE(ABORT,'crash after result'); END;",
    )
    .unwrap();
    assert!(queue::complete(c, lease(&reply, "one", "owner", NOW), completion()).is_err());
    assert_eq!(queue::get(c, "one").unwrap().unwrap().state, "claimed");
    assert_eq!(count(c, "work_results"), 0);
    assert_eq!(count(c, "delegation_outbox"), 2);
}

#[test]
fn retry_budget_and_epoch_invalidation_survive_root_changes() {
    let (_dir, db) = db();
    let c = db.connection();
    c.execute("UPDATE library_epoch SET epoch=?1", [OLD_EPOCH])
        .unwrap();
    assert!(!queue::sync_root(c, Some("/old"), NOW).unwrap());
    let mut work = item("one");
    work.max_attempts = 2;
    queue::enqueue(c, &[work], NOW).unwrap();
    let first = queue::claim(c, claim("one", "owner", NOW))
        .unwrap()
        .unwrap();
    let next = queue::fail(
        c,
        lease(&first, "one", "owner", NOW),
        "unavailable",
        "later",
        true,
    )
    .unwrap()
    .unwrap();
    assert_eq!(next.state, "queued");
    assert_eq!(
        queue::claim(
            c,
            Claim {
                idempotency_key: "next",
                ..claim("one", "owner", NOW)
            }
        )
        .unwrap(),
        Err(Refusal::RateLimited)
    );
    let second = queue::claim(
        c,
        Claim {
            idempotency_key: "next",
            ..claim("one", "owner", next.available_at)
        },
    )
    .unwrap()
    .unwrap();
    let failed = queue::release(c, lease(&second, "one", "owner", next.available_at), "done")
        .unwrap()
        .unwrap();
    assert_eq!(failed.state, "failed");
    assert_eq!(
        queue::retry(c, "one", failed.revision, next.available_at)
            .unwrap()
            .unwrap()
            .attempts,
        0
    );
    let third = queue::claim(
        c,
        Claim {
            idempotency_key: "third",
            ..claim("one", "owner", next.available_at)
        },
    )
    .unwrap()
    .unwrap();
    assert!(queue::sync_root(c, Some("/new"), next.available_at).unwrap());
    assert_eq!(
        queue::check_lease(c, lease(&third, "one", "owner", next.available_at)).unwrap(),
        Err(Refusal::StaleContext)
    );
    assert!(
        queue::claim_replay(c, "owner", "third", "claim")
            .unwrap()
            .unwrap()
            .is_none()
    );
    let rows = queue::outbox(c, 100).unwrap();
    assert_eq!(rows.len(), 1);
    assert_ne!(rows[0].library_epoch, OLD_EPOCH);
    let newer = queue::enqueue(c, &[item("new")], NOW).unwrap();
    assert_ne!(newer[0].library_epoch, OLD_EPOCH);
    queue::ack_outbox(c, OLD_EPOCH, i64::MAX).unwrap();
    assert_eq!(queue::outbox(c, 100).unwrap().len(), 2);
}

#[test]
fn review_reservation_is_single_use_and_interruption_never_replays_writes() {
    let (_dir, db) = db();
    let c = db.connection();
    queue::enqueue(c, &[item("one")], NOW).unwrap();
    let reply = queue::claim(c, claim("one", "owner", NOW))
        .unwrap()
        .unwrap();
    let result = queue::complete(
        c,
        lease(&reply, "one", "owner", NOW),
        Completion {
            validation: "pending",
            ..completion()
        },
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        queue::reserve_review(c, "one", "result-digest", result.revision, NOW).unwrap(),
        Err(Refusal::InvalidResult)
    );
    let result = queue::set_verification(c, "one", "result-digest", "verified", NOW)
        .unwrap()
        .unwrap();
    let reserved = queue::reserve_review(c, "one", "result-digest", result.revision, NOW)
        .unwrap()
        .unwrap();
    assert!(reserved.review_reserved);
    assert_eq!(
        queue::reserve_review(c, "one", "result-digest", result.revision, NOW).unwrap(),
        Err(Refusal::StaleContext)
    );
    assert_eq!(
        queue::discard(c, "one", Some("result-digest"), reserved.revision, NOW).unwrap(),
        Err(Refusal::StaleContext)
    );
    queue::recover_reserved(c, NOW + 1).unwrap();
    let interrupted = queue::get(c, "one").unwrap().unwrap();
    assert_eq!(interrupted.state, "failed");
    assert!(interrupted.last_error.unwrap().contains("check_backup"));
    assert_eq!(count(c, "work_results"), 1);
}

#[test]
fn concurrent_enqueues_share_the_only_unresolved_row() {
    let (dir, db) = db();
    let file = dir.path().join("library.db");
    let barrier = Barrier::new(2);
    let (one, two) = std::thread::scope(|scope| {
        let enqueue = |id| {
            let c = open_connection(&file).unwrap();
            barrier.wait();
            queue::enqueue(&c, &[item(id)], NOW).unwrap().remove(0)
        };
        let first = scope.spawn(move || enqueue("first"));
        let second = scope.spawn(move || enqueue("second"));
        (first.join().unwrap(), second.join().unwrap())
    });
    assert_eq!(one, two);
    assert_eq!(count(db.connection(), "delegated_work"), 1);
    assert_eq!(count(db.connection(), "delegation_outbox"), 1);
}

#[test]
fn restore_rotates_the_epoch_before_publishing_the_database() {
    use std::time::{Duration, UNIX_EPOCH};

    let (dir, db) = db();
    let snapshot_dir = dir.path().join("snapshots");
    let conn = db.connection();
    let old = queue::epoch(conn).unwrap();
    queue::sync_root(conn, Some("/synthetic"), NOW).unwrap();
    queue::enqueue(conn, &[item("work")], NOW).unwrap();
    let claim = queue::claim(conn, claim("work", "owner", NOW))
        .unwrap()
        .unwrap();
    let now = UNIX_EPOCH + Duration::from_millis(NOW as u64);
    let snapshot =
        musaic_db::backup::create_snapshot(&db, &snapshot_dir, 0, "fixture", now).unwrap();
    let (db, _) =
        musaic_db::backup::restore_from(db, &snapshot_dir.join(snapshot.filename), "restored", now)
            .unwrap();
    let conn = db.connection();
    let current = queue::epoch(conn).unwrap();
    assert_ne!(old, current);
    assert!(!queue::sync_root(conn, Some("/synthetic"), NOW).unwrap());
    assert_eq!(
        queue::check_lease(conn, lease(&claim, "work", "owner", NOW)).unwrap(),
        Err(Refusal::StaleContext)
    );
    assert_eq!(
        queue::get(conn, "work").unwrap().unwrap().state,
        "cancelled"
    );
    let outbox = queue::outbox(conn, 100).unwrap();
    assert_eq!(outbox.len(), 1);
    assert_eq!(outbox[0].library_epoch, current);
    assert_eq!(outbox[0].work_id, None);
}

#[test]
fn a_context_change_refuses_the_transaction_but_does_not_override_replay() {
    let (_dir, db) = db();
    let c = db.connection();
    queue::enqueue(c, &[item("work")], NOW).unwrap();
    assert_eq!(
        queue::claim_with_context(c, claim("work", "owner", NOW), |_, _| Ok(false)).unwrap(),
        Err(Refusal::StaleContext),
    );
    assert_eq!(count(c, "work_attempts"), 0);
    let first = queue::claim(c, claim("work", "owner", NOW))
        .unwrap()
        .unwrap();
    let lease = lease(&first, "work", "owner", NOW);
    assert_eq!(
        queue::complete_with_context(c, lease, completion(), |_, _| Ok(false)).unwrap(),
        Err(Refusal::StaleContext),
    );
    assert_eq!(count(c, "work_results"), 0);
    let complete = queue::complete(c, lease, completion()).unwrap().unwrap();
    assert_eq!(
        queue::complete_with_context(c, lease, completion(), |_, _| Ok(false))
            .unwrap()
            .unwrap(),
        complete,
    );
}

#[test]
fn combined_enqueue_adopts_separate_claimed_children_under_one_card() {
    let (_dir, db) = db();
    let conn = db.connection();
    let metadata = NewWork {
        group_id: "metadata-card",
        ..item("metadata")
    };
    let cover = NewWork {
        group_id: "cover-card",
        kind: "cover.research",
        ..item("cover")
    };
    queue::enqueue(conn, &[metadata], NOW).unwrap();
    queue::enqueue(conn, &[cover], NOW + 1).unwrap();
    let cover_claim = queue::claim(conn, claim("cover", "assistant", NOW + 2))
        .unwrap()
        .unwrap();
    let combined = queue::enqueue(
        conn,
        &[
            NewWork {
                group_id: "new-card",
                ..item("new-metadata")
            },
            NewWork {
                group_id: "new-card",
                kind: "cover.research",
                ..item("new-cover")
            },
        ],
        NOW + 3,
    )
    .unwrap();
    assert_eq!(
        combined.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        ["metadata", "cover"]
    );
    assert!(combined.iter().all(|r| r.group_id == "metadata-card"));
    assert_eq!(count(conn, "delegated_work"), 2);
    assert_eq!(
        musaic_db::research_jobs::group(conn, "cover-card")
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        musaic_db::research_jobs::group(conn, "metadata-card")
            .unwrap()
            .len(),
        2
    );
    assert!(
        queue::check_lease(conn, lease(&cover_claim, "cover", "assistant", NOW + 4))
            .unwrap()
            .is_ok()
    );
}

// ---------- batches, multi-item claim and renew (spec 006) ----------

struct Owned {
    id: String,
    album: String,
    digest: String,
}

fn owned(id: &str) -> Owned {
    Owned {
        id: id.to_owned(),
        album: format!("album-{id}"),
        digest: format!("input-{id}"),
    }
}

fn new_work<'a>(o: &'a Owned, kind: &'a str) -> NewWork<'a> {
    NewWork {
        id: &o.id,
        group_id: &o.id,
        kind,
        album_id: &o.album,
        base_revision: "album-rev",
        input_json: "{}",
        input_digest: &o.digest,
        max_attempts: queue::DEFAULT_MAX_ATTEMPTS,
    }
}

const BATCH: &str = "6f0c2a3e-1b2c-4d5e-8f90-0123456789ab";
const OTHER_BATCH: &str = "7a1d3b4f-2c3d-4e5f-9a01-123456789abc";

fn batch(id: &str) -> queue::NewBatch<'_> {
    queue::NewBatch {
        id,
        check_id: "unidentified-album",
        started_at: NOW - 5,
    }
}

/// `n` queued, hydrated items `prefix-0 ..`, one per album, in `batch_id`
/// (or none), created one millisecond apart.
fn seed(
    conn: &Connection,
    prefix: &str,
    n: usize,
    batch_id: Option<&str>,
    kind: &str,
) -> Vec<String> {
    (0..n)
        .map(|i| {
            let o = owned(&format!("{prefix}-{i:04}"));
            let w = new_work(&o, kind);
            let now = NOW + i as i64;
            match batch_id {
                Some(b) => queue::enqueue_batched(conn, &[w], &batch(b), now).unwrap(),
                None => queue::enqueue(conn, &[w], now).unwrap(),
            };
            o.id
        })
        .collect()
}

fn many<'a>(principal: &'a str, key: &'a str, digest: &'a str, max: i64) -> queue::ClaimMany<'a> {
    queue::ClaimMany {
        principal_id: principal,
        idempotency_key: key,
        request_digest: digest,
        batch_id: None,
        kind: None,
        max,
        now: NOW + 10_000,
    }
}

fn claim_many(conn: &Connection, c: queue::ClaimMany<'_>) -> Result<Vec<String>, Refusal> {
    let principal = c.principal_id.to_owned();
    queue::claim_many_with_context(conn, c, |_| principal.clone(), |_, _| Ok(true))
        .unwrap()
        .map(|r| r.claims.into_iter().map(|c| c.work_id).collect())
}

#[test]
fn claim_many_wins_what_is_eligible_and_fewer_is_not_an_error() {
    let (_dir, db) = db();
    let c = db.connection();
    let ids = seed(c, "w", 3, Some(BATCH), "metadata.identify");
    let won = claim_many(c, many("agent", "burst-0001", "d1", 25)).unwrap();
    assert_eq!(won, ids, "oldest first, all three of the 25 asked for");
    for id in &ids {
        let row = queue::get(c, id).unwrap().unwrap();
        assert_eq!(row.state, "claimed");
        assert_eq!(row.principal_id.as_deref(), Some("agent"));
        assert_eq!(row.batch_id.as_deref(), Some(BATCH));
    }
    assert_eq!(count(c, "work_attempts"), 3);
    // Nothing left: an empty answer, never an error.
    assert!(
        claim_many(c, many("agent", "burst-0002", "d2", 25))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn claim_many_skips_stale_items_and_honours_the_filters() {
    let (_dir, db) = db();
    let c = db.connection();
    let batched = seed(c, "b", 4, Some(BATCH), "metadata.identify");
    let other = seed(c, "o", 2, Some(OTHER_BATCH), "metadata.identify");
    let covers = seed(c, "c", 2, Some(BATCH), "cover.research");
    let loose = seed(c, "l", 2, None, "metadata.identify");
    // An unhydrated item is never eligible.
    c.execute(
        "UPDATE delegated_work SET base_revision='' WHERE id=?1",
        [&batched[3]],
    )
    .unwrap();

    // The stale item (context no longer current) is skipped, left queued, and
    // does not hide the ones after it.
    let stale = batched[0].clone();
    let got = queue::claim_many_with_context(
        c,
        queue::ClaimMany {
            batch_id: Some(BATCH),
            kind: Some("metadata.identify"),
            ..many("agent", "filter-0001", "f1", 25)
        },
        |_| "agent".into(),
        |_, row| Ok(row.id != stale),
    )
    .unwrap()
    .unwrap();
    let won: Vec<_> = got.claims.iter().map(|c| c.work_id.clone()).collect();
    assert_eq!(won, vec![batched[1].clone(), batched[2].clone()]);
    assert_eq!(queue::get(c, &stale).unwrap().unwrap().state, "queued");
    for id in other.iter().chain(&covers).chain(&loose) {
        assert_eq!(queue::get(c, id).unwrap().unwrap().state, "queued", "{id}");
    }
    // Kind alone reaches every batch and the unbatched queue; max bounds it.
    let won = claim_many(
        c,
        queue::ClaimMany {
            kind: Some("cover.research"),
            ..many("agent", "filter-0002", "f2", 1)
        },
    )
    .unwrap();
    assert_eq!(won, vec![covers[0].clone()]);
    // An unknown batch: nothing, not an error.
    let won = claim_many(
        c,
        queue::ClaimMany {
            batch_id: Some("00000000-0000-4000-8000-000000000000"),
            ..many("agent", "filter-0003", "f3", 25)
        },
    )
    .unwrap();
    assert!(won.is_empty());
}

#[test]
fn claim_many_replays_its_answer_and_refuses_a_changed_input() {
    let (_dir, db) = db();
    let c = db.connection();
    seed(c, "w", 5, Some(BATCH), "metadata.identify");
    let first = queue::claim_many_with_context(
        c,
        many("agent", "replay-0001", "digest-a", 3),
        |id| format!("hash-{id}"),
        |_, _| Ok(true),
    )
    .unwrap()
    .unwrap();
    assert_eq!(first.claims.len(), 3);
    let again = queue::claim_many_with_context(
        c,
        many("agent", "replay-0001", "digest-a", 3),
        |_| panic!("a replay derives no new lease"),
        |_, _| Ok(true),
    )
    .unwrap()
    .unwrap();
    assert_eq!(again, first, "same ids, generations and expiries");
    // The saved answer holds no token or verifier.
    let saved: String = c
        .query_row(
            "SELECT response_json FROM work_replays WHERE operation='claim_many'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!saved.contains("hash-"), "{saved}");
    assert_eq!(
        claim_many(c, many("agent", "replay-0001", "digest-b", 3)),
        Err(Refusal::IdempotencyConflict)
    );
    // Another principal's key space is its own.
    assert_eq!(
        claim_many(c, many("other", "replay-0001", "digest-b", 3))
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn racing_claim_many_never_holds_an_item_twice() {
    let (dir, db) = db();
    let file = dir.path().join("library.db");
    let ids = seed(db.connection(), "r", 40, Some(BATCH), "metadata.identify");
    let barrier = Barrier::new(2);
    let won: Vec<Vec<String>> = std::thread::scope(|scope| {
        let run = |principal: &'static str| {
            let conn = open_connection(&file).unwrap();
            let barrier = &barrier;
            move || {
                barrier.wait();
                let mut all = Vec::new();
                for round in 0..4 {
                    let key = format!("race-{principal}-{round}");
                    all.extend(claim_many(&conn, many(principal, &key, &key, 25)).unwrap());
                }
                all
            }
        };
        let a = scope.spawn(run("alpha"));
        let b = scope.spawn(run("beta"));
        vec![a.join().unwrap(), b.join().unwrap()]
    });
    let mut every: Vec<String> = won.concat();
    every.sort();
    let before = every.len();
    every.dedup();
    assert_eq!(before, every.len(), "an item was claimed twice");
    assert_eq!(every.len(), ids.len());
}

#[test]
fn an_epoch_rotation_leaves_nothing_to_claim_and_refuses_the_old_batch() {
    let (_dir, db) = db();
    let c = db.connection();
    seed(c, "w", 3, Some(BATCH), "metadata.identify");
    queue::rotate_epoch(c, Some("/elsewhere"), NOW + 1).unwrap();
    assert!(
        claim_many(c, many("agent", "epoch-0001", "e", 25))
            .unwrap()
            .is_empty()
    );
    let cancelled: i64 = c
        .query_row(
            "SELECT count(*) FROM delegated_work WHERE batch_id=?1 AND state='cancelled'",
            [BATCH],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(cancelled, 3);
    // The batch belongs to the old epoch: a new request cannot join it.
    let o = owned("late");
    let refused = queue::enqueue_batched(
        c,
        &[new_work(&o, "metadata.identify")],
        &batch(BATCH),
        NOW + 2,
    );
    assert!(matches!(refused, Err(musaic_db::Error::ForeignBatch(_))));
    assert!(queue::get(c, "late").unwrap().is_none());
    // Still listed, with its items in their (invalidated) state.
    let listed = queue::batch(c, BATCH).unwrap().unwrap();
    assert_eq!(listed.count("cancelled"), 3);
}

#[test]
fn renew_many_renews_each_lease_on_its_own() {
    let (_dir, db) = db();
    let c = db.connection();
    let ids = seed(c, "w", 3, Some(BATCH), "metadata.identify");
    let claimed = queue::claim_many_with_context(
        c,
        many("agent", "renew-0001", "r", 25),
        |_| "agent".into(),
        |_, _| Ok(true),
    )
    .unwrap()
    .unwrap();
    let at = NOW + 10_000 + 1_000;
    let mut leases: Vec<Lease<'_>> = claimed
        .claims
        .iter()
        .map(|r| lease(r, &r.work_id, "agent", at))
        .collect();
    leases[1].lease_token_hash = "someone-else";
    leases[2].claim_generation += 1;
    let stale = ids[0].clone();
    let answers = queue::renew_many(c, &leases, |_, row| Ok(row.id != stale)).unwrap();
    assert_eq!(answers[0], Err(Refusal::StaleContext));
    assert_eq!(answers[1], Err(Refusal::NotFound));
    assert_eq!(answers[2], Err(Refusal::StaleGeneration));
    let ok = queue::renew_many(c, &leases[..1], |_, _| Ok(true)).unwrap();
    assert_eq!(ok, vec![Ok(at + CLAIM_MS)]);
    assert_eq!(
        queue::get(c, &ids[0]).unwrap().unwrap().lease_expires_at,
        Some(at + CLAIM_MS)
    );
    // An expired lease is refused as expired.
    let late = Lease {
        now: at + 2 * CLAIM_MS,
        ..leases[0]
    };
    let answers = queue::renew_many(c, &[late], |_, _| Ok(true)).unwrap();
    assert_eq!(answers, vec![Err(Refusal::LeaseExpired)]);
}

#[test]
fn the_list_filters_by_batch_and_single_claims_still_work_on_batched_items() {
    let (_dir, db) = db();
    let c = db.connection();
    let batched = seed(c, "b", 2, Some(BATCH), "metadata.identify");
    seed(c, "l", 2, None, "metadata.identify");
    let listed = queue::list(
        c,
        List {
            principal_id: "agent",
            state: None,
            kind: None,
            batch_id: Some(BATCH),
            after: None,
            limit: 100,
        },
    )
    .unwrap();
    let mut got: Vec<_> = listed.iter().map(|r| r.id.clone()).collect();
    got.sort();
    assert_eq!(got, batched);
    let all = queue::list(
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
    assert_eq!(all.len(), 4);
    // FR-035: the single-item claim is unchanged for a batched item.
    let reply = queue::claim(c, claim(&batched[0], "agent", NOW + 100))
        .unwrap()
        .unwrap();
    assert_eq!(reply.work_id, batched[0]);
}

#[test]
fn batch_ids_stay_on_event_rows_after_clear_and_clear_drops_the_batch_and_its_replays() {
    let (_dir, db) = db();
    let c = db.connection();
    let ids = seed(c, "w", 2, Some(BATCH), "metadata.identify");
    let won = claim_many(c, many("agent", "clear-0001", "c", 1)).unwrap();
    assert_eq!(won.len(), 1);
    assert_eq!(
        queue::cancel_batch(c, BATCH, NOW + 20_000).unwrap(),
        Some(2)
    );
    let epoch = queue::epoch(c).unwrap();
    let before = musaic_db::delegation_events::page(c, &epoch, 0, &[], &[], 100).unwrap();
    assert!(before.iter().all(|e| e.batch_id.as_deref() == Some(BATCH)));
    assert_eq!(count(c, "work_replays"), 1);

    let removed =
        musaic_db::research_jobs::clear(c, &["failed".to_string()], NOW + 30_000).unwrap();
    assert_eq!(removed, 2);
    for id in &ids {
        assert!(queue::get(c, id).unwrap().is_none());
    }
    assert_eq!(
        count(c, "work_replays"),
        0,
        "the multi-item claim's answer goes too"
    );
    assert!(queue::batch(c, BATCH).unwrap().is_none());
    assert_eq!(count(c, "work_batches"), 0);
    let after = musaic_db::delegation_events::page(c, &epoch, 0, &[], &[], 100).unwrap();
    assert_eq!(after.len(), before.len());
    assert!(after.iter().all(|e| e.batch_id.as_deref() == Some(BATCH)));
}

#[test]
fn a_batch_counts_its_items_and_cancel_leaves_review_alone() {
    let (_dir, db) = db();
    let c = db.connection();
    let ids = seed(c, "w", 4, Some(BATCH), "metadata.identify");
    // One awaiting review, one claimed, two queued.
    queue::claim(c, claim(&ids[0], "agent", NOW + 100))
        .unwrap()
        .unwrap();
    c.execute(
        "UPDATE delegated_work SET state='awaiting_review', lease_token_hash=NULL,
           lease_expires_at=NULL WHERE id=?1",
        [&ids[0]],
    )
    .unwrap();
    queue::claim(c, claim(&ids[1], "agent", NOW + 100))
        .unwrap()
        .unwrap();
    let b = queue::batch(c, BATCH).unwrap().unwrap();
    assert_eq!(b.check_id, "unidentified-album");
    assert_eq!(b.started_at, NOW - 5);
    assert_eq!(b.finished_at, None);
    assert_eq!(b.size(), 4);
    assert_eq!(
        (
            b.count("awaiting_review"),
            b.count("claimed"),
            b.count("queued")
        ),
        (1, 1, 2)
    );
    assert_eq!(queue::cancel_batch(c, BATCH, NOW + 200).unwrap(), Some(3));
    let b = queue::batch(c, BATCH).unwrap().unwrap();
    assert_eq!((b.count("awaiting_review"), b.count("cancelled")), (1, 3));
    assert_eq!(queue::cancel_batch(c, OTHER_BATCH, NOW).unwrap(), None);
    queue::finish_batch(c, BATCH, NOW + 300).unwrap();
    assert_eq!(
        queue::batch(c, BATCH).unwrap().unwrap().finished_at,
        Some(NOW + 300)
    );
    assert_eq!(queue::batches(c).unwrap().len(), 1);
    assert_eq!(queue::batch_items(c, BATCH).unwrap().len(), 4);
}
