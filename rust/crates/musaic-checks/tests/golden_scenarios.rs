//! Scripted runner sequences replayed from the Python app
//! (testdata/golden/checks/scenarios.json): findings and check_runs tables
//! after every step, plus each step's return value.
//!
//! Detect / re-detect / fix -> resolved / regress -> reopened on the
//! same row / ignore survives / album-scoped auto-resolve / Missing albums.
//! The post-scan guard (only runs_during_scan checks, registry
//! order, flag set, nested call short-circuits, flag reset after a failing
//! check). One failing check does not stop the sweep.
//!
//! Enumerated difference (follows from the recursion guard; see `post_scan_guard_delta`): in the Python app
//! a check body runs in a `run_in_executor` thread without the caller's
//! context, so it sees `in_post_scan_rerun == False` and a nested
//! `run_post_scan_checks` from inside a check runs a whole second sweep. In
//! Rust the check runs on the sweep's thread: the flag is visible and the
//! nested call short-circuits. Everything else must be equal.
//!
//! Deliberate-change deltas each applied to the Python app's
//! record by an edit that first asserts the recorded old value (so a delta
//! that is no longer in the record fails):
//! - Missing albums (`missing_album`, `missing_album_ignored`): a library-wide run
//!   no longer resolves the findings of Missing albums.
//! - Unreachable album (`duplicates_delete_file_unreachable`): delete-file with the
//!   album directory unreachable is refused with 409; the fingerprint rows
//!   and the album row stay.
//! - Two libraries (`two_libraries`): a library-wide run no longer resolves the
//!   findings of another library's albums.
//! - Failed runs (`raising_check_does_not_stop_the_sweep`, `post_scan_guard`):
//!   a failed run's `issue_count` counts the findings it upserted before
//!   failing (the Python app left it 0).
//! - Artist-sort clusters (`fix_artist_sort`): an album-scoped run of
//!   inconsistent-artist-sort resolves across the library, so fixing another
//!   member closes the cluster's finding.

mod common;

use std::sync::{Arc, Mutex};

use common::{
    ProbeLog, RecordedResolver, Sandbox, env, findings_json, registry, run_json, runs_json, setup,
    str_ids,
};
use musaic_checks::checks::duplicate_tracks::{
    cluster_preview, derive_cluster, trash_duplicate_file,
};
use musaic_checks::runner::{ignore_finding, list_checks_with_summary, unignore_finding};
use musaic_checks::{ScopeKind, run_check, run_post_scan_checks};
use musaic_fsops::plan::{Plan, derive_plan};
use serde_json::{Value as J, json};

fn plan_json(sb: &Sandbox, p: Option<Plan>) -> J {
    let Some(p) = p else { return J::Null };
    let mut o = json!({
        "dir_name": p.dir_name,
        "is_compilation": p.is_compilation,
        "template": p.template,
        "dir_template": p.dir_template,
        "headline": p.headline,
        "renames": p.renames.iter().map(|e| json!({
            "filename": e.filename, "expected": e.expected, "tracknumber": e.tracknumber,
            "title": e.title, "artist": e.artist, "collides_with": e.collides_with,
        })).collect::<Vec<_>>(),
        "dir_rename": p.dir_rename.map(|d| json!({"old": d.old, "new": d.new})),
    });
    if p.album_missing {
        o["album_missing"] = json!(true);
    }
    if let Some(e) = p.error {
        o["error"] = json!(e);
    }
    serde_json::from_str(&sb.unsub(&o.to_string())).unwrap()
}

fn trash_listing(root: &std::path::Path) -> Vec<String> {
    fn walk(d: &std::path::Path, root: &std::path::Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
            if e.file_type().unwrap().is_dir() {
                walk(&e.path(), root, out);
            } else {
                out.push(musaic_testdata::slash_path(
                    e.path().strip_prefix(root).unwrap(),
                ));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

fn cluster_json(c: Option<musaic_checks::checks::duplicate_tracks::Cluster>) -> J {
    c.map_or(J::Null, |c| c.to_pyjson().to_value())
}

/// Replay one scenario; returns the steps in the golden's shape.
fn replay(sc: &J) -> Vec<J> {
    let (sb, db) = Sandbox::new();
    let conn = db.connection();
    setup(&sb, conn, sc);
    let log: ProbeLog = Arc::new(Mutex::new(Vec::new()));
    let env = env(
        registry(sc.get("registry"), &log),
        Arc::new(RecordedResolver::from_case(sc)),
    );
    let mut out = Vec::new();
    for st in sc["steps"].as_array().unwrap() {
        let op = st["op"].as_str().unwrap();
        let result = match op {
            "run" => {
                let kind = ScopeKind::parse(st["scope"].as_str().unwrap_or("all")).unwrap();
                let ids = str_ids(&st["album_ids"]);
                // `library`: a run for another library path (`<LIB>`-relative).
                let library = st["library"]
                    .as_str()
                    .map_or(sb.lib_s.clone(), |l| sb.sub(l));
                match run_check(
                    conn,
                    &env,
                    st["check"].as_str().unwrap(),
                    &library,
                    kind,
                    ids.as_deref(),
                ) {
                    Ok(r) => json!({ "run": run_json(&sb, &r) }),
                    Err(e) => json!({ "error": e.to_string() }),
                }
            }
            "post_scan" => {
                let ids = str_ids(&st["album_ids"]);
                assert!(run_post_scan_checks(conn, &env, &sb.lib_s, ids.as_deref()).is_some());
                J::Null
            }
            "flag" => json!({ "in_post_scan_rerun": musaic_events::in_post_scan_rerun() }),
            "sql" => {
                conn.execute(st["sql"].as_str().unwrap(), []).unwrap();
                J::Null
            }
            "ignore" => {
                let r = ignore_finding(
                    conn,
                    &env,
                    st["finding_id"].as_i64().unwrap(),
                    st["reason"].as_str(),
                )
                .unwrap();
                r.map_or(J::Null, |r| {
                    json!({"id": r.id, "ignored_at": r.ignored_at, "ignored_reason": r.ignored_reason})
                })
            }
            "unignore" => {
                let r = unignore_finding(conn, &env, st["finding_id"].as_i64().unwrap()).unwrap();
                r.map_or(J::Null, |r| json!({"id": r.id, "ignored_at": r.ignored_at}))
            }
            "rename_file" => {
                std::fs::rename(
                    sb.lib.join(st["from"].as_str().unwrap()),
                    sb.lib.join(st["to"].as_str().unwrap()),
                )
                .unwrap();
                J::Null
            }
            "plan_preview" => plan_json(
                &sb,
                derive_plan(conn, st["album_id"].as_str().unwrap()).unwrap(),
            ),
            "derive_cluster" => {
                let orig = str_ids(&st["original"]).unwrap();
                cluster_json(derive_cluster(conn, st["album_id"].as_str().unwrap(), &orig).unwrap())
            }
            "cluster_preview" => {
                let f = musaic_db::findings::get_finding(conn, st["finding_id"].as_i64().unwrap())
                    .unwrap();
                match cluster_preview(conn, f.as_ref()) {
                    Ok(v) => v,
                    Err(e) => json!({"http_error": e.status, "detail": e.detail}),
                }
            }
            "delete_file" => {
                let f = musaic_db::findings::get_finding(conn, st["finding_id"].as_i64().unwrap())
                    .unwrap();
                let trash = sb.root.join("trash");
                let r = trash_duplicate_file(
                    conn,
                    &sb.lib_s,
                    &trash,
                    f.as_ref(),
                    st["filename"].as_str().unwrap(),
                    || panic!("no trash collision expected"),
                );
                let mut v = match r {
                    Ok(t) => json!({
                        "ok": true,
                        "message": t.message,
                        "trashed_to": t.trashed_to.map(|p| musaic_testdata::replace_root(&p.to_string_lossy(), &trash.to_string_lossy(), "<TRASH>")),
                    }),
                    Err(e) => json!({"http_error": e.status, "detail": sb.unsub(&e.detail)}),
                };
                v["trash"] = json!(trash_listing(&trash));
                v
            }
            "fingerprints" => {
                let mut stmt = conn
                    .prepare(
                        "SELECT album_id, filename FROM track_fingerprints ORDER BY album_id, filename",
                    )
                    .unwrap();
                let rows: Vec<J> = stmt
                    .query_map([], |r| {
                        Ok(json!({"album_id": r.get::<_, String>(0)?, "filename": r.get::<_, String>(1)?}))
                    })
                    .unwrap()
                    .collect::<rusqlite::Result<_>>()
                    .unwrap();
                J::Array(rows)
            }
            "albums" => {
                let mut stmt = conn
                    .prepare("SELECT id, missing_since FROM albums ORDER BY id")
                    .unwrap();
                let rows: Vec<J> = stmt
                    .query_map([], |r| {
                        Ok(json!({"id": r.get::<_, String>(0)?, "missing_since": r.get::<_, Option<String>>(1)?}))
                    })
                    .unwrap()
                    .collect::<rusqlite::Result<_>>()
                    .unwrap();
                J::Array(rows)
            }
            "summary" => {
                let mut v = J::Array(
                    list_checks_with_summary(conn, &env, musaic_core::AiWording::NamesAi).unwrap(),
                );
                for c in v.as_array_mut().unwrap() {
                    if let Some(r) = c["last_run"].as_object_mut() {
                        r.remove("duration_ms");
                        let lp = r["library_path"].as_str().unwrap().to_string();
                        r.insert("library_path".into(), json!(sb.unsub(&lp)));
                    }
                }
                v
            }
            other => panic!("unknown op {other}"),
        };
        let probe = std::mem::take(&mut *log.lock().unwrap());
        let mut step = st.clone();
        let o = step.as_object_mut().unwrap();
        for k in ["result", "findings", "runs", "probe"] {
            o.remove(k);
        }
        o.insert("result".into(), result);
        o.insert("findings".into(), findings_json(&sb, conn));
        o.insert("runs".into(), runs_json(&sb, conn));
        o.insert("probe".into(), J::Array(probe));
        out.push(step);
    }
    out
}

/// Delta, applied to the Python app's record of a scenario so it states
/// what Rust must produce. (1) Probes inside a sweep see the flag set.
/// (2) The nested sweep the Python app ran from inside the `nested` check is
/// removed from the probe log and the check_runs table (its rows are the
/// `n_sweep` runs right after the outer `nested` row); ids are renumbered.
/// (3) With rows removed, clock values shift, so timestamps of runs and
/// findings are blanked in such scenarios.
/// Delta: a failed run counts the findings it upserted before failing. The
/// fake `boom` check emits one finding and then fails; the Python app left
/// its run's `issue_count` at 0. Every failed `boom` run row (and a step's
/// returned run): 0 -> 1.
fn failed_run_count_delta(mut steps: Vec<J>) -> Vec<J> {
    let mut changed = 0;
    let mut fix = |r: &mut J| {
        if r["check_id"] == "boom" && r["status"] == "failed" {
            assert_eq!(r["issue_count"], 0, "Python app record");
            r["issue_count"] = json!(1);
            changed += 1;
        }
    };
    for st in &mut steps {
        for r in st["runs"].as_array_mut().into_iter().flatten() {
            fix(r);
        }
        if let Some(r) = st["result"].get_mut("run") {
            fix(r);
        }
    }
    assert!(changed > 0, "failed-run delta changed nothing");
    steps
}

fn post_scan_guard_delta(steps: &[J], n_sweep: usize) -> Vec<J> {
    let had_nested = steps.iter().any(|s| {
        s["probe"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["check"] == "nested")
    });
    let mut removed_ids: Vec<i64> = Vec::new();
    let mut out = Vec::new();
    for st in steps {
        let mut st = st.clone();
        let mut probe: Vec<J> = st["probe"].as_array().unwrap().clone();
        if st["op"] == "post_scan" {
            for p in &mut probe {
                if p.get("in_post_scan_rerun").is_some() {
                    p["in_post_scan_rerun"] = json!(true);
                }
            }
            if let Some(i) = probe.iter().position(|p| p["check"] == "nested") {
                let j = probe
                    .iter()
                    .rposition(|p| p["check"] == "nested-returned")
                    .unwrap();
                probe.drain(i + 1..j);
                // The runs of this step: drop the n_sweep rows after the outer nested row.
                let runs = st["runs"].as_array().unwrap();
                let outer = runs
                    .iter()
                    .filter(|r| {
                        r["check_id"] == "nested"
                            && !removed_ids.contains(&r["id"].as_i64().unwrap())
                    })
                    .map(|r| r["id"].as_i64().unwrap())
                    .find(|id| {
                        !removed_ids.contains(id)
                            && *id > removed_ids.iter().copied().max().unwrap_or(0)
                    })
                    .unwrap();
                removed_ids.extend(outer + 1..=outer + n_sweep as i64);
            }
        }
        st["probe"] = J::Array(probe);
        let mut runs: Vec<J> = st["runs"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| !removed_ids.contains(&r["id"].as_i64().unwrap()))
            .cloned()
            .collect();
        if had_nested {
            for (i, r) in runs.iter_mut().enumerate() {
                r["id"] = json!(i as i64 + 1);
                r["started_at"] = J::Null;
                r["completed_at"] = J::Null;
            }
            for f in st["findings"].as_array_mut().unwrap() {
                for k in ["first_seen_at", "last_seen_at", "resolved_at"] {
                    if !f[k].is_null() {
                        f[k] = json!("<T>");
                    }
                }
            }
        }
        st["runs"] = J::Array(runs);
        out.push(st);
    }
    out
}

fn blank_times(steps: &mut [J]) {
    for st in steps {
        for r in st["runs"].as_array_mut().unwrap() {
            r["started_at"] = J::Null;
            r["completed_at"] = J::Null;
        }
        for f in st["findings"].as_array_mut().unwrap() {
            for k in ["first_seen_at", "last_seen_at", "resolved_at"] {
                if !f[k].is_null() {
                    f[k] = json!("<T>");
                }
            }
        }
    }
}

fn scenario(id: &str) -> J {
    let g = common::golden("scenarios.json");
    g["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == id)
        .unwrap_or_else(|| panic!("no scenario {id}"))
        .clone()
}

/// The bus event the Python app's delete-file endpoint publishes after its
/// rescan belongs to the API layer (this crate stops after trash + row
/// delete), so `published` is dropped from the Python app's delete_file results.
fn drop_published(steps: &[J]) -> Vec<J> {
    steps
        .iter()
        .map(|s| {
            let mut s = s.clone();
            if s["op"] == "delete_file" {
                s["result"].as_object_mut().unwrap().remove("published");
            }
            s
        })
        .collect()
}

fn assert_steps(id: &str, got: &[J], want: &[J]) {
    assert_eq!(got.len(), want.len(), "{id}: step count");
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        for k in ["result", "findings", "runs", "probe"] {
            // The recorded messages carry macOS's text for ENOENT.
            let w_k: J =
                serde_json::from_str(&musaic_testdata::os_error_text(&w[k].to_string())).unwrap();
            assert_eq!(g[k], w_k, "{id}: step {i} ({}) {k}", w["op"]);
        }
    }
}

fn exact(id: &str) {
    let sc = scenario(id);
    let got = replay(&sc);
    assert_steps(id, &got, sc["steps"].as_array().unwrap());
}

/// Upsert key (check_id, album_id, summary), last_seen bump,
/// detail/severity refresh, resolved_at cleared on re-detection, ignore kept
/// and never auto-resolved, unknown finding id.
#[test]
fn lifecycle() {
    exact("lifecycle");
}

/// Album-scoped runs resolve only the listed albums; SQL-pure checks
/// still upsert every album; empty / unknown album lists; unknown check;
/// incremental never resolves.
#[test]
fn album_scoped() {
    exact("album-scoped");
}

/// One field of one finding row in a step of the Python app's record: assert the
/// recorded value, then write the new one.
fn finding_delta(st: &mut J, finding_id: i64, field: &str, old: J, new: J) {
    let f = st["findings"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|f| f["id"] == finding_id)
        .unwrap_or_else(|| panic!("no finding {finding_id} in the record"));
    assert_eq!(
        f[field], old,
        "finding {finding_id}.{field}: Python app record"
    );
    f[field] = new;
}

/// The Python app's record of a scenario with `edit(step_index, step)` applied
/// to every step; fails unless the edits changed something.
fn with_delta(sc: &J, edit: impl Fn(usize, &mut J)) -> Vec<J> {
    let steps = sc["steps"].as_array().unwrap();
    let out: Vec<J> = steps
        .iter()
        .enumerate()
        .map(|(i, st)| {
            let mut st = st.clone();
            edit(i, &mut st);
            st
        })
        .collect();
    assert_ne!(&out, steps, "the delta changed nothing");
    out
}

/// Missing albums are out of scope.albums; an album-scoped run
/// naming one resolves nothing for it.
///
/// Delta: the Python app's library-wide run at
/// step 4 resolved a2's finding while a2 was Missing. Steps 4-5: finding 2
/// `resolved_at` "2026-01-01T00:00:14Z" -> null. From step 6 (a2 back, run
/// again) the record is the Python app's.
#[test]
fn missing_album() {
    let sc = scenario("missing-album");
    let want = with_delta(&sc, |i, st| {
        if (4..=5).contains(&i) {
            finding_delta(st, 2, "resolved_at", json!("2026-01-01T00:00:14Z"), J::Null);
        }
    });
    assert_steps("missing-album", &replay(&sc), &want);
}

/// Library-wide runs of a scope.albums check
/// (track-gaps) and a SQL-pure one (unidentified-album) while a2 and a3 are
/// Missing; a2's findings are ignored.
///
/// Delta, from the Python app's record:
/// - steps 5-7: finding 3 (track-gaps, a3) `resolved_at`
///   "2026-01-01T00:00:16Z" -> null;
/// - steps 6-8: finding 6 (unidentified-album, a3) `resolved_at`
///   "2026-01-01T00:00:20Z" -> null (step 8 runs track-gaps only; step 9
///   re-detects it, as the Python app does).
///
/// No other difference: the ignored findings 2 and 5 keep their ignore and
/// are not resolved (as in the Python app), no Missing album's `last_seen_at`
/// moves while it is Missing, and step 9 equals the Python app.
#[test]
fn missing_album_ignored() {
    let sc = scenario("missing-album-ignored");
    let want = with_delta(&sc, |i, st| {
        if (5..=7).contains(&i) {
            finding_delta(st, 3, "resolved_at", json!("2026-01-01T00:00:16Z"), J::Null);
        }
        if (6..=8).contains(&i) {
            finding_delta(st, 6, "resolved_at", json!("2026-01-01T00:00:20Z"), J::Null);
        }
    });
    let got = replay(&sc);
    assert_steps("missing-album-ignored", &got, &want);
    // Spelled out on the Rust result: while Missing (steps 5-7) the Missing
    // albums' findings are exactly as they were before (step 4).
    let before: Vec<J> = got[4]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["album_id"] != "a1")
        .cloned()
        .collect();
    for st in &got[5..=7] {
        let now: Vec<J> = st["findings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f["album_id"] != "a1")
            .cloned()
            .collect();
        assert_eq!(now, before, "Missing albums' findings untouched");
    }
}

/// On-disk fix + album-scoped rerun resolves (filename-mismatch);
/// plan preview re-derived after the fix is empty.
#[test]
fn fix_on_disk_filename() {
    exact("fix-on-disk-filename");
}

/// Inconsistent-artist-sort finding hangs on the representative
/// album; scoped rerun of another member does not resolve it.
///
/// Delta: inconsistent-artist-sort sees the whole library on every run,
/// so the album-scoped rerun for m2 (step 2) resolves the cluster's finding
/// stored on m1; the Python app left it open until the library-wide run of
/// step 3. Steps 2-3: finding 1 `resolved_at` -> "2026-01-01T00:00:05Z" (from
/// null at step 2, from "2026-01-01T00:00:08Z" at step 3).
#[test]
fn fix_artist_sort() {
    let sc = scenario("fix-artist-sort");
    let want = with_delta(&sc, |i, st| match i {
        2 => finding_delta(st, 1, "resolved_at", J::Null, json!("2026-01-01T00:00:05Z")),
        3 => finding_delta(
            st,
            1,
            "resolved_at",
            json!("2026-01-01T00:00:08Z"),
            json!("2026-01-01T00:00:05Z"),
        ),
        _ => {}
    });
    assert_steps("fix-artist-sort", &replay(&sc), &want);
}

/// Preview contract: derive_cluster / cluster-preview after
/// partial deletion, full resolution, unrelated cluster, empty original,
/// missing finding; then the rerun resolves.
#[test]
fn duplicates_preview_and_fix() {
    exact("duplicates-preview-and-fix");
}

/// post_scan.py: the real registry's five runs_during_scan checks
/// in order, and the GET /api/checks summary (meta table, open counts, last run).
#[test]
fn default_post_scan_and_summary() {
    exact("default-post-scan");
}

/// A failing check: the failing run is 'failed' with the message,
/// its partial finding stays, its stale finding is not resolved, and the
/// sweep continues. Deltas: the probe sees the flag; the failed run counts
/// its partial finding.
#[test]
fn raising_check_does_not_stop_the_sweep() {
    let sc = scenario("raising-check");
    let got = replay(&sc);
    let want = failed_run_count_delta(post_scan_guard_delta(sc["steps"].as_array().unwrap(), 4));
    assert_steps("raising-check", &got, &want);
}

/// Guard semantics; delta as documented on `post_scan_guard_delta`.
#[test]
fn post_scan_guard() {
    let sc = scenario("post-scan-guard");
    let mut got = replay(&sc);
    let want = failed_run_count_delta(post_scan_guard_delta(sc["steps"].as_array().unwrap(), 4));
    blank_times(&mut got);
    assert_steps("post-scan-guard", &got, &want);
    // The Python app really did run a second sweep (so the delta is not vacuous).
    let python_runs = sc["steps"][0]["runs"].as_array().unwrap().len();
    assert_eq!(python_runs, 8);
    assert_eq!(got[0]["runs"].as_array().unwrap().len(), 4);
}

/// Duplicate delete-file body (trash + fingerprint row): refusals, the move
/// into the trash, the already-gone path, the containment
/// check; the rerun then resolves the finding.
#[test]
fn duplicates_delete_file() {
    let sc = scenario("duplicates-delete-file");
    let got = replay(&sc);
    assert_steps(
        "duplicates-delete-file",
        &got,
        &drop_published(sc["steps"].as_array().unwrap()),
    );
}

/// Delete-file while the album directory is
/// unreachable. Deltas from the Python app's record:
/// - step 2 result: {ok: true, message: "02.flac was already gone ...",
///   trashed_to: null, published: [AlbumDeleted]} -> {http_error: 409,
///   detail: "Album directory ... cannot be reached ...; nothing was
///   deleted. ..."} (trash listing unchanged: empty);
/// - step 3 fingerprints: [] -> both rows (01.flac, 02.flac) kept;
/// - step 4 albums: [] -> the a1 row kept (the Python app's rescan deleted it;
///   that step belongs to the API layer, which must not run after a refusal).
///
/// Findings and runs equal the Python app's.
#[test]
fn duplicates_delete_file_unreachable() {
    let sc = scenario("duplicates-delete-file-unreachable");
    let want = with_delta(&sc, |i, st| match i {
        2 => {
            let r = &st["result"];
            assert_eq!(r["ok"], true);
            assert!(r["message"].as_str().unwrap().contains("already gone"));
            assert_eq!(r["published"], json!(["AlbumDeleted"]));
            assert_eq!(r["trash"], json!([]));
            st["result"] = json!({
                "http_error": 409,
                "detail": "Album directory '<LIB>/Dupes' cannot be reached (No such file or \
            directory (os error 2)); nothing was deleted. Reconnect the drive and try again.",
                "trash": [],
            });
        }
        3 => {
            assert_eq!(st["result"], json!([]));
            st["result"] = json!([
                {"album_id": "a1", "filename": "01.flac"},
                {"album_id": "a1", "filename": "02.flac"},
            ]);
        }
        4 => {
            assert_eq!(st["result"], json!([]));
            st["result"] = json!([{"id": "a1", "missing_since": null}]);
        }
        _ => {}
    });
    assert_steps("duplicates-delete-file-unreachable", &replay(&sc), &want);
}

/// Library-wide runs for two libraries sharing one
/// database (`<LIB>` holds a1, `<LIB>-other` holds b1), with a scope.albums
/// check (track-gaps) and a SQL-pure one (unidentified-album). Scenario
/// `two-libraries`, recorded to put the Python app's behaviour on record.
///
/// Kept from the Python app (no delta): the library-level finding 1 (album_id
/// NULL) and finding 2 (album row "gone" does not exist) are resolved by the
/// first library-wide run, which is a run for `<LIB>-other`: findings with no
/// album or no album row are resolved by a library-wide run of any library.
///
/// Delta, from the Python app's record; each is a
/// finding of one library that the Python app resolved during a run for the other:
/// - steps 1-3: finding 3 (track-gaps, b1) `resolved_at`
///   "2026-01-01T00:00:06Z" -> null (step 4 re-detects it in its own library);
/// - step 5: finding 3 `resolved_at` "2026-01-01T00:00:22Z" -> null;
/// - steps 3-5: finding 5 (unidentified-album, a1) `resolved_at`
///   "2026-01-01T00:00:14Z" -> null;
/// - step 4: finding 4 (track-gaps, a1) `resolved_at`
///   "2026-01-01T00:00:18Z" -> null (step 5 re-detects it).
#[test]
fn two_libraries() {
    let sc = scenario("two-libraries");
    let want = with_delta(&sc, |i, st| {
        if (1..=3).contains(&i) {
            finding_delta(st, 3, "resolved_at", json!("2026-01-01T00:00:06Z"), J::Null);
        }
        if i == 5 {
            finding_delta(st, 3, "resolved_at", json!("2026-01-01T00:00:22Z"), J::Null);
        }
        if (3..=5).contains(&i) {
            finding_delta(st, 5, "resolved_at", json!("2026-01-01T00:00:14Z"), J::Null);
        }
        if i == 4 {
            finding_delta(st, 4, "resolved_at", json!("2026-01-01T00:00:18Z"), J::Null);
        }
    });
    let got = replay(&sc);
    assert_steps("two-libraries", &got, &want);
    // Spelled out on the Rust result: no run for one library ever resolves a
    // finding of the other library's album.
    for (i, st) in got.iter().enumerate() {
        let lib = sc["steps"][i]["library"].as_str().unwrap_or("<LIB>");
        let other_album = if lib == "<LIB>" { "b1" } else { "a1" };
        for f in st["findings"].as_array().unwrap() {
            if f["album_id"] == other_album {
                let before = i.checked_sub(1).and_then(|p| {
                    got[p]["findings"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|g| g["id"] == f["id"])
                });
                if let Some(b) = before {
                    assert_eq!(
                        f["resolved_at"], b["resolved_at"],
                        "step {i}: finding {} moved",
                        f["id"]
                    );
                }
            }
        }
    }
}
