//! tags-out-of-sync severity: a blank genre alone is informational (this
//! build cannot fill one in, so a warning would have no fix); a blank title,
//! album artist or year is a warning. The check carries a rule version in its
//! inputs digest so existing libraries rerun it once, and only it.

mod common;

use std::time::{Duration, UNIX_EPOCH};

use common::{Sandbox, env, registry, setup};
use musaic_checks::inputs::{album_check_digest, album_digests, plan_stale};
use musaic_checks::{Resolution, ResolutionSource, ScopeKind, run_check};
use musaic_db::findings::all_findings;
use rusqlite::Connection;
use serde_json::{Value as J, json};

const CHECK: &str = "tags-out-of-sync";

struct Nothing;

impl ResolutionSource for Nothing {
    fn resolve(&self, _conn: &Connection, _album_id: &str) -> Result<Resolution, String> {
        Ok(Resolution::default())
    }
}

/// A complete album, with the named tags blanked.
fn album(id: &str, blank: &[&str]) -> J {
    let mut a = json!({
        "id": id, "library_path": "<LIB>", "path": format!("<LIB>/{id}"), "dir_name": id,
        "title": "T", "artist": "A", "album_artist": "A", "year": "2001", "genre": "Rock",
        "track_count": 2, "track_numbers_json": "[1, 2]",
    });
    for f in blank {
        a[*f] = J::Null;
    }
    a
}

fn world(albums: Vec<J>) -> (Sandbox, musaic_db::Db, musaic_checks::CheckEnv) {
    let (sb, db) = Sandbox::new();
    setup(&sb, db.connection(), &json!({ "albums": albums }));
    let env = env(
        registry(None, &Default::default()),
        std::sync::Arc::new(Nothing),
    );
    (sb, db, env)
}

fn run(conn: &Connection, sb: &Sandbox, env: &musaic_checks::CheckEnv, check: &str) {
    let r = run_check(conn, env, check, &sb.lib_s, ScopeKind::All, None).unwrap();
    assert_eq!(r.status, "completed", "{r:?}");
}

fn severity_of(conn: &Connection, album: &str) -> String {
    all_findings(conn)
        .unwrap()
        .into_iter()
        .find(|f| f.check_id == CHECK && f.album_id.as_deref() == Some(album))
        .unwrap_or_else(|| panic!("no finding for {album}"))
        .severity
}

#[test]
fn only_a_blank_genre_is_informational() {
    let (sb, db, env) = world(vec![
        album("genre", &["genre"]),
        album("genre_year", &["genre", "year"]),
        album("title", &["title"]),
        album("year", &["year"]),
        album("artist", &["album_artist", "artist"]),
    ]);
    let conn = db.connection();
    run(conn, &sb, &env, CHECK);
    assert_eq!(severity_of(conn, "genre"), "info");
    assert_eq!(severity_of(conn, "genre_year"), "warn");
    assert_eq!(severity_of(conn, "title"), "warn");
    assert_eq!(severity_of(conn, "year"), "warn");
    assert_eq!(severity_of(conn, "artist"), "warn");
    // The finding still says the genre is missing.
    let f = all_findings(conn)
        .unwrap()
        .into_iter()
        .find(|f| f.album_id.as_deref() == Some("genre"))
        .unwrap();
    assert!(f.summary.contains("genre"), "{}", f.summary);
    let d: J = serde_json::from_str(f.detail_json.as_deref().unwrap()).unwrap();
    assert_eq!(d["missing_core"], json!(["genre"]));
}

/// A genre-only finding recorded as `warn` becomes `info` on the next run; an
/// ignored one stays ignored.
#[test]
fn an_existing_genre_only_warning_becomes_info_and_ignored_stays_ignored() {
    let (sb, db, env) = world(vec![album("g1", &["genre"]), album("g2", &["genre"])]);
    let conn = db.connection();
    run(conn, &sb, &env, CHECK);
    let ids: Vec<(String, i64)> = all_findings(conn)
        .unwrap()
        .iter()
        .map(|f| (f.album_id.clone().unwrap(), f.id))
        .collect();
    assert_eq!(ids.len(), 2);
    // As the earlier version left them.
    conn.execute("UPDATE check_findings SET severity = 'warn'", [])
        .unwrap();
    let ignored = ids.iter().find(|(a, _)| a == "g2").unwrap().1;
    musaic_checks::runner::ignore_finding(conn, &env, ignored, Some("on purpose"))
        .unwrap()
        .unwrap();

    run(conn, &sb, &env, CHECK);
    assert_eq!(severity_of(conn, "g1"), "info");
    let f = all_findings(conn)
        .unwrap()
        .into_iter()
        .find(|f| f.id == ignored)
        .unwrap();
    assert_eq!(f.severity, "info", "severity is refreshed either way");
    assert!(f.ignored_at.is_some(), "and it is still ignored");
    assert!(f.resolved_at.is_none());
}

/// Libraries that ran the checks before the rule changed recorded this
/// check's digest without the rule version. Only this check is stale for them.
#[test]
fn only_this_check_is_stale_after_the_rule_change() {
    let (sb, db, env) = world(vec![album("g1", &["genre"])]);
    let conn = db.connection();
    let ids: Vec<&str> = env.registry.iter().map(|c| c.meta().id).collect();
    for id in &ids {
        run(conn, &sb, &env, id);
    }
    // The test clock stamps runs 2026-01-01; look just after, inside every
    // check's refresh window.
    let now = UNIX_EPOCH + Duration::from_secs(1_767_312_000); // 2026-01-02
    let stale = |conn: &Connection| -> Vec<&'static str> {
        plan_stale(conn, &env, &sb.lib_s, now)
            .unwrap()
            .iter()
            .map(|w| w.check_id)
            .collect()
    };
    // A check that could not evaluate this synthetic album (it has no files)
    // records nothing and stays stale whatever else happens: the baseline.
    let baseline = stale(conn);
    assert!(!baseline.contains(&CHECK), "{baseline:?}");

    // This check's rows as the previous version recorded them: no extras.
    let bases = album_digests(conn, &sb.lib_s, None).unwrap();
    let old: Vec<(String, String)> = bases
        .iter()
        .map(|(a, b)| (a.clone(), album_check_digest(CHECK, "", b)))
        .collect();
    musaic_db::check_inputs::record(conn, CHECK, &sb.lib_s, &old, "2026-01-01T00:00:00Z").unwrap();
    let mut want = baseline;
    want.insert(0, CHECK);
    assert_eq!(stale(conn), want, "this check is stale; no other changed");
}
