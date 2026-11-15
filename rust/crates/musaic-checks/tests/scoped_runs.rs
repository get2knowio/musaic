//! Album-scoped runs do only the scoped albums' work:
//! - tags-out-of-sync reads and resolves only the scoped albums (resolving
//!   an anchored album is MusicBrainz work through the resolver), and leaves
//!   other albums' findings as they were;
//! - duplicate-tracks loads and clusters only the scoped albums'
//!   fingerprints (a duplicate cluster lies within one album).
//!
//! A library-wide run still covers every album.

mod common;

use std::sync::{Arc, Mutex};

use common::{Sandbox, env, registry, setup};
use musaic_checks::{Resolution, ResolutionSource, ScopeKind, run_check};
use musaic_db::findings::{FindingRow, all_findings};
use rusqlite::Connection;
use serde_json::{Value as J, json};

/// Records every album it is asked to resolve; resolves to nothing.
#[derive(Default)]
struct Counting(Mutex<Vec<String>>);

impl ResolutionSource for Counting {
    fn resolve(&self, _conn: &Connection, album_id: &str) -> Result<Resolution, String> {
        self.0.lock().unwrap().push(album_id.to_string());
        Ok(Resolution::default())
    }
}

fn album(id: &str) -> J {
    // Anchored (so the check asks the resolver) and missing a genre (so it
    // has a finding either way).
    json!({
        "id": id, "library_path": "<LIB>", "path": format!("<LIB>/{id}"), "dir_name": id,
        "title": "T", "artist": "A", "album_artist": "A", "year": "2001",
        "track_count": 2, "track_numbers_json": "[1, 2]",
        "mb_release_group_mbid": format!("rg-{id}"),
    })
}

fn of(conn: &Connection, check: &str, album: &str) -> Vec<FindingRow> {
    all_findings(conn)
        .unwrap()
        .into_iter()
        .filter(|f| f.check_id == check && f.album_id.as_deref() == Some(album))
        .collect()
}

#[test]
fn tags_out_of_sync_resolves_only_the_scoped_albums() {
    let (sb, db) = Sandbox::new();
    let conn = db.connection();
    setup(&sb, conn, &json!({"albums": [album("a1"), album("a2")]}));
    let resolver = Arc::new(Counting::default());
    let env = env(registry(None, &Default::default()), resolver.clone());

    let run = run_check(
        conn,
        &env,
        "tags-out-of-sync",
        &sb.lib_s,
        ScopeKind::All,
        None,
    )
    .unwrap();
    assert_eq!(run.status, "completed", "{:?}", run.error);
    assert_eq!(run.issue_count, 2);
    let mut asked = resolver.0.lock().unwrap().clone();
    asked.sort();
    assert_eq!(
        asked,
        ["a1", "a2"],
        "a library-wide run resolves every album"
    );
    resolver.0.lock().unwrap().clear();
    let a2_before = of(conn, "tags-out-of-sync", "a2");

    let ids = vec!["a1".to_string()];
    let run = run_check(
        conn,
        &env,
        "tags-out-of-sync",
        &sb.lib_s,
        ScopeKind::Albums,
        Some(&ids),
    )
    .unwrap();
    assert_eq!(run.status, "completed", "{:?}", run.error);
    assert_eq!(
        *resolver.0.lock().unwrap(),
        ["a1"],
        "no resolver call for a2"
    );
    assert_eq!(run.issue_count, 1, "only a1's finding");
    assert_eq!(
        of(conn, "tags-out-of-sync", "a2"),
        a2_before,
        "a2 untouched"
    );
}

#[test]
fn duplicate_tracks_reads_only_the_scoped_albums() {
    let (sb, db) = Sandbox::new();
    let conn = db.connection();
    let fp = "AQAAA0mUaEkSRZEGAA";
    let fps: Vec<J> = ["a1", "a2"]
        .iter()
        .flat_map(|a| {
            ["01.flac", "02.flac"].map(|f| {
                json!({"album_id": a, "filename": f, "fingerprint": fp, "duration_s": 3.0,
                       "acoustid_recording_mbid": null})
            })
        })
        .collect();
    setup(
        &sb,
        conn,
        &json!({"albums": [album("a1"), album("a2")], "fingerprints": fps}),
    );
    let env = env(
        registry(None, &Default::default()),
        Arc::new(Counting::default()),
    );
    let run = run_check(
        conn,
        &env,
        "duplicate-tracks",
        &sb.lib_s,
        ScopeKind::All,
        None,
    )
    .unwrap();
    assert_eq!(run.status, "completed", "{:?}", run.error);
    assert_eq!(run.issue_count, 2, "one duplicate pair per album");
    let a2_before = of(conn, "duplicate-tracks", "a2");

    let ids = vec!["a1".to_string()];
    let run = run_check(
        conn,
        &env,
        "duplicate-tracks",
        &sb.lib_s,
        ScopeKind::Albums,
        Some(&ids),
    )
    .unwrap();
    assert_eq!(run.issue_count, 1, "only a1 is clustered");
    assert_eq!(
        of(conn, "duplicate-tracks", "a2"),
        a2_before,
        "a2 untouched"
    );
    assert_eq!(of(conn, "duplicate-tracks", "a1").len(), 1);
}
