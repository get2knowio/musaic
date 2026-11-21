//! SC-009 and FR-054: the before-image of an agent-caused apply can be
//! replayed through the apply pipeline and the rename service to restore the
//! prior state, and an apply whose before-image cannot be recorded writes
//! nothing.

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use common::{AgentWorld, agent_world_scanned};
use musaic_app::ops::Listening;
use musaic_app::ops::apply::ApplyChanges;
use musaic_commands::{Decision, PermissionKind as K};
use musaic_fsops::rename::PlanEntry;
use musaic_scan::flacmeta;
use musaic_services::agent_changes::{ALBUM_PREFIX, BeforeImage, Prior, TRACK_PREFIX};
use musaic_services::library_ops::{RenameParts, rename_album_with_plan};
use musaic_tags::apply::{FIELD_TO_VORBIS, TRACK_FIELD_TO_VORBIS};
use serde_json::json;

const ALL: &[K] = &[
    K::LibraryRead,
    K::ProposalSubmit,
    K::TagsWrite,
    K::CoversWrite,
    K::FilesRename,
];

struct Case {
    w: AgentWorld,
    p: musaic_app::ops::agent::Principal,
    album: String,
    path: String,
}

fn case() -> Case {
    let w = agent_world_scanned();
    let id = w.pair("a", ALL).pairing.id;
    for k in [K::TagsWrite, K::CoversWrite, K::FilesRename] {
        w.app
            .ops()
            .agent()
            .set_policy(&id, &[(k, Decision::Allow)])
            .unwrap();
    }
    let p = w.login(&id);
    let ops = w.app.ops();
    let a = ops.library().list_albums(false).unwrap().albums.remove(0);
    let (album, path) = (a.id().to_string(), a.path().to_string());
    drop(ops);
    Case { w, p, album, path }
}

/// Every album and track field the vocabularies can touch, per file, as the
/// files hold them: the state a replay must restore.
fn tag_state(dir: &Path) -> BTreeMap<String, BTreeMap<String, Vec<String>>> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        if e.path().extension().is_none_or(|x| x != "flac") {
            continue;
        }
        let meta = flacmeta::read(&e.path()).unwrap();
        let mut fields = BTreeMap::new();
        for (f, v) in FIELD_TO_VORBIS {
            fields.insert(
                format!("{ALBUM_PREFIX}{f}"),
                meta.values(v).map(str::to_string).collect(),
            );
        }
        for (f, v) in TRACK_FIELD_TO_VORBIS {
            fields.insert(
                format!("{TRACK_PREFIX}{f}"),
                meta.values(v).map(str::to_string).collect(),
            );
        }
        out.insert(e.file_name().to_string_lossy().into_owned(), fields);
    }
    out
}

fn before_image(c: &Case) -> (BeforeImage, String) {
    let conn = musaic_db::open_connection(c.w.app.db_path()).unwrap();
    let rows = musaic_db::agent::list_changes(&conn).unwrap();
    assert_eq!(rows.len(), 1);
    (
        serde_json::from_str(&rows[0].before).unwrap(),
        rows[0].id.clone(),
    )
}

/// Put the prior values back through the apply pipeline (what an undo does).
fn replay_tags(c: &Case, album_id: &str, before: &BeforeImage) {
    let mut changes = ApplyChanges::default();
    let mut album_seen: BTreeMap<String, Prior> = BTreeMap::new();
    for (file, fields) in &before.files {
        let mut sets = Vec::new();
        let mut clears = Vec::new();
        for (key, prior) in fields {
            if let Some(f) = key.strip_prefix(ALBUM_PREFIX) {
                album_seen.insert(f.to_string(), prior.clone());
            } else if let Some(f) = key.strip_prefix(TRACK_PREFIX) {
                match prior.values().first() {
                    Some(v) => sets.push((f.to_string(), v.clone())),
                    None => clears.push(f.to_string()),
                }
            }
        }
        if !sets.is_empty() {
            changes.track_suggestions.push((file.clone(), sets));
        }
        if !clears.is_empty() {
            changes.track_clear.push((file.clone(), clears));
        }
    }
    for (f, prior) in album_seen {
        match prior.values().first() {
            Some(v) => changes.suggestions.push((f, v.clone())),
            None => changes.clear.push(f),
        }
    }
    c.w.app
        .ops()
        .apply()
        .apply_changes(album_id, &changes, &mut |_| Listening::Yes)
        .unwrap();
}

fn apply(c: &Case, patch: serde_json::Value) -> serde_json::Value {
    let rev =
        c.w.call(&c.p, "musaic_album_get", json!({"albumId": c.album}))
            .unwrap()
            .to_json()["revision"]
            .clone();
    let prev =
        c.w.call(
            &c.p,
            "musaic_fix_preview",
            json!({"albumId": c.album, "baseRevision": rev, "proposedPatch": patch}),
        )
        .unwrap()
        .to_json();
    let h = c
        .w
        .call(&c.p, "musaic_fix_apply", json!({"proposalId": prev["proposalId"], "digest": prev["digest"], "idempotencyKey": "key-roundtrip-1"}))
        .unwrap()
        .to_json();
    assert_eq!(h["state"], "succeeded", "{h}");
    h
}

#[test]
fn a_tag_apply_is_undone_by_replaying_its_before_image() {
    let c = case();
    let before = tag_state(Path::new(&c.path));
    let first_file = before.keys().next().unwrap().clone();
    apply(
        &c,
        json!({"tags": {"album": {"genre": "Replayed Genre", "year": "1987", "album_artist": null},
                        "tracks": {first_file.clone(): {"title": "A Different Title", "artist": "Someone Else"}}}}),
    );
    let during = tag_state(Path::new(&c.path));
    assert_ne!(during, before, "the apply changed tags");
    let (image, id) = before_image(&c);
    assert!(image.files[&first_file].contains_key("track.title"));
    assert!(image.files[&first_file].contains_key("album.genre"));
    assert!(
        image
            .files
            .values()
            .all(|f| !f.keys().any(|k| k.contains('/'))),
        "bare names only"
    );
    replay_tags(&c, &c.album, &image);
    assert_eq!(
        tag_state(Path::new(&c.path)),
        before,
        "every touched field is back to its prior value"
    );
    // The record is complete and survives: a row and a before.json beside it.
    let dir = c.w.scratch.config.join("agent-changes").join(&id);
    assert!(dir.join("before.json").is_file());
    let on_disc: BeforeImage =
        serde_json::from_slice(&std::fs::read(dir.join("before.json")).unwrap()).unwrap();
    assert_eq!(on_disc, image);
}

#[test]
fn a_rename_apply_is_undone_by_reversing_the_recorded_names() {
    let c = case();
    let before_files: Vec<String> = std::fs::read_dir(&c.path)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    let folder_before = Path::new(&c.path)
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let tags_before = tag_state(Path::new(&c.path));
    let h = apply(
        &c,
        json!({"tags": {"album": {"album_artist": "Roundtrip Artist"}}, "rename": true}),
    );
    let after_id = h["result"]["albumIdAfter"].as_str().unwrap().to_string();
    let (image, _) = before_image(&c);
    let folder = image
        .renames
        .folder
        .clone()
        .expect("the folder rename was recorded");
    assert_eq!(folder.from, folder_before);
    assert!(folder.to.contains("Roundtrip Artist"));

    // Reverse: names first (the plan is keyed by the names the files have now),
    // then the tags, then the folder.
    let ops = c.w.app.ops();
    let moved = ops.library().album(&after_id).unwrap().path().to_string();
    assert_ne!(moved, c.path);
    replay_tags(&c, &after_id, &image);
    let mut conn = musaic_db::open_connection(c.w.app.db_path()).unwrap();
    let files: Vec<PlanEntry> = image
        .renames
        .files
        .iter()
        .map(|n| PlanEntry {
            old: n.to.clone(),
            new: n.from.clone(),
        })
        .collect();
    let dir = PlanEntry {
        old: folder.to.clone(),
        new: folder.from.clone(),
    };
    let lib = c.w.app.library_dir();
    rename_album_with_plan(
        RenameParts {
            conn: &mut conn,
            bus: c.w.app.bus(),
            library_dir: lib.as_deref(),
            now: "2027-01-15T08:00:00Z",
            track_order: musaic_scan::TrackOrder::DiscTrack,
        },
        &after_id,
        &files,
        Some(&dir),
    )
    .unwrap();
    assert!(
        Path::new(&c.path).is_dir(),
        "the folder has its old name again"
    );
    let mut after: Vec<String> = std::fs::read_dir(&c.path)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    let mut want = before_files;
    after.sort();
    want.sort();
    // The scan may add its own files; every original name is back.
    for n in &want {
        assert!(after.contains(n), "{n} is missing from {after:?}");
    }
    assert_eq!(tag_state(Path::new(&c.path)), tags_before);
}

#[test]
fn a_cover_is_copied_byte_for_byte_before_it_could_be_replaced() {
    let c = case();
    let conn = musaic_db::open_connection(c.w.app.db_path()).unwrap();
    let lib = c.w.app.library_dir();
    // An album that has a cover (a sidecar file or an embedded picture).
    let ops = c.w.app.ops();
    let with_cover = ops
        .library()
        .list_albums(false)
        .unwrap()
        .albums
        .into_iter()
        .find(|a| {
            ops.library()
                .cover(a.id(), musaic_app::ops::library::CoverSize::Full)
                .is_ok()
        })
        .expect("the synthetic library has an album with a cover");
    let row =
        musaic_services::agent_read::cached_row(&conn, lib.as_deref(), with_cover.id()).unwrap();
    let original = match musaic_services::covers::full_cover(&row.album.path, &row.json).unwrap() {
        musaic_services::covers::FullCover::Disk(p) => std::fs::read(p).unwrap(),
        musaic_services::covers::FullCover::Embedded(b, _) => b,
    };
    let patch: musaic_commands::tools::fix::ProposedPatch = serde_json::from_value(
        json!({"cover": {"source": "url", "url": "https://example.com/x.jpg"}}),
    )
    .unwrap();
    let diff = musaic_commands::tools::fix::FixDiff::default();
    let id = musaic_services::agent_changes::record(
        &conn,
        c.w.app.config_dir(),
        &musaic_services::agent_changes::ChangeInput {
            pairing_id: &c.p.pairing_id,
            proposal_id: "pr_x",
            approval_id: None,
            now: "2027-01-15T08:00:00Z",
        },
        &row,
        &patch,
        &diff,
    )
    .unwrap();
    let rows = musaic_db::agent::list_changes(&conn).unwrap();
    let image: BeforeImage = serde_json::from_str(&rows[0].before).unwrap();
    let saved = image.cover.unwrap();
    assert!(matches!(saved.source.as_str(), "sidecar" | "embedded"));
    let copy = c.w.scratch.config.join(saved.saved_as.unwrap());
    assert_eq!(std::fs::read(copy).unwrap(), original, "not re-encoded");
    assert_eq!(rows[0].id, id);
    // Pruning removes the row and its directory.
    assert_eq!(
        musaic_db::agent::prune_changes(&conn, "2099-01-01T00:00:00Z").unwrap(),
        [id]
    );
}

#[cfg(unix)]
#[test]
fn a_before_image_that_cannot_be_recorded_refuses_the_apply_and_writes_nothing() {
    use std::os::unix::fs::PermissionsExt;
    let c = case();
    let dir = c.w.scratch.config.join("agent-changes");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    // Running as root ignores the mode; the case would prove nothing then.
    let probe = dir.join("probe");
    if std::fs::create_dir(&probe).is_ok() {
        std::fs::remove_dir(&probe).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        eprintln!("skipped: the directory mode is not enforced for this user");
        return;
    }
    let digest = |p: &str| -> BTreeMap<String, Vec<u8>> {
        std::fs::read_dir(p)
            .unwrap()
            .flatten()
            .filter(|e| e.path().is_file())
            .map(|e| {
                (
                    e.file_name().to_string_lossy().into_owned(),
                    std::fs::read(e.path()).unwrap(),
                )
            })
            .collect()
    };
    let before = digest(&c.path);
    let rev =
        c.w.call(&c.p, "musaic_album_get", json!({"albumId": c.album}))
            .unwrap()
            .to_json()["revision"]
            .clone();
    let prev = c
        .w
        .call(&c.p, "musaic_fix_preview", json!({"albumId": c.album, "baseRevision": rev, "proposedPatch": {"tags": {"album": {"genre": "Never Written"}}}}))
        .unwrap()
        .to_json();
    let r = c.w.call(&c.p, "musaic_fix_apply", json!({"proposalId": prev["proposalId"], "digest": prev["digest"], "idempotencyKey": "key-noimage-01"}));
    let e = r.unwrap_err();
    assert_eq!(e.code, musaic_commands::ToolErrorCode::Unavailable);
    assert_eq!(digest(&c.path), before, "0 files changed");
    let conn = musaic_db::open_connection(c.w.app.db_path()).unwrap();
    assert!(musaic_db::agent::list_changes(&conn).unwrap().is_empty());
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
}
