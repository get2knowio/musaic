//! inconsistent-artist-sort beyond the Python app's record:
//! - an album-scoped run resolves a cluster finding stored on another
//!   member (the finding lives on the member with the smallest album id, and
//!   fixing a different member used to leave it open until a library-wide run);
//! - an album with ARTIST but no ALBUMARTIST is an album-level cluster member,
//!   credited with its ARTIST (`display_artist`), so its ALBUMARTISTSORT is
//!   compared with the cluster's.

mod common;

use std::sync::Arc;

use common::{RecordedResolver, Sandbox, env, registry, setup};
use musaic_checks::{ScopeKind, run_check};
use rusqlite::Connection;
use serde_json::{Value as J, json};

const CHECK: &str = "inconsistent-artist-sort";

fn album(id: &str, album_artist: Option<&str>, artist: &str, sort: Option<&str>) -> J {
    json!({
        "id": id,
        "library_path": "<LIB>",
        "path": format!("<LIB>/{id}"),
        "dir_name": id,
        "album_artist": album_artist,
        "artist": artist,
        "album_artist_sort": sort,
        "track_count": 1,
        "track_numbers_json": "[1]",
    })
}

fn world(albums: Vec<J>) -> (Sandbox, musaic_db::Db, musaic_checks::CheckEnv) {
    let (sb, db) = Sandbox::new();
    let case = json!({ "albums": albums });
    setup(&sb, db.connection(), &case);
    let log = Default::default();
    let env = env(
        registry(None, &log),
        Arc::new(RecordedResolver::from_case(&case)),
    );
    (sb, db, env)
}

fn run(conn: &Connection, sb: &Sandbox, env: &musaic_checks::CheckEnv, ids: Option<&[&str]>) {
    let ids: Option<Vec<String>> = ids.map(|i| i.iter().map(|s| s.to_string()).collect());
    let kind = if ids.is_some() {
        ScopeKind::Albums
    } else {
        ScopeKind::All
    };
    let r = run_check(conn, env, CHECK, &sb.lib_s, kind, ids.as_deref()).unwrap();
    assert_eq!(r.status, "completed", "{r:?}");
}

/// (album_id, summary, state) of every open finding of the check.
fn open(conn: &Connection) -> Vec<(String, String, String)> {
    musaic_db::findings::all_findings(conn)
        .unwrap()
        .into_iter()
        .filter(|f| f.check_id == CHECK && f.resolved_at.is_none())
        .map(|f| {
            let d: J = serde_json::from_str(f.detail_json.as_deref().unwrap()).unwrap();
            (
                f.album_id.unwrap(),
                f.summary,
                d["state"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// Fixing the member the finding is not stored on, then rerunning the check
/// for that album only (what a single-album apply triggers), resolves it.
#[test]
fn fixing_another_member_resolves_the_cluster_finding() {
    let (sb, db, env) = world(vec![
        album("a1", Some("The Kilo"), "The Kilo", Some("Kilo, The")),
        album("a2", Some("The Kilo"), "The Kilo", None),
    ]);
    let conn = db.connection();
    run(conn, &sb, &env, None);
    let found = open(conn);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].0, "a1", "stored on the smallest album id");
    assert_eq!(found[0].2, "propagatable");

    conn.execute(
        "UPDATE albums SET album_artist_sort = 'Kilo, The' WHERE id = 'a2'",
        [],
    )
    .unwrap();
    run(conn, &sb, &env, Some(&["a2"]));
    assert_eq!(open(conn), vec![], "the cluster is consistent now");
}

/// An album-scoped run still leaves a cluster that is still inconsistent
/// open, and does not touch an unrelated cluster's finding.
#[test]
fn an_album_scoped_run_keeps_what_is_still_inconsistent() {
    let (sb, db, env) = world(vec![
        album("a1", Some("The Kilo"), "The Kilo", Some("Kilo, The")),
        album("a2", Some("The Kilo"), "The Kilo", None),
        album("b1", Some("Lima"), "Lima", Some("Lima")),
        album("b2", Some("Lima"), "Lima", Some("Lima, X")),
    ]);
    let conn = db.connection();
    run(conn, &sb, &env, None);
    assert_eq!(open(conn).len(), 2);
    run(conn, &sb, &env, Some(&["a2"]));
    let left: Vec<String> = open(conn).into_iter().map(|f| f.0).collect();
    assert_eq!(left, ["a1", "b1"]);
}

/// An ARTIST-only album joins its artist's cluster at album level, so a
/// conflicting ALBUMARTISTSORT on it is reported (the Python app left it
/// out: a single-member cluster, no finding).
#[test]
fn an_artist_only_album_is_an_album_level_member() {
    let (sb, db, env) = world(vec![
        album("x1", None, "Harold Mansell", Some("Mansell")),
        album(
            "x2",
            Some("Harold Mansell"),
            "Harold Mansell",
            Some("Mansell, Harold"),
        ),
    ]);
    let conn = db.connection();
    run(conn, &sb, &env, None);
    let f = musaic_db::findings::all_findings(conn).unwrap();
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0].album_id.as_deref(), Some("x1"));
    assert_eq!(f[0].summary, "2 credit instances for 'Harold Mansell'");
    let d: J = serde_json::from_str(f[0].detail_json.as_deref().unwrap()).unwrap();
    assert_eq!(d["state"], "conflicting");
    assert_eq!(
        d["members"][0],
        json!({"kind": "album", "album_id": "x1", "dir_name": "x1",
               "album_artist": "Harold Mansell", "sort": "Mansell"})
    );
}

/// A multi-artist album or a compilation without ALBUMARTIST is not credited
/// with its sample track's ARTIST: a cluster apply would otherwise write
/// that artist's sort as ALBUMARTISTSORT on the whole album.
#[test]
fn a_various_artists_album_without_albumartist_is_not_a_member() {
    let mut split = album("x1", None, "Harold Mansell", Some("Zzz"));
    split["track_artists_json"] = json!(
        r#"[{"filename": "01.flac", "artist": "Harold Mansell"}, {"filename": "02.flac", "artist": "Somebody Else"}]"#
    );
    let mut comp = album("x2", None, "Harold Mansell", Some("Yyy"));
    comp["is_compilation"] = json!(1);
    let (sb, db, env) = world(vec![
        split,
        comp,
        album(
            "x3",
            Some("Harold Mansell"),
            "Harold Mansell",
            Some("Mansell, Harold"),
        ),
    ]);
    let conn = db.connection();
    run(conn, &sb, &env, None);
    // Only x1's own track credits join (as track-level members, sort "").
    let f = musaic_db::findings::all_findings(conn).unwrap();
    assert_eq!(f.len(), 1, "{f:?}");
    let d: J = serde_json::from_str(f[0].detail_json.as_deref().unwrap()).unwrap();
    let kinds: Vec<(&str, &str)> = d["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| (m["kind"].as_str().unwrap(), m["album_id"].as_str().unwrap()))
        .collect();
    assert_eq!(kinds, [("track", "x1"), ("album", "x3")]);
}

/// An ARTIST-only album whose tracks carry its artist and its sort is one
/// member, not two: no finding on its own.
#[test]
fn an_artist_only_album_alone_is_not_a_cluster() {
    let mut a = album("x1", None, "Harold Mansell", Some("Mansell, Harold"));
    a["track_artists_json"] = json!(
        r#"[{"filename": "01.flac", "artist": "Harold Mansell", "artist_sort": "Mansell, Harold"}]"#
    );
    let mut b = album("y1", None, "Other", None);
    b["track_artists_json"] = json!(r#"[{"filename": "01.flac", "artist": "Other"}]"#);
    let (sb, db, env) = world(vec![a, b]);
    let conn = db.connection();
    run(conn, &sb, &env, None);
    assert_eq!(open(conn), vec![]);
}
