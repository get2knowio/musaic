//! A renumber re-applies the configured filename pattern to the file's
//! numeric prefix: with the disc when the pattern has one and the set has
//! more than one disc, with the track alone otherwise. Real FLAC files, a
//! real scan, then the apply pipeline.

mod common;

use std::ops::ControlFlow;
use std::path::Path;
use std::sync::Arc;

use common::{connect, flac_bytes, fresh_db, recording_bus};
use musaic_events::Bus;
use musaic_scan::TrackOrder;
use musaic_services::apply::{ApplyEnv, ApplyRequest, CoverSource, apply_album_changes};
use musaic_services::scan::{ScanDeps, ScanService, StartScan};
use rusqlite::Connection;

struct NoCovers;

impl CoverSource for NoCovers {
    fn download(&self, _url: &str) -> Option<(Vec<u8>, String)> {
        None
    }
}

const SINGLE_KEY: &str = "filename_template_single_artist";
const DISC_TRACK: &str = "{disc}-{tracknumber:02} - {title}";

/// One file: (name, DISCNUMBER or None, TRACKNUMBER).
type F = (&'static str, Option<&'static str>, &'static str);

struct World {
    _td: tempfile::TempDir,
    album_dir: std::path::PathBuf,
    album_id: String,
    lib: String,
    conn: Connection,
}

fn world(dir: &str, files: &[F], extra: &[(&str, &str)]) -> World {
    let td = tempfile::tempdir().unwrap();
    let root = musaic_scan::py_resolve(td.path());
    let lib = root.join("lib");
    let album_dir = lib.join(dir);
    std::fs::create_dir_all(&album_dir).unwrap();
    for (name, disc, track) in files {
        let mut tags: Vec<(String, String)> = vec![
            ("ARTIST".into(), "Test Artist".into()),
            ("ALBUMARTIST".into(), "Test Artist".into()),
            ("ALBUM".into(), "Box".into()),
            ("TITLE".into(), "Song".into()),
            ("TRACKNUMBER".into(), (*track).into()),
        ];
        if let Some(disc) = disc {
            tags.push(("DISCNUMBER".into(), (*disc).into()));
        }
        for (k, v) in extra {
            tags.push(((*k).into(), (*v).into()));
        }
        std::fs::write(album_dir.join(name), flac_bytes(&tags, 1, 0)).unwrap();
    }
    let db = fresh_db(&root);
    let (bus, _events) = recording_bus();
    let opener_db = db.clone();
    let deps = ScanDeps {
        open_db: Arc::new(move || Ok(connect(&opener_db))),
        bus,
        clock: Arc::new(|| "2030-01-01T00:00:00Z".to_string()),
        track_order: TrackOrder::DiscTrack,
        after_scan: None,
        on_album: None,
        on_worker_done: None,
        log: None,
    };
    let lib_s = lib.to_string_lossy().into_owned();
    let svc = ScanService::new();
    let StartScan::Started(_, job) = svc.start_scan(&lib_s, false, deps) else {
        panic!("scan already running")
    };
    job.run();
    assert_eq!(svc.state().status.as_str(), "done");
    let conn = connect(&db);
    let album_id: String = conn
        .query_row("SELECT id FROM albums", [], |r| r.get(0))
        .unwrap();
    World {
        _td: td,
        album_dir,
        album_id,
        lib: lib_s,
        conn,
    }
}

impl World {
    fn set_template(&self, value: &str) {
        musaic_db::settings::set_raw(&self.conn, SINGLE_KEY, value).unwrap();
    }

    /// Set every file's track number to `to` (a name -> number list).
    fn renumber(&mut self, to: &[(&str, &str)]) {
        let req = ApplyRequest {
            album_id: self.album_id.clone(),
            album_path: self.album_dir.to_string_lossy().into_owned(),
            library_dir: Some(self.lib.clone()),
            track_suggestions: to
                .iter()
                .map(|(f, n)| {
                    (
                        f.to_string(),
                        vec![("tracknumber".to_string(), Some(n.to_string()))],
                    )
                })
                .collect(),
            ..ApplyRequest::default()
        };
        let bus = Bus::new();
        let now = || "2030-01-02T00:00:00Z".to_string();
        let mut env = ApplyEnv {
            conn: &mut self.conn,
            bus: &bus,
            covers: &NoCovers,
            now: &now,
            track_order: TrackOrder::DiscTrack,
        };
        let mut errors = Vec::new();
        apply_album_changes(&mut env, &req, &mut |it| {
            if it.event == "error" {
                errors.push(it.data);
            }
            ControlFlow::Continue(())
        });
        assert!(errors.is_empty());
    }

    fn names(&self) -> Vec<String> {
        names(&self.album_dir)
    }
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".flac"))
        .collect();
    v.sort();
    v
}

const TWO_DISCS: &[F] = &[
    ("1-05 - Song.flac", Some("1"), "5"),
    ("2-05 - Song.flac", Some("2"), "5"),
];

#[test]
fn the_disc_track_pattern_keeps_each_files_disc_in_a_multi_disc_folder() {
    let mut w = world("Test Artist - Box [FLAC]", TWO_DISCS, &[]);
    w.set_template(DISC_TRACK);
    w.renumber(&[("1-05 - Song.flac", "6"), ("2-05 - Song.flac", "6")]);
    assert_eq!(w.names(), ["1-06 - Song.flac", "2-06 - Song.flac"]);
}

#[test]
fn the_disc_total_tag_makes_a_set_multi_disc_before_the_other_discs_are_here() {
    let mut w = world(
        "Test Artist - Box [FLAC]",
        &[("1-05 - Song.flac", Some("1"), "5")],
        &[("DISCTOTAL", "2")],
    );
    w.set_template(DISC_TRACK);
    w.renumber(&[("1-05 - Song.flac", "6")]);
    assert_eq!(w.names(), ["1-06 - Song.flac"]);
}

#[test]
fn the_disc_track_pattern_renumbers_with_the_track_alone_on_a_single_disc_album() {
    let mut w = world(
        "Test Artist - Box [FLAC]",
        &[("1-05 - Song.flac", Some("1"), "5")],
        &[],
    );
    w.set_template(DISC_TRACK);
    w.renumber(&[("1-05 - Song.flac", "6")]);
    assert_eq!(w.names(), ["06 - Song.flac"]);
}

#[test]
fn a_track_only_pattern_renumbers_a_multi_disc_folder_with_the_track_alone() {
    let mut w = world(
        "Test Artist - Box [FLAC]",
        &[
            ("1-05 - Song.flac", Some("1"), "5"),
            ("2-07 - Song.flac", Some("2"), "7"),
        ],
        &[],
    );
    // The stored default pattern is track only.
    w.renumber(&[("1-05 - Song.flac", "6"), ("2-07 - Song.flac", "8")]);
    assert_eq!(w.names(), ["06 - Song.flac", "08 - Song.flac"]);
}

#[test]
fn the_folders_disc_number_stands_in_for_a_file_that_names_none() {
    let mut w = world(
        "Test Artist - Box (disc 2 of 3) [FLAC]",
        &[("05 - Song.flac", None, "5")],
        &[],
    );
    w.set_template(DISC_TRACK);
    w.renumber(&[("05 - Song.flac", "6")]);
    assert_eq!(w.names(), ["2-06 - Song.flac"]);
}

#[test]
fn a_pattern_whose_number_does_not_come_first_renames_nothing() {
    let mut w = world(
        "Test Artist - Box [FLAC]",
        &[("05 - Song.flac", None, "5")],
        &[],
    );
    w.set_template("{title} - {tracknumber:02}");
    w.renumber(&[("05 - Song.flac", "6")]);
    assert_eq!(w.names(), ["05 - Song.flac"]);
}
