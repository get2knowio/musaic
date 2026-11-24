//! SC-003: an assistant claims every item of a 1 000-item batch in at most 40
//! multi-item claims, and two assistants racing never hold one item twice.

mod common;

use std::collections::BTreeSet;
use std::sync::Barrier;

use musaic_app::ops::fixes::{Listening, QuickFixEvent};
use musaic_commands::PermissionKind as K;
use serde_json::json;

const N: usize = 1000;

#[test]
fn a_thousand_item_batch_is_claimed_once_each_in_forty_calls() {
    let w = common::agent_world_scanned();
    w.app
        .ops()
        .checks()
        .run("no-cover-art", "all", None)
        .unwrap();
    let lib = w.scratch.tmp.path().join("library");
    common::clone_finding_albums(&w.app, &lib, "no-cover-art", N - 1);
    let mut batch = None;
    w.app
        .ops()
        .fixes()
        .quick_fix_all("no-cover-art", true, &mut |e| {
            if let QuickFixEvent::Done(s) = &e {
                batch = s.research.clone();
            }
            Listening::Yes
        })
        .unwrap();
    let r = batch.expect("research counts");
    assert_eq!(r.sent_for_research, N);
    let batch = r.batch_id.unwrap();

    let research = &[K::LibraryRead, K::ProposalSubmit];
    let alpha = w.login(&w.pair("Alpha", research).pairing.id);
    let beta = w.login(&w.pair("Beta", research).pairing.id);
    let barrier = Barrier::new(2);
    let results: Vec<(Vec<String>, Vec<usize>)> = std::thread::scope(|scope| {
        let run = |who: &'static str, p| {
            let (w, barrier, batch) = (&w, &barrier, &batch);
            move || {
                barrier.wait();
                let (mut won, mut sizes) = (Vec::new(), Vec::new());
                for call in 0.. {
                    let out = w
                        .call(
                            p,
                            "musaic_work_claim_many",
                            json!({"batchId": batch, "max": 25,
                                   "idempotencyKey": format!("{who}-{call:04}")}),
                        )
                        .unwrap()
                        .to_json();
                    let claims = out["claims"].as_array().unwrap();
                    if claims.is_empty() {
                        break;
                    }
                    sizes.push(claims.len());
                    won.extend(
                        claims
                            .iter()
                            .map(|c| c["workId"].as_str().unwrap().to_string()),
                    );
                }
                (won, sizes)
            }
        };
        let a = scope.spawn(run("alpha", &alpha));
        let b = scope.spawn(run("beta", &beta));
        vec![a.join().unwrap(), b.join().unwrap()]
    });
    let calls: usize = results.iter().map(|(_, s)| s.len()).sum();
    assert!(calls <= N.div_ceil(25), "{calls} claiming calls");
    let all: Vec<String> = results.iter().flat_map(|(w, _)| w.clone()).collect();
    let unique: BTreeSet<&String> = all.iter().collect();
    assert_eq!(unique.len(), all.len(), "an item was claimed twice");
    assert_eq!(all.len(), N);
}
