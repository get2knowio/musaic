//! The track-gaps check on albums kept in one folder with several discs,
//! end to end: real FLAC files, a real scan, then the check over what the
//! scan stored. Each disc is numbered from 1, so evaluating the numbers of
//! the whole folder together reads disc 2's track 1 as a repeat of disc 1's.
//!
//! Gaps and repeats are judged per disc; an album with no disc information
//! behaves as it always did.

mod common;

use std::path::Path;
use std::sync::Arc;

use common::{connect, flac_bytes, fresh_db, recording_bus};
use musaic_checks::base::SystemClock;
use musaic_checks::{CheckEnv, Registry, Resolution, ResolutionSource, ScopeKind, run_check};
use musaic_db::findings::all_findings;
use musaic_scan::TrackOrder;
use musaic_services::scan::{ScanDeps, ScanService, StartScan};
use rusqlite::Connection;

struct NoResolver;

impl ResolutionSource for NoResolver {
    fn resolve(&self, _conn: &Connection, _album_id: &str) -> Result<Resolution, String> {
        Ok(Resolution::default())
    }
}

/// One track: (DISCNUMBER or None, TRACKNUMBER or None).
type T = (Option<&'static str>, Option<&'static str>);

fn album(lib: &Path, dir: &str, tracks: &[T]) {
    let d = lib.join(format!("Test Artist - {dir} [FLAC]"));
    std::fs::create_dir_all(&d).unwrap();
    for (i, (disc, track)) in tracks.iter().enumerate() {
        let mut tags: Vec<(String, String)> = vec![
            ("ARTIST".into(), "Test Artist".into()),
            ("ALBUMARTIST".into(), "Test Artist".into()),
            ("ALBUM".into(), dir.into()),
            ("TITLE".into(), format!("Song {i}")),
            ("DATE".into(), "2020".into()),
        ];
        if let Some(disc) = disc {
            tags.push(("DISCNUMBER".into(), (*disc).into()));
        }
        if let Some(track) = track {
            tags.push(("TRACKNUMBER".into(), (*track).into()));
        }
        std::fs::write(d.join(format!("{i:02}.flac")), flac_bytes(&tags, 1, 0)).unwrap();
    }
}

fn d(disc: &'static str, track: &'static str) -> T {
    (Some(disc), Some(track))
}

fn n(track: &'static str) -> T {
    (None, Some(track))
}

struct World {
    _td: tempfile::TempDir,
    lib: String,
    conn: Connection,
}

fn world(albums: &[(&str, Vec<T>)]) -> World {
    let td = tempfile::tempdir().unwrap();
    let root = musaic_scan::py_resolve(td.path());
    let lib = root.join("lib");
    std::fs::create_dir(&lib).unwrap();
    for (name, tracks) in albums {
        album(&lib, name, tracks);
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
    World {
        _td: td,
        lib: lib_s,
        conn: connect(&db),
    }
}

impl World {
    fn run(&self) {
        let env = CheckEnv {
            registry: Registry::default(),
            resolver: Arc::new(NoResolver),
            clock: Arc::new(SystemClock),
            notify: Arc::new(|| {}),
            publish: Arc::new(|_| {}),
        };
        let run = run_check(
            &self.conn,
            &env,
            "track-gaps",
            &self.lib,
            ScopeKind::All,
            None,
        )
        .unwrap();
        assert_eq!(run.status, "completed", "{:?}", run.error);
    }

    /// Open findings of the check for the album named `dir`, as
    /// `severity: summary`, sorted.
    fn findings(&self, dir: &str) -> Vec<String> {
        let id: String = self
            .conn
            .query_row(
                "SELECT id FROM albums WHERE dir_name = ?1",
                [format!("Test Artist - {dir} [FLAC]")],
                |r| r.get(0),
            )
            .unwrap();
        let mut v: Vec<String> = all_findings(&self.conn)
            .unwrap()
            .into_iter()
            .filter(|f| {
                f.check_id == "track-gaps"
                    && f.album_id.as_deref() == Some(id.as_str())
                    && f.resolved_at.is_none()
            })
            .map(|f| format!("{}: {}", f.severity, f.summary))
            .collect();
        v.sort();
        v
    }
}

fn none() -> Vec<String> {
    Vec::new()
}

#[test]
fn discs_numbered_from_one_are_not_repeats() {
    let w = world(&[
        (
            "Boxed Set",
            vec![
                d("1", "1"),
                d("1", "2"),
                d("1", "3"),
                d("2", "1"),
                d("2", "2"),
                d("2", "3"),
            ],
        ),
        (
            "Fractions",
            vec![
                d("1/2", "1/3"),
                d("1/2", "2/3"),
                d("1/2", "3/3"),
                d("2/2", "1/3"),
                d("2/2", "2/3"),
                d("2/2", "3/3"),
            ],
        ),
        (
            "Prefixed",
            vec![n("1-1"), n("1-2"), n("1-3"), n("2-1"), n("2-2"), n("2-3")],
        ),
        (
            "Continuous",
            vec![
                d("1", "1"),
                d("1", "2"),
                d("1", "3"),
                d("2", "4"),
                d("2", "5"),
                d("2", "6"),
            ],
        ),
        (
            "Vinyl",
            vec![d("1", "A1"), d("1", "A2"), d("1", "B1"), d("1", "B2")],
        ),
    ]);
    w.run();
    for dir in ["Boxed Set", "Fractions", "Prefixed", "Continuous", "Vinyl"] {
        assert_eq!(w.findings(dir), none(), "{dir}");
    }
}

#[test]
fn a_real_repeat_or_gap_on_one_disc_is_still_reported() {
    let w = world(&[
        (
            "Disc Two Repeat",
            vec![
                d("1", "1"),
                d("1", "2"),
                d("1", "3"),
                d("2", "1"),
                d("2", "2"),
                d("2", "2"),
            ],
        ),
        (
            "Disc Two Gap",
            vec![
                d("1", "1"),
                d("1", "2"),
                d("1", "3"),
                d("2", "1"),
                d("2", "3"),
            ],
        ),
    ]);
    w.run();
    assert_eq!(
        w.findings("Disc Two Repeat"),
        ["error: Duplicate track numbers on disc 2: 2"]
    );
    assert_eq!(
        w.findings("Disc Two Gap"),
        ["warn: Track-number gaps on disc 2: missing 2"]
    );
}

#[test]
fn albums_without_disc_information_behave_as_before() {
    let w = world(&[
        ("Plain Repeat", vec![n("1"), n("2"), n("2")]),
        ("Plain Gap", vec![n("1"), n("2"), n("4")]),
        ("Clean", vec![n("1"), n("2"), n("3")]),
        // One disc, whose number is not 1: still one disc.
        ("Only Disc Two", vec![d("2", "1"), d("2", "3")]),
        (
            "Disc Tag On Some Files",
            vec![d("1", "1"), d("1", "2"), n("2"), n("3")],
        ),
    ]);
    w.run();
    assert_eq!(
        w.findings("Plain Repeat"),
        ["error: Duplicate track numbers: 2"]
    );
    assert_eq!(
        w.findings("Plain Gap"),
        ["warn: Track-number gaps: missing 3"]
    );
    assert_eq!(w.findings("Clean"), none());
    assert_eq!(
        w.findings("Only Disc Two"),
        ["warn: Track-number gaps: missing 2"]
    );
    assert_eq!(
        w.findings("Disc Tag On Some Files"),
        ["error: Duplicate track numbers: 2"]
    );
}

/// A stale `error` finding from before disc numbers were considered closes
/// on the next run, because the runner resolves findings a run no longer
/// emits.
#[test]
fn a_stale_finding_is_resolved_by_the_next_run() {
    let w = world(&[(
        "Boxed Set",
        vec![d("1", "1"), d("1", "2"), d("2", "1"), d("2", "2")],
    )]);
    let id: String = w
        .conn
        .query_row("SELECT id FROM albums", [], |r| r.get(0))
        .unwrap();
    musaic_db::findings::upsert_finding(
        &w.conn,
        "track-gaps",
        Some(&id),
        "error",
        "Duplicate track numbers: 1, 2",
        None,
        "2030-01-01T00:00:00Z",
    )
    .unwrap();
    assert_eq!(w.findings("Boxed Set").len(), 1);
    w.run();
    assert_eq!(w.findings("Boxed Set"), none());
}

/// The renumbering fix leaves a multi-disc album alone: the MusicBrainz
/// tracklist runs straight through the discs.
#[test]
fn the_renumbering_fix_skips_albums_spanning_discs() {
    use musaic_checks::checks::track_gaps::spans_multiple_discs;
    let w = world(&[
        (
            "Boxed Set",
            vec![d("1", "1"), d("1", "2"), d("2", "1"), d("2", "2")],
        ),
        ("Plain Gap", vec![n("1"), n("2"), n("4")]),
        ("Only Disc Two", vec![d("2", "1"), d("2", "3")]),
    ]);
    let dir =
        |name: &str| std::path::Path::new(&w.lib).join(format!("Test Artist - {name} [FLAC]"));
    assert!(spans_multiple_discs(&dir("Boxed Set")));
    assert!(!spans_multiple_discs(&dir("Plain Gap")));
    assert!(!spans_multiple_discs(&dir("Only Disc Two")));
}

/// A file that cannot be read means the album is not evaluated: its open
/// finding stays rather than being resolved or re-derived from guesswork.
#[test]
fn an_unreadable_file_leaves_the_albums_findings_alone() {
    let w = world(&[(
        "Boxed Set",
        vec![d("1", "1"), d("1", "2"), d("2", "1"), d("2", "2")],
    )]);
    let id: String = w
        .conn
        .query_row("SELECT id FROM albums", [], |r| r.get(0))
        .unwrap();
    musaic_db::findings::upsert_finding(
        &w.conn,
        "track-gaps",
        Some(&id),
        "error",
        "Duplicate track numbers: 1, 2",
        None,
        "2030-01-01T00:00:00Z",
    )
    .unwrap();
    let dir = std::path::Path::new(&w.lib).join("Test Artist - Boxed Set [FLAC]");
    std::fs::write(dir.join("03.flac"), b"not a flac").unwrap();
    w.run();
    assert_eq!(
        w.findings("Boxed Set"),
        ["error: Duplicate track numbers: 1, 2"]
    );
}
