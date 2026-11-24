mod common;

use common::agent_world_scanned;
use musaic_commands::PermissionKind;
use serde_json::json;

#[test]
fn an_assistant_submits_once_and_only_the_desktop_changes_files() {
    exercise_review(false);
}

#[test]
fn recovery_recognizes_a_finished_apply_when_the_review_decision_was_interrupted() {
    exercise_review(true);
}

fn exercise_review(interrupt_decision: bool) {
    let w = agent_world_scanned();
    let ops = w.app.ops();
    let albums = ops.library().list_albums(false).unwrap().albums;
    let album = albums
        .iter()
        .find(|album| album.path().ends_with("[FLAC]"))
        .unwrap_or(&albums[0]);
    let files = musaic_services::agent_read::file_facts(album.path());
    let path = std::path::Path::new(album.path()).join(&files[0].0);
    let original = std::fs::read(&path).unwrap();
    let tags = || {
        let mut tags = musaic_scan::flacmeta::read(&path).unwrap().tags.unwrap();
        tags.sort();
        tags
    };
    let original_tags = tags();
    let jobs = ops.jobs();
    let job = jobs
        .enqueue(album.id(), "metadata", "Check the artist credit", false)
        .unwrap();
    let items = jobs.research_items(&job.id).unwrap();
    let work = &items[0];
    let p = w.login(
        &w.pair(
            "researcher",
            &[PermissionKind::LibraryRead, PermissionKind::ProposalSubmit],
        )
        .pairing
        .id,
    );
    let call = |name: &str, body| w.call(&p, name, body).unwrap().to_json();
    let ctx = call("musaic_work_get", json!({"workId":work.id}));
    let claim = call(
        "musaic_work_claim",
        json!({"workId":work.id,"idempotencyKey":"claim-test"}),
    );
    let completion = json!({
        "workId":work.id,"claimGeneration":claim["claimGeneration"],"leaseToken":claim["leaseToken"],
        "idempotencyKey":"completion-test","inputDigest":claim["inputDigest"],
        "result":{"schemaVersion":1,"workId":work.id,"kind":work.kind,"inputDigest":claim["inputDigest"],
        "outcome":"proposal","evidence":[{"source":"urn:musaic:fixture:liner-notes","claim":"The sleeve credits Fixture Artist.","retrievedAt":"2026-11-22T12:00:00Z","limitations":[]}],
        "confidence":0.8,"uncertainties":[],"recommendation":{"type":"metadata","identifiers":[],"albumFields":{"album_artist":"Fixture Artist"},"tracks":[]}}
    });
    assert!(ctx.is_object());
    let first = call("musaic_work_complete", completion.clone());
    assert_eq!(first, call("musaic_work_complete", completion));
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let proposed = jobs.research_items(&job.id).unwrap().remove(0);
    assert_eq!(proposed.state, "awaiting_review");
    let digest = proposed.digest.as_deref().unwrap();
    let preview = jobs.research_preview(&work.id, digest).unwrap();
    assert!(
        preview
            .diff
            .tags
            .iter()
            .any(|c| c.after.as_deref() == Some("Fixture Artist"))
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(
        jobs.research_decide(
            &work.id,
            preview.revision,
            Some(digest),
            "apply",
            Some("stale")
        )
        .is_err()
    );
    let conn = musaic_db::open_connection(w.app.db_path()).unwrap();
    if interrupt_decision {
        conn.execute_batch(
            "CREATE TRIGGER stop_review BEFORE UPDATE ON delegated_work
            WHEN NEW.state='applied' BEGIN SELECT RAISE(ABORT,'interrupted'); END;",
        )
        .unwrap();
    }
    let decision = jobs.research_decide(
        &work.id,
        preview.revision,
        Some(digest),
        "apply",
        Some(&preview.digest),
    );
    if interrupt_decision {
        assert!(decision.is_err());
        assert!(
            musaic_db::delegated_work::get(&conn, &work.id)
                .unwrap()
                .unwrap()
                .review_reserved
        );
        let partial = jobs
            .research_items(&job.id)
            .unwrap()
            .remove(0)
            .apply_outcome
            .unwrap();
        assert!(
            partial
                .files
                .iter()
                .any(|f| f.status == musaic_commands::tools::operation::FileStatus::Ok)
        );
        conn.execute_batch("DROP TRIGGER stop_review").unwrap();
        musaic_db::delegated_work::recover_reserved(&conn, 1_800_000_000_000).unwrap();
    } else {
        decision.unwrap();
    }
    assert_ne!(std::fs::read(&path).unwrap(), original);
    assert_eq!(jobs.research_items(&job.id).unwrap()[0].state, "applied");
    assert!(
        jobs.research_decide(
            &work.id,
            preview.revision,
            Some(digest),
            "apply",
            Some(&preview.digest)
        )
        .is_err()
    );
    let records =
        musaic_db::agent::list_changes(&musaic_db::open_connection(w.app.db_path()).unwrap())
            .unwrap();
    assert_eq!(records.len(), 1);
    assert!(
        w.app
            .config_dir()
            .join("agent-changes")
            .join(&records[0].id)
            .join("before.json")
            .is_file()
    );
    let outcome = ops.agent().undo_change(&records[0].id).unwrap();
    assert!(outcome.complete, "{:?}", outcome.message);
    assert_eq!(tags(), original_tags);
}

#[test]
fn the_desktop_can_cancel_a_claim_and_explicitly_retry_current_work() {
    let world = agent_world_scanned();
    let ops = world.app.ops();
    let album = ops.library().list_albums(false).unwrap().albums.remove(0);
    let jobs = ops.jobs();
    let group = jobs
        .enqueue(album.id(), "metadata", "Check this release", false)
        .unwrap();
    let item = jobs.research_items(&group.id).unwrap().remove(0);
    let principal = world.login(
        &world
            .pair(
                "researcher",
                &[PermissionKind::LibraryRead, PermissionKind::ProposalSubmit],
            )
            .pairing
            .id,
    );
    let claim = world
        .call(
            &principal,
            "musaic_work_claim",
            json!({"workId":item.id,"idempotencyKey":"cancel-claim"}),
        )
        .unwrap()
        .to_json();
    assert!(
        jobs.research_decide(&item.id, item.revision, None, "cancel", None)
            .is_err()
    );
    let claimed = jobs.research_items(&group.id).unwrap().remove(0);
    jobs.research_decide(&item.id, claimed.revision, None, "cancel", None)
        .unwrap();
    let cancelled = jobs.research_items(&group.id).unwrap().remove(0);
    assert_eq!(cancelled.state, "cancelled");
    assert!(world.call(&principal,"musaic_work_renew",json!({"workId":item.id,"claimGeneration":claim["claimGeneration"],"leaseToken":claim["leaseToken"]})).is_err());
    jobs.research_decide(&item.id, cancelled.revision, None, "retry", None)
        .unwrap();
    assert_eq!(jobs.research_items(&group.id).unwrap()[0].state, "queued");
}

/// The album panel's "Reviewed" stamp, as stored.
fn reviewed_at(w: &common::AgentWorld, album_id: &str) -> Option<String> {
    musaic_db::open_connection(w.app.db_path())
        .unwrap()
        .query_row(
            "SELECT metadata_ai_acted_on_at FROM albums WHERE id = ?1",
            [album_id],
            |r| r.get(0),
        )
        .unwrap()
}

/// A metadata research request for the album, claimed and answered with a
/// proposal: the item awaiting review.
fn metadata_proposal(
    w: &common::AgentWorld,
    p: &musaic_app::ops::agent::Principal,
    album_id: &str,
    key: &str,
) -> musaic_app::ops::jobs::ResearchItem {
    let ops = w.app.ops();
    let jobs = ops.jobs();
    let job = jobs.enqueue(album_id, "metadata", "", false).unwrap();
    let work = jobs.research_items(&job.id).unwrap().remove(0);
    let call = |name: &str, body| w.call(p, name, body).unwrap().to_json();
    let claim = call(
        "musaic_work_claim",
        json!({"workId":work.id,"idempotencyKey":format!("claim-{key}")}),
    );
    call(
        "musaic_work_complete",
        json!({
            "workId":work.id,"claimGeneration":claim["claimGeneration"],"leaseToken":claim["leaseToken"],
            "idempotencyKey":format!("complete-{key}"),"inputDigest":claim["inputDigest"],
            "result":{"schemaVersion":1,"workId":work.id,"kind":work.kind,"inputDigest":claim["inputDigest"],
            "outcome":"proposal","evidence":[{"source":"urn:musaic:fixture:liner-notes","claim":"The sleeve credits Fixture Artist.","retrievedAt":"2026-11-22T12:00:00Z","limitations":[]}],
            "confidence":0.8,"uncertainties":[],"recommendation":{"type":"metadata","identifiers":[],"albumFields":{"album_artist":"Fixture Artist"},"tracks":[]}}
        }),
    );
    let item = jobs.research_items(&job.id).unwrap().remove(0);
    assert_eq!(item.state, "awaiting_review");
    item
}

// Applying or discarding a metadata proposal stamps the album's "Reviewed"
// row and publishes `AlbumTagsChanged` of that field, which refreshes the
// album caches.
#[test]
fn applying_or_discarding_a_metadata_proposal_stamps_the_reviewed_row() {
    use musaic_events::{Event, Subscriber, Topic};
    use std::sync::{Arc, Mutex};

    let w = agent_world_scanned();
    let seen: Arc<Mutex<Vec<Event>>> = Arc::default();
    let s = seen.clone();
    w.app.bus().register(
        Topic::Event,
        Subscriber::from_fn(move |e| {
            s.lock().unwrap().push(e.clone());
            Ok(())
        })
        .key("reviewed_test")
        .priority(500)
        .name("reviewed_test"),
    );
    let stamped = |album: &str| {
        seen.lock().unwrap().iter().any(|e| {
            matches!(e, Event::AlbumTagsChanged { album_id, fields }
                if album_id == album && fields == &["metadata_ai_acted_on_at".to_string()])
        })
    };
    let ops = w.app.ops();
    let albums = ops.library().list_albums(false).unwrap().albums;
    let (discarded, applied) = (albums[0].id().to_string(), albums[1].id().to_string());
    let p = w.login(
        &w.pair(
            "researcher",
            &[PermissionKind::LibraryRead, PermissionKind::ProposalSubmit],
        )
        .pairing
        .id,
    );
    let jobs = ops.jobs();

    let item = metadata_proposal(&w, &p, &discarded, "discard");
    assert_eq!(reviewed_at(&w, &discarded), None);
    jobs.research_decide(
        &item.id,
        item.revision,
        item.digest.as_deref(),
        "discard",
        None,
    )
    .unwrap();
    assert!(reviewed_at(&w, &discarded).is_some());
    assert!(stamped(&discarded));

    let item = metadata_proposal(&w, &p, &applied, "apply");
    let digest = item.digest.as_deref().unwrap();
    let preview = jobs.research_preview(&item.id, digest).unwrap();
    assert_eq!(reviewed_at(&w, &applied), None);
    assert!(!stamped(&applied));
    jobs.research_decide(
        &item.id,
        preview.revision,
        Some(digest),
        "apply",
        Some(&preview.digest),
    )
    .unwrap();
    assert!(reviewed_at(&w, &applied).is_some());
    assert!(stamped(&applied));
}
