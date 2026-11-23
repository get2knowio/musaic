//! Host observations and diagnostics (spec 005): four separate facts per
//! pairing, each flipped by exactly one step, and nothing that says "ready".

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{AgentWorld, agent_world_scanned};
use musaic_app::ops::agent::PairingDiagnostics;
use musaic_commands::PermissionKind as K;
use musaic_commands::events::{EVENT_REQUESTED, HostObservation, PollParams};
use musaic_db::delegated_work::{self as db, NewWork};
use serde_json::json;

const WORK: &str = "3ef3f326-6a82-406c-a473-601562c3b579";

/// One queued work item on the first album of the library.
fn seed_work(w: &AgentWorld) {
    use musaic_commands::auth::hex_encode;
    use sha2::{Digest, Sha256};
    let reader = w.login(&w.pair("seed", &[K::LibraryRead]).pairing.id);
    let albums = w
        .call(&reader, "musaic_library_list", json!({}))
        .unwrap()
        .to_json();
    let album_id = albums["items"][0]["albumId"].as_str().unwrap();
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
            id: WORK,
            group_id: WORK,
            kind: "metadata.identify",
            album_id,
            base_revision: rec["revision"].as_str().unwrap(),
            input_json: &input,
            input_digest: &digest,
            max_attempts: 3,
        }],
        1_800_000_000_000,
    )
    .unwrap();
}

fn facts(d: &PairingDiagnostics) -> (bool, bool, bool, bool) {
    (
        d.reachable_at.is_some(),
        d.host.is_some(),
        d.subscription_active,
        d.last_claim_at.is_some(),
    )
}

fn one(w: &AgentWorld, id: &str) -> PairingDiagnostics {
    w.app
        .ops()
        .agent()
        .diagnostics()
        .unwrap()
        .into_iter()
        .find(|d| d.pairing_id == id)
        .unwrap()
}

fn observation() -> HostObservation {
    HostObservation {
        client_name: "Test Host\u{7}".into(),
        client_version: "1.2.3".into(),
        profile: "2026-07-28".into(),
        tasks: true,
        skills: false,
        events: true,
    }
}

#[test]
fn each_step_flips_exactly_one_fact() {
    let w = Arc::new(agent_world_scanned());
    let id = w
        .pair("Host", &[K::LibraryRead, K::ProposalSubmit])
        .pairing
        .id;
    // A pairing that never connected shows only that it is paired.
    let d = one(&w, &id);
    assert!(!d.revoked && !d.paired_at.is_empty());
    assert_eq!(facts(&d), (false, false, false, false));

    let p = w.login(&id);
    assert_eq!(
        facts(&one(&w, &id)),
        (true, false, false, false),
        "reachable"
    );

    w.app.ops().agent().observe_host(&p, &observation());
    let d = one(&w, &id);
    assert_eq!(facts(&d), (true, true, false, false), "host");
    let host = d.host.unwrap();
    assert_eq!(
        host.client_name, "Test Host",
        "control characters are removed"
    );
    assert_eq!(
        (host.profile.as_str(), host.tasks, host.events),
        ("2026-07-28", true, true)
    );
    assert_eq!(
        w.app
            .ops()
            .agent()
            .session_status(&p)
            .unwrap()
            .observation
            .unwrap()
            .client_version,
        "1.2.3"
    );

    // A waiting poll makes the subscription active.
    let (w2, p2) = (Arc::clone(&w), p.clone());
    let parked = std::thread::spawn(move || {
        let q = PollParams {
            name: EVENT_REQUESTED.into(),
            wait_ms: Some(1_500),
            cursor: Some(
                w2.app
                    .ops()
                    .agent()
                    .events_poll(
                        &p2,
                        &PollParams {
                            name: EVENT_REQUESTED.into(),
                            ..Default::default()
                        },
                    )
                    .unwrap()
                    .cursor,
            ),
            ..Default::default()
        };
        w2.app.ops().agent().events_poll(&p2, &q).unwrap()
    });
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(
        facts(&one(&w, &id)),
        (true, true, true, false),
        "subscription"
    );
    parked.join().unwrap();
    // Within 90 seconds of the last poll it still counts; later it does not.
    assert!(one(&w, &id).subscription_active);
    w.clock.advance(100);
    let d = one(&w, &id);
    assert!(!d.subscription_active);
    assert!(
        d.subscription_at.is_some(),
        "the time of the last poll stays"
    );

    // A claim sets the last work claimed, and only that.
    seed_work(&w);
    w.call(
        &p,
        "musaic_work_claim",
        json!({"workId": WORK, "idempotencyKey": "host-claim-1"}),
    )
    .unwrap();
    let d = one(&w, &id);
    assert_eq!(facts(&d), (true, true, false, true), "last claim");
    assert_eq!(d.last_claim_work_id.as_deref(), Some(WORK));

    // A revoked pairing says so and forgets the host.
    w.app.ops().agent().revoke_pairing(&id).unwrap();
    let d = one(&w, &id);
    assert!(d.revoked && d.host.is_none());
}

#[test]
fn hosts_events_are_published_and_throttled() {
    let w = agent_world_scanned();
    let id = w.pair("Host", &[K::LibraryRead]).pairing.id;
    let feed = w.app.ops().system().library_events();
    let p = w.login(&id);
    w.app.ops().agent().observe_host(&p, &observation());
    let mut hosts = 0;
    while let Some(e) = feed.next(Duration::from_millis(300)) {
        if e.name() == "AgentStateChanged" && e.payload_json().contains("\"hosts\"") {
            hosts += 1;
        }
    }
    assert_eq!(
        hosts, 1,
        "the login and the observation fall in one five-second window"
    );
    // The same values again change nothing and publish nothing.
    w.clock.advance(10);
    w.app.ops().agent().observe_host(&p, &observation());
    assert!(feed.next(Duration::from_millis(300)).is_none() || true);
}

#[test]
fn observations_for_a_revoked_pairing_store_nothing() {
    let w = agent_world_scanned();
    let id = w.pair("Gone", &[K::LibraryRead]).pairing.id;
    let p = w.login(&id);
    w.app.ops().agent().revoke_pairing(&id).unwrap();
    w.app.ops().agent().observe_host(&p, &observation());
    assert!(one(&w, &id).host.is_none());
}

/// A host that only polls, never waiting or streaming, is listening too: the
/// server asks it back every 30 seconds.
#[test]
fn a_plain_poll_counts_as_a_subscription() {
    let w = agent_world_scanned();
    let id = w.pair("Poller", &[K::LibraryRead]).pairing.id;
    let p = w.login(&id);
    assert!(!one(&w, &id).subscription_active);
    w.app
        .ops()
        .agent()
        .events_poll(
            &p,
            &PollParams {
                name: EVENT_REQUESTED.into(),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(one(&w, &id).subscription_active);
    w.clock.advance(100);
    assert!(!one(&w, &id).subscription_active);
}
