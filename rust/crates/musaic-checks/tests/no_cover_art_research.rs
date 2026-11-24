//! no-cover-art skips an album whose cover research recently failed or ended
//! without a cover, read from both research stores (schema v24): a native
//! `cover.research` request, and an old job's history record. A request
//! linked to an old job is judged by its history record only.

mod common;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use common::{Sandbox, env, registry, setup};
use musaic_checks::{Resolution, ResolutionSource, ScopeKind, run_check};
use musaic_db::delegated_work::{self, NewWork};
use musaic_db::findings::all_findings;
use musaic_db::research_history::{self, JobRow};
use rusqlite::{Connection, params};
use serde_json::{Value as J, json};

const CHECK: &str = "no-cover-art";
const DAY_MS: i64 = 24 * 3600 * 1000;

struct Nothing;

impl ResolutionSource for Nothing {
    fn resolve(&self, _conn: &Connection, _album_id: &str) -> Result<Resolution, String> {
        Ok(Resolution::default())
    }
}

fn album(id: &str) -> J {
    json!({
        "id": id, "library_path": "<LIB>", "path": format!("<LIB>/{id}"), "dir_name": id,
        "title": "T", "artist": "A", "album_artist": "A", "year": "2001",
        "track_count": 1, "track_numbers_json": "[1]", "has_embedded_cover": 0,
    })
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn iso_days_ago(days: u64) -> String {
    musaic_db::backup::iso_seconds(SystemTime::now() - Duration::from_secs(days * 24 * 3600))
}

/// A native request of `album`, `age_ms` old, in `state`, with an optional
/// result whose recommendation has `recommendation_type`.
fn native(
    conn: &Connection,
    n: u32,
    album: &str,
    kind: &str,
    state: &str,
    age_ms: i64,
    recommendation_type: Option<&str>,
) {
    let id = format!("00000000-0000-4000-8000-{n:012}");
    let digest = format!("{n:064}");
    delegated_work::enqueue(
        conn,
        &[NewWork {
            id: &id,
            group_id: &id,
            kind,
            album_id: album,
            base_revision: "r",
            input_json: "{}",
            input_digest: &digest,
            max_attempts: 3,
        }],
        now_ms(),
    )
    .unwrap();
    let created = now_ms() - age_ms;
    let result_id = recommendation_type.map(|t| {
        let rid = format!("{id}-result");
        let body = json!({"outcome": if t == "cover" { "proposal" } else { "no_match" },
                          "recommendation": {"type": t}});
        conn.execute(
            "INSERT INTO work_results(id,work_id,input_digest,digest,result_json,validation,created_at)
             VALUES (?1,?2,?3,'d',?4,'verified',?5)",
            params![rid, id, digest, body.to_string(), created],
        )
        .unwrap();
        rid
    });
    conn.execute(
        "UPDATE delegated_work SET state=?2, result_id=?3, created_at=?4, updated_at=?4 WHERE id=?1",
        params![id, state, result_id, created],
    )
    .unwrap();
}

fn old_job(
    conn: &Connection,
    id: &str,
    album: &str,
    status: &str,
    days: u64,
    result: Option<J>,
) -> JobRow {
    let mut job = JobRow::seed(id, album, "cover", status, &iso_days_ago(days));
    job.result_json = result.map(|r| r.to_string());
    if status == "acted_on" {
        job.acted_on_action = Some("applied".into());
    }
    research_history::import_legacy_job(conn, &job).unwrap();
    job
}

fn flagged(albums: &[&str], seed: impl FnOnce(&Connection)) -> Vec<String> {
    let (sb, db) = Sandbox::new();
    let rows: Vec<J> = albums.iter().map(|a| album(a)).collect();
    setup(&sb, db.connection(), &json!({ "albums": rows }));
    let conn = db.connection();
    seed(conn);
    let env = env(
        registry(None, &Default::default()),
        std::sync::Arc::new(Nothing),
    );
    let r = run_check(conn, &env, CHECK, &sb.lib_s, ScopeKind::All, None).unwrap();
    assert_eq!(r.status, "completed", "{r:?}");
    let mut ids: Vec<String> = all_findings(conn)
        .unwrap()
        .into_iter()
        .filter(|f| f.check_id == CHECK && f.resolved_at.is_none())
        .filter_map(|f| f.album_id)
        .collect();
    ids.sort();
    ids
}

#[test]
fn native_requests_suppress_by_outcome() {
    let got = flagged(
        &[
            "failed",
            "applied_cover",
            "applied_nomatch",
            "discarded_none",
            "discarded_cover",
            "queued",
            "awaiting",
            "cancelled",
            "metadata_failed",
            "plain",
        ],
        |c| {
            native(c, 1, "failed", "cover.research", "failed", DAY_MS, None);
            native(
                c,
                2,
                "applied_cover",
                "cover.research",
                "applied",
                DAY_MS,
                Some("cover"),
            );
            native(
                c,
                3,
                "applied_nomatch",
                "cover.research",
                "applied",
                DAY_MS,
                Some("unresolved"),
            );
            native(
                c,
                4,
                "discarded_none",
                "cover.research",
                "discarded",
                DAY_MS,
                None,
            );
            native(
                c,
                5,
                "discarded_cover",
                "cover.research",
                "discarded",
                DAY_MS,
                Some("cover"),
            );
            native(c, 6, "queued", "cover.research", "queued", DAY_MS, None);
            native(
                c,
                7,
                "awaiting",
                "cover.research",
                "awaiting_review",
                DAY_MS,
                Some("cover"),
            );
            native(
                c,
                8,
                "cancelled",
                "cover.research",
                "cancelled",
                DAY_MS,
                None,
            );
            native(
                c,
                9,
                "metadata_failed",
                "metadata.identify",
                "failed",
                DAY_MS,
                None,
            );
        },
    );
    assert_eq!(
        got,
        [
            "applied_cover",
            "awaiting",
            "cancelled",
            "discarded_cover",
            "metadata_failed",
            "plain",
            "queued"
        ]
    );
}

#[test]
fn native_window_edges() {
    let got = flagged(&["inside", "outside"], |c| {
        native(
            c,
            1,
            "inside",
            "cover.research",
            "failed",
            29 * DAY_MS,
            None,
        );
        native(
            c,
            2,
            "outside",
            "cover.research",
            "failed",
            31 * DAY_MS,
            None,
        );
    });
    assert_eq!(got, ["outside"]);
}

#[test]
fn old_jobs_are_judged_by_their_history_record() {
    let got = flagged(
        &[
            "old_failed",
            "old_cover",
            "old_empty",
            "old_stale",
            "old_linked",
        ],
        |c| {
            old_job(c, "j1", "old_failed", "failed", 1, None);
            old_job(
                c,
                "j2",
                "old_cover",
                "acted_on",
                1,
                Some(json!({"cover": {"cover_url": "https://x/c.jpg"}})),
            );
            old_job(
                c,
                "j3",
                "old_empty",
                "acted_on",
                1,
                Some(json!({"cover": {"cover_url": ""}})),
            );
            old_job(c, "j4", "old_stale", "failed", 31, None);
            // An applied old cover job: its linked request is `applied` with
            // no `work_results` row, which the native rule would read as "no
            // candidate"; the history record has a cover, so it is flagged.
            old_job(
                c,
                "j5",
                "old_linked",
                "acted_on",
                1,
                Some(json!({"cover": {"cover_url": "https://x/c.jpg"}})),
            );
            let state: String = c
                .query_row(
                    "SELECT state FROM delegated_work WHERE legacy_job_id='j5'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(state, "applied");
        },
    );
    assert_eq!(got, ["old_cover", "old_linked", "old_stale"]);
}
