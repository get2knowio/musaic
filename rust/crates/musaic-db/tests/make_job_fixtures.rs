//! Generator for the committed fixtures `testdata/db/v20-jobs.db`,
//! `v22-jobs.db` and `v23-jobs.db`: databases as the builds that wrote schema
//! v20, v22 and v23 left them, holding old research jobs in every status and
//! type. Synthetic data only. Run once, by hand, when a fixture must be
//! rebuilt (the files are frozen so they keep testing what those builds
//! wrote, even if a step's code changes later):
//!
//! ```text
//! cargo test -p musaic-db --test make_job_fixtures -- --ignored
//! ```

use std::path::Path;

use rusqlite::{Connection, params};

/// `(id, album_id, job_type, status, acted_on_action, error, result_json,
/// hint)`. `created_at` and the other times are derived from the position.
type Job = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    Option<&'static str>,
    Option<&'static str>,
    Option<&'static str>,
    &'static str,
);

const COVER_RESULT: &str = r#"{"metadata": null, "cover": {"cover_url": "https://coverartarchive.org/release/00000000-0000-4000-8000-000000000001/front", "source": "cover_art_archive", "confidence": 0.9, "reasoning": "synthetic"}}"#;
const METADATA_RESULT: &str = r#"{"metadata": {"album_artist": "Synthetic Artist", "album": "Synthetic Album", "year": "2001", "genre": null, "confidence": 0.8, "reasoning": "synthetic", "tracks": []}, "cover": null}"#;
const BOTH_RESULT: &str = r#"{"metadata": {"album_artist": "Synthetic Artist", "album": "Synthetic Album", "year": "2001", "genre": null, "confidence": 0.8, "reasoning": "synthetic", "tracks": []}, "cover": {"cover_url": "", "source": null, "confidence": 0.0, "reasoning": "none found"}}"#;

const JOBS: &[Job] = &[
    (
        "0000000000000a01",
        "a000000000000001",
        "metadata",
        "queued",
        None,
        None,
        None,
        "a hint",
    ),
    (
        "0000000000000a02",
        "a000000000000002",
        "cover",
        "running",
        None,
        None,
        None,
        "",
    ),
    (
        "0000000000000a03",
        "a000000000000003",
        "metadata+cover",
        "completed",
        None,
        None,
        Some(BOTH_RESULT),
        "",
    ),
    (
        "0000000000000a04",
        "a000000000000004",
        "metadata",
        "failed",
        None,
        Some("synthetic failure"),
        None,
        "",
    ),
    (
        "0000000000000a05",
        "a000000000000005",
        "cover",
        "acted_on",
        Some("applied"),
        None,
        Some(COVER_RESULT),
        "",
    ),
    (
        "0000000000000a06",
        "a000000000000006",
        "metadata+cover",
        "acted_on",
        Some("discarded"),
        None,
        Some(BOTH_RESULT),
        "",
    ),
    (
        "0000000000000a07",
        "a000000000000007",
        "metadata",
        "paused",
        None,
        None,
        None,
        "",
    ),
    (
        "0000000000000a08",
        "a000000000000008",
        "lyrics",
        "completed",
        None,
        None,
        Some(r#"{"lyrics": "la la"}"#),
        "",
    ),
    (
        // Its album row is gone.
        "0000000000000a09",
        "a0000000000000ff",
        "metadata",
        "acted_on",
        Some("applied"),
        None,
        Some(METADATA_RESULT),
        "",
    ),
    (
        // Its stored result is not JSON.
        "0000000000000a10",
        "a000000000000010",
        "cover",
        "failed",
        None,
        Some("synthetic failure"),
        Some("not json {"),
        "",
    ),
];

const LIBRARY: &str = "/var/tmp/musaic-testdata/library";

fn fresh_v16(dest: &Path) -> Connection {
    std::fs::copy(musaic_testdata::public("db/v16-empty.db"), dest).unwrap();
    let conn = Connection::open(dest).unwrap();
    musaic_db::configure_connection(&conn).unwrap();
    conn
}

fn seed(conn: &Connection) {
    conn.execute(
        "INSERT OR IGNORE INTO libraries(path, scanned_at) VALUES (?1, '2026-01-01T00:00:00Z')",
        [LIBRARY],
    )
    .unwrap();
    for (n, job) in JOBS.iter().enumerate() {
        let (id, album, job_type, status, action, error, result, hint) = *job;
        let path = format!("{LIBRARY}/Synthetic Artist - Album {n} [FLAC]");
        let title = format!("Album {n}");
        if album != "a0000000000000ff" {
            conn.execute(
                "INSERT INTO albums(id, library_path, path, dir_name, title, artist)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'Synthetic Artist')",
                params![
                    album,
                    LIBRARY,
                    path,
                    path.rsplit('/').next().unwrap(),
                    title
                ],
            )
            .unwrap();
        }
        let created = format!("2026-01-{:02}T10:00:00Z", n + 1);
        let started = (status != "queued").then(|| format!("2026-01-{:02}T10:00:05Z", n + 1));
        let completed = matches!(status, "completed" | "failed" | "acted_on")
            .then(|| format!("2026-01-{:02}T10:01:00Z", n + 1));
        let acted = (status == "acted_on").then(|| format!("2026-01-{:02}T11:00:00Z", n + 1));
        conn.execute(
            "INSERT INTO ai_jobs(id, album_id, album_path, album_title, job_type, hint, status,
                                 result_json, error, acted_on_action, created_at, started_at,
                                 completed_at, acted_on_at, attach_cover)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
            params![
                id,
                album,
                path,
                title,
                job_type,
                hint,
                status,
                result,
                error,
                action,
                created,
                started,
                completed,
                acted,
                i64::from(n % 2 == 0)
            ],
        )
        .unwrap();
    }
    // The retired provider settings an older build kept.
    for (k, v) in [
        ("ai_provider_id", "\"none\""),
        ("ai_max_parallel", "1"),
        ("ai_queue_paused", "false"),
    ] {
        conn.execute(
            "INSERT OR REPLACE INTO settings(key, value) VALUES (?1, ?2)",
            [k, v],
        )
        .unwrap();
    }
}

fn save(conn: &Connection, name: &str) {
    let out = musaic_testdata::public("db").join(name);
    let _ = std::fs::remove_file(&out);
    conn.execute("VACUUM INTO ?1", [out.to_str().unwrap()])
        .unwrap();
}

#[test]
#[ignore = "generator: run by hand to rebuild the committed fixtures"]
fn make_job_fixtures() {
    let dir = tempfile::tempdir().unwrap();

    let mut v20 = fresh_v16(&dir.path().join("v20.db"));
    musaic_db::migrate::upgrade_to(&mut v20, 20).unwrap();
    seed(&v20);
    save(&v20, "v20-jobs.db");

    // Jobs go in before the v21 -> v22 step, so the real step links them.
    let mut later = fresh_v16(&dir.path().join("v22.db"));
    musaic_db::migrate::upgrade_to(&mut later, 21).unwrap();
    seed(&later);
    musaic_db::migrate::upgrade_to(&mut later, 22).unwrap();
    save(&later, "v22-jobs.db");
    musaic_db::migrate::upgrade_to(&mut later, 23).unwrap();
    save(&later, "v23-jobs.db");
}
