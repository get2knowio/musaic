//! Tool calls through a paired app on a copy of the public fixture library.

mod common;

use common::{AgentWorld, agent_world_scanned};
use musaic_commands::auth::hex_encode;
use musaic_commands::{Decision, PermissionKind as K, ToolErrorCode as Code, ToolOutput};
use musaic_db::delegated_work::{self as db, NewWork};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const WORK: &str = "3ef3f326-6a82-406c-a473-601562c3b579";
const RESEARCH: &[K] = &[K::LibraryRead, K::ProposalSubmit];

fn seed(w: &AgentWorld, kind: &str) -> (String, String) {
    let pair = w.pair("fixture reader", &[K::LibraryRead]);
    let reader = w.login(&pair.pairing.id);
    let albums = w
        .call(&reader, "musaic_library_list", json!({}))
        .unwrap()
        .to_json();
    let album_id = albums["items"][0]["albumId"].as_str().unwrap();
    let album = w
        .call(&reader, "musaic_album_get", json!({"albumId": album_id}))
        .unwrap()
        .to_json();
    let input = json!({
        "schemaVersion": 1,
        "albumId": album_id,
        "title": album["album"]["title"],
        "artist": album["album"]["artist"],
        "hint": null,
        "tracks": []
    })
    .to_string();
    let digest = hex_encode(&Sha256::digest(input.as_bytes()));
    let conn = musaic_db::open_connection(w.app.db_path()).unwrap();
    let now = w
        .app
        .now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    db::enqueue(
        &conn,
        &[NewWork {
            id: WORK,
            group_id: WORK,
            kind,
            album_id,
            base_revision: album["revision"].as_str().unwrap(),
            input_json: &input,
            input_digest: &digest,
            max_attempts: 3,
        }],
        now,
    )
    .unwrap();
    (album_id.into(), digest)
}

fn complete(claim: &Value, kind: &str, envelope: Value) -> Value {
    let mut body = lease(claim);
    let digest = &claim["inputDigest"];
    let mut result = json!({
        "schemaVersion":1, "workId":WORK, "kind":kind, "inputDigest":digest,
        "outcome":"proposal", "confidence":0.5, "uncertainties":[],
        "evidence":[{"source":"fixture:disc-label", "retrievedAt":"2026-11-22T00:00:00Z", "claim":"Label supplies a title", "limitations":[]}],
        "recommendation": envelope,
    });
    if result["recommendation"]["type"] == "unresolved" {
        result["outcome"] = json!("no_match");
    }
    body["inputDigest"] = digest.clone();
    body["idempotencyKey"] = json!("first-completion");
    body["result"] = result;
    body
}

fn lease(claim: &Value) -> Value {
    json!({"workId": WORK, "claimGeneration": claim["claimGeneration"], "leaseToken": claim["leaseToken"]})
}

fn claim_body(key: &str) -> Value {
    json!({"workId": WORK, "idempotencyKey": key})
}

fn code(result: Result<ToolOutput, musaic_commands::ToolError>) -> Code {
    result.unwrap_err().code
}

#[test]
fn separate_pairings_race_and_stale_attempts_are_fenced() {
    let w = agent_world_scanned();
    let (_, digest) = seed(&w, "metadata.identify");
    let alice = w.login(&w.pair("Alice", RESEARCH).pairing.id);
    let bob = w.login(&w.pair("Bob", RESEARCH).pairing.id);
    let a = w
        .call(&alice, "musaic_work_get", json!({"workId":WORK}))
        .unwrap()
        .to_json();
    assert_eq!(a["inputDigest"], digest);
    assert_eq!(a["schemaVersion"], 1);
    let barrier = std::sync::Barrier::new(2);
    let (one, two) = std::thread::scope(|s| {
        let a = s.spawn(|| {
            barrier.wait();
            w.call(&alice, "musaic_work_claim", claim_body("alice-claim"))
        });
        let b = s.spawn(|| {
            barrier.wait();
            w.call(&bob, "musaic_work_claim", claim_body("bob-claim"))
        });
        (a.join().unwrap(), b.join().unwrap())
    });
    let (winner, loser, key, successful) = match (one, two) {
        (Ok(winner), Err(other)) => {
            assert_eq!(other.code, Code::AlreadyClaimed);
            (&alice, &bob, "alice-claim", winner)
        }
        (Err(other), Ok(winner)) => {
            assert_eq!(other.code, Code::AlreadyClaimed);
            (&bob, &alice, "bob-claim", winner)
        }
        _ => panic!("the claim race must have one winner"),
    };
    let winning = successful.to_json();
    assert_eq!(
        w.call(winner, "musaic_work_claim", claim_body(key))
            .unwrap()
            .to_json(),
        winning
    );
    assert_eq!(
        code(w.call(loser, "musaic_work_get", json!({"workId":WORK}))),
        Code::NotFound
    );
    assert_eq!(
        code(w.call(loser, "musaic_work_renew", lease(&winning))),
        Code::NotFound
    );

    let mut progress = lease(&winning);
    progress["sequence"] = json!(2);
    progress["stage"] = json!("checking evidence");
    let updated = w
        .call(winner, "musaic_work_progress", progress.clone())
        .unwrap()
        .to_json();
    progress["sequence"] = json!(1);
    assert_eq!(
        w.call(winner, "musaic_work_progress", progress)
            .unwrap()
            .to_json(),
        updated
    );
    w.clock.advance(300);
    assert_eq!(
        code(w.call(winner, "musaic_work_renew", lease(&winning))),
        Code::LeaseExpired
    );
    let reclaimed = w
        .call(loser, "musaic_work_claim", claim_body("reclaim-lease"))
        .unwrap()
        .to_json();
    assert!(reclaimed["claimGeneration"].as_i64() > winning["claimGeneration"].as_i64());
    assert_eq!(
        code(w.call(winner, "musaic_work_renew", lease(&winning))),
        Code::StaleGeneration
    );
}

#[test]
fn completion_replays_after_restart_and_keeps_unverified_evidence_pending() {
    let mut w = agent_world_scanned();
    seed(&w, "metadata.identify");
    let id = w.pair("Disc researcher", RESEARCH).pairing.id;
    let researcher = w.login(&id);
    let claim = w
        .call(&researcher, "musaic_work_claim", claim_body("first-claim"))
        .unwrap()
        .to_json();
    let result = complete(
        &claim,
        "metadata.identify",
        json!({
            "type":"metadata",
            "identifiers":[{"type":"release_group","id":"6f23226c-84ec-42dc-ac3d-ce2f828647ec"}],
            "albumFields":{"title":"Found the title"},
            "tracks":[]
        }),
    );
    let first = w
        .call(&researcher, "musaic_work_complete", result.clone())
        .unwrap()
        .to_json();
    assert_eq!(first["state"], "awaiting_review");
    let conn = musaic_db::open_connection(w.app.db_path()).unwrap();
    let result_id = first["resultId"].as_str().unwrap();
    assert_eq!(
        db::result(&conn, result_id).unwrap().unwrap().validation,
        "pending"
    );
    let token = claim["leaseToken"].as_str().unwrap();
    let replay_has_secret: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM work_replays WHERE response_json LIKE '%' || ?1 || '%')",
            [token],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!replay_has_secret);
    drop(conn);

    w.app.shutdown();
    w.app = musaic_app::App::builder(&w.scratch.config)
        .agent_secrets(w.store.clone())
        .clock(w.clock.wall())
        .subscribers(false)
        .check_sweep(false)
        .ai_worker(false)
        .auto_snapshot(false)
        .build()
        .unwrap();
    let fresh = w.login(&id);
    assert_eq!(
        w.call(&fresh, "musaic_work_claim", claim_body("first-claim"))
            .unwrap()
            .to_json(),
        claim
    );
    assert_eq!(
        w.call(&fresh, "musaic_work_complete", result.clone())
            .unwrap()
            .to_json(),
        first
    );
    let mut different = result;
    different["result"]["confidence"] = json!(0.3);
    assert_eq!(
        code(w.call(&fresh, "musaic_work_complete", different)),
        Code::IdempotencyConflict
    );
    let conn = musaic_db::open_connection(w.app.db_path()).unwrap();
    assert_eq!(
        conn.query_row("SELECT count(*) FROM work_results", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn cancellation_requires_explicit_policy_and_respects_restore_gate() {
    let w = agent_world_scanned();
    seed(&w, "metadata.correct");
    let researcher = w.login(&w.pair("Reader", RESEARCH).pairing.id);
    let owner = w
        .pair("Research coordinator", &[K::LibraryRead, K::WorkCancel])
        .pairing
        .id;
    let operator = w.login(&owner);
    let request = json!({"workId":WORK, "expectedRevision":1});
    assert_eq!(
        code(w.call(&researcher, "musaic_work_cancel", request.clone())),
        Code::PolicyDenied
    );
    assert_eq!(
        code(w.call(&operator, "musaic_work_cancel", request.clone())),
        Code::PolicyDenied
    );
    w.app
        .ops()
        .agent()
        .set_policy(&owner, &[(K::WorkCancel, Decision::Allow)])
        .unwrap();
    let restore = common::hold_restore(&w.app);
    assert_eq!(
        code(w.call(&operator, "musaic_work_cancel", request.clone())),
        Code::Restoring
    );
    drop(restore);
    assert_eq!(
        w.call(&operator, "musaic_work_cancel", request)
            .unwrap()
            .to_json()["state"],
        "cancelled"
    );
}

#[test]
fn hostile_cover_is_refused_and_retry_backoff_is_enforced() {
    let w = agent_world_scanned();
    seed(&w, "cover.research");
    let p = w.login(&w.pair("Cover researcher", RESEARCH).pairing.id);
    let claim = w
        .call(&p, "musaic_work_claim", claim_body("cover-first-claim"))
        .unwrap()
        .to_json();
    let result = complete(
        &claim,
        "cover.research",
        json!({
            "type":"cover", "url":"https://127.0.0.1/cover.png",
            "sourcePage":"https://covers.example/record",
            "rationale":"Cover matches the public catalog", "rightsUncertainty":"Use is undocumented"
        }),
    );
    assert_eq!(
        code(w.call(&p, "musaic_work_complete", result)),
        Code::InvalidResult
    );

    let fail = |c: &Value| {
        let mut body = lease(c);
        body["code"] = json!("source_missing");
        body["message"] = json!("No source has the cover");
        body["retryable"] = json!(true);
        w.call(&p, "musaic_work_fail", body).unwrap().to_json()
    };
    assert_eq!(fail(&claim)["state"], "queued");
    assert_eq!(
        code(w.call(&p, "musaic_work_claim", claim_body("cover-second-claim"))),
        Code::RateLimited
    );
    w.clock.advance(2);
    let second = w
        .call(&p, "musaic_work_claim", claim_body("cover-second-claim"))
        .unwrap()
        .to_json();
    assert_eq!(fail(&second)["state"], "queued");
    w.clock.advance(4);
    let third = w
        .call(&p, "musaic_work_claim", claim_body("cover-third-claim"))
        .unwrap()
        .to_json();
    let done = fail(&third);
    assert_eq!(done["state"], "failed");
    assert_eq!(done["attempts"], 3);
}

#[test]
fn completion_is_stale_after_the_album_files_change() {
    let w = agent_world_scanned();
    let (album_id, _) = seed(&w, "metadata.correct");
    let p = w.login(&w.pair("Researcher", RESEARCH).pairing.id);
    let claim = w
        .call(&p, "musaic_work_claim", claim_body("claim-before-edit"))
        .unwrap()
        .to_json();
    let conn = musaic_db::open_connection(w.app.db_path()).unwrap();
    let row =
        musaic_services::agent_read::cached_row(&conn, w.app.library_dir().as_deref(), &album_id)
            .unwrap();
    let path = musaic_scan::list_flacs(std::path::Path::new(&row.album.path)).remove(0);
    std::fs::rename(&path, path.with_file_name("Edited name.flac")).unwrap();
    let result = complete(
        &claim,
        "metadata.correct",
        json!({
            "type":"unresolved", "explanation":"The same title occurs on different discs",
            "question":"Which catalog number is printed on the sleeve?"
        }),
    );
    assert_eq!(
        code(w.call(&p, "musaic_work_complete", result)),
        Code::StaleContext
    );
}

#[test]
fn active_claim_and_original_expiry_survive_shutdown() {
    let mut w = agent_world_scanned();
    seed(&w, "metadata.identify");
    let id = w.pair("Researcher", RESEARCH).pairing.id;
    let principal = w.login(&id);
    let claim = w
        .call(&principal, "musaic_work_claim", claim_body("still-working"))
        .unwrap()
        .to_json();
    w.clock.advance(100);
    w.app.shutdown();
    w.app = musaic_app::App::builder(&w.scratch.config)
        .agent_secrets(w.store.clone())
        .clock(w.clock.wall())
        .subscribers(false)
        .check_sweep(false)
        .ai_worker(false)
        .auto_snapshot(false)
        .build()
        .unwrap();
    let resumed = w.login(&id);
    assert_eq!(
        w.call(&resumed, "musaic_work_claim", claim_body("still-working"))
            .unwrap()
            .to_json(),
        claim
    );
    let mut progress = lease(&claim);
    progress["sequence"] = json!(1);
    progress["stage"] = json!("researching");
    assert_eq!(
        w.call(&resumed, "musaic_work_progress", progress)
            .unwrap()
            .to_json()["state"],
        "running"
    );
    w.clock.advance(200);
    assert_eq!(
        code(w.call(&resumed, "musaic_work_renew", lease(&claim))),
        Code::LeaseExpired
    );
}

#[test]
fn changing_the_configured_library_invalidates_an_external_claim() {
    let w = agent_world_scanned();
    seed(&w, "metadata.identify");
    let p = w.login(&w.pair("Owner", RESEARCH).pairing.id);
    let claim = w
        .call(&p, "musaic_work_claim", claim_body("claim-before-switch"))
        .unwrap()
        .to_json();
    assert_eq!(w.app.work_snapshot().external_claims, 1);
    let replacement = w.scratch.tmp.path().join("replacement-library");
    std::fs::create_dir(&replacement).unwrap();
    w.app
        .ops()
        .library()
        .set_library_dir(replacement.to_str().unwrap())
        .unwrap();
    assert_eq!(w.app.work_snapshot().external_claims, 0);
    assert_eq!(
        code(w.call(&p, "musaic_work_renew", lease(&claim))),
        Code::StaleContext
    );
    let items = w.call(&p, "musaic_work_list", json!({})).unwrap().to_json();
    assert_eq!(items["items"], json!([]));
}

#[test]
fn restoring_a_claimed_snapshot_invalidates_its_lease_and_outbox() {
    use musaic_events::{AgentWhat, Event, EventKind, Subscriber, Topic};
    use std::sync::{Arc, Mutex};

    let w = agent_world_scanned();
    seed(&w, "metadata.identify");
    let p = w.login(&w.pair("Owner", RESEARCH).pairing.id);
    let claim = w
        .call(&p, "musaic_work_claim", claim_body("claim-before-restore"))
        .unwrap()
        .to_json();
    let snapshot = w.app.work_snapshot();
    assert_eq!(snapshot.external_claims, 1);
    assert!(snapshot.running().is_empty());
    let message = snapshot.quit_prompt().unwrap();
    assert!(message.contains("Musaic reopens") && message.contains("expire"));
    assert!(w.app.ops().database().restore_allowed().is_ok());

    {
        let conn = musaic_db::open_connection(w.app.db_path()).unwrap();
        let tx = musaic_db::immediate(&conn).unwrap();
        let epoch = db::epoch(&tx).unwrap();
        db::record_change(
            &tx,
            &epoch,
            Some(WORK),
            None,
            "committed_without_notification",
            1800000000000,
        )
        .unwrap();
        tx.commit().unwrap();
    }
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    w.app.bus().register(
        Topic::Kind(EventKind::AgentStateChanged),
        Subscriber::from_fn(move |event| {
            if let Event::AgentStateChanged {
                what: AgentWhat::Work,
                ids,
            } = event
            {
                sink.lock().unwrap().push(ids.clone());
            }
            Ok(())
        })
        .key("delegation-restore")
        .name("delegation-restore"),
    );

    let ops = w.app.ops();
    let db = ops.database();
    let backup = db.backup().unwrap();
    db.restore_from(&backup.file.path).unwrap();
    assert_eq!(w.app.work_snapshot().external_claims, 0);
    assert_eq!(
        code(w.call(&p, "musaic_work_renew", lease(&claim))),
        Code::StaleContext
    );
    assert_eq!(seen.lock().unwrap().as_slice(), &[Vec::<String>::new()]);
}

#[test]
fn a_saved_scan_makes_waiting_legacy_work_claimable_when_the_album_returns() {
    let mut w = common::agent_world();
    let lib = common::configure(&w.app, &w.scratch);
    let album = lib.join("Aurora Fields - First Light [FLAC]");
    // The scanner's resolver: on Windows canonicalize() leaves a `\\?\` prefix
    // that the album id never hashes.
    let album_path = musaic_scan::py_resolve(&album);
    let album_id = musaic_core::album_id(album_path.to_str().unwrap());
    let away = w.scratch.tmp.path().join("album temporarily absent");
    std::fs::rename(&album, &away).unwrap();
    {
        let conn = musaic_db::open_connection(w.app.db_path()).unwrap();
        // The intentional migration placeholder has a legacy reference
        // without a snapshot-derived context.
        db::enqueue(
            &conn,
            &[NewWork {
                id: WORK,
                group_id: WORK,
                kind: "metadata.identify",
                album_id: &album_id,
                base_revision: "",
                input_json: "{}",
                input_digest: "legacy:waiting:metadata.identify",
                max_attempts: 3,
            }],
            0,
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ai_jobs (id,album_id,album_path,job_type,status,created_at)
             VALUES ('waiting',?1,?2,'metadata','queued','2026-11-22T00:00:00Z')",
            [&album_id, album_path.to_str().unwrap()],
        )
        .unwrap();
        conn.execute(
            "UPDATE delegated_work SET legacy_job_id='waiting' WHERE id=?1",
            [WORK],
        )
        .unwrap();
    }
    w.app.shutdown();
    w.app = musaic_app::App::builder(&w.scratch.config)
        .network(musaic_services::production::Network::Offline)
        .fingerprinting(musaic_services::production::Fingerprinting::Local(
            std::sync::Arc::new(common::Fixed),
        ))
        .pool_sleeper(std::sync::Arc::new(common::NoSleep))
        .agent_secrets(w.store.clone())
        .clock(w.clock.wall())
        .subscribers(false)
        .check_sweep(false)
        .ai_worker(false)
        .auto_snapshot(false)
        .build()
        .unwrap();

    let agent = w.login(&w.pair("Fixture researcher", RESEARCH).pairing.id);
    let listed = || {
        w.call(&agent, "musaic_work_list", json!({}))
            .unwrap()
            .to_json()
    };
    assert_eq!(listed()["items"], json!([]));
    assert!(w.app.ops().scan().subscribe(true).unwrap().count() > 2);
    assert_eq!(listed()["items"], json!([]));
    common::wait_for_fingerprints(&w.app);

    let events = w.app.relay().connect();
    std::fs::rename(&away, &album).unwrap();
    assert!(w.app.ops().scan().subscribe(true).unwrap().count() > 2);
    assert_eq!(listed()["items"][0]["workId"], WORK);
    let changes = std::iter::from_fn(|| events.try_recv());
    assert!(
        changes
            .filter(|e| e.name.as_ref() == "AgentStateChanged")
            .any(|e| {
                let event: Value = serde_json::from_str(&e.data).unwrap();
                event["what"] == "work" && event["ids"] == json!([WORK])
            })
    );
    let job = w
        .call(&agent, "musaic_work_get", json!({"workId":WORK}))
        .unwrap()
        .to_json();
    assert_eq!(job["context"]["title"], "First Light");
    assert!(!job["context"]["tracks"].as_array().unwrap().is_empty());
    assert_eq!(
        w.call(&agent, "musaic_work_claim", claim_body("returning-fixture"))
            .unwrap()
            .to_json()["workId"],
        WORK
    );
}
