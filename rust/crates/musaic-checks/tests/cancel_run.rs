//! A check run cancelled part-way (#349): it stops before the check's next
//! album, keeps the findings it wrote, resolves nothing and records no inputs,
//! so the background sweep evaluates every album again. The cancel is set
//! from inside the run when the check reaches its second album (observed
//! through the resolver it calls once per album), not by timing.

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use common::{Sandbox, env, setup};
use musaic_checks::{
    Registry, Resolution, ResolutionSource, ScopeKind, run_check, run_check_cancellable,
};
use rusqlite::Connection;
use serde_json::json;

const CHECK: &str = "tags-out-of-sync";

/// Answers "nothing known" for every album, and sets `cancel` while it is
/// asked about album number `cancel_at` (1-based), so the check is stopped
/// with that album still to finish.
struct CancelAt {
    calls: AtomicUsize,
    cancel_at: usize,
    cancel: Arc<AtomicBool>,
}

impl ResolutionSource for CancelAt {
    fn resolve(&self, _conn: &Connection, _album_id: &str) -> Result<Resolution, String> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if n == self.cancel_at {
            self.cancel.store(true, Ordering::SeqCst);
        }
        Ok(Resolution::default())
    }
}

/// Four anchored albums with no title: tags-out-of-sync reports each one
/// (a missing core field) and asks the resolver once per album, in row order.
fn four_albums(sb: &Sandbox, conn: &Connection) {
    let album = |id: &str| {
        json!({
            "id": id, "library_path": "<LIB>", "path": format!("<LIB>/{id}"), "dir_name": id,
            "track_count": 1, "artist": "Someone", "year": "2001", "genre": "Rock",
            "mb_release_group_mbid": format!("rg-{id}"),
        })
    };
    let case = json!({"albums": [album("a1"), album("a2"), album("a3"), album("a4")]});
    setup(sb, conn, &case);
}

fn open_finding_albums(conn: &Connection) -> BTreeSet<String> {
    musaic_db::findings::all_findings(conn)
        .unwrap()
        .into_iter()
        .filter(|f| f.check_id == CHECK && f.resolved_at.is_none())
        .filter_map(|f| f.album_id)
        .collect()
}

fn recorded_inputs(conn: &Connection, sb: &Sandbox) -> BTreeSet<String> {
    musaic_db::check_inputs::rows_for_library(conn, &sb.lib_s)
        .unwrap()
        .into_iter()
        .filter(|r| r.check_id == CHECK)
        .map(|r| r.album_id)
        .collect()
}

fn ids(v: &[&str]) -> BTreeSet<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_cancelled_run_stops_before_the_next_album_and_records_no_inputs() {
    let (sb, db) = Sandbox::new();
    let conn = db.connection();
    four_albums(&sb, conn);
    let cancel = Arc::new(AtomicBool::new(false));
    let resolver = Arc::new(CancelAt {
        calls: AtomicUsize::new(0),
        cancel_at: 2,
        cancel: cancel.clone(),
    });
    let env = env(Registry::default(), resolver.clone());

    let run = run_check_cancellable(
        conn,
        &env,
        CHECK,
        &sb.lib_s,
        ScopeKind::All,
        None,
        Some(&cancel),
    )
    .unwrap();
    assert_eq!(
        resolver.calls.load(Ordering::SeqCst),
        2,
        "the album being looked at when the cancel came was finished, no other was started"
    );
    assert_eq!(run.status, "failed");
    assert_eq!(run.error.as_deref(), Some("check run cancelled"));
    assert_eq!(run.issue_count, 2, "the partial count");
    assert_eq!(open_finding_albums(conn), ids(&["a1", "a2"]));
    assert!(
        recorded_inputs(conn, &sb).is_empty(),
        "no inputs recorded: the sweep runs the check again for every album"
    );

    // Uncancelled, the same run reaches every album and records them all.
    let env = env_uncancelled();
    let run = run_check(conn, &env, CHECK, &sb.lib_s, ScopeKind::All, None).unwrap();
    assert_eq!(run.status, "completed", "{:?}", run.error);
    assert_eq!(run.issue_count, 4);
    let all = ids(&["a1", "a2", "a3", "a4"]);
    assert_eq!(open_finding_albums(conn), all);
    assert_eq!(recorded_inputs(conn, &sb), all);
    let before = musaic_db::check_inputs::rows_for_library(conn, &sb.lib_s).unwrap();

    // Fix the first two albums, then cancel at the first album: nothing is
    // resolved (not even the fixed album it looked at), the findings of the
    // albums it did not reach stay open, and the recorded inputs are as they
    // were.
    conn.execute(
        "UPDATE albums SET title = 'Fixed' WHERE id IN ('a1', 'a2')",
        [],
    )
    .unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let resolver = Arc::new(CancelAt {
        calls: AtomicUsize::new(0),
        cancel_at: 1,
        cancel: cancel.clone(),
    });
    let env = common::env(Registry::default(), resolver.clone());
    let run = run_check_cancellable(
        conn,
        &env,
        CHECK,
        &sb.lib_s,
        ScopeKind::All,
        None,
        Some(&cancel),
    )
    .unwrap();
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
    assert_eq!(run.error.as_deref(), Some("check run cancelled"));
    assert_eq!(run.issue_count, 0);
    assert_eq!(open_finding_albums(conn), all, "nothing auto-resolved");
    assert_eq!(
        musaic_db::check_inputs::rows_for_library(conn, &sb.lib_s).unwrap(),
        before
    );
}

#[test]
fn a_flag_that_is_never_set_changes_nothing() {
    let (sb, db) = Sandbox::new();
    let conn = db.connection();
    four_albums(&sb, conn);
    let cancel = AtomicBool::new(false);
    let run = run_check_cancellable(
        conn,
        &env_uncancelled(),
        CHECK,
        &sb.lib_s,
        ScopeKind::All,
        None,
        Some(&cancel),
    )
    .unwrap();
    assert_eq!(run.status, "completed", "{:?}", run.error);
    assert_eq!(run.issue_count, 4);
    assert_eq!(recorded_inputs(conn, &sb).len(), 4);
}

fn env_uncancelled() -> musaic_checks::CheckEnv {
    env(
        Registry::default(),
        Arc::new(CancelAt {
            calls: AtomicUsize::new(0),
            cancel_at: usize::MAX,
            cancel: Arc::new(AtomicBool::new(false)),
        }),
    )
}
