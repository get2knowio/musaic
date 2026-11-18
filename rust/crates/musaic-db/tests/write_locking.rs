//! Write transactions start with `BEGIN IMMEDIATE` (fixes intermittent
//! "database is locked" check runs).
//!
//! Cause, reproduced here: a transaction that starts DEFERRED and reads before
//! it writes (`upsert_finding`: SELECT, then UPDATE/INSERT) holds a WAL read
//! snapshot when it asks for the write lock. SQLite then never calls the busy
//! handler (`sqlite3BtreeBeginTrans` only retries from TRANS_NONE), so while
//! another connection holds the write lock the upgrade fails at once with
//! SQLITE_BUSY ("database is locked") instead of waiting out `busy_timeout`;
//! and if another connection committed since the snapshot was taken it fails
//! with SQLITE_BUSY_SNAPSHOT. Every read-then-write transaction now begins
//! IMMEDIATE (`musaic_db::immediate`), which takes the write lock up front
//! from TRANS_NONE, where the 30 s busy handler applies.
//!
//! The Python app (`musaic/db.py:_transaction`) issues a plain `BEGIN` (deferred)
//! on a fresh `isolation_level=None, timeout=30` connection per operation; it
//! has the same latent race but its writers are serialised by the event loop
//! and the GIL far more often than this build's threads. BEGIN IMMEDIATE keeps
//! its semantics (one atomic unit per helper) and only takes the lock earlier.

use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use musaic_core::Album;
use musaic_db::{Db, albums, findings, open_connection};

fn fresh() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.db");
    let (db, _) = Db::open_or_create(&path).unwrap();
    drop(db);
    (dir, path)
}

fn album(i: usize) -> Album {
    Album {
        id: format!("album-{i}"),
        path: format!("/lib/Artist - Album {i} [FLAC]"),
        dir_name: format!("Artist - Album {i} [FLAC]"),
        title: Some(format!("Album {i}")),
        artist: Some("Artist".into()),
        album_artist: None,
        year: None,
        genre: None,
        track_count: 3,
        total_seconds: 600.0,
        has_embedded_cover: false,
        cover_file: None,
        sample_track: "01.flac".into(),
        dir_mtime: i as f64,
        dir_artist: Some("Artist".into()),
        dir_album: Some(format!("Album {i}")),
        is_compilation: false,
        disc_number: None,
        disc_total: None,
        sample_rate: None,
        bits_per_sample: None,
        channels: None,
        track_numbers: vec![Some(1), Some(2), Some(3)],
        album_artist_sort: None,
        artist_sort: None,
        album_sort: None,
        track_artists: vec![],
        track_discs: None,
        tag_disc_total: None,
    }
}

/// The deterministic reproduction. Connection B holds the write lock
/// for 300 ms; `upsert_finding` on connection A must wait for it (busy_timeout
/// is 30 s) and succeed. Before the fix it failed immediately with
/// "database is locked".
#[test]
fn upsert_finding_waits_for_a_concurrent_writer() {
    let (_dir, path) = fresh();
    let a = open_connection(&path).unwrap();
    let b = open_connection(&path).unwrap();
    b.execute_batch("BEGIN IMMEDIATE; INSERT INTO libraries(path, scanned_at) VALUES ('/x', '');")
        .unwrap();
    let release = thread::spawn(move || {
        thread::sleep(Duration::from_millis(300));
        b.execute_batch("COMMIT").unwrap();
    });
    let t0 = Instant::now();
    let r = findings::upsert_finding(&a, "track-gaps", None, "warning", "s", None, "2026-11-11");
    let waited = t0.elapsed();
    release.join().unwrap();
    assert!(r.is_ok(), "upsert_finding failed after {waited:?}: {r:?}");
    assert!(
        waited >= Duration::from_millis(250),
        "did not wait: {waited:?}"
    );
}

/// The stale-snapshot variant. A reads the finding inside an open
/// read snapshot, B commits a write, then A writes. Under DEFERRED this is
/// SQLITE_BUSY_SNAPSHOT; `immediate` cannot hold a stale snapshot.
#[test]
fn immediate_transaction_never_holds_a_stale_snapshot() {
    let (_dir, path) = fresh();
    let a = open_connection(&path).unwrap();
    let b = open_connection(&path).unwrap();
    // The raw mechanism, to document it: deferred read, foreign commit, write.
    a.execute_batch("BEGIN").unwrap();
    let _: i64 = a
        .query_row("SELECT COUNT(*) FROM check_findings", [], |r| r.get(0))
        .unwrap();
    findings::insert_run(&b, "track-gaps", "t", "/lib", "all").unwrap();
    let err = a
        .execute(
            "INSERT INTO libraries(path, scanned_at) VALUES ('/y', '')",
            [],
        )
        .unwrap_err();
    assert!(err.to_string().contains("locked"), "{err}");
    a.execute_batch("ROLLBACK").unwrap();
    // The helper takes the write lock first, so the same sequence succeeds.
    let tx = musaic_db::immediate(&a).unwrap();
    let _: i64 = tx
        .query_row("SELECT COUNT(*) FROM check_findings", [], |r| r.get(0))
        .unwrap();
    let blocked = thread::spawn(move || {
        // B must wait for A now (A holds the write lock); it succeeds after.
        findings::insert_run(&b, "track-gaps", "t", "/lib", "all").map(|_| ())
    });
    thread::sleep(Duration::from_millis(100));
    tx.execute(
        "INSERT INTO libraries(path, scanned_at) VALUES ('/y', '')",
        [],
    )
    .unwrap();
    tx.commit().unwrap();
    blocked.join().unwrap().unwrap();
}

/// Stress: N threads, each on its own connection to one file, run
/// check-finding upserts (the runner's write path) and scan upserts
/// (`save_snapshot`, `upsert_album`) at the same time. No operation may fail.
#[test]
fn concurrent_check_and_scan_writers_never_see_database_is_locked() {
    const THREADS: usize = 8;
    const ROUNDS: usize = 60;
    let (_dir, path) = fresh();
    let barrier = Arc::new(Barrier::new(THREADS));
    let handles: Vec<_> = (0..THREADS)
        .map(|t| {
            let path = path.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                let mut conn = open_connection(&path).unwrap();
                barrier.wait();
                let mut errors = Vec::new();
                for r in 0..ROUNDS {
                    let res = match t % 4 {
                        0 | 1 => findings::upsert_finding(
                            &conn,
                            "track-gaps",
                            None,
                            "warning",
                            &format!("t{t} r{}", r % 7),
                            Some("{}"),
                            "2026-11-11T00:00:00",
                        )
                        .map(|_| ()),
                        2 => {
                            let batch: Vec<Album> = (0..5).map(|i| album(i + r % 3)).collect();
                            albums::save_snapshot(&mut conn, "/lib", "now", "now", &batch)
                                .map(|_| ())
                        }
                        _ => albums::upsert_album(&mut conn, "/lib", "now", &album(r % 9)),
                    };
                    if let Err(e) = res {
                        errors.push(e.to_string());
                    }
                }
                errors
            })
        })
        .collect();
    let errors: Vec<String> = handles
        .into_iter()
        .flat_map(|h| h.join().unwrap())
        .collect();
    assert!(
        errors.is_empty(),
        "{} failures, e.g. {:?}",
        errors.len(),
        errors.first()
    );
}
