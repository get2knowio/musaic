//! Offline grouping and deterministic preflight through the desktop enqueue operation.

mod common;

use std::sync::Arc;

use musaic_commands::PermissionKind;
use musaic_net::{Method, Request, Response, Transport, TransportError};
use musaic_services::production::Network;
use serde_json::json;

#[test]
fn a_combined_request_reuses_two_existing_work_items() {
    let w = common::agent_world_scanned();
    let ops = w.app.ops();
    let album_id = ops.library().list_albums(false).unwrap().albums[0]
        .id()
        .to_owned();
    let jobs = ops.jobs();
    let metadata = jobs.enqueue(&album_id, "metadata", "", false).unwrap();
    let cover = jobs.enqueue(&album_id, "cover", "", false).unwrap();
    let metadata_work = jobs.research_items(&metadata.id).unwrap().remove(0);
    let cover_work = jobs.research_items(&cover.id).unwrap().remove(0);
    let combined = jobs
        .enqueue(&album_id, "metadata+cover", "", false)
        .unwrap();
    assert_eq!(combined.job_type, "metadata+cover");
    let children = jobs.research_items(&combined.id).unwrap();
    assert_eq!(children.len(), 2);
    assert!(children.iter().any(|row| row.id == metadata_work.id));
    assert!(children.iter().any(|row| row.id == cover_work.id));
    assert_eq!(jobs.research_items(&cover.id).unwrap().len(), 2);
    assert_eq!(jobs.research_items(&metadata.id).unwrap().len(), 2);
}

const RELEASE_GROUP: &str = "19026171-5bd4-4b5f-950e-64501c21d169";

// One red pixel, produced from a synthetic byte buffer.
const PROPOSED_COVER: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0x00,
    0x00, 0x03, 0x01, 0x01, 0x00, 0xc9, 0xfe, 0x92, 0xef, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
    0x44, 0xae, 0x42, 0x60, 0x82,
];

struct FoundCover;

impl Transport for FoundCover {
    fn send(&self, request: &Request) -> Result<Response, TransportError> {
        if request.method == Method::Get
            && request.host() == "coverartarchive.org"
            && request.path.ends_with("/front")
        {
            return Ok(Response {
                status: 200,
                headers: vec![("content-type".into(), "image/png".into())],
                body: PROPOSED_COVER.to_vec(),
            });
        }
        let (status, body) = match (request.method, request.host(), request.path.as_str()) {
            (Method::Head, "coverartarchive.org", path) if path.ends_with("/front") => {
                (200, Vec::new())
            }
            (Method::Get, "musicbrainz.org", "/ws/2/release") => {
                // The cover is known from the anchor, but there is no tracklist
                // to finish deterministic metadata resolution.
                (200, br#"{"releases":[]}"#.to_vec())
            }
            (Method::Get, "musicbrainz.org", path) if path.ends_with(RELEASE_GROUP) => (
                200,
                json!({"id":RELEASE_GROUP,"title":"Fixture pressing"})
                    .to_string()
                    .into_bytes(),
            ),
            _ => (404, Vec::new()),
        };
        Ok(Response {
            status,
            headers: Vec::new(),
            body,
        })
    }
}

fn anchored_album() -> (common::AgentWorld, String) {
    let clock = common::FakeClock::new();
    let store = Arc::new(musaic_secrets::MemoryStore::new());
    let (app, scratch) = common::app_quiet_pool(|b| {
        b.network(Network::Live(Arc::new(FoundCover)))
            .clock(clock.wall())
            .agent_secrets(store.clone())
    });
    let w = common::AgentWorld {
        app,
        scratch,
        clock,
        store,
    };
    common::configure(&w.app, &w.scratch);
    w.app.ops().scan().subscribe(false).unwrap().count();
    common::wait_for_fingerprints(&w.app);
    let ops = w.app.ops();
    let album_id = ops.library().list_albums(false).unwrap().albums[0]
        .id()
        .to_owned();
    let conn = musaic_db::open_connection(w.app.db_path()).unwrap();
    assert!(
        musaic_db::mb_anchor::set_album_mb_anchor(
            &conn,
            &album_id,
            RELEASE_GROUP,
            None,
            100,
            &w.app.now_iso()
        )
        .unwrap()
    );

    (w, album_id)
}

#[test]
fn a_locally_resolved_cover_is_kept_with_unresolved_metadata() {
    let (w, album_id) = anchored_album();
    let ops = w.app.ops();
    let jobs = ops.jobs();
    let grouped = jobs
        .enqueue(&album_id, "metadata+cover", "", false)
        .unwrap();
    let children = jobs.research_items(&grouped.id).unwrap();
    assert_eq!(children.len(), 2);
    let cover = children
        .iter()
        .find(|work| work.kind == "cover.research")
        .unwrap();
    assert_eq!(cover.state, "awaiting_review");
    assert_eq!(cover.validation.as_deref(), Some("pending"));
    assert!(cover.result.is_some());
    let metadata = children
        .iter()
        .find(|work| work.kind == "metadata.correct")
        .unwrap();
    assert_eq!(metadata.state, "queued");
    let p = w.login(
        &w.pair("Fixture assistant", &[PermissionKind::LibraryRead])
            .pairing
            .id,
    );
    let items = w.call(&p, "musaic_work_list", json!({})).unwrap().to_json();
    assert_eq!(items["items"].as_array().unwrap().len(), 1);
    assert_eq!(items["items"][0]["workId"], metadata.id);
}

#[test]
fn a_local_only_cover_uses_desktop_review_backup_and_undo_without_a_pairing() {
    let (w, album_id) = anchored_album();
    let ops = w.app.ops();
    assert!(ops.agent().list_pairings().unwrap().is_empty());
    ops.checks().run("filename-mismatch", "all", None).unwrap();
    ops.library().resolution(&album_id, false, "").unwrap();

    let row = ops.library().album(&album_id).unwrap();
    let before = musaic_services::agent_read::file_facts(row.path());
    let path = std::path::Path::new(row.path()).join(&before[0].0);
    let graphics = || {
        musaic_tags::flac::FlacFile::open(&path)
            .unwrap()
            .pictures()
            .into_iter()
            .map(|picture| picture.data.clone())
            .collect::<Vec<_>>()
    };
    let original = graphics();
    let jobs = ops.jobs();
    let job = jobs.enqueue(&album_id, "cover", "", false).unwrap();
    let item = jobs.research_items(&job.id).unwrap().remove(0);
    assert_eq!(item.state, "awaiting_review");
    let digest = item.digest.as_deref().unwrap();
    let preview = jobs.research_preview(&item.id, digest).unwrap();
    assert!(preview.diff.cover.is_some());
    jobs.research_decide(
        &item.id,
        preview.revision,
        Some(digest),
        "apply",
        Some(&preview.digest),
    )
    .unwrap();
    let changed = graphics();
    assert_ne!(changed, original);
    let item = jobs.research_items(&job.id).unwrap().remove(0);
    let id = item.change_id.as_deref().expect("saved undo");
    let agent = ops.agent();
    assert!(agent.list_pairings().unwrap().is_empty());
    let records = agent.list_changes(None).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, id);
    assert_eq!(records[0].pairing_id, None);
    assert_eq!(records[0].pairing_name, "Musaic");
    assert!(
        w.app
            .config_dir()
            .join("agent-changes")
            .join(id)
            .join("before.json")
            .is_file()
    );
    assert!(agent.undo_change(id).unwrap().complete);
    assert_eq!(graphics(), original);
    assert!(jobs.research_items(&job.id).unwrap()[0].change_id.is_none());
}

#[test]
fn discarding_a_pending_cover_records_the_disposition_without_writing() {
    let (w, album_id) = anchored_album();
    let ops = w.app.ops();
    let row = ops.library().album(&album_id).unwrap();
    let folder = std::path::Path::new(row.path());
    let before = musaic_services::agent_read::file_facts(row.path());
    let jobs = ops.jobs();
    let job = jobs.enqueue(&album_id, "cover", "", false).unwrap();
    let item = jobs.research_items(&job.id).unwrap().remove(0);
    assert_eq!(item.validation.as_deref(), Some("pending"));
    assert!(
        jobs.research_decide(
            &item.id,
            item.revision,
            Some("stale-digest"),
            "discard",
            None
        )
        .is_err()
    );
    jobs.research_decide(
        &item.id,
        item.revision,
        item.digest.as_deref(),
        "discard",
        None,
    )
    .unwrap();
    assert_eq!(jobs.research_items(&job.id).unwrap()[0].state, "discarded");
    assert_eq!(musaic_services::agent_read::file_facts(row.path()), before);
    assert!(ops.agent().list_changes(None).unwrap().is_empty());
    assert!(folder.is_dir());
}
