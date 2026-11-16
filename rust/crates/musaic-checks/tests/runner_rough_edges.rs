//! Check runner behaviour that deliberately differs from the Python app:
//! - a failed run reports the findings it upserted before failing;
//! - no-cover-art's 30-day retry window is compared as points in time.

mod common;

use std::sync::Arc;

use common::{RecordedResolver, Sandbox, env, registry, setup};
use musaic_checks::{ScopeKind, run_check};
use serde_json::json;

/// The fake `boom` check emits one finding, then fails: the run is `failed`
/// and counts that finding (the Python app reported 0).
#[test]
fn a_failed_run_counts_the_findings_it_upserted() {
    let (sb, db) = Sandbox::new();
    let conn = db.connection();
    let case = json!({});
    setup(&sb, conn, &case);
    let log = Default::default();
    let env = env(
        registry(Some(&json!(["boom"])), &log),
        Arc::new(RecordedResolver::from_case(&case)),
    );
    let run = run_check(conn, &env, "boom", &sb.lib_s, ScopeKind::All, None).unwrap();
    assert_eq!(run.status, "failed");
    assert_eq!(run.error.as_deref(), Some("boom"));
    assert_eq!(run.issue_count, 1, "the partial finding is counted");
    assert_eq!(musaic_db::findings::all_findings(conn).unwrap().len(), 1);
}

/// A failed cover job just outside the 30 days no longer suppresses the
/// finding; one just inside still does. `age` is an SQLite modifier on
/// `now`; `created_at` is stored ISO-8601 with a `T`, as the job queue
/// writes it.
#[test]
fn no_cover_art_retry_window_is_exact() {
    let (sb, db) = Sandbox::new();
    let conn = db.connection();
    let album = |id: &str| {
        json!({
            "id": id, "library_path": "<LIB>", "path": format!("<LIB>/{id}"), "dir_name": id,
            "has_embedded_cover": 0, "track_count": 1,
        })
    };
    let job = |id: &str, album: &str, age: &str| json!({"id": id, "album_id": album, "job_type": "cover", "status": "failed", "age": age});
    let case = json!({
        "albums": [album("old"), album("recent")],
        "ai_jobs": [
            // 30 days and one minute ago: outside the window, though on the
            // cut-off date (except in the first minute of a UTC day).
            job("j1", "old", "-2592060 seconds"),
            // 30 days less one minute ago: inside.
            job("j2", "recent", "-2591940 seconds"),
        ],
    });
    setup(&sb, conn, &case);
    let log = Default::default();
    let env = env(
        registry(None, &log),
        Arc::new(RecordedResolver::from_case(&case)),
    );
    let run = run_check(conn, &env, "no-cover-art", &sb.lib_s, ScopeKind::All, None).unwrap();
    assert_eq!(run.status, "completed", "{:?}", run.error);
    let albums: Vec<String> = musaic_db::findings::all_findings(conn)
        .unwrap()
        .into_iter()
        .filter_map(|f| f.album_id)
        .collect();
    assert_eq!(albums, ["old"]);
}

/// A resolver that answers with an empty resolution until told to fail,
/// like a database that turns busy during an apply.
struct Flaky(std::sync::atomic::AtomicBool);

impl musaic_checks::ResolutionSource for Flaky {
    fn resolve(
        &self,
        _conn: &rusqlite::Connection,
        _album_id: &str,
    ) -> Result<musaic_checks::Resolution, String> {
        if self.0.load(std::sync::atomic::Ordering::SeqCst) {
            Err("database is locked".into())
        } else {
            Ok(Default::default())
        }
    }
}

/// A resolver error is not "no resolution": the album is not evaluated, so
/// the finding it had stays open instead of closing as if the album were
/// clean, and the album is not recorded as checked.
#[test]
fn a_resolver_error_leaves_the_albums_findings_open() {
    let (sb, db) = Sandbox::new();
    let conn = db.connection();
    let case = json!({
        "albums": [{
            "id": "a1", "library_path": "<LIB>", "path": "<LIB>/a1", "dir_name": "a1",
            "title": "T", "artist": "A", "album_artist": "A", "year": "2000", "genre": null,
            "mb_release_group_mbid": "rg-1", "track_count": 1,
        }],
        "dirs": ["a1"],
    });
    setup(&sb, conn, &case);
    let log = Default::default();
    let flaky = Arc::new(Flaky(Default::default()));
    let env = env(registry(None, &log), flaky.clone());
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
    assert_eq!(
        musaic_db::findings::all_findings(conn).unwrap().len(),
        1,
        "the missing genre is reported while the resolver answers"
    );

    // The genre is fixed, so a run that could evaluate the album would close
    // the finding; one whose resolver fails must not.
    conn.execute("UPDATE albums SET genre = 'Rock' WHERE id = 'a1'", [])
        .unwrap();
    flaky.0.store(true, std::sync::atomic::Ordering::SeqCst);
    run_check(
        conn,
        &env,
        "tags-out-of-sync",
        &sb.lib_s,
        ScopeKind::All,
        None,
    )
    .unwrap();
    let open: Vec<_> = musaic_db::findings::all_findings(conn)
        .unwrap()
        .into_iter()
        .filter(|f| f.resolved_at.is_none())
        .collect();
    assert_eq!(open.len(), 1, "an unevaluated album keeps its finding open");

    flaky.0.store(false, std::sync::atomic::Ordering::SeqCst);
    run_check(
        conn,
        &env,
        "tags-out-of-sync",
        &sb.lib_s,
        ScopeKind::All,
        None,
    )
    .unwrap();
    assert!(
        musaic_db::findings::all_findings(conn)
            .unwrap()
            .iter()
            .all(|f| f.resolved_at.is_some()),
        "evaluated again, the finding closes"
    );
}
