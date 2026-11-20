//! (Moved here from `musaic-services/tests` when the pipeline began to report
//! typed events: the items compared below are `musaic_api::render::apply`'s
//! rendering of them. Every assertion is the old one.)
//!
//! Removing a tag through the apply pipeline (issue #210): the stream items,
//! the database row after the single-album upsert, `AlbumTagsChanged`, and
//! the check-fix path, which never removes anything.
//!
//! An invented album of minimal FLACs (no audio frames) scanned into a copy
//! of the fixture v16 database.

use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use musaic_api::render::apply as render;
use musaic_db::Db;
use musaic_events::{Bus, Event, EventKind, Subscriber, Topic};
use musaic_scan::{TrackOrder, py_resolve, scan_library};
use musaic_services::Json;
use musaic_services::apply::{ApplyEnv, ApplyRequest, NoCovers, apply_album_changes, apply_direct};
use musaic_services::{Removals, albums};

const NOW: &str = "2026-01-01T00:00:00Z";

/// Minimal FLAC (the recipe of musaic-tags' logging_seams).
fn flac_bytes(tags: &[(&str, &str)]) -> Vec<u8> {
    let mut si = Vec::new();
    si.extend_from_slice(&[0x10, 0, 0x10, 0, 0, 0, 0, 0, 0, 0]);
    let packed: u64 = (44100u64 << 44) | (1 << 41) | (15 << 36) | 44100;
    si.extend_from_slice(&packed.to_be_bytes());
    si.extend_from_slice(&[0; 16]);
    let mut vc = Vec::new();
    vc.extend_from_slice(&4u32.to_le_bytes());
    vc.extend_from_slice(b"test");
    vc.extend_from_slice(&(tags.len() as u32).to_le_bytes());
    for (k, v) in tags {
        let e = format!("{k}={v}");
        vc.extend_from_slice(&(e.len() as u32).to_le_bytes());
        vc.extend_from_slice(e.as_bytes());
    }
    let mut out = b"fLaC".to_vec();
    out.push(0);
    out.extend_from_slice(&(si.len() as u32).to_be_bytes()[1..]);
    out.extend_from_slice(&si);
    out.push(0x04);
    out.extend_from_slice(&(vc.len() as u32).to_be_bytes()[1..]);
    out.extend_from_slice(&vc);
    out.push(0x81);
    out.extend_from_slice(&1024u32.to_be_bytes()[1..]);
    out.extend_from_slice(&[0; 1024]);
    out
}

struct Lib {
    _tmp: tempfile::TempDir,
    db: Db,
    lib_s: String,
    id: String,
    album: PathBuf,
}

fn library() -> Lib {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let lib = base.join("lib");
    let album = lib.join("Copper Wren - Field Notes [FLAC]");
    std::fs::create_dir_all(&album).unwrap();
    for (name, n) in [("01 - Moss.flac", "1"), ("02 - Fern.flac", "2")] {
        std::fs::write(
            album.join(name),
            flac_bytes(&[
                ("TITLE", "Song"),
                ("ARTIST", "Copper Wren"),
                ("ALBUM", "Field Notes"),
                ("ALBUMARTIST", "Copper Wren"),
                ("ALBUMARTISTSORT", "Stray, Value"),
                ("GENRE", "Folk"),
                ("TRACKNUMBER", n),
            ]),
        )
        .unwrap();
    }
    let db_path = base.join("library.db");
    std::fs::copy(musaic_testdata::public("db/v16-empty.db"), &db_path).unwrap();
    let (mut db, _) = Db::open(&db_path).unwrap();
    let lib_s = py_resolve(&lib).to_string_lossy().into_owned();
    let scanned = scan_library(&lib, TrackOrder::Filename);
    musaic_db::albums::save_snapshot(db.connection_mut(), &lib_s, NOW, NOW, &scanned).unwrap();
    let id = scanned[0].id.clone();
    Lib {
        _tmp: tmp,
        db,
        lib_s,
        id,
        album,
    }
}

/// Run one apply; the stream items and every `AlbumTagsChanged.fields`.
fn apply(l: &mut Lib, req: &ApplyRequest) -> (Vec<(String, Json)>, Vec<Vec<String>>) {
    let bus = Bus::new();
    let seen: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
    let s2 = seen.clone();
    bus.register(
        Topic::Kind(EventKind::AlbumTagsChanged),
        Subscriber::from_fn(move |e| {
            if let Event::AlbumTagsChanged { fields, .. } = e {
                s2.lock().unwrap().push(fields.clone());
            }
            Ok(())
        }),
    );
    let now = || NOW.to_string();
    let mut env = ApplyEnv {
        conn: l.db.connection_mut(),
        bus: &bus,
        covers: &NoCovers,
        now: &now,
        track_order: TrackOrder::Filename,
    };
    let mut items = Vec::new();
    apply_album_changes(&mut env, req, &mut |it| {
        let it = musaic_app::ops::apply::typed(it);
        items.push((render::event_name(&it).to_string(), render::payload(&it)));
        ControlFlow::Continue(())
    });
    let fields = seen.lock().unwrap().clone();
    (items, fields)
}

fn request(l: &Lib, removals: Removals, dry_run: bool) -> ApplyRequest {
    ApplyRequest {
        album_id: l.id.clone(),
        album_path: l.album.to_string_lossy().into_owned(),
        library_dir: Some(l.lib_s.clone()),
        suggestions: vec![("genre".into(), Some("Chamber Folk".into()))],
        removals,
        dry_run,
        ..Default::default()
    }
}

fn first<'a>(items: &'a [(String, Json)], event: &str) -> &'a Json {
    &items.iter().find(|(e, _)| e == event).unwrap().1
}

fn sort_removal() -> Removals {
    Removals {
        album: vec!["album_artist_sort".into()],
        tracks: Vec::new(),
    }
}

/// The removal shows in every `file_done` as `new: null`, `start` / `done`
/// list it under `cleared`, the row loses the field, and `AlbumTagsChanged`
/// names it.
#[test]
fn removal_reaches_stream_row_and_event() {
    let mut l = library();
    let before = albums::get_album_by_id(l.db.connection(), &l.id)
        .unwrap()
        .unwrap();
    assert_eq!(
        before.album.album_artist_sort.as_deref(),
        Some("Stray, Value")
    );

    let req = request(&l, sort_removal(), false);
    let (items, fields) = apply(&mut l, &req);
    let names: Vec<&str> = items.iter().map(|(e, _)| e.as_str()).collect();
    assert_eq!(names.last(), Some(&"updated_album"), "{items:?}");
    let start = first(&items, "start");
    assert_eq!(
        start.get("cleared").unwrap().dumps(),
        r#"["album_artist_sort"]"#
    );
    assert_eq!(
        start.get("suggestions_applied").unwrap().dumps(),
        r#"{"genre": "Chamber Folk"}"#
    );
    let done = items.iter().find(|(e, _)| e == "file_done").unwrap();
    assert_eq!(
        done.1.get("diffs").unwrap().dumps(),
        r#"{"genre": {"old": "Folk", "new": "Chamber Folk"}, "albumartistsort": {"old": "Stray, Value", "new": null}}"#
    );
    assert!(first(&items, "done").get("cleared").is_some());

    let row = albums::get_album_by_id(l.db.connection(), &l.id)
        .unwrap()
        .unwrap();
    assert_eq!(row.album.album_artist_sort, None);
    assert_eq!(row.album.genre.as_deref(), Some("Chamber Folk"));
    assert_eq!(
        first(&items, "updated_album").get("album_artist_sort"),
        Some(&Json::Null)
    );
    assert_eq!(fields, vec![vec!["genre", "album_artist_sort"]]);
}

/// A dry run reports the removal and changes neither files nor the row.
#[test]
fn dry_run_reports_removal_only() {
    let mut l = library();
    let file = l.album.join("01 - Moss.flac");
    let bytes = std::fs::read(&file).unwrap();
    let req = request(&l, sort_removal(), true);
    let (items, fields) = apply(&mut l, &req);
    let done = items.iter().find(|(e, _)| e == "file_done").unwrap();
    assert!(
        done.1
            .get("diffs")
            .unwrap()
            .dumps()
            .contains(r#""albumartistsort": {"old": "Stray, Value", "new": null}"#)
    );
    assert_eq!(std::fs::read(&file).unwrap(), bytes);
    assert!(fields.is_empty());
    let row = albums::get_album_by_id(l.db.connection(), &l.id)
        .unwrap()
        .unwrap();
    assert_eq!(row.album.album_artist_sort.as_deref(), Some("Stray, Value"));
}

/// Without removals the payloads carry no `cleared` key (the shape the
/// Python app sent), and an empty suggestion — what a source sends when its
/// data is missing — removes nothing, on the stream path and the check-fix
/// path alike.
#[test]
fn missing_data_never_clears() {
    let mut l = library();
    let mut req = request(&l, Removals::default(), false);
    req.suggestions = vec![
        ("album_artist_sort".into(), Some(String::new())),
        ("genre".into(), Some("  ".into())),
    ];
    let (items, _) = apply(&mut l, &req);
    assert!(first(&items, "start").get("cleared").is_none());
    assert!(first(&items, "done").get("cleared").is_none());
    let done = items.iter().find(|(e, _)| e == "file_done").unwrap();
    assert_eq!(done.1.get("diffs").unwrap().dumps(), "{}");

    let bus = Bus::new();
    let now = || NOW.to_string();
    let mut env = ApplyEnv {
        conn: l.db.connection_mut(),
        bus: &bus,
        covers: &NoCovers,
        now: &now,
        track_order: TrackOrder::Filename,
    };
    let (ok, msg) = apply_direct(
        &mut env,
        Some(&l.lib_s),
        &l.id,
        &vec![("album_artist_sort".into(), Some(String::new()))],
        &Vec::new(),
        None,
    );
    assert!(ok, "{msg}");
    assert_eq!(msg, "Wrote 0 files");
    let row = albums::get_album_by_id(l.db.connection(), &l.id)
        .unwrap()
        .unwrap();
    assert_eq!(row.album.album_artist_sort.as_deref(), Some("Stray, Value"));
}
