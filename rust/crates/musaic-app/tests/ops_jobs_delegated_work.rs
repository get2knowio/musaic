//! `jobs.delegated_work`: the desktop's list of delegated work in its six
//! states (spec 005). Reads only; a Missing album's work is still listed.

mod common;

use common::{AgentWorld, agent_world_scanned};
use musaic_app::ops::jobs::DelegatedItem;
use musaic_commands::PermissionKind as K;
use musaic_commands::auth::hex_encode;
use musaic_db::delegated_work::{self as db, NewWork};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const RESEARCH: &[K] = &[K::LibraryRead, K::ProposalSubmit];

fn id(n: u8) -> String {
    format!("3ef3f326-6a82-406c-a473-6015620000{n:02}")
}

fn seed(w: &AgentWorld, n: u8) -> String {
    let reader = w.login(&w.pair(&format!("seed {n}"), &[K::LibraryRead]).pairing.id);
    let albums = w
        .call(&reader, "musaic_library_list", json!({}))
        .unwrap()
        .to_json();
    let album_id = albums["items"][n as usize]["albumId"].as_str().unwrap();
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
    let wid = id(n);
    let conn = musaic_db::open_connection(w.app.db_path()).unwrap();
    db::enqueue(
        &conn,
        &[NewWork {
            id: &wid,
            group_id: &wid,
            kind: "metadata.identify",
            album_id,
            base_revision: rec["revision"].as_str().unwrap(),
            input_json: &input,
            input_digest: &digest,
            max_attempts: 3,
        }],
        w.app
            .now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64,
    )
    .unwrap();
    album_id.to_string()
}

fn list(w: &AgentWorld) -> Vec<DelegatedItem> {
    w.app.ops().jobs().delegated_work().unwrap()
}

fn item(items: &[DelegatedItem], n: u8) -> &DelegatedItem {
    items.iter().find(|i| i.work_id == id(n)).unwrap()
}

fn lease(c: &Value, n: u8) -> Value {
    json!({"workId": id(n), "claimGeneration": c["claimGeneration"], "leaseToken": c["leaseToken"]})
}

#[test]
fn each_state_is_listed_with_its_details() {
    let w = agent_world_scanned();
    assert!(list(&w).is_empty());
    for n in 0..5 {
        seed(&w, n);
    }
    let alice = w.login(&w.pair("Alice", RESEARCH).pairing.id);
    // 0 stays queued; 1 is claimed; 2 is running; 3 awaits review; 4 failed.
    let claim = |n: u8, key: &str| {
        w.call(
            &alice,
            "musaic_work_claim",
            json!({"workId": id(n), "idempotencyKey": key}),
        )
        .unwrap()
        .to_json()
    };
    claim(1, "claim-key-1");
    let c2 = claim(2, "claim-key-2");
    let mut progress = lease(&c2, 2);
    progress["sequence"] = json!(1);
    progress["stage"] = json!("reading");
    w.call(&alice, "musaic_work_progress", progress).unwrap();
    let c3 = claim(3, "claim-key-3");
    let c4 = claim(4, "claim-key-4");
    let mut fail = lease(&c4, 4);
    fail["code"] = json!("gave_up");
    fail["message"] = json!("it did not work");
    fail["retryable"] = json!(false);
    w.call(&alice, "musaic_work_fail", fail).unwrap();
    let mut done = lease(&c3, 3);
    done["inputDigest"] = c3["inputDigest"].clone();
    done["idempotencyKey"] = json!("complete-key-3");
    done["result"] = json!({
        "schemaVersion": 1, "workId": id(3), "kind": "metadata.identify",
        "inputDigest": c3["inputDigest"], "outcome": "no_match", "confidence": 0.1,
        "uncertainties": [], "evidence": [],
        "recommendation": {"type": "unresolved", "explanation": "nothing", "question": "which?"}
    });
    w.call(&alice, "musaic_work_complete", done).unwrap();
    let items = list(&w);
    let states: Vec<(u8, &str)> = (0..5)
        .map(|n| (n, item(&items, n).state.as_str()))
        .collect();
    assert_eq!(
        states,
        [
            (0, "queued"),
            (1, "claimed"),
            (2, "running"),
            (3, "awaiting_review"),
            (4, "failed")
        ]
    );
    // Held work names its holder, heartbeat and lease; others do not.
    let held = item(&items, 1);
    assert_eq!(held.holder.as_deref(), Some("Alice"));
    assert!(held.last_heartbeat_at.is_some() && held.lease_expires_at.is_some());
    assert!(item(&items, 2).last_heartbeat_at.is_some());
    assert!(item(&items, 0).holder.is_none() && item(&items, 0).lease_expires_at.is_none());
    let failed = item(&items, 4);
    assert_eq!(
        failed.last_error.as_deref(),
        Some("gave_up: it did not work")
    );
    assert_eq!((failed.attempts, failed.max_attempts), (1, 3));
    assert!(item(&items, 0).album_label.is_some());
    // Order: held work, then queued, review, failed.
    let order: Vec<&str> = items.iter().map(|i| i.state.as_str()).collect();
    assert_eq!(
        order,
        ["running", "claimed", "queued", "awaiting_review", "failed"]
    );
}

#[test]
fn waiting_for_agent_means_queued_and_nobody_listening() {
    let w = agent_world_scanned();
    seed(&w, 0);
    assert!(item(&list(&w), 0).waiting_for_agent);
    // A pairing that polled with a wait is a listener for the next 90 seconds.
    let p = w.login(&w.pair("Listener", RESEARCH).pairing.id);
    let cursor = w
        .app
        .ops()
        .agent()
        .events_poll(&p, &poll("c"))
        .unwrap()
        .cursor;
    let mut q = poll("c");
    q.cursor = Some(cursor);
    q.wait_ms = Some(300);
    w.app.ops().agent().events_poll(&p, &q).unwrap();
    assert!(!item(&list(&w), 0).waiting_for_agent);
    w.clock.advance(100);
    assert!(item(&list(&w), 0).waiting_for_agent);
}

fn poll(_: &str) -> musaic_commands::events::PollParams {
    musaic_commands::events::PollParams {
        name: musaic_commands::events::EVENT_REQUESTED.into(),
        ..Default::default()
    }
}

#[test]
fn a_missing_album_is_still_listed_and_nothing_touches_files() {
    let w = agent_world_scanned();
    let album = seed(&w, 0);
    let lib = std::path::PathBuf::from(w.app.library_dir().unwrap());
    let snapshot = |dir: &std::path::Path| -> Vec<(String, u64)> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(d).unwrap().flatten() {
                let m = e.metadata().unwrap();
                if m.is_dir() {
                    stack.push(e.path());
                } else {
                    out.push((e.path().display().to_string(), m.len()));
                }
            }
        }
        out.sort();
        out
    };
    let conn = musaic_db::open_connection(w.app.db_path()).unwrap();
    musaic_db::albums::mark_album_missing(&conn, &album, "2026-11-23T00:00:00Z").unwrap();
    let before = snapshot(&lib);
    let items = list(&w);
    assert_eq!(item(&items, 0).state, "queued");
    assert_eq!(item(&items, 0).album_id, album);
    assert_eq!(snapshot(&lib), before, "listing touched no file");
}

#[test]
fn retry_requeues_a_failed_item_exactly_once() {
    let w = agent_world_scanned();
    seed(&w, 0);
    let alice = w.login(&w.pair("Alice", RESEARCH).pairing.id);
    let c = w
        .call(
            &alice,
            "musaic_work_claim",
            json!({"workId": id(0), "idempotencyKey": "claim-key-9"}),
        )
        .unwrap()
        .to_json();
    let mut fail = lease(&c, 0);
    fail["code"] = json!("gave_up");
    fail["message"] = json!("no");
    fail["retryable"] = json!(false);
    w.call(&alice, "musaic_work_fail", fail).unwrap();
    let failed = item(&list(&w), 0).clone();
    assert_eq!(failed.state, "failed");
    w.app
        .ops()
        .jobs()
        .research_decide(&failed.work_id, failed.revision, None, "retry", None)
        .unwrap();
    let after = list(&w);
    assert_eq!(after.iter().filter(|i| i.work_id == id(0)).count(), 1);
    assert_eq!(item(&after, 0).state, "queued");
    assert_eq!(item(&after, 0).attempts, 0);
    // A second retry with the old revision is refused: no second requeue.
    assert!(
        w.app
            .ops()
            .jobs()
            .research_decide(&failed.work_id, failed.revision, None, "retry", None)
            .is_err()
    );
    assert_eq!(list(&w).len(), 1);
}
