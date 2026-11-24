//! FR-055: the desktop user's Undo of an assistant's applied change. It puts
//! the tags, the cover (sidecars and every embedded picture, byte for byte)
//! and the names back, publishes what any apply and rename publish, and
//! refuses, writing nothing, when the album changed since, is Missing, a
//! restore is running, or the change was already undone.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

use common::{AgentWorld, FakeClock, agent_err_code, app_quiet_pool, configure, hold_restore};
use musaic_app::ops::Listening;
use musaic_app::ops::agent::{AgentError, Principal};
use musaic_app::ops::apply::ApplyChanges;
use musaic_commands::{Decision, PermissionKind as K};
use musaic_events::{AgentWhat, Event, Subscriber, Topic};
use musaic_net::{Request, Response, Transport, TransportError};
use musaic_secrets::MemoryStore;
use musaic_services::production::Network;
use musaic_tags::flac::{FlacFile, Picture};
use serde_json::json;

const ALL: &[K] = &[
    K::LibraryRead,
    K::ProposalSubmit,
    K::TagsWrite,
    K::CoversWrite,
    K::FilesRename,
];

/// The cover an assistant asks for: the JPEG signature, then bytes no
/// decoder takes, so the pipeline keeps them as they are.
const NEW_COVER: &[u8] = b"\xFF\xD8\xFF\xE0 the assistant's cover \xFF\xD9";

/// Answers every request with [`NEW_COVER`]; nothing leaves the process.
struct CoverServer;

impl Transport for CoverServer {
    fn send(&self, _request: &Request) -> Result<Response, TransportError> {
        Ok(Response {
            status: 200,
            headers: vec![("content-type".into(), "image/jpeg".into())],
            body: NEW_COVER.to_vec(),
        })
    }
}

type Seen = Arc<Mutex<Vec<Event>>>;

struct Case {
    w: AgentWorld,
    p: Principal,
    album: String,
    path: String,
    seen: Seen,
    keys: std::cell::Cell<u32>,
}

/// An agent world on a scanned copy of the library, with every write kind
/// allowed and a recorder of every bus event.
fn case() -> Case {
    let clock = FakeClock::new();
    let store = Arc::new(MemoryStore::new());
    let (c2, s2) = (clock.clone(), store.clone());
    let (app, scratch) = app_quiet_pool(move |b| {
        b.clock(c2.wall())
            .agent_secrets(s2)
            .network(Network::Live(Arc::new(CoverServer)))
    });
    let w = AgentWorld {
        app,
        scratch,
        clock,
        store,
    };
    configure(&w.app, &w.scratch);
    rescan(&w);
    let id = w.pair("a", ALL).pairing.id;
    for k in [K::TagsWrite, K::CoversWrite, K::FilesRename] {
        w.app
            .ops()
            .agent()
            .set_policy(&id, &[(k, Decision::Allow)])
            .unwrap();
    }
    let p = w.login(&id);
    let a = w
        .app
        .ops()
        .library()
        .list_albums(false)
        .unwrap()
        .albums
        .remove(0);
    let (album, path) = (a.id().to_string(), a.path().to_string());
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    w.app.bus().register(
        Topic::Event,
        Subscriber::from_fn(move |e| {
            s.lock().unwrap().push(e.clone());
            Ok(())
        })
        .key("undo_test")
        .priority(500)
        .name("undo_test"),
    );
    Case {
        w,
        p,
        album,
        path,
        seen,
        keys: std::cell::Cell::new(0),
    }
}

fn rescan(w: &AgentWorld) {
    assert!(w.app.ops().scan().subscribe(false).unwrap().count() > 2);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while w.app.gate().snapshot() != (0, false) {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    common::wait_for_fingerprints(&w.app);
}

impl Case {
    /// Preview and apply `patch` as the assistant; returns the operation.
    fn apply(&self, patch: serde_json::Value) -> serde_json::Value {
        let rev = self
            .w
            .call(&self.p, "musaic_album_get", json!({"albumId": self.album}))
            .unwrap()
            .to_json()["revision"]
            .clone();
        let prev = self
            .w
            .call(
                &self.p,
                "musaic_fix_preview",
                json!({"albumId": self.album, "baseRevision": rev, "proposedPatch": patch}),
            )
            .unwrap()
            .to_json();
        self.keys.set(self.keys.get() + 1);
        let h = self
            .w
            .call(
                &self.p,
                "musaic_fix_apply",
                json!({"proposalId": prev["proposalId"], "digest": prev["digest"],
                       "idempotencyKey": format!("key-undo-{:04}", self.keys.get())}),
            )
            .unwrap()
            .to_json();
        assert_eq!(h["state"], "succeeded", "{h}");
        h
    }

    /// The one change the assistant made.
    fn change_id(&self) -> String {
        let list = self
            .w
            .app
            .ops()
            .agent()
            .list_changes(Some(&self.p.pairing_id))
            .unwrap();
        let originals: Vec<_> = list.iter().filter(|c| c.undoes.is_none()).collect();
        assert_eq!(originals.len(), 1, "{list:?}");
        originals[0].id.clone()
    }

    fn undo(&self, id: &str) -> Result<musaic_app::ops::agent::UndoOutcome, AgentError> {
        self.w.app.ops().agent().undo_change(id)
    }

    /// The album-scoped library events since the last call, without the
    /// album ids (an undo's run uses the ids the album has as it goes).
    fn take_library_events(&self) -> Vec<(String, BTreeSet<String>)> {
        assert!(
            self.w
                .app
                .bus()
                .wait_background_idle(std::time::Duration::from_secs(30))
        );
        let mut seen = self.seen.lock().unwrap();
        let out = seen
            .iter()
            .filter_map(|e| match e {
                Event::AlbumTagsChanged { fields, .. } => Some((
                    "AlbumTagsChanged".to_string(),
                    fields.iter().cloned().collect(),
                )),
                Event::AlbumCoverChanged { .. } => {
                    Some(("AlbumCoverChanged".to_string(), BTreeSet::new()))
                }
                Event::AlbumRenamed { .. } => Some(("AlbumRenamed".to_string(), BTreeSet::new())),
                _ => None,
            })
            .collect();
        seen.clear();
        out
    }

    fn agent_events(&self) -> Vec<(AgentWhat, Vec<String>)> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                Event::AgentStateChanged { what, ids } => Some((*what, ids.clone())),
                _ => None,
            })
            .collect()
    }
}

/// Every Vorbis comment of every FLAC file, keys folded to lower case, values
/// in file order: the state an undo must restore.
fn tag_state(dir: &Path) -> BTreeMap<String, BTreeMap<String, Vec<String>>> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".flac") {
            continue;
        }
        let f = FlacFile::open(&e.path()).unwrap();
        let mut keys: BTreeMap<String, Vec<String>> = BTreeMap::new();
        if let Some(t) = f.tags() {
            let names: BTreeSet<String> = t
                .comments
                .iter()
                .map(|(k, _)| k.to_ascii_lowercase())
                .collect();
            for k in names {
                keys.insert(k.clone(), t.values(&k).map(str::to_string).collect());
            }
        }
        out.insert(name, keys);
    }
    out
}

/// Every file of the folder, byte for byte.
fn digest(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(dir)
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
}

fn flacs(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".flac"))
        .collect();
    v.sort();
    v
}

#[test]
fn undo_puts_back_the_tags_with_the_events_of_an_apply() {
    let c = case();
    let dir = Path::new(&c.path);
    // A multi-valued genre and an alias spelling a removal also deletes:
    // a single value or the main key alone could not restore these.
    let first = flacs(dir)[0].clone();
    {
        let mut f = FlacFile::open(&dir.join(&first)).unwrap();
        let t = f.tags_mut();
        t.set("GENRE", "Ambient");
        t.append("GENRE", "Drone");
        t.set("ALBUM_ARTIST", "Alias Spelling");
        f.save().unwrap();
    }
    rescan(&c.w);
    let before = tag_state(dir);
    c.take_library_events();

    c.apply(json!({"tags": {
        "album": {"genre": "Replayed Genre", "year": "1987", "album_artist": null},
        "tracks": {first.clone(): {"title": "A Different Title"}}}}));
    let during = tag_state(dir);
    assert_ne!(during, before);
    let apply_events = c.take_library_events();
    assert!(!apply_events.is_empty());

    let id = c.change_id();
    let listed =
        c.w.app
            .ops()
            .agent()
            .list_changes(Some(&c.p.pairing_id))
            .unwrap();
    assert!(listed[0].undoable, "{listed:?}");
    assert!(listed[0].summary.contains("genre"), "{}", listed[0].summary);

    let out = c.undo(&id).unwrap();
    assert!(out.complete, "{out:?}");
    assert_eq!(out.change.state, "undone");
    assert!(!out.change.undoable);
    assert_eq!(
        tag_state(dir),
        before,
        "every value, the second genre and the alias spelling included"
    );
    assert!(
        c.agent_events()
            .contains(&(AgentWhat::Changes, vec![c.p.pairing_id.clone()]))
    );
    assert_eq!(
        c.take_library_events(),
        apply_events,
        "the undo publishes what the apply published"
    );

    // The undo is recorded with its own before-image: what it overwrote.
    let conn = musaic_db::open_connection(c.w.app.db_path()).unwrap();
    let rows = musaic_db::agent::list_changes(&conn).unwrap();
    let undo = rows
        .iter()
        .find(|r| r.undoes.as_deref() == Some(id.as_str()))
        .expect("the undo's record");
    assert_eq!(undo.id, out.undo_id);
    assert_eq!(undo.state, "applied");
    let image: musaic_services::agent_changes::BeforeImage =
        serde_json::from_str(&undo.before).unwrap();
    assert_eq!(
        image.files[&first]["album.genre"].values(),
        ["Replayed Genre"]
    );
    let activity = c.w.app.ops().agent().activity(&c.p.pairing_id).unwrap();
    assert_eq!(
        (activity[0].tool.as_str(), activity[0].outcome.as_str()),
        ("undo", "ok")
    );

    // Twice is refused, and so is undoing the undo; nothing is written.
    let files = digest(dir);
    let again = c.undo(&id).unwrap_err();
    assert_eq!(agent_err_code(&again), "CONFLICT", "{again:?}");
    let of_undo = c.undo(&out.undo_id).unwrap_err();
    assert_eq!(agent_err_code(&of_undo), "CONFLICT", "{of_undo:?}");
    assert_eq!(digest(dir), files);
}

/// The cover art a cover apply replaces: every cover sidecar and every
/// file's PICTURE blocks.
type Art = (BTreeMap<String, Vec<u8>>, BTreeMap<String, Vec<Picture>>);

fn art_state(dir: &Path) -> Art {
    let mut sidecars = BTreeMap::new();
    let mut pictures = BTreeMap::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.to_ascii_lowercase().starts_with("cover.") {
            sidecars.insert(name, std::fs::read(e.path()).unwrap());
        } else if name.ends_with(".flac") {
            let f = FlacFile::open(&e.path()).unwrap();
            pictures.insert(name, f.pictures().into_iter().cloned().collect());
        }
    }
    (sidecars, pictures)
}

fn picture(kind: u32, mime: &str, desc: &str, data: &[u8]) -> Picture {
    Picture {
        kind,
        mime: mime.into(),
        desc: desc.into(),
        width: 7,
        height: 5,
        depth: 24,
        colors: 0,
        data: data.to_vec(),
    }
}

#[test]
fn undo_puts_back_every_sidecar_and_embedded_picture_byte_for_byte() {
    let c = case();
    let dir = Path::new(&c.path);
    let names = flacs(dir);
    assert!(names.len() >= 3, "{names:?}");
    // A `cover.jpg` and a competing `cover.png` (the apply deletes it), a
    // front and a back cover in the first file, one shared picture in the
    // others, none in the last.
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        if e.file_name()
            .to_string_lossy()
            .to_ascii_lowercase()
            .starts_with("cover.")
        {
            std::fs::remove_file(e.path()).unwrap();
        }
    }
    std::fs::write(dir.join("cover.jpg"), b"prior sidecar jpg").unwrap();
    std::fs::write(dir.join("cover.png"), b"prior sidecar png").unwrap();
    let shared = picture(3, "image/jpeg", "", b"shared embedded front");
    for (i, name) in names.iter().enumerate() {
        let mut f = FlacFile::open(&dir.join(name)).unwrap();
        f.clear_pictures();
        if i == 0 {
            f.add_picture(picture(3, "image/png", "front", b"first file front"));
            f.add_picture(picture(4, "image/jpeg", "back", b"first file back"));
        } else if i + 1 < names.len() {
            f.add_picture(shared.clone());
        }
        f.save().unwrap();
    }
    rescan(&c.w);
    let before = art_state(dir);
    let tags_before = tag_state(dir);
    c.take_library_events();

    c.apply(json!({"cover": {"source": "url", "url": "https://covers.example.com/front.jpg"}}));
    let during = art_state(dir);
    assert_ne!(during.0, before.0, "the apply replaced the sidecars");
    assert_ne!(during.1, before.1, "the apply replaced the embedded art");
    let apply_events = c.take_library_events();
    assert!(apply_events.iter().any(|(k, _)| k == "AlbumCoverChanged"));

    let out = c.undo(&c.change_id()).unwrap();
    assert!(out.complete, "{out:?}");
    assert_eq!(
        art_state(dir),
        before,
        "the sidecars and every embedded picture are back byte for byte"
    );
    assert_eq!(tag_state(dir), tags_before);
    assert_eq!(c.take_library_events(), apply_events);
}

#[test]
fn undo_renames_the_files_and_the_folder_back() {
    let c = case();
    let dir = Path::new(&c.path);
    let files_before = flacs(dir);
    let tags_before = tag_state(dir);
    c.take_library_events();
    let h =
        c.apply(json!({"tags": {"album": {"album_artist": "Roundtrip Artist"}}, "rename": true}));
    let moved_id = h["result"]["albumIdAfter"].as_str().unwrap().to_string();
    assert!(!dir.exists(), "the folder was renamed");
    let apply_events = c.take_library_events();
    assert!(apply_events.iter().any(|(k, _)| k == "AlbumRenamed"));

    let id = c.change_id();
    let listed =
        c.w.app
            .ops()
            .agent()
            .list_changes(Some(&c.p.pairing_id))
            .unwrap();
    assert_eq!(listed[0].album_id, moved_id, "listed under its id now");
    let out = c.undo(&id).unwrap();
    assert!(out.complete, "{out:?}");
    assert!(dir.is_dir(), "the folder has its old name again");
    assert_eq!(flacs(dir), files_before);
    assert_eq!(tag_state(dir), tags_before);
    let ids: Vec<String> =
        c.w.app
            .ops()
            .library()
            .list_albums(false)
            .unwrap()
            .albums
            .iter()
            .map(|a| a.id().to_string())
            .collect();
    assert!(ids.contains(&c.album), "the album has its old id again");
    assert!(!ids.contains(&moved_id));
    let mut undo_events = c.take_library_events();
    let mut want = apply_events;
    undo_events.sort();
    want.sort();
    assert_eq!(undo_events, want, "a rename and an apply, as before");
}

/// The album changes id after the assistant's change: its folder is listed
/// under another spelling of the same folder, and a scan adopts it (rule 3:
/// the files are the same, only the id changed). The change record follows
/// the album, so it is listed under the new id and the Undo still puts the
/// tags back. A proposal made before follows too, expired: it was made
/// against the old row, so applying it asks for a new preview.
#[test]
fn undo_still_works_after_the_album_changed_id() {
    let c = case();
    let dir = Path::new(&c.path);
    let tags_before = tag_state(dir);
    c.apply(json!({"tags": {"album": {"genre": "Assistant Genre"}}}));
    let change = c.change_id();
    let rev =
        c.w.call(&c.p, "musaic_album_get", json!({"albumId": c.album}))
            .unwrap()
            .to_json()["revision"]
            .clone();
    let pending =
        c.w.call(
            &c.p,
            "musaic_fix_preview",
            json!({"albumId": c.album, "baseRevision": rev,
                   "proposedPatch": {"tags": {"album": {"year": "1999"}}}}),
        )
        .unwrap()
        .to_json();

    // Another spelling of the same folder: lower case, which still opens
    // the old spelling on a case-insensitive volume, and a symlink from the
    // old spelling elsewhere (as the scan adoption tests do).
    let name = dir.file_name().unwrap().to_string_lossy().into_owned();
    let to = name.to_lowercase();
    assert_ne!(name, to, "the folder name has upper-case letters");
    let respelled = dir.parent().unwrap().join(&to);
    std::fs::rename(dir, &respelled).unwrap();
    #[cfg(target_os = "linux")]
    std::os::unix::fs::symlink(&to, dir).unwrap();
    if !dir.is_dir() {
        eprintln!("skipped: this volume tells {name:?} and {to:?} apart");
        return;
    }
    rescan(&c.w);
    let albums = c.w.app.ops().library().list_albums(false).unwrap().albums;
    let moved = albums
        .iter()
        .find(|a| Path::new(a.path()).file_name() == Some(std::ffi::OsStr::new(&to)))
        .expect("the album was adopted under its new spelling");
    let new_id = moved.id().to_string();
    assert_ne!(new_id, c.album, "the album changed id");
    assert!(!albums.iter().any(|a| a.id() == c.album));

    let listed =
        c.w.app
            .ops()
            .agent()
            .list_changes(Some(&c.p.pairing_id))
            .unwrap();
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(
        listed[0].album_id, new_id,
        "listed under the album's id now"
    );
    assert!(listed[0].undoable, "{listed:?}");

    let out = c.undo(&change).unwrap();
    assert!(out.complete, "{out:?}");
    assert_eq!(tag_state(&respelled), tags_before);

    let applied = c.w.call(
        &c.p,
        "musaic_fix_apply",
        json!({"proposalId": pending["proposalId"], "digest": pending["digest"],
               "idempotencyKey": "key-undo-after-adoption"}),
    );
    assert_eq!(common::tool_code(&applied), "PROPOSAL_EXPIRED");
}

#[test]
fn a_later_edit_of_a_field_the_change_wrote_refuses_the_undo_and_writes_nothing() {
    let c = case();
    let dir = Path::new(&c.path);
    c.apply(json!({"tags": {"album": {"genre": "Assistant Genre"}}}));
    let id = c.change_id();
    // The user edits the same field afterwards.
    c.w.app
        .ops()
        .apply()
        .apply_changes(
            &c.album,
            &ApplyChanges {
                suggestions: vec![("genre".into(), "My Own Genre".into())],
                ..ApplyChanges::default()
            },
            &mut |_| Listening::Yes,
        )
        .unwrap();
    let files = digest(dir);
    let e = c.undo(&id).unwrap_err();
    match &e {
        AgentError::ChangedSince { changed, message } => {
            assert!(
                changed.iter().any(|s| s.ends_with(": genre")),
                "{changed:?}"
            );
            assert!(message.contains("nothing was put back"), "{message}");
        }
        other => panic!("not a ChangedSince refusal: {other:?}"),
    }
    assert_eq!(digest(dir), files, "0 files changed");
    let listed =
        c.w.app
            .ops()
            .agent()
            .list_changes(Some(&c.p.pairing_id))
            .unwrap();
    assert_eq!(listed.len(), 1, "no undo was recorded");
    assert_eq!(listed[0].state, "applied");
    assert!(!listed[0].undoable);
    assert!(
        listed[0]
            .reason
            .as_deref()
            .is_some_and(|r| r.starts_with("Changed since")),
        "{listed:?}"
    );
}

#[test]
fn a_missing_album_refuses_the_undo_and_writes_nothing() {
    let c = case();
    let dir = Path::new(&c.path);
    c.apply(json!({"tags": {"album": {"genre": "Assistant Genre"}}}));
    let id = c.change_id();
    let conn = musaic_db::open_connection(c.w.app.db_path()).unwrap();
    assert!(
        musaic_db::albums::mark_album_missing(&conn, &c.album, "2027-01-01T00:00:00Z").unwrap()
    );
    let files = digest(dir);
    let e = c.undo(&id).unwrap_err();
    assert_eq!(agent_err_code(&e), "CONFLICT", "{e:?}");
    assert!(format!("{e:?}").contains("missing"), "{e:?}");
    assert_eq!(digest(dir), files);
}

#[test]
fn a_running_restore_refuses_the_undo_and_writes_nothing() {
    let c = case();
    let dir = Path::new(&c.path);
    c.apply(json!({"tags": {"album": {"genre": "Assistant Genre"}}}));
    let id = c.change_id();
    let files = digest(dir);
    let guard = hold_restore(&c.w.app);
    let e = c.undo(&id).unwrap_err();
    assert_eq!(agent_err_code(&e), "RESTORING", "{e:?}");
    drop(guard);
    assert_eq!(digest(dir), files);
    // Once it ends, the undo runs.
    assert!(c.undo(&id).unwrap().complete);
}

#[test]
fn an_unknown_change_is_not_found() {
    let c = case();
    let e = c.undo("ch_nope").unwrap_err();
    assert_eq!(agent_err_code(&e), "NOT_FOUND");
    let e =
        c.w.app
            .ops()
            .agent()
            .list_changes(Some("p_nope"))
            .unwrap_err();
    assert_eq!(agent_err_code(&e), "NOT_FOUND");
    assert!(c.w.app.ops().agent().list_changes(None).unwrap().is_empty());
}
