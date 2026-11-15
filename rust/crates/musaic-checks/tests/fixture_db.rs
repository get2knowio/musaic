//! The seven checks over the database the Python app wrote after
//! scanning the synthetic fixture library and running its checks
//! (testdata/db/v16-synthetic-offline.db, captures/api-offline), with
//! a fresh copy of testdata/synthetic-library/library standing in for the library it scanned.
//!
//! The Rust checks run on top of the Python app's findings, so the upsert must
//! find every Python app row by (check_id, album_id, summary) and leave the same
//! open set, detail bytes included, except for the deliberate differences
//! enumerated in `ALLOWED`. Album rows are the Python app's (no rescan), so
//! the scanner's track order does not matter. Offline: no album is anchored, so
//! the MusicBrainz rules change nothing here; the capture used a dummy AcoustID
//! key, so the Python app had fingerprints and the lookup-outcome columns change
//! nothing either.

mod common;

use std::path::Path;
use std::sync::Arc;

use common::{env, registry};
use musaic_checks::{NoResolver, ScopeKind, run_check};
use musaic_db::Db;
use musaic_db::findings::{FindingFilter, FindingRow, list_findings};

/// (check_id, album_id, why): the Python app's findings the Rust checks must NOT keep.
const ALLOWED: &[(&str, &str, &str)] = &[
    (
        "filename-mismatch",
        "76386d46b26c9798", // Café Orchestra - Été [FLAC] (NFD directory name)
        "deliberate change: the proposed dir rename differs only by NFC/NFD form",
    ),
    (
        "inconsistent-artist-sort",
        "0c97baca60f74928", // Mono Harbor - Gaps [FLAC], the cluster's smallest id
        "deliberate change: the ARTIST-only Mono Harbor - Gaps joins the cluster at \
         album level, so its summary (part of the row's identity) changes: ADDED",
    ),
];

/// (check_id, album_id, summary, why): rows the Rust checks add.
const ADDED: &[(&str, &str, &str, &str)] = &[(
    "inconsistent-artist-sort",
    "0c97baca60f74928", // Mono Harbor - Gaps [FLAC]
    "4 album-level + 1 track-level (4 tracks) for 'Mono Harbor'",
    "deliberate change: an ARTIST-only album is an album-level member \
     (was '3 album-level + 1 track-level (4 tracks)')",
)];

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap().flatten() {
        let to = dst.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &to);
        } else {
            std::fs::copy(e.path(), &to).unwrap();
        }
    }
}

fn open_set(conn: &rusqlite::Connection) -> Vec<FindingRow> {
    let mut v = list_findings(conn, &FindingFilter::default()).unwrap();
    v.sort_by_key(|f| f.id);
    v
}

#[test]
fn checks_on_the_fixture_library_match_the_python() {
    let src_lib = musaic_testdata::synthetic_library();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let db_path = root.join("library.db");
    std::fs::copy(
        musaic_testdata::public("db/v16-synthetic-offline.db"),
        &db_path,
    )
    .unwrap();
    let lib = root.join("library");
    copy_dir(&src_lib, &lib);
    let lib_s = lib.to_string_lossy().into_owned();

    let (db, _) = Db::open(&db_path).unwrap();
    let conn = db.connection();
    let old: String = conn
        .query_row("SELECT path FROM libraries", [], |r| r.get(0))
        .unwrap();
    conn.execute(
        "INSERT INTO libraries(path, scanned_at) VALUES (?1, '')",
        [&lib_s],
    )
    .unwrap();
    conn.execute(
        "UPDATE albums SET library_path = ?1, path = ?1 || substr(path, length(?2) + 1)",
        [&lib_s, &old],
    )
    .unwrap();
    conn.execute("DELETE FROM libraries WHERE path = ?1", [&old])
        .unwrap();

    let python = open_set(conn);
    assert_eq!(python.len(), 21, "the capture's open findings");
    let max_id: i64 = python.iter().map(|f| f.id).max().unwrap();

    let env = env(registry(None, &Default::default()), Arc::new(NoResolver));
    for meta in env.registry.all_meta() {
        let run = run_check(conn, &env, meta.id, &lib_s, ScopeKind::All, None).unwrap();
        assert_eq!(run.status, "completed", "{}: {:?}", meta.id, run.error);
    }
    let rust = open_set(conn);

    // No new rows but ADDED: every other Rust finding upserted onto a Python
    // app row.
    let is_added = |f: &FindingRow| {
        ADDED.iter().any(|(c, a, s, _)| {
            f.check_id == *c && f.album_id.as_deref() == Some(*a) && f.summary == *s
        })
    };
    let (added, rust): (Vec<FindingRow>, Vec<FindingRow>) =
        rust.into_iter().partition(|f| f.id > max_id);
    assert!(added.iter().all(is_added), "new finding rows: {added:#?}");
    assert_eq!(
        added.len(),
        ADDED.len(),
        "every ADDED row appears: {added:#?}"
    );
    let key = |f: &FindingRow| {
        (
            f.id,
            f.check_id.clone(),
            f.album_id.clone(),
            f.summary.clone(),
            f.severity.clone(),
            f.detail_json.clone(),
        )
    };
    let expected: Vec<_> = python
        .iter()
        .filter(|f| {
            !ALLOWED
                .iter()
                .any(|(c, a, _)| f.check_id == *c && f.album_id.as_deref() == Some(*a))
        })
        .map(key)
        .collect();
    let got: Vec<_> = rust.iter().map(key).collect();
    assert_eq!(got, expected);
    // Each allowed difference really occurred (not vacuous).
    for (c, a, why) in ALLOWED {
        assert!(
            python
                .iter()
                .any(|f| f.check_id == *c && f.album_id.as_deref() == Some(*a)),
            "{why}: Python app row missing"
        );
        assert!(
            !rust
                .iter()
                .any(|f| f.check_id == *c && f.album_id.as_deref() == Some(*a)),
            "{why}: Rust still reports it"
        );
    }
    // Every check produced at least one of the compared (or added) findings.
    for meta in env.registry.all_meta() {
        assert!(
            got.iter().any(|k| k.1 == meta.id) || added.iter().any(|f| f.check_id == meta.id),
            "{} has no finding",
            meta.id
        );
    }
}
