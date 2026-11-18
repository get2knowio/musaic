//! multi-disc-layout through a real scan of generated FLACs in temp folders:
//! each finding kind, each case that must not be reported, unread disc
//! numbers as unknown, Missing members, MusicBrainz anchors, stability
//! across runs, resolution when the set changes, and ignores.

mod common;

use std::path::Path;
use std::sync::Arc;

use common::{Sandbox, env, flac_bytes};
use musaic_checks::{NoResolver, Registry, ScopeKind, run_check};
use musaic_db::findings::{FindingRow, all_findings};
use musaic_scan::TrackOrder;
use rusqlite::Connection;
use serde_json::Value as J;

const CHECK: &str = "multi-disc-layout";

/// One track: (file name, [(tag, value)]).
type Track = (String, Vec<(&'static str, String)>);

fn track(name: &str, n: i64, disc: Option<i64>, title: &str) -> Track {
    let mut tags = vec![
        ("ARTIST", "Band".to_string()),
        ("ALBUMARTIST", "Band".to_string()),
        ("ALBUM", "Record".to_string()),
        ("TITLE", title.to_string()),
        ("TRACKNUMBER", n.to_string()),
        ("DATE", "1999".to_string()),
    ];
    if let Some(d) = disc {
        tags.push(("DISCNUMBER", d.to_string()));
    }
    (name.to_string(), tags)
}

fn write_album(dir: &Path, tracks: &[Track]) {
    std::fs::create_dir_all(dir).unwrap();
    for (name, tags) in tracks {
        let tags: Vec<(String, String)> = tags
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        std::fs::write(dir.join(name), flac_bytes(&tags, 1)).unwrap();
    }
}

/// `n` tracks of one disc folder, tagged with the disc or not.
fn disc_folder(lib: &Path, name: &str, disc: i64, n: i64, tag_disc: bool) {
    let tracks: Vec<Track> = (1..=n)
        .map(|k| {
            track(
                &format!("{k:02} - Song {disc}-{k}.flac"),
                k,
                tag_disc.then_some(disc),
                &format!("Song {disc}-{k}"),
            )
        })
        .collect();
    write_album(&lib.join(name), &tracks);
}

struct World {
    sb: Sandbox,
    conn: Connection,
}

impl World {
    fn new() -> World {
        let (sb, _db) = Sandbox::new();
        std::fs::create_dir_all(&sb.lib).unwrap();
        let conn = sb.open();
        World { sb, conn }
    }

    fn scan(&mut self) {
        let albums = musaic_scan::scan_library(&self.sb.lib, TrackOrder::DiscTrack);
        let now = "2026-01-01T00:00:00Z";
        musaic_db::albums::save_snapshot(&mut self.conn, &self.sb.lib_s, now, now, &albums)
            .unwrap();
    }

    fn run(&self) -> musaic_db::findings::RunRow {
        let env = env(Registry::default(), Arc::new(NoResolver));
        run_check(
            &self.conn,
            &env,
            CHECK,
            &self.sb.lib_s,
            ScopeKind::All,
            None,
        )
        .unwrap()
    }

    fn open(&self) -> Vec<FindingRow> {
        all_findings(&self.conn)
            .unwrap()
            .into_iter()
            .filter(|f| f.check_id == CHECK && f.resolved_at.is_none())
            .collect()
    }

    fn id_of(&self, dir: &str) -> String {
        self.conn
            .query_row("SELECT id FROM albums WHERE dir_name = ?1", [dir], |r| {
                r.get(0)
            })
            .unwrap()
    }

    fn library_inputs_recorded(&self) -> bool {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM check_inputs WHERE check_id = ?1",
                [CHECK],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
            > 0
    }
}

fn detail(f: &FindingRow) -> J {
    serde_json::from_str(f.detail_json.as_deref().unwrap()).unwrap()
}

#[test]
fn one_folder_holding_two_discs() {
    let mut w = World::new();
    write_album(
        &w.sb.lib.join("Band - Record [FLAC]"),
        &[
            track("1-01 A.flac", 1, Some(1), "A"),
            track("1-02 B.flac", 2, Some(1), "B"),
            track("2-01 C.flac", 1, Some(2), "C"),
            track("2-02 D.flac", 2, Some(2), "D"),
        ],
    );
    w.scan();
    let run = w.run();
    assert_eq!(run.status, "completed");
    let open = w.open();
    assert_eq!(open.len(), 1, "{open:?}");
    let f = &open[0];
    assert_eq!(
        f.album_id.as_deref(),
        Some(w.id_of("Band - Record [FLAC]").as_str())
    );
    assert_eq!(
        f.summary,
        "'Band - Record [FLAC]' holds 2 discs in one folder"
    );
    assert_eq!(f.severity, "info");
    let d = detail(f);
    assert_eq!(d["kind"], "one_folder");
    assert_eq!(d["numbering"], "per_disc");
    assert!(w.library_inputs_recorded());
}

#[test]
fn one_folder_numbered_through_but_with_disc_tags() {
    let mut w = World::new();
    write_album(
        &w.sb.lib.join("Band - Record [FLAC]"),
        &[
            track("01 A.flac", 1, Some(1), "A"),
            track("02 B.flac", 2, Some(1), "B"),
            track("03 C.flac", 3, Some(2), "C"),
        ],
    );
    w.scan();
    w.run();
    let open = w.open();
    assert_eq!(open.len(), 1);
    assert_eq!(
        open[0].summary,
        "'Band - Record [FLAC]' is numbered straight through its 2 discs but still has disc tags"
    );
    assert_eq!(detail(&open[0])["numbering"], "continuous");
}

#[test]
fn three_disc_folders_are_one_set_on_disc_one() {
    let mut w = World::new();
    for d in 1..=3 {
        disc_folder(
            &w.sb.lib,
            &format!("Band - Record (disc {d} of 3) [FLAC]"),
            d,
            2,
            d == 2,
        );
    }
    w.scan();
    w.run();
    let open = w.open();
    assert_eq!(open.len(), 1, "{open:?}");
    let f = &open[0];
    let d1 = w.id_of("Band - Record (disc 1 of 3) [FLAC]");
    assert_eq!(f.album_id.as_deref(), Some(d1.as_str()));
    assert_eq!(
        f.summary,
        "'Band - Record (disc 1 of 3) [FLAC]' is disc 1 of a set kept in 3 folders"
    );
    let members: Vec<(String, i64)> = detail(f)["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["album_id"].as_str().unwrap().to_string(),
                m["disc"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        members,
        vec![
            (d1, 1),
            (w.id_of("Band - Record (disc 2 of 3) [FLAC]"), 2),
            (w.id_of("Band - Record (disc 3 of 3) [FLAC]"), 3),
        ]
    );
}

#[test]
fn an_incomplete_set_is_info_and_says_which_discs() {
    let mut w = World::new();
    disc_folder(&w.sb.lib, "Band - Record (disc 1 of 3) [FLAC]", 1, 2, false);
    disc_folder(&w.sb.lib, "Band - Record (disc 3 of 3) [FLAC]", 3, 2, false);
    w.scan();
    w.run();
    let open = w.open();
    assert_eq!(open.len(), 1);
    assert_eq!(
        open[0].summary,
        "'Band - Record (disc 1 of 3) [FLAC]': only discs 1, 3 of 3 found, disc 2 not"
    );
    assert_eq!(open[0].severity, "info");
    assert_eq!(detail(&open[0])["absent"], serde_json::json!([2]));
}

#[test]
fn cases_that_are_not_reported() {
    let mut w = World::new();
    // A single-disc album.
    disc_folder(&w.sb.lib, "Band - Plain [FLAC]", 1, 3, false);
    // Already in the target layout: one folder, numbered through, no tags.
    write_album(
        &w.sb.lib.join("Band - Done [FLAC]"),
        &[
            track("01 A.flac", 1, None, "A"),
            track("02 B.flac", 2, None, "B"),
        ],
    );
    // Repeated track numbers with no disc evidence: track-gaps' business.
    write_album(
        &w.sb.lib.join("Band - Repeats [FLAC]"),
        &[track("a.flac", 1, None, "A"), track("b.flac", 1, None, "B")],
    );
    // Folders in different parents.
    disc_folder(
        &w.sb.lib.join("x"),
        "Band - Split (disc 1 of 2) [FLAC]",
        1,
        2,
        false,
    );
    disc_folder(
        &w.sb.lib.join("y"),
        "Band - Split (disc 2 of 2) [FLAC]",
        2,
        2,
        false,
    );
    // Same tags, no disc marker in the folder names.
    disc_folder(&w.sb.lib, "Band - Twice [FLAC]", 1, 2, false);
    disc_folder(&w.sb.lib, "Band - Twice (bonus) [FLAC]", 1, 2, false);
    // A lone disc 1 of 1.
    disc_folder(&w.sb.lib, "Band - Single (disc 1 of 1) [FLAC]", 1, 2, false);
    w.scan();
    let run = w.run();
    assert_eq!(run.status, "completed");
    assert_eq!(w.open(), Vec::<FindingRow>::new());
    assert!(w.library_inputs_recorded());
}

#[test]
fn unread_disc_numbers_are_unknown_never_no_tags() {
    let mut w = World::new();
    let dir = "Band - Record [FLAC]";
    write_album(
        &w.sb.lib.join(dir),
        &[
            track("1-01 A.flac", 1, Some(1), "A"),
            track("2-01 B.flac", 1, Some(2), "B"),
        ],
    );
    w.scan();
    w.run();
    assert_eq!(w.open().len(), 1);
    // A row from before the per-file disc numbers were stored.
    w.conn
        .execute("UPDATE albums SET track_discs_json = NULL", [])
        .unwrap();
    w.conn.execute("DELETE FROM check_inputs", []).unwrap();
    let run = w.run();
    assert_eq!(run.status, "completed");
    // Not judged: the finding stays, and the run is not recorded as
    // having seen the library, so it runs again once a scan reads them.
    assert_eq!(w.open().len(), 1);
    assert!(!w.library_inputs_recorded());

    // Without an earlier finding, nothing is reported either.
    let mut w = World::new();
    write_album(
        &w.sb.lib.join(dir),
        &[
            track("1-01 A.flac", 1, Some(1), "A"),
            track("2-01 B.flac", 1, Some(2), "B"),
        ],
    );
    w.scan();
    w.conn
        .execute("UPDATE albums SET track_discs_json = NULL", [])
        .unwrap();
    w.run();
    assert!(w.open().is_empty());
}

#[test]
fn a_missing_member_keeps_the_finding_unjudged() {
    let mut w = World::new();
    disc_folder(&w.sb.lib, "Band - Record (disc 1 of 2) [FLAC]", 1, 2, false);
    disc_folder(&w.sb.lib, "Band - Record (disc 2 of 2) [FLAC]", 2, 2, false);
    w.scan();
    w.run();
    let before = w.open();
    assert_eq!(before.len(), 1);
    // Disc 2's folder goes away; the scan marks it Missing.
    std::fs::remove_dir_all(w.sb.lib.join("Band - Record (disc 2 of 2) [FLAC]")).unwrap();
    w.conn
        .execute(
            "UPDATE albums SET missing_since = '2026-01-02T00:00:00Z' WHERE dir_name LIKE '%disc 2 of 2%'",
            [],
        )
        .unwrap();
    w.run();
    // Not reported as incomplete; the set's finding is left as it was.
    assert_eq!(w.open(), before);
}

#[test]
fn anchors_one_release_group_or_none() {
    let mut w = World::new();
    disc_folder(&w.sb.lib, "Band - Record (disc 1 of 2) [FLAC]", 1, 2, false);
    disc_folder(&w.sb.lib, "Band - Record (disc 2 of 2) [FLAC]", 2, 2, false);
    w.scan();
    let set_rg = |w: &World, disc: i64, rg: &str| {
        w.conn
            .execute(
                "UPDATE albums SET mb_release_group_mbid = ?1 WHERE disc_number = ?2",
                rusqlite::params![rg, disc],
            )
            .unwrap();
    };
    // Partly anchored: still one set.
    set_rg(&w, 1, "rg-one");
    w.run();
    assert_eq!(w.open().len(), 1);
    // Both anchored to one release group: one set.
    set_rg(&w, 2, "rg-one");
    w.run();
    assert_eq!(w.open().len(), 1);
    // Two release groups: not one set; the finding resolves.
    set_rg(&w, 2, "rg-two");
    w.run();
    assert!(w.open().is_empty());
}

#[test]
fn stable_across_runs_and_resolved_when_the_set_changes() {
    let mut w = World::new();
    disc_folder(&w.sb.lib, "Band - Record (disc 1 of 2) [FLAC]", 1, 2, false);
    disc_folder(&w.sb.lib, "Band - Record (disc 2 of 2) [FLAC]", 2, 2, false);
    w.scan();
    w.run();
    let first = w.open();
    w.run();
    let second = w.open();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].id, second[0].id);
    assert_eq!(first[0].first_seen_at, second[0].first_seen_at);
    // Disc 2 now names another album artist: not one set any more.
    w.conn
        .execute(
            "UPDATE albums SET album_artist = 'Someone Else' WHERE disc_number = 2",
            [],
        )
        .unwrap();
    w.run();
    assert!(w.open().is_empty());
}

#[test]
fn an_ignore_survives_reruns_and_follows_a_changed_set() {
    let mut w = World::new();
    for d in 1..=3 {
        disc_folder(
            &w.sb.lib,
            &format!("Band - Record (disc {d} of 3) [FLAC]"),
            d,
            2,
            false,
        );
    }
    w.scan();
    w.run();
    let f = w.open().remove(0);
    musaic_db::findings::ignore_finding(&w.conn, f.id, Some("later"), "2026-01-01T00:00:01Z")
        .unwrap();
    w.run();
    let again = w.open();
    assert_eq!(again.len(), 1);
    assert_eq!(again[0].id, f.id);
    assert!(again[0].ignored_at.is_some());

    // The user removes disc 3 from the library: the set is now incomplete,
    // a new finding on the same album, and the ignore follows it.
    let d3 = w.id_of("Band - Record (disc 3 of 3) [FLAC]");
    musaic_db::albums::delete_album(&w.conn, &d3).unwrap();
    w.run();
    let now = w.open();
    assert_eq!(now.len(), 1, "{now:?}");
    assert_ne!(now[0].id, f.id);
    assert!(
        now[0].summary.contains("only discs 1, 2 of 3 found"),
        "{}",
        now[0].summary
    );
    assert!(now[0].ignored_at.is_some(), "the ignore was not carried");
}
