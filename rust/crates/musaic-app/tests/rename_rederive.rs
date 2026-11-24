//! A queued research request follows its album through a folder rename: the
//! rename moves it to the new album id in the album's transaction, and the
//! `AlbumRenamed` subscriber derives its context again, so an agent can claim
//! it instead of being refused as stale. A held request is left to the claim
//! and completion rules.

mod common;

use std::time::Duration;

use common::agent_world_scanned;
use musaic_app::ops::checks::{FindingsQuery, PlanPreview};
use musaic_app::ops::fixes::PlanEntry;
use musaic_commands::PermissionKind as K;
use musaic_db::delegated_work::{self as db, WorkRow};
use serde_json::json;

const RESEARCH: &[K] = &[K::LibraryRead, K::ProposalSubmit];

fn row(app: &musaic_app::App, id: &str) -> WorkRow {
    let conn = musaic_db::open_connection(app.db_path()).unwrap();
    db::get(&conn, id).unwrap().expect("the request")
}

fn audit(app: &musaic_app::App, id: &str, transition: &str) -> i64 {
    let conn = musaic_db::open_connection(app.db_path()).unwrap();
    conn.query_row(
        "SELECT count(*) FROM delegation_audit WHERE work_id=?1 AND transition=?2",
        [id, transition],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn a_queued_request_is_rederived_after_a_rename_and_a_held_one_is_left() {
    let w = agent_world_scanned();
    let app = &w.app;
    app.ops()
        .checks()
        .run("filename-mismatch", "all", None)
        .unwrap();
    let finding = app
        .ops()
        .checks()
        .list_findings(&FindingsQuery {
            check_id: Some("filename-mismatch".into()),
            ..FindingsQuery::default()
        })
        .unwrap()
        .into_iter()
        .find(|f| f.summary().contains("Legacy Folder Name"))
        .expect("the legacy folder finding");
    let old_album = finding.album_id().expect("an album finding").to_string();

    // A cover request an agent holds, and a metadata request still queued.
    let cover = app
        .ops()
        .jobs()
        .enqueue(&old_album, "cover", "", true)
        .unwrap();
    let job = app
        .ops()
        .jobs()
        .enqueue(&old_album, "metadata", "the sleeve says 1999", true)
        .unwrap();
    let conn = musaic_db::open_connection(app.db_path()).unwrap();
    let ids = |group: &str| -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT id FROM delegated_work WHERE group_id=?1")
            .unwrap();
        stmt.query_map([group], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let held = ids(&cover.id).remove(0);
    let queued = ids(&job.id).remove(0);
    let p = w.login(&w.pair("Researcher", RESEARCH).pairing.id);
    w.call(
        &p,
        "musaic_work_claim",
        json!({"workId": held, "idempotencyKey": "hold-the-cover"}),
    )
    .unwrap();
    let before_held = row(app, &held);
    let before = row(app, &queued);
    assert_eq!(
        (before.state.as_str(), before_held.state.as_str()),
        ("queued", "claimed")
    );

    let PlanPreview::Derived { plan, .. } = app
        .ops()
        .checks()
        .filename_plan_preview(finding.id())
        .unwrap()
    else {
        panic!("not derived")
    };
    let dir = plan.dir_rename.expect("a folder rename");
    let feed = app.ops().system().library_events();
    let out = app
        .ops()
        .fixes()
        .apply_filename_rename(
            finding.id(),
            &[],
            Some(&PlanEntry {
                old: dir.old,
                new: dir.new,
            }),
        )
        .unwrap();
    assert!(out.ok, "{out:?}");
    let mut seen = Vec::new();
    while let Some(e) = feed.next(Duration::from_millis(300)) {
        seen.push(e.name().to_string());
    }
    assert_eq!(
        seen.iter().filter(|n| *n == "AlbumRenamed").count(),
        1,
        "{seen:?}"
    );

    let after = row(app, &queued);
    assert_ne!(after.album_id, old_album, "the request followed its album");
    assert_eq!(after.state, "queued");
    assert_ne!(after.base_revision, before.base_revision);
    assert_eq!(after.revision, before.revision + 1);
    assert_eq!(audit(app, &queued, "rederived"), 1, "derived once");
    // The hint is the request's own.
    let ctx: serde_json::Value = serde_json::from_str(&after.input_json).unwrap();
    assert_eq!(ctx["hint"], "the sleeve says 1999");
    assert_eq!(ctx["albumId"], after.album_id.as_str());

    // The held request moved with the album, its context untouched.
    let held_after = row(app, &held);
    assert_eq!(held_after.album_id, after.album_id);
    assert_eq!(
        (
            held_after.state.as_str(),
            held_after.base_revision.as_str(),
            held_after.revision
        ),
        (
            "claimed",
            before_held.base_revision.as_str(),
            before_held.revision
        )
    );
    assert_eq!(audit(app, &held, "rederived"), 0);

    // Claimable: not refused as stale.
    let claim = w
        .call(
            &p,
            "musaic_work_claim",
            json!({"workId": queued, "idempotencyKey": "after-the-rename"}),
        )
        .unwrap()
        .to_json();
    assert_eq!(claim["workId"], queued.as_str());
}
