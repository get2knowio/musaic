//! `events/list` and `events/poll` through the app's operations (spec 005,
//! contracts/events.md tests 2 to 8 and 11), with the injected clock.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{AgentWorld, agent_world_scanned};
use musaic_app::ops::Listening;
use musaic_app::ops::agent::Principal;
use musaic_app::ops::apply::{ApplyChanges, ApplyEvent, ApplyOutcome, TagEvent};
use musaic_commands::auth::hex_encode;
use musaic_commands::events::{
    EVENT_REQUESTED, EVENT_UPDATED, EventArguments, MAX_WAIT_MS, PollParams, PollResult,
};
use musaic_commands::tools::work::{WorkKind, WorkState};
use musaic_commands::{Decision, PermissionKind as K, ToolErrorCode as Code};
use musaic_db::delegated_work::{self as db, NewWork};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const WORK: &str = "3ef3f326-6a82-406c-a473-601562c3b579";
const RESEARCH: &[K] = &[K::LibraryRead, K::ProposalSubmit];

fn now_ms(w: &AgentWorld) -> i64 {
    w.app
        .now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// One queued work item of `kind` on the first album.
fn seed(w: &AgentWorld, id: &str, kind: &str, album: usize) -> String {
    let reader = w.login(&w.pair(&format!("seed {id}"), &[K::LibraryRead]).pairing.id);
    let albums = w
        .call(&reader, "musaic_library_list", json!({}))
        .unwrap()
        .to_json();
    let album_id = albums["items"][album]["albumId"].as_str().unwrap();
    let rec = w
        .call(&reader, "musaic_album_get", json!({"albumId": album_id}))
        .unwrap()
        .to_json();
    let input = json!({
        "schemaVersion": 1, "albumId": album_id, "title": rec["album"]["title"],
        "artist": rec["album"]["artist"], "hint": null, "tracks": []
    })
    .to_string();
    let digest = hex_encode(&Sha256::digest(input.as_bytes()));
    let conn = musaic_db::open_connection(w.app.db_path()).unwrap();
    db::enqueue(
        &conn,
        &[NewWork {
            id,
            group_id: id,
            kind,
            album_id,
            base_revision: rec["revision"].as_str().unwrap(),
            input_json: &input,
            input_digest: &digest,
            max_attempts: 3,
        }],
        now_ms(w),
    )
    .unwrap();
    // Publish the change as the app does after a commit.
    w.app.ops().agent().sweep_delegation();
    album_id.into()
}

fn params(name: &str, cursor: Option<&str>) -> PollParams {
    PollParams {
        name: name.into(),
        cursor: cursor.map(str::to_string),
        ..Default::default()
    }
}

fn poll(w: &AgentWorld, p: &Principal, params: &PollParams) -> PollResult {
    w.app.ops().agent().events_poll(p, params).unwrap()
}

fn head(w: &AgentWorld, p: &Principal) -> String {
    poll(w, p, &params(EVENT_REQUESTED, None)).cursor
}

fn claim(w: &AgentWorld, p: &Principal, id: &str, key: &str) -> Value {
    w.call(
        p,
        "musaic_work_claim",
        json!({"workId": id, "idempotencyKey": key}),
    )
    .unwrap()
    .to_json()
}

fn lease(c: &Value, id: &str) -> Value {
    json!({"workId": id, "claimGeneration": c["claimGeneration"], "leaseToken": c["leaseToken"]})
}

fn names(r: &PollResult) -> Vec<(String, String, WorkState)> {
    r.events
        .iter()
        .map(|e| (e.name.clone(), e.data.work_id.clone(), e.data.state))
        .collect()
}

fn researcher(w: &AgentWorld, name: &str) -> Principal {
    w.login(&w.pair(name, RESEARCH).pairing.id)
}

#[test]
fn a_null_cursor_returns_the_head_then_new_work_is_one_requested_event() {
    let w = agent_world_scanned();
    let alice = researcher(&w, "Alice");
    let boot = poll(&w, &alice, &params(EVENT_REQUESTED, None));
    assert!(boot.events.is_empty() && !boot.truncated && !boot.has_more);
    seed(&w, WORK, "metadata.identify", 0);
    let r = poll(&w, &alice, &params(EVENT_REQUESTED, Some(&boot.cursor)));
    assert_eq!(
        names(&r),
        [(EVENT_REQUESTED.into(), WORK.into(), WorkState::Queued)]
    );
    let e = &r.events[0];
    assert!(e.event_id.starts_with("evt_") && e.cursor.is_some());
    assert_eq!(e.data.kind, WorkKind::MetadataIdentify);
    // The same cursor again, and a fresh poll from the new cursor.
    assert_eq!(
        poll(&w, &alice, &params(EVENT_REQUESTED, Some(&boot.cursor))),
        r
    );
    assert!(
        poll(&w, &alice, &params(EVENT_REQUESTED, Some(&r.cursor)))
            .events
            .is_empty()
    );
}

#[test]
fn a_claim_is_an_updated_event_only_its_holder_sees() {
    let w = agent_world_scanned();
    let (alice, bob) = (researcher(&w, "Alice"), researcher(&w, "Bob"));
    seed(&w, WORK, "metadata.identify", 0);
    let (ca, cb) = (head(&w, &alice), head(&w, &bob));
    claim(&w, &alice, WORK, "alice-claim");
    let mine = poll(&w, &alice, &params(EVENT_UPDATED, Some(&ca)));
    assert_eq!(
        names(&mine),
        [(EVENT_UPDATED.into(), WORK.into(), WorkState::Claimed)]
    );
    // Bob is not the holder: skipped, and his cursor still advances.
    let theirs = poll(&w, &bob, &params(EVENT_UPDATED, Some(&cb)));
    assert!(theirs.events.is_empty() && !theirs.truncated);
    assert_ne!(theirs.cursor, cb);
}

#[test]
fn a_cursor_resumes_across_a_reopened_database_with_identical_ids() {
    let mut w = agent_world_scanned();
    let alice = researcher(&w, "Alice");
    let id = alice.pairing_id.clone();
    let boot = head(&w, &alice);
    seed(&w, WORK, "metadata.identify", 0);
    let before = poll(&w, &alice, &params(EVENT_REQUESTED, Some(&boot)));
    assert_eq!(before.events.len(), 1);
    w.app.shutdown();
    w.app = musaic_app::App::builder(&w.scratch.config)
        .agent_secrets(w.store.clone())
        .clock(w.clock.wall())
        .subscribers(false)
        .check_sweep(false)
        .auto_snapshot(false)
        .build()
        .unwrap();
    let alice = w.login(&id);
    let after = poll(&w, &alice, &params(EVENT_REQUESTED, Some(&boot)));
    assert_eq!(after, before, "same cursor, same events, same eventIds");
}

#[test]
fn old_history_and_a_new_epoch_truncate_a_cursor() {
    let w = agent_world_scanned();
    let alice = researcher(&w, "Alice");
    let boot = head(&w, &alice);
    seed(&w, WORK, "metadata.identify", 0);
    let after_one = poll(&w, &alice, &params(EVENT_REQUESTED, Some(&boot))).cursor;
    // Eight days later the app's upkeep prunes the history.
    w.clock.advance(8 * 24 * 3600);
    w.app.ops().agent().sweep_delegation();
    let again = researcher(&w, "Alice again");
    let stale = poll(&w, &again, &params(EVENT_REQUESTED, Some(&boot)));
    assert!(stale.truncated && stale.events.is_empty(), "{stale:?}");
    assert_ne!(stale.cursor, boot);
    // The fresh cursor works.
    let ok = poll(&w, &again, &params(EVENT_REQUESTED, Some(&stale.cursor)));
    assert!(!ok.truncated);
    let _ = after_one;

    // A restore or root change rotates the epoch.
    let conn = musaic_db::open_connection(w.app.db_path()).unwrap();
    let cursor = head(&w, &again);
    db::rotate_epoch(&conn, None, now_ms(&w)).unwrap();
    let rotated = poll(&w, &again, &params(EVENT_REQUESTED, Some(&cursor)));
    assert!(rotated.truncated && rotated.events.is_empty());
    // maxAgeMs raises the floor the same way.
    let c = head(&w, &again);
    seed(
        &w,
        "6c1d2e3f-0000-4000-8000-000000000001",
        "cover.research",
        1,
    );
    w.clock.advance(120);
    let mut aged = params(EVENT_REQUESTED, Some(&c));
    aged.max_age_ms = Some(60_000);
    assert!(poll(&w, &again, &aged).truncated);
    aged.max_age_ms = Some(600_000);
    assert!(!poll(&w, &again, &aged).truncated);
}

#[test]
fn kind_and_state_filters_and_bad_arguments() {
    let w = agent_world_scanned();
    let alice = researcher(&w, "Alice");
    let boot = head(&w, &alice);
    seed(&w, WORK, "metadata.identify", 0);
    seed(
        &w,
        "6c1d2e3f-0000-4000-8000-000000000001",
        "cover.research",
        1,
    );
    let mut p = params(EVENT_REQUESTED, Some(&boot));
    p.arguments = Some(EventArguments {
        kinds: Some(vec![WorkKind::CoverResearch]),
        states: None,
    });
    let covers = poll(&w, &alice, &p);
    assert_eq!(covers.events.len(), 1);
    assert_eq!(covers.events[0].data.kind, WorkKind::CoverResearch);
    p.arguments = Some(EventArguments {
        kinds: None,
        states: Some(vec![WorkState::Claimed]),
    });
    assert!(poll(&w, &alice, &p).events.is_empty());
    let all = poll(&w, &alice, &params(EVENT_REQUESTED, Some(&boot)));
    assert_eq!(all.events.len(), 2);
    // maxEvents pages and says there is more.
    let mut one = params(EVENT_REQUESTED, Some(&boot));
    one.max_events = Some(1);
    let first = poll(&w, &alice, &one);
    assert!(first.has_more && first.events.len() == 1);
    one.cursor = Some(first.cursor);
    let second = poll(&w, &alice, &one);
    assert!(!second.has_more && second.events.len() == 1);
    assert_ne!(first.events[0].event_id, second.events[0].event_id);
    // Bad parameters.
    for bad in [
        PollParams {
            max_events: Some(0),
            ..params(EVENT_REQUESTED, None)
        },
        PollParams {
            max_events: Some(101),
            ..params(EVENT_REQUESTED, None)
        },
        PollParams {
            wait_ms: Some(25_001),
            ..params(EVENT_REQUESTED, None)
        },
    ] {
        assert_eq!(
            w.app
                .ops()
                .agent()
                .events_poll(&alice, &bad)
                .unwrap_err()
                .code,
            Code::EventInvalid
        );
    }
    assert_eq!(
        w.app
            .ops()
            .agent()
            .events_poll(&alice, &params("musaic.nope.v1", None))
            .unwrap_err()
            .code,
        Code::EventNotFound
    );
    assert_eq!(
        w.app
            .ops()
            .agent()
            .events_list(&alice)
            .unwrap()
            .events
            .len(),
        2
    );
}

#[test]
fn scope_policy_and_revocation_refuse_events() {
    let w = agent_world_scanned();
    let blind = w.login(&w.pair("No read", &[K::ProposalSubmit]).pairing.id);
    assert_eq!(
        w.app.ops().agent().events_list(&blind).unwrap_err().code,
        Code::EventForbidden
    );
    let owner = w.pair("Strict", &[K::LibraryRead]).pairing.id;
    let strict = w.login(&owner);
    w.app
        .ops()
        .agent()
        .set_policy(&owner, &[(K::LibraryRead, Decision::Refuse)])
        .unwrap();
    assert_eq!(
        w.app
            .ops()
            .agent()
            .events_poll(&strict, &params(EVENT_REQUESTED, None))
            .unwrap_err()
            .code,
        Code::EventForbidden
    );
    let gone = researcher(&w, "Gone");
    w.app
        .ops()
        .agent()
        .revoke_pairing(&gone.pairing_id)
        .unwrap();
    assert_eq!(
        w.app
            .ops()
            .agent()
            .events_poll(&gone, &params(EVENT_REQUESTED, None))
            .unwrap_err()
            .code,
        Code::EventForbidden
    );
}

fn waiting(
    w: &Arc<AgentWorld>,
    p: &Principal,
    ms: u32,
) -> std::thread::JoinHandle<Result<PollResult, Code>> {
    let (w, p) = (Arc::clone(w), p.clone());
    std::thread::spawn(move || {
        let mut q = params(EVENT_REQUESTED, None);
        q.wait_ms = Some(ms);
        // Start from the head so the poll has nothing to return.
        q.cursor = Some(head(&w, &p));
        w.app.ops().agent().events_poll(&p, &q).map_err(|e| e.code)
    })
}

#[test]
fn a_parked_poll_wakes_on_new_work_and_on_revocation() {
    let w = Arc::new(agent_world_scanned());
    let alice = researcher(&w, "Alice");
    let started = Instant::now();
    let parked = waiting(&w, &alice, 20_000);
    std::thread::sleep(Duration::from_millis(500));
    seed(&w, WORK, "metadata.identify", 0);
    let r = parked.join().unwrap().unwrap();
    assert_eq!(r.events.len(), 1);
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "woken, not timed out"
    );

    let parked = waiting(&w, &alice, 20_000);
    std::thread::sleep(Duration::from_millis(500));
    w.app
        .ops()
        .agent()
        .revoke_pairing(&alice.pairing_id)
        .unwrap();
    assert_eq!(parked.join().unwrap().unwrap_err(), Code::EventForbidden);
}

#[test]
fn a_poll_without_news_ends_at_its_wait() {
    let w = Arc::new(agent_world_scanned());
    let alice = researcher(&w, "Alice");
    let started = Instant::now();
    let r = waiting(&w, &alice, 1_000).join().unwrap().unwrap();
    assert!(r.events.is_empty() && !r.truncated);
    assert!(started.elapsed() >= Duration::from_millis(900));
}

/// One step of an apply as a letter, so two runs can be compared step by step.
fn step(e: &ApplyEvent) -> char {
    match e {
        ApplyEvent::Tags(TagEvent::Start { .. }) => 'S',
        ApplyEvent::Tags(TagEvent::FileStarted { .. }) => 'f',
        ApplyEvent::Tags(TagEvent::FileDone(_)) => 'd',
        ApplyEvent::Tags(TagEvent::FileRenamed { .. }) => 'r',
        ApplyEvent::Tags(TagEvent::Done { .. }) => 'D',
        ApplyEvent::UpdatedAlbum(_) => 'U',
        other => panic!("unexpected {other:?}"),
    }
}

/// A year apply on `album` run to its end: its steps, once the work guard is
/// released and the background subscribers it woke have drained.
fn apply_year(w: &AgentWorld, album: &str, year: &str) -> String {
    let changes = ApplyChanges {
        suggestions: vec![("year".into(), year.into())],
        ..Default::default()
    };
    let mut steps = String::new();
    let outcome = w
        .app
        .ops()
        .apply()
        .apply_changes(album, &changes, &mut |e| {
            steps.push(step(&e));
            Listening::Yes
        })
        .unwrap();
    assert_eq!(outcome, ApplyOutcome::Finished);
    assert!(w.app.bus().wait_background_idle(Duration::from_secs(30)));
    let deadline = Instant::now() + Duration::from_secs(30);
    while w.app.gate().snapshot() != (0, false) {
        assert!(Instant::now() < deadline, "the work guard was released");
        std::thread::sleep(Duration::from_millis(5));
    }
    steps
}

/// The album's stored year.
fn stored_year(w: &AgentWorld, album: &str) -> Option<String> {
    let albums = w.app.ops().library().list_albums(false).unwrap().albums;
    let a = albums.iter().find(|a| a.id() == album).unwrap();
    a.iter()
        .find(|(k, _)| *k == "year")
        .and_then(|(_, v)| v.as_str().map(str::to_string))
}

#[test]
fn the_fifth_waiting_poll_is_refused_and_parked_polls_delay_nothing() {
    let w = Arc::new(agent_world_scanned());
    let alice = researcher(&w, "Alice");
    let album = w.app.ops().library().list_albums(false).unwrap().albums[0]
        .id()
        .to_string();
    // The same apply with no poll parked, to compare against.
    let alone = apply_year(&w, &album, "2031");
    assert!(alone.starts_with('S') && alone.ends_with("DU"), "{alone}");
    let cursor = head(&w, &alice);
    // The longest wait a poll may ask for, so only the revocation below ends
    // them and "still parked" never depends on how fast this machine is.
    let parked: Vec<_> = (0..4).map(|_| waiting(&w, &alice, MAX_WAIT_MS)).collect();
    // Give the four time to park: the fifth is refused at once.
    std::thread::sleep(Duration::from_millis(1_500));
    let mut fifth = params(EVENT_REQUESTED, Some(&cursor));
    fifth.wait_ms = Some(5_000);
    let started = Instant::now();
    let code = w
        .app
        .ops()
        .agent()
        .events_poll(&alice, &fifth)
        .unwrap_err()
        .code;
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(code, Code::EventLimit);
    // A concurrent tool call, an album apply and a library scan are not
    // delayed by them: each runs to its end while all four are still parked.
    let t = Instant::now();
    w.call(&alice, "musaic_work_list", json!({})).unwrap();
    assert!(t.elapsed() < Duration::from_secs(5));
    assert!(parked.iter().all(|h| !h.is_finished()), "still parked");
    // The apply's events wake the parked polls, which find nothing for them
    // and park again; the apply takes the same steps as with none parked.
    let beside = apply_year(&w, &album, "2032");
    assert_eq!(beside, alone, "the same run with four polls parked");
    assert_eq!(stored_year(&w, &album).as_deref(), Some("2032"));
    assert!(parked.iter().all(|h| !h.is_finished()), "still parked");
    let n = w.app.ops().scan().subscribe(false).unwrap().count();
    assert!(n > 2, "a scan ran to its end while four polls were parked");
    assert!(parked.iter().all(|h| !h.is_finished()), "still parked");
    // Still four waiting: neither the apply nor the scan freed a slot.
    let again = w.app.ops().agent().events_poll(&alice, &fifth);
    assert_eq!(again.unwrap_err().code, Code::EventLimit);
    // Another pairing is not limited by Alice's polls.
    let bob = researcher(&w, "Bob");
    assert!(
        w.app
            .ops()
            .agent()
            .events_poll(&bob, &params(EVENT_REQUESTED, None))
            .is_ok()
    );
    w.app
        .ops()
        .agent()
        .revoke_pairing(&alice.pairing_id)
        .unwrap();
    for h in parked {
        assert_eq!(h.join().unwrap().unwrap_err(), Code::EventForbidden);
    }
}

fn data_keys(r: &PollResult) -> Vec<Vec<String>> {
    r.events
        .iter()
        .map(|e| {
            let v = serde_json::to_value(&e.data).unwrap();
            let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
            k.sort();
            k
        })
        .collect()
}

#[test]
fn every_transition_of_a_lifecycle_carries_exactly_the_five_keys() {
    let w = agent_world_scanned();
    let alice = researcher(&w, "Alice");
    let start = head(&w, &alice);
    seed(&w, WORK, "metadata.identify", 0);
    let c = claim(&w, &alice, WORK, "life-claim-1");
    let mut progress = lease(&c, WORK);
    progress["sequence"] = json!(1);
    progress["stage"] = json!("reading");
    w.call(&alice, "musaic_work_progress", progress).unwrap();
    let mut fail = lease(&c, WORK);
    fail["code"] = json!("source_missing");
    fail["message"] = json!("nothing found");
    fail["retryable"] = json!(true);
    w.call(&alice, "musaic_work_fail", fail).unwrap();
    w.clock.advance(5);
    let c2 = claim(&w, &alice, WORK, "life-claim-2");
    let mut release = lease(&c2, WORK);
    release["reason"] = json!("cannot continue");
    w.call(&alice, "musaic_work_release", release).unwrap();
    let all = poll(
        &w,
        &alice,
        &PollParams {
            max_events: Some(100),
            ..params(EVENT_REQUESTED, Some(&start))
        },
    );
    // Only `requested` events came back; ask for `updated` too.
    let updated = poll(&w, &alice, &params(EVENT_UPDATED, Some(&start)));
    let seen: Vec<_> = names(&all)
        .into_iter()
        .chain(names(&updated))
        .map(|(n, _, s)| format!("{n}:{s:?}"))
        .collect();
    assert!(
        seen.iter().filter(|s| s.contains("requested")).count() >= 3,
        "{seen:?}"
    );
    assert!(seen.iter().any(|s| s.contains("Claimed")), "{seen:?}");
    assert!(seen.iter().any(|s| s.contains("Running")), "{seen:?}");
    for keys in data_keys(&all).into_iter().chain(data_keys(&updated)) {
        assert_eq!(
            keys,
            ["kind", "libraryEpoch", "revision", "state", "workId"]
        );
    }
}

#[test]
fn work_events_of_a_batch_carry_its_reference_and_keep_it_after_clear() {
    use musaic_app::ops::fixes::{Listening, QuickFixEvent};
    let w = agent_world_scanned();
    w.app
        .ops()
        .checks()
        .run("unidentified-album", "all", None)
        .unwrap();
    let alice = researcher(&w, "Alice");
    let start = head(&w, &alice);
    let mut batch = None;
    w.app
        .ops()
        .fixes()
        .quick_fix_all("unidentified-album", true, &mut |e| {
            if let QuickFixEvent::Done(s) = &e {
                batch = s.research.as_ref().and_then(|r| r.batch_id.clone());
            }
            Listening::Yes
        })
        .unwrap();
    let batch = batch.expect("a batch");
    let r = poll(&w, &alice, &params(EVENT_REQUESTED, Some(&start)));
    assert!(!r.events.is_empty());
    for e in &r.events {
        assert_eq!(e.data.batch_id.as_deref(), Some(batch.as_str()));
        let json = serde_json::to_value(&e.data).unwrap();
        assert_eq!(json["batchId"], json!(batch));
    }
    // A request made on its own carries no batch: the key is absent.
    seed(&w, WORK, "cover.research", 0);
    let r2 = poll(&w, &alice, &params(EVENT_REQUESTED, Some(&r.cursor)));
    let single = r2.events.iter().find(|e| e.data.work_id == WORK).unwrap();
    assert!(single.data.batch_id.is_none());
    assert!(
        serde_json::to_value(&single.data)
            .unwrap()
            .get("batchId")
            .is_none()
    );

    // Cancel and Clear the batch: its items are gone, the event history keeps
    // the reference on every row.
    w.app.ops().jobs().cancel_research_batch(&batch).unwrap();
    w.app
        .ops()
        .jobs()
        .delete_many(&["failed".to_string()])
        .unwrap();
    let conn = musaic_db::open_connection(w.app.db_path()).unwrap();
    let left: i64 = conn
        .query_row(
            "SELECT count(*) FROM delegated_work WHERE batch_id=?1",
            [&batch],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(left, 0);
    let epoch = db::epoch(&conn).unwrap();
    let rows = musaic_db::delegation_events::page(&conn, &epoch, 0, &[], &[], 1000).unwrap();
    let batched: Vec<_> = rows
        .iter()
        .filter(|e| e.batch_id.as_deref() == Some(batch.as_str()))
        .collect();
    assert!(
        batched.len() >= 2 * r.events.len(),
        "requested and cancelled rows"
    );
    // The poll still answers (the cleared items are simply no longer visible).
    poll(&w, &alice, &params(EVENT_UPDATED, Some(&start)));
}
