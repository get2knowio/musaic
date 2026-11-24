//! The check fix routes (per-finding fix, quick-fix-all
//! SSE, filename-mismatch apply, delete-duplicate, artist-sort apply)
//! replayed through this router against
//! testdata/golden/services/check_fixes.json (recorded from the Python app's
//! real routes through TestClient, resolver / fingerprinting /
//! MusicBrainz stubbed; two runs byte-identical).
//!
//! Same library, setup through the same routes (config, scan, seeded
//! fingerprints and anchor by SQL, every check run through
//! `POST /api/checks/{id}/run`), the Python app's stubs fed through the
//! builder's seams (`fix_sources`, `checks_resolution`). Then every step in
//! order, the four recorded 422 cases included: status, body (JSON, or the
//! SSE text for quick-fix-all), bus events and the state after the step.
//! Finding ids are mapped by (check, album, summary).
//!
//! The eight steps where the Python app enqueued an AI job (or answered
//! its conflict) record a research request since every build delegates
//! (the golden's `_meta.rust_changes`): `{ok, message, job_id}` with the
//! request group's id masked `<JOB>` and checked to name a research group,
//! the `AgentStateChanged` work event (item ids masked `<WORK>`), and under
//! `ai_jobs` the request groups in job vocabulary (album, job type, hint,
//! status; `RESEARCH_REQUESTS`) after every step. The fix that writes first
//! and then requests research ("fix tags-out-of-sync resolver") prefixes
//! what it wrote. A second Fix of the same album and kind returns the
//! unresolved request it already has. The state is built without lifespan,
//! as in the Python app's recording.
//!
//! Enumerated deltas (each must be used; anything else fails):
//! - after "rename files" the Echo sidecar follows the moved
//!   track_tags rows.
//! - "delete unreachable album" is refused with 409; nothing
//!   changes.
//! - the "rename collision" refusal also carries `reason_code` and
//!   `user_message` beside its recorded `detail`.
//! - `dir_migration` also counts `album_blocked_mb_release_groups`.
//! - "delete last copy (album empties)" keeps Juliet's row and marks
//!   it Missing (AlbumMarkedMissing; its tags and surviving fingerprint
//!   stay): musaic-services tests/deltas/check_fixes.rs. The harness then
//!   removes that Missing album through `DELETE /api/albums/{id}` (the user's
//!   "Remove from library"), which must give exactly the Python app's recorded
//!   state (AlbumDeleted, as the Python app published), so later steps compare
//!   unchanged.
//! - quick-fix-all's
//!   unidentified-album skip messages lose ", or use the per-finding Fix to
//!   escalate to AI": musaic-services tests/deltas/check_fixes.rs.
//! - quick-fix-all reruns its check over the albums whose fix reported
//!   success: `done` counts only the findings that are gone as `fixed` and
//!   the rest as `still_open`, and the findings the rerun resolved (or
//!   reopened under a new summary) are so from that step on:
//!   musaic-services tests/deltas/check_fixes.rs (`STILL_OPEN`).
//! - Harness step (reruns really run the checks): after an artist-sort bulk write the
//!   test runs the scan-time check sweep the `BulkBatchCompleted`
//!   subscriber runs in the app (subscribers are off here, as in the
//!   Python app's no-lifespan run, where the sweep ran inside the apply).
//!
//! Every fix route is refused with 409 while a restore holds the
//! gate, and quick-fix-all holds the gate until its stream's work ends.

#[path = "../../musaic-services/tests/deltas/check_fixes.rs"]
mod cf_deltas;
mod common;
mod http_world;

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use http_world::{World, svc};
use musaic_checks::{CheckEnv, Fact, Resolution, ResolutionSource};
use musaic_services::check_fixes::{
    Anchor, CoverMatch, FingerprintFailure, Fingerprinted, FixSources, TracklistMatch,
};
use regex::Regex;
use rusqlite::Connection;
use serde_json::{Value, json};

const TAG_KEYS: [&str; 5] = [
    "TRACKNUMBER",
    "TITLE",
    "ALBUMARTIST",
    "ALBUMARTISTSORT",
    "ARTISTSORT",
];
const CLUSTER_CHECK: &str = "inconsistent-artist-sort";

/// The research requests in the old job vocabulary the golden records under
/// `ai_jobs`: one row per request group (one review card), with the album,
/// the job type its kinds map back to, the hint and the job-style status.
const RESEARCH_REQUESTS: &str = "SELECT album_id, \
       CASE WHEN SUM(kind = 'cover.research') = 0 THEN 'metadata' \
            WHEN SUM(kind <> 'cover.research') = 0 THEN 'cover' \
            ELSE 'metadata+cover' END, \
       COALESCE(json_extract(MIN(input_json), '$.hint'), ''), \
       CASE MIN(state) WHEN 'queued' THEN 'queued' WHEN 'claimed' THEN 'running' \
            WHEN 'running' THEN 'running' WHEN 'awaiting_review' THEN 'completed' \
            ELSE MIN(state) END \
     FROM delegated_work GROUP BY group_id";

/// The generator's stubs (musaic-services' golden test mirrors the same).
#[derive(Default)]
struct Stubs {
    anchored: Mutex<HashMap<String, (String, String)>>,
}

impl ResolutionSource for Stubs {
    fn resolve(&self, _conn: &Connection, album_id: &str) -> Result<Resolution, String> {
        let Some((rg, title)) = self.anchored.lock().unwrap().get(album_id).cloned() else {
            return Ok(Resolution::default());
        };
        let f = |v: &str| Fact {
            value: v.into(),
            source: "musicbrainz".into(),
        };
        Ok(Resolution {
            anchor: Some(f(&rg)),
            canonical_title: Some(f(&title)),
            ..Resolution::default()
        })
    }
}

impl FixSources for Stubs {
    fn fingerprint_available(&self) -> bool {
        false
    }
    fn fingerprint_album(
        &self,
        _c: &mut Connection,
        _a: &str,
        _p: &str,
    ) -> Result<Vec<Fingerprinted>, FingerprintFailure> {
        Err(FingerprintFailure {
            code: "no-api-key".into(),
            detail: "AcoustID API key not configured".into(),
        })
    }
    fn resolve_anchor(
        &self,
        _c: &mut Connection,
        album_id: &str,
    ) -> Result<Option<Anchor>, String> {
        Ok(self
            .anchored
            .lock()
            .unwrap()
            .get(album_id)
            .map(|(rg, _)| Anchor {
                value: rg.clone(),
                strategy: "cache".into(),
                confidence: Some(0.95),
            }))
    }
    fn find_cover(&self, _c: &mut Connection, _a: &str) -> CoverMatch {
        CoverMatch::default()
    }
    fn find_tracklist(&self, _c: &mut Connection, _a: &str) -> TracklistMatch {
        TracklistMatch::default()
    }
}

struct Run {
    w: World,
    stubs: Arc<Stubs>,
    pauses: Arc<AtomicUsize>,
}

impl Run {
    fn n(&self, s: &str) -> String {
        let cfg = self.w.cfg.to_string_lossy();
        self.w
            .norm
            .str(&musaic_testdata::replace_root(s, cfg.as_ref(), "<CFG>"))
    }

    fn nv(&self, v: &Value) -> Value {
        match v {
            Value::String(s) => Value::String(self.n(s)),
            Value::Array(a) => Value::Array(a.iter().map(|x| self.nv(x)).collect()),
            Value::Object(o) => {
                Value::Object(o.iter().map(|(k, x)| (k.clone(), self.nv(x))).collect())
            }
            other => other.clone(),
        }
    }

    /// The bus events since the last call; the work item ids an
    /// `AgentStateChanged` names are random, masked `<WORK>` as the golden
    /// records them.
    fn events(&self) -> Vec<Value> {
        let mut l = self.w.log.lock().unwrap();
        let out = l
            .iter()
            .map(|e| {
                let mut v = self.nv(&svc::event_json(e));
                if v["type"] == "AgentStateChanged"
                    && let Some(ids) = v["ids"].as_array_mut()
                {
                    ids.iter_mut().for_each(|i| *i = json!("<WORK>"));
                }
                v
            })
            .collect();
        l.clear();
        out
    }

    fn tree(&self) -> Vec<Value> {
        fn walk(root: &Path, dir: &Path, out: &mut Vec<Value>) {
            let mut entries: Vec<_> = std::fs::read_dir(dir)
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect();
            entries.sort();
            for p in entries {
                if p.is_dir() {
                    walk(root, &p, out);
                    continue;
                }
                let rel = musaic_testdata::slash_path(p.strip_prefix(root).unwrap());
                if rel.to_lowercase().ends_with(".flac") {
                    let meta = musaic_scan::flacmeta::read(&p).unwrap();
                    let mut tags = serde_json::Map::new();
                    for k in TAG_KEYS {
                        let vals: Vec<&str> = meta.values(k).collect();
                        if !vals.is_empty() {
                            tags.insert(k.into(), json!(vals));
                        }
                    }
                    out.push(json!([rel, tags]));
                } else {
                    out.push(json!([rel, std::fs::read_to_string(&p).unwrap()]));
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.w.lib, &self.w.lib, &mut out);
        out
    }

    fn trash(&self) -> Vec<Value> {
        fn walk(root: &Path, dir: &Path, out: &mut Vec<Value>) {
            let Ok(rd) = std::fs::read_dir(dir) else {
                return;
            };
            for e in rd {
                let p = e.unwrap().path();
                if p.is_dir() {
                    walk(root, &p, out);
                } else {
                    out.push(json!(musaic_testdata::slash_path(
                        p.strip_prefix(root).unwrap()
                    )));
                }
            }
        }
        let root = self.w.cfg.join("trash");
        let mut out = Vec::new();
        walk(&root, &root, &mut out);
        out
    }

    fn rows(&self, conn: &Connection, sql: &str) -> Vec<Value> {
        let mut st = conn.prepare(sql).unwrap();
        let n = st.column_count();
        st.query_map([], |r| {
            Ok(Value::Array(
                (0..n)
                    .map(|i| match r.get_ref(i).unwrap() {
                        rusqlite::types::ValueRef::Null => Value::Null,
                        rusqlite::types::ValueRef::Integer(x) => json!(x),
                        rusqlite::types::ValueRef::Real(x) => json!(x),
                        rusqlite::types::ValueRef::Text(t) => {
                            Value::String(self.n(&String::from_utf8_lossy(t)))
                        }
                        rusqlite::types::ValueRef::Blob(_) => unreachable!(),
                    })
                    .collect(),
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    }

    fn state(&self) -> Value {
        let conn = self.w.conn();
        json!({
            "tree": self.tree(),
            "trash": self.trash(),
            "albums": self.rows(&conn, "SELECT id, path, dir_name, track_count, album_artist_sort, missing_since FROM albums"),
            "track_tags": self.rows(&conn, "SELECT album_id, filename, tag FROM track_tags"),
            "track_fingerprints": self.rows(&conn, "SELECT album_id, filename FROM track_fingerprints"),
            "ai_jobs": self.rows(&conn, RESEARCH_REQUESTS),
            "open_findings": self.rows(&conn, "SELECT check_id, album_id, summary FROM check_findings WHERE resolved_at IS NULL AND ignored_at IS NULL"),
        })
    }
}

/// Order-insensitive view of a state (the cluster finding's album masked,
/// It depends on where the library lives).
fn canon(v: &Value) -> Value {
    let mut v = v.clone();
    if let Some(Value::Array(rows)) = v.get_mut("open_findings") {
        for r in rows.iter_mut() {
            if r[0] == json!(CLUSTER_CHECK) {
                r[1] = json!("<cluster member>");
            }
        }
    }
    if let Value::Object(o) = &mut v {
        for (_, x) in o.iter_mut() {
            if let Value::Array(a) = x {
                a.sort_by_key(|e| e.to_string());
            }
        }
    }
    v
}

async fn setup(g: &Value) -> Run {
    setup_with(g, None).await
}

async fn setup_with(g: &Value, pause: Option<musaic_api::state::Pause>) -> Run {
    let mut extra = Vec::new();
    for st in g["steps"].as_array().unwrap() {
        if let Some(n) = st["args"]["dir_rename"]["new"].as_str() {
            extra.push(n.to_string());
        }
    }
    let stubs = Arc::new(Stubs::default());
    let pauses = Arc::new(AtomicUsize::new(0));
    let (s1, s2, p) = (stubs.clone(), stubs.clone(), pauses.clone());
    let w = World::new(g, &extra, move |b| {
        let counting: musaic_api::state::Pause = Arc::new(move || {
            p.fetch_add(1, Ordering::SeqCst);
        });
        b.fix_sources(s1)
            .checks_resolution(s2)
            .quick_fix_pause(pause.unwrap_or(counting))
    });
    w.configure_and_scan().await;
    {
        let conn = w.conn();
        for s in g["seeded_fingerprints"].as_array().unwrap() {
            let (d, f, name) = (
                s[0].as_str().unwrap(),
                s[1].as_str().unwrap(),
                s[2].as_str().unwrap(),
            );
            conn.execute(
                "INSERT INTO track_fingerprints (album_id, filename, fingerprint, duration_s, \
                 acoustid_recording_mbid, acoustid_score, last_computed_at) VALUES (?1,?2,?3,3.0,NULL,NULL,'2001-01-01T00:00:00Z')",
                rusqlite::params![w.id(d), f, g["fingerprints"][name].as_str().unwrap()],
            )
            .unwrap();
        }
        for (d, rg) in g["anchored"].as_object().unwrap() {
            conn.execute(
                "UPDATE albums SET mb_release_group_mbid = ?1 WHERE id = ?2",
                [rg.as_str().unwrap(), &w.id(d)],
            )
            .unwrap();
            let title = g["resolved"][d]["title"].as_str().unwrap().to_string();
            stubs
                .anchored
                .lock()
                .unwrap()
                .insert(w.id(d), (rg.as_str().unwrap().to_string(), title));
        }
    }
    let order: Vec<String> = w.get("/api/checks").await.json()["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["meta"]["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(json!(order), g["check_order"]);
    for c in &order {
        let r = w
            .post(
                &format!("/api/checks/{c}/run"),
                Some(&json!({"scope": "all"})),
            )
            .await;
        assert_eq!(r.status, 200, "{c}: {}", r.text());
    }
    let run = Run { w, stubs, pauses };
    run.events();
    run
}

/// The Python app's finding ids <-> ours, by (check, album, summary).
struct Ids {
    to_ours: HashMap<i64, i64>,
    to_python: HashMap<i64, i64>,
}

impl Ids {
    fn new(run: &Run, g: &Value) -> Ids {
        let ours = run.rows(
            &run.w.conn(),
            "SELECT id, check_id, album_id, summary FROM check_findings ORDER BY id",
        );
        let mut ids = Ids {
            to_ours: HashMap::new(),
            to_python: HashMap::new(),
        };
        for f in g["setup_findings"].as_array().unwrap() {
            let m = ours
                .iter()
                .find(|o| {
                    o[1] == f[1] && (o[2] == f[2] || f[1] == json!(CLUSTER_CHECK)) && o[3] == f[3]
                })
                .unwrap_or_else(|| panic!("no Rust finding for {f}"));
            ids.to_ours
                .insert(f[0].as_i64().unwrap(), m[0].as_i64().unwrap());
            ids.to_python
                .insert(m[0].as_i64().unwrap(), f[0].as_i64().unwrap());
        }
        assert_eq!(ours.len(), ids.to_ours.len(), "same findings after setup");
        ids
    }

    /// The recorded request with its finding id mapped to ours.
    fn request(&self, req: &Value) -> Value {
        let re = Regex::new(r"/findings/(\d+)/").unwrap();
        let path = req["path"].as_str().unwrap();
        let mapped = re.replace(path, |c: &regex::Captures| {
            let o: i64 = c[1].parse().unwrap();
            format!("/findings/{}/", self.to_ours.get(&o).copied().unwrap_or(o))
        });
        let mut r = req.clone();
        r["path"] = json!(mapped);
        r
    }

    /// quick-fix-all frames with our finding ids mapped back.
    fn frames(&self, text: &str) -> String {
        let re = Regex::new(r#""finding_id": (\d+)"#).unwrap();
        re.replace_all(text, |c: &regex::Captures| {
            let i: i64 = c[1].parse().unwrap();
            format!("\"finding_id\": {}", self.to_python[&i])
        })
        .into_owned()
    }
}

/// The scan-time sweep the BulkBatchCompleted subscriber runs,
/// with new finding ids mapped in creation order.
fn sweep(run: &Run, ids: &mut Ids) {
    let conn = run.w.conn();
    let env = CheckEnv::new(run.stubs.clone());
    let lib = musaic_scan::py_resolve(&run.w.lib)
        .to_string_lossy()
        .into_owned();
    musaic_checks::run_post_scan_checks(&conn, &env, &lib, None);
    let mut fresh: Vec<i64> = run
        .rows(&conn, "SELECT id FROM check_findings ORDER BY id")
        .iter()
        .map(|r| r[0].as_i64().unwrap())
        .filter(|i| !ids.to_python.contains_key(i))
        .collect();
    fresh.sort();
    let mut next = ids.to_python.values().max().copied().unwrap_or(0);
    for id in fresh {
        next += 1;
        ids.to_python.insert(id, next);
        ids.to_ours.insert(next, id);
    }
}

// Every fix route (+ the enumerated deltas above).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn check_fix_routes_replay_the_python_app() {
    let g = svc::load_golden("check_fixes.json");
    let run = setup(&g).await;
    let mut ids = Ids::new(&run, &g);
    assert_eq!(
        canon(&run.state()),
        canon(&g["initial_state"]),
        "state after setup"
    );
    let mut used = BTreeMap::from([
        ("research requested", 0usize),
        ("sidecar after rename", 0),
        ("rename refusal sentence", 0),
        ("delete unreachable", 0),
        ("blocklist rows follow", 0),
        ("last copy keeps album", 0),
        ("no model backend", 0),
    ]);
    let mut sidecar_delta: Option<(String, String)> = None;
    let mut prev_state = g["initial_state"].clone();
    let mut pauses_seen = 0;
    let total_re = Regex::new(r#""total": (\d+)"#).unwrap();
    // quick-fix-all's rerun of its check: see the services replay.
    let mut rerun_resolved: Vec<Value> = Vec::new();
    let mut rerun_opened: Vec<Value> = Vec::new();
    let mut used_rerun = 0;
    let mut last_group: Option<String> = None;

    for st in g["steps"].as_array().unwrap() {
        let label = st["label"].as_str().unwrap();
        for act in st["before"].as_array().unwrap() {
            let p = |i: usize| run.w.lib.join(act[i].as_str().unwrap());
            match act[0].as_str().unwrap() {
                "unlink" => std::fs::remove_file(p(1)).unwrap(),
                "rename" => std::fs::rename(p(1), p(2)).unwrap(),
                other => panic!("unknown action {other}"),
            }
        }
        let order = match st["args"]["check_id"].as_str() {
            Some(c) if st["op"] == "quick_fix_all" => cf_deltas::walk_order(&run.w.conn(), c),
            _ => Vec::new(),
        };
        let open_before = run.state()["open_findings"].as_array().unwrap().clone();
        let got = run.w.send(&ids.request(&st["request"])).await;
        let mut body = if got.ctype().starts_with("text/event-stream") {
            json!({"sse": run.n(&ids.frames(&got.text()))})
        } else {
            run.nv(&got.json())
        };
        // The generator records job ids as <JOB>. Ours names the research
        // request group the Fix made or found (one review card).
        let group = body["job_id"].as_str().map(str::to_owned);
        if let Some(group) = &group {
            let items = musaic_db::research_jobs::group(&run.w.conn(), group).unwrap();
            assert!(
                !items.is_empty(),
                "{label}: {group} names no research request"
            );
            body["job_id"] = json!("<JOB>");
        }
        let mut step_resolved: Vec<Value> = Vec::new();
        if st["op"] == "quick_fix_all" {
            let after = run.state()["open_findings"].as_array().unwrap().clone();
            step_resolved = open_before
                .iter()
                .filter(|r| !after.contains(r))
                .cloned()
                .collect();
            rerun_opened.extend(after.into_iter().filter(|r| !open_before.contains(r)));
        }
        let events = run.events();
        let bulk = events
            .iter()
            .filter(|e| e["type"] == "BulkBatchCompleted")
            .count();
        let cluster_bulk = if st["op"] == "quick_fix_all" {
            bulk.saturating_sub(1)
        } else {
            bulk
        };
        let cluster_op = st["op"] == "apply_cluster_sort"
            || (st["op"] == "quick_fix_all" && st["args"]["check_id"] == CLUSTER_CHECK);
        if cluster_op && cluster_bulk > 0 {
            sweep(&run, &mut ids);
        }
        let state = canon(&run.state());

        let mut want_status = st["status"].clone();
        let mut want_body = st["body"].clone();
        let mut want_events = st["events"].clone();
        let mut want_state = st["state"].clone();

        // ---- enumerated deltas ----
        if st["unknowns"]
            .as_array()
            .unwrap()
            .contains(&json!("needs-ai-job"))
            || label == "fix tags-out-of-sync resolver"
        {
            // Recorded as a research request (the golden's `rust_changes`).
            *used.get_mut("research requested").unwrap() += 1;
            assert!(group.is_some(), "{label}: no research request");
            if label == "fix no-cover-art again (job conflict)" {
                // The unresolved request of that album and kind is returned,
                // not a second one.
                assert_eq!(group, last_group, "{label}: same request");
            }
            last_group.clone_from(&group);
        }
        if st["unknowns"]
            .as_array()
            .unwrap()
            .contains(&json!("delete-duplicate-unreachable"))
        {
            *used.get_mut("delete unreachable").unwrap() += 1;
            want_status = json!(409);
            let d = body["detail"].as_str().unwrap_or_default();
            assert!(
                d.starts_with("Album directory ") && d.contains("nothing was deleted"),
                "{label}: {d}"
            );
            want_body = body.clone();
            want_events = json!([]);
            for k in [
                "albums",
                "track_tags",
                "track_fingerprints",
                "open_findings",
                "trash",
            ] {
                want_state[k] = prev_state[k].clone();
            }
        }
        if label == "rename collision" {
            *used.get_mut("rename refusal sentence").unwrap() += 1;
            want_body["reason_code"] = json!("already_exists");
            want_body["user_message"] = json!(
                "A file named \u{201c}y.txt\u{201d} is already in the folder, so nothing was \
                 renamed. Rename or move that file, then try again."
            );
        }
        if label == "rename files" {
            *used.get_mut("sidecar after rename").unwrap() += 1;
            let echo = run.n(&run.w.id("Echo - Five [FLAC]"));
            let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for r in st["state"]["track_tags"].as_array().unwrap() {
                if r[0] == json!(echo) {
                    map.entry(r[1].as_str().unwrap().into())
                        .or_default()
                        .push(r[2].as_str().unwrap().into());
                }
            }
            sidecar_delta = Some((
                "Echo - Five [FLAC]/.media-manager-tags.json".into(),
                musaic_fsops::sidecar::render_sidecar(&map),
            ));
        }
        if label == cf_deltas::LAST_COPY_STEP {
            *used.get_mut("last copy keeps album").unwrap() += 1;
            let want = cf_deltas::last_copy_expected(st, &prev_state);
            want_events = want["events"].clone();
            for k in ["albums", "track_tags", "track_fingerprints"] {
                want_state[k] = want["state"][k].clone();
            }
        }
        if st["op"] == "quick_fix_all"
            && let Some(sse) = want_body["sse"].as_str()
        {
            // Without a model backend the skip advice loses its AI escalation.
            let (text, n) = cf_deltas::no_model_skip_message(
                sse,
                musaic_services::check_fixes::UNRESOLVED_SKIP_MESSAGE,
            );
            *used.get_mut("no model backend").unwrap() += n;
            let text = cf_deltas::done_with_still_open(&text, cf_deltas::still_open_for(label));
            // Finding order: see cf_deltas::walk_order.
            want_body["sse"] = json!(cf_deltas::progress_in_our_finding_order(&text, |p| {
                cf_deltas::walk_pos(&order, ids.to_ours.get(&p).copied().unwrap_or(p))
            }));
            let check = st["args"]["check_id"].as_str().unwrap();
            let conn = run.w.conn();
            cf_deltas::events_in_walk_order(&mut want_events, &order, |e| {
                let id = e.get("album_id")?.as_str()?;
                let dir = id.strip_prefix("<ID:")?.strip_suffix('>')?;
                conn.query_row(
                    "SELECT MIN(id) FROM check_findings WHERE check_id = ?1 AND album_id = ?2",
                    [check, run.w.id(dir).as_str()],
                    |r| r.get(0),
                )
                .unwrap()
            });
        }
        if want_body["dir_migration"]["albums"] == json!(1) {
            *used.get_mut("blocklist rows follow").unwrap() += 1;
            want_body["dir_migration"]["album_blocked_mb_release_groups"] = json!(0);
        }
        if let Some((path, text)) = &sidecar_delta {
            for e in want_state["tree"].as_array_mut().unwrap() {
                if e[0] == json!(path) {
                    e[1] = json!(text);
                }
            }
        }

        for r in &step_resolved {
            assert!(
                want_state["open_findings"].as_array().unwrap().contains(r),
                "{label}: {r} was open in the Python app's record"
            );
        }
        used_rerun += step_resolved.len();
        rerun_resolved.extend(step_resolved);
        cf_deltas::rerun_open_findings(&mut want_state, &rerun_resolved, &rerun_opened);
        assert_eq!(json!(got.status), want_status, "{label}: status ({body})");
        assert_eq!(body, want_body, "{label}: body");
        assert_eq!(json!(events), want_events, "{label}: events");
        assert_eq!(state, canon(&want_state), "{label}: state");
        if label == cf_deltas::LAST_COPY_STEP {
            // The user removes the Missing album: the Python app's own outcome.
            let juliet = run.w.id("Juliet - Ten [FLAC]");
            let r = run
                .w
                .send(&json!({"method": "DELETE", "path": format!("/api/albums/{juliet}")}))
                .await;
            assert_eq!(r.status, 200, "{}", r.text());
            assert_eq!(json!(run.events()), st["events"], "{label}: removal event");
            let mut bridged = want_state.clone();
            for k in ["albums", "track_tags", "track_fingerprints"] {
                bridged[k] = st["state"][k].clone();
            }
            assert_eq!(
                canon(&run.state()),
                canon(&bridged),
                "{label}: after removal"
            );
        }

        // As recorded: one pause between two findings, none
        // before the first.
        if st["op"] == "quick_fix_all" && st["status"] == 200 {
            let total = total_re
                .captures(st["body"]["sse"].as_str().unwrap())
                .unwrap()[1]
                .parse::<usize>()
                .unwrap();
            let now = run.pauses.load(Ordering::SeqCst);
            assert_eq!(
                now - pauses_seen,
                total.saturating_sub(1),
                "{label}: pauses"
            );
            pauses_seen = now;
        }
        prev_state = st["state"].clone();
    }
    assert_eq!(used["research requested"], 8, "research request steps");
    assert_eq!(used["sidecar after rename"], 1);
    assert_eq!(used["rename refusal sentence"], 1);
    assert_eq!(used["delete unreachable"], 1);
    assert_eq!(used["no model backend"], 11, "skipped unidentified albums");
    assert_eq!(used["blocklist rows follow"], 1);
    assert_eq!(used["last copy keeps album"], 1);
    assert!(used_rerun > 0, "quick-fix-all rerun delta unused");
    // The last stream's work ends just after its final frame.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while run.w.state.gate().snapshot() != (0, false) && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(run.w.state.gate().snapshot(), (0, false), "gate released");
}

/// One request per fix route, with a body that passes validation.
fn fix_requests(g: &Value) -> Vec<Value> {
    let mut seen = std::collections::BTreeSet::new();
    g["steps"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["layer"] != "http" && s["status"] != 422)
        .filter(|s| seen.insert(s["op"].as_str().unwrap().to_string()))
        .map(|s| s["request"].clone())
        .collect()
}

// Every fix route is refused (409) while a restore holds
// the gate, before anything is read or written; also
// quick-fix-all's unknown check (409 before the Python app's 404).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fix_routes_take_the_restore_gate() {
    let g = svc::load_golden("check_fixes.json");
    let run = setup(&g).await;
    let ids = Ids::new(&run, &g);
    let reqs = fix_requests(&g);
    assert_eq!(reqs.len(), 5, "{reqs:#?}");
    let before = canon(&run.state());
    let restore = run.w.state.gate().begin_restore().unwrap();
    for r in &reqs {
        let got = run.w.send(&ids.request(r)).await;
        // The busy 409 comes before the unknown-check 404.
        assert_eq!(got.status, 409, "{r}: {}", got.text());
        assert_eq!(got.json()["detail"], musaic_api::gate::WORK_REFUSED);
    }
    let qfa = json!({"method": "POST", "path": "/api/checks/track-gaps/quick-fix-all"});
    let got = run.w.send(&qfa).await;
    assert_eq!(got.status, 409);
    assert!(run.events().is_empty());
    assert_eq!(canon(&run.state()), before, "nothing changed");
    drop(restore);
}

// Quick-fix-all holds the gate for as long as its work runs, not
// as long as the client listens: while it is between two findings a restore
// is refused (409); a client that goes away does not release it early; the
// gate is free once the run ends, and BulkBatchCompleted is still published
// (the Python app's `finally`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn quick_fix_all_holds_the_gate_until_its_work_ends() {
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let g = svc::load_golden("check_fixes.json");
    let hold = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let entered = Arc::new(AtomicUsize::new(0));
    let (h, e) = (hold.clone(), entered.clone());
    let pause: musaic_api::state::Pause = Arc::new(move || {
        e.fetch_add(1, Ordering::SeqCst);
        while h.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    });
    let run = setup_with(&g, Some(pause)).await;
    // Releases the pause however this test ends (a failed assertion must
    // not leave the blocking task spinning, which would hang the runtime).
    struct Release(Arc<std::sync::atomic::AtomicBool>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.store(false, Ordering::SeqCst);
        }
    }
    let _release = Release(hold.clone());
    let open: i64 = run
        .w
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM check_findings WHERE check_id = 'no-cover-art' \
             AND resolved_at IS NULL AND ignored_at IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(open >= 2, "{open}");
    let resp = run
        .w
        .app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/checks/no-cover-art/quick-fix-all")
                .header("host", common::HOST)
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let mut body = resp.into_body();
    let first = body.frame().await.unwrap().unwrap().into_data().unwrap();
    assert!(String::from_utf8_lossy(&first).starts_with("event: start"));
    for _ in 0..2000 {
        if entered.load(Ordering::SeqCst) > 0 {
            break;
        }
        // The progress frame of the first finding must be read for the
        // run to reach the pause (one frame in flight).
        let _ = tokio::time::timeout(std::time::Duration::from_millis(5), body.frame()).await;
    }
    assert_eq!(entered.load(Ordering::SeqCst), 1, "paused between findings");
    assert_eq!(run.w.state.gate().snapshot(), (1, false));
    let r = run
        .w
        .post("/api/db/snapshots/library-x.db/restore", None)
        .await;
    assert_eq!(r.status, 409);
    assert_eq!(r.json()["detail"], musaic_api::gate::RESTORE_REFUSED);
    drop(body); // the client goes away mid-run
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert_eq!(run.w.state.gate().snapshot(), (1, false), "still running");
    hold.store(false, Ordering::SeqCst);
    for _ in 0..2000 {
        if run.w.state.gate().snapshot() == (0, false) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    assert_eq!(run.w.state.gate().snapshot(), (0, false));
    let evs = run.events();
    assert_eq!(
        evs.iter()
            .filter(|e| e["type"] == "BulkBatchCompleted")
            .count(),
        1,
        "{evs:?}"
    );
}
