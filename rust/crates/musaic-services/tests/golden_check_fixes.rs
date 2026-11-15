//! The check fix actions replayed against the Python app's record
//! (testdata/golden/services/check_fixes.json, written by
//! a generator that drove the Python app's
//! real routes through TestClient with the resolver, fingerprinting and
//! MusicBrainz stubbed; two runs byte-identical).
//!
//! Same library, same seeded fingerprints and anchor, the same checks run
//! library-wide; then every service-layer step in order, comparing status,
//! body, bus events and the state after the step (library tree with the
//! FLACs' tags, trash, album rows, track_tags, track_fingerprints, ai_jobs,
//! open findings). Finding ids are mapped by (check, album, summary).
//!
//! What the harness plays for the host, as in production:
//! - the AI queue (NeedsAiJob): one active job per album; a second request
//!   for the same album is the Python app's conflict answer;
//! - after an artist-sort bulk apply, the library-wide
//!   sweep of the scan-time checks (the Python app ran it inside the apply; in
//!   the Rust build the BulkBatchCompleted subscriber runs it).
//!
//! Enumerated deliberate changes (each must be used; anything else fails):
//! - after "rename files", the Echo sidecar is rewritten from the
//!   moved track_tags rows (the Python app left the old filenames in it);
//! - "delete unreachable album" is refused with 409; nothing changes
//!   (the Python app deleted the album row and its fingerprints);
//! - a directory rename's `dir_migration` also counts the
//!   repointed blocklist rows (`album_blocked_mb_release_groups`, 0 here),
//!   as musaic-fsops reports it (its golden test enumerates the same key);
//! - "delete last copy (album empties)" keeps Juliet's row and marks
//!   it Missing (AlbumMarkedMissing; its track tags and the surviving
//!   fingerprint stay): tests/deltas/check_fixes.rs. Then the harness plays
//!   the user removing that Missing album from the library
//!   (`DELETE /api/albums/{id}` -> `musaic_db::albums::delete_album`), which
//!   must give exactly the Python app's recorded state, so the later steps are
//!   compared unchanged;
//! - quick-fix-all's
//!   unidentified-album skip messages lose the Python app's ", or use the
//!   per-finding Fix to escalate to AI" (the Fix queues no AI job without a
//!   model backend): tests/deltas/check_fixes.rs.
//!
//! The `per-track-artist` section (its own library and database) is
//! replayed by [`tags_out_of_sync_applies_per_track_artist`], with no delta.
//!
//! Covered: route bodies and refusals, the events of each mutation, one
//! BulkBatchCompleted per bulk run, the rename glue, the trash, tags
//! following renames, and the unreachable-album refusal at the endpoint.

#[path = "deltas/check_fixes.rs"]
mod cf_deltas;
mod common;

use std::collections::{BTreeMap, HashMap};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{Norm, build_album, connect, event_json, recording_bus};
use musaic_checks::checks::track_gaps::MbTrack;
use musaic_checks::{CheckEnv, Fact, Resolution, ResolutionSource, ScopeKind, TrackMatch};
use musaic_db::Db;
use musaic_fsops::rename::PlanEntry;
use musaic_scan::{TrackOrder, py_resolve};
use musaic_services::check_fixes::{
    self, Anchor, CoverMatch, FingerprintFailure, Fingerprinted, FixEnv, FixOutcome, FixSources,
    TracklistMatch,
};
use musaic_services::{HttpError, Json, NoCovers, ScanDeps, ScanService, StartScan};
use rusqlite::Connection;
use serde_json::{Value, json};

const NOW: &str = "2030-01-01T00:00:00Z";
const TAG_KEYS: [&str; 5] = [
    "TRACKNUMBER",
    "TITLE",
    "ALBUMARTIST",
    "ALBUMARTISTSORT",
    "ARTISTSORT",
];

/// The Python app's stubs, mirrored.
struct Stubs {
    anchored: HashMap<String, (String, String)>,
    /// Section per-track-artist: album id -> (anchor, [(filename, canonical artist)]).
    tracks: HashMap<String, (String, Vec<(String, String)>)>,
}

impl ResolutionSource for Stubs {
    fn resolve(&self, _conn: &Connection, album_id: &str) -> Result<Resolution, String> {
        let f = |v: &str| Fact {
            value: v.into(),
            source: "musicbrainz".into(),
        };
        if let Some((rg, tracks)) = self.tracks.get(album_id) {
            return Ok(Resolution {
                anchor: Some(f(rg)),
                tracks: tracks
                    .iter()
                    .map(|(fname, artist)| TrackMatch {
                        filename: fname.clone(),
                        canonical_artist: Some(f(artist)),
                    })
                    .collect(),
                ..Resolution::default()
            });
        }
        let Some((rg, title)) = self.anchored.get(album_id) else {
            return Ok(Resolution::default());
        };
        Ok(Resolution {
            anchor: Some(f(rg)),
            canonical_title: Some(f(title)),
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
        Ok(self.anchored.get(album_id).map(|(rg, _)| Anchor {
            value: rg.clone(),
            strategy: "cache".into(),
            confidence: Some(0.95),
        }))
    }
    fn find_cover(&self, _c: &mut Connection, _a: &str) -> CoverMatch {
        CoverMatch::default()
    }
    fn find_tracklist(&self, _c: &mut Connection, _a: &str) -> TracklistMatch {
        let _ = MbTrack {
            title: String::new(),
            position: 0,
            recording_mbid: None,
        };
        TracklistMatch::default()
    }
}

struct World {
    _td: tempfile::TempDir,
    lib: PathBuf,
    cfg: PathBuf,
    db: PathBuf,
    norm: Norm,
    ids: HashMap<String, String>,
    bus: Arc<musaic_events::Bus>,
    log: Arc<std::sync::Mutex<Vec<musaic_events::Event>>>,
}

impl World {
    fn id(&self, dir: &str) -> String {
        self.ids[dir].clone()
    }

    fn n(&self, s: &str) -> String {
        let cfg = self.cfg.to_string_lossy();
        self.norm.str(&s.replace(cfg.as_ref(), "<CFG>"))
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

    fn take_events(&self) -> Vec<Value> {
        let mut l = self.log.lock().unwrap();
        let out = l.iter().map(|e| self.nv(&event_json(e))).collect();
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
                let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
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
        walk(&self.lib, &self.lib, &mut out);
        out
    }

    fn trash(&self) -> Vec<Value> {
        let root = self.cfg.join("trash");
        let mut out = Vec::new();
        fn walk(root: &Path, dir: &Path, out: &mut Vec<Value>) {
            let Ok(rd) = std::fs::read_dir(dir) else {
                return;
            };
            for e in rd {
                let p = e.unwrap().path();
                if p.is_dir() {
                    walk(root, &p, out);
                } else {
                    out.push(json!(p.strip_prefix(root).unwrap().to_string_lossy()));
                }
            }
        }
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

    fn state(&self, conn: &Connection, ai_jobs: &[Value]) -> Value {
        json!({
            "tree": self.tree(),
            "trash": self.trash(),
            "albums": self.rows(conn, "SELECT id, path, dir_name, track_count, album_artist_sort, missing_since FROM albums"),
            "track_tags": self.rows(conn, "SELECT album_id, filename, tag FROM track_tags"),
            "track_fingerprints": self.rows(conn, "SELECT album_id, filename FROM track_fingerprints"),
            "ai_jobs": ai_jobs,
            "open_findings": self.rows(conn, "SELECT check_id, album_id, summary FROM check_findings WHERE resolved_at IS NULL AND ignored_at IS NULL"),
        })
    }
}

/// inconsistent-artist-sort attaches a cluster's finding to the member with
/// the smallest album id; ids hash the absolute path, so which
/// member that is depends on where the library lives. Its album is masked.
const CLUSTER_CHECK: &str = "inconsistent-artist-sort";

/// Order-insensitive view of a state: every list sorted by its JSON text.
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

fn setup(g: &Value) -> (World, Connection, CheckEnv, Arc<Stubs>, String) {
    let td = tempfile::tempdir().unwrap();
    let root = py_resolve(td.path());
    let lib = root.join("lib");
    let cfg = root.join("cfg");
    std::fs::create_dir_all(&lib).unwrap();
    std::fs::create_dir_all(&cfg).unwrap();
    for a in g["albums"].as_array().unwrap() {
        build_album(&lib, a);
    }
    let db = cfg.join("library.db");
    drop(Db::open_or_create(&db).unwrap());
    let mut norm = Norm::new(&lib);
    let mut ids = HashMap::new();
    let mut dirs: Vec<String> = g["albums"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["dir"].as_str().unwrap().to_string())
        .collect();
    for st in g["steps"].as_array().unwrap() {
        if let Some(n) = st["args"]["dir_rename"]["new"].as_str() {
            dirs.push(n.to_string());
        }
    }
    for d in &dirs {
        norm.learn_dir(d);
        ids.insert(
            d.clone(),
            musaic_core::album_id(&py_resolve(&lib.join(d)).to_string_lossy()),
        );
    }
    let (bus, log) = recording_bus();
    let w = World {
        _td: td,
        lib: lib.clone(),
        cfg,
        db: db.clone(),
        norm,
        ids,
        bus,
        log,
    };
    let lib_s = lib.to_string_lossy().into_owned();
    // The scan (with sidecar import), as the Python app's /api/scan/stream did.
    let dbp = db.clone();
    let deps = ScanDeps {
        open_db: Arc::new(move || {
            let c = Connection::open(&dbp).map_err(|e| e.to_string())?;
            c.pragma_update(None, "foreign_keys", "ON")
                .map_err(|e| e.to_string())?;
            Ok(c)
        }),
        bus: Arc::clone(&w.bus),
        clock: Arc::new(|| NOW.to_string()),
        track_order: TrackOrder::DiscTrack,
        after_scan: None,
        on_album: None,
        on_worker_done: None,
        log: None,
    };
    match ScanService::new().start_scan(&lib_s, false, deps) {
        StartScan::Started(_, job) => job.run(),
        StartScan::AlreadyRunning(_) => unreachable!(),
    }
    let conn = connect(&w.db);
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
    let mut anchored = HashMap::new();
    for (d, rg) in g["anchored"].as_object().unwrap() {
        conn.execute(
            "UPDATE albums SET mb_release_group_mbid = ?1 WHERE id = ?2",
            [rg.as_str().unwrap(), &w.id(d)],
        )
        .unwrap();
        let title = g["resolved"][d]["title"].as_str().unwrap().to_string();
        anchored.insert(w.id(d), (rg.as_str().unwrap().to_string(), title));
    }
    let stubs = Arc::new(Stubs {
        anchored,
        tracks: HashMap::new(),
    });
    let checks = CheckEnv::new(stubs.clone());
    let lib_resolved = py_resolve(&lib).to_string_lossy().into_owned();
    for c in g["check_order"].as_array().unwrap() {
        musaic_checks::run_check(
            &conn,
            &checks,
            c.as_str().unwrap(),
            &lib_resolved,
            ScopeKind::All,
            None,
        )
        .unwrap();
    }
    w.take_events();
    (w, conn, checks, stubs, lib_s)
}

/// Every service step equals the Python app's, up to the enumerated
/// deliberate changes.
#[test]
fn check_fixes_replay_the_python_app() {
    let g = common::load_golden("check_fixes.json");
    let (w, mut conn, checks, stubs, lib_s) = setup(&g);

    // Finding ids: Python app -> ours, by (check, album, summary).
    let ours = w.rows(
        &conn,
        "SELECT id, check_id, album_id, summary FROM check_findings ORDER BY id",
    );
    let mut to_ours: HashMap<i64, i64> = HashMap::new();
    let mut to_python_app: HashMap<i64, i64> = HashMap::new();
    for f in g["setup_findings"].as_array().unwrap() {
        let m = ours
            .iter()
            .find(|o| {
                o[1] == f[1] && (o[2] == f[2] || f[1] == json!(CLUSTER_CHECK)) && o[3] == f[3]
            })
            .unwrap_or_else(|| panic!("no Rust finding for {f}"));
        to_ours.insert(f[0].as_i64().unwrap(), m[0].as_i64().unwrap());
        to_python_app.insert(m[0].as_i64().unwrap(), f[0].as_i64().unwrap());
    }
    assert_eq!(
        ours.len(),
        to_ours.len(),
        "same number of findings after setup"
    );
    let mut ai_jobs: Vec<Value> = Vec::new();
    assert_eq!(
        canon(&w.state(&conn, &ai_jobs)),
        canon(&g["initial_state"]),
        "state after setup"
    );

    let trash_root = w.cfg.join("trash");
    let now = || NOW.to_string();
    let stamp = || "2030-01-01T00-00-00".to_string();
    let mut used_sidecar = false;
    let mut used_unreachable = false;
    let mut used_blocklist_count = false;
    let mut used_kept_row = false;
    let mut used_skip_message = 0;
    let mut sidecar_delta: Option<(String, String)> = None;
    let mut prev_state = g["initial_state"].clone();
    let lib_dir = lib_s.clone();
    let mut replayed = 0;

    for st in g["steps"].as_array().unwrap() {
        let label = st["label"].as_str().unwrap();
        if st["layer"] == json!("http") {
            continue;
        }
        for act in st["before"].as_array().unwrap() {
            let p = |i: usize| w.lib.join(act[i].as_str().unwrap());
            match act[0].as_str().unwrap() {
                "unlink" => std::fs::remove_file(p(1)).unwrap(),
                "rename" => std::fs::rename(p(1), p(2)).unwrap(),
                other => panic!("unknown action {other}"),
            }
        }
        let a = &st["args"];
        let fid = |k: &str| {
            let o = a[k].as_i64().unwrap();
            *to_ours.get(&o).unwrap_or(&o)
        };
        let order = match a["check_id"].as_str() {
            Some(c) if st["op"] == json!("quick_fix_all") => cf_deltas::walk_order(&conn, c),
            _ => Vec::new(),
        };
        let mut env = FixEnv {
            conn: &mut conn,
            bus: &w.bus,
            checks: &checks,
            sources: stubs.as_ref(),
            covers: &NoCovers,
            library_dir: Some(&lib_dir),
            trash_root: &trash_root,
            now: &now,
            trash_stamp: &stamp,
            track_order: TrackOrder::DiscTrack,
            model_unavailable: None,
        };
        let mut sweep = false;
        let res: Result<Value, HttpError> = match st["op"].as_str().unwrap() {
            "fix_finding" => check_fixes::fix_finding(
                &mut env,
                a["check_id"].as_str().unwrap(),
                fid("finding_id"),
                a["hint"].as_str().unwrap(),
            )
            .map(|o| match o {
                FixOutcome::Done(r) => w.nv(&r.to_json().to_value()),
                // The host's queue (emulated): one active job per album.
                FixOutcome::NeedsAiJob(req) => {
                    let album = w.n(&req.album_id);
                    if ai_jobs.iter().any(|j| j[0] == json!(album)) {
                        json!({"ok": false, "message": "Already has an active job (queued)", "job_id": "<JOB>"})
                    } else {
                        ai_jobs.push(json!([album, req.job_type, req.hint, "queued"]));
                        json!({"ok": true, "message": req.message, "job_id": "<JOB>"})
                    }
                }
            }),
            "apply_filename_rename" => {
                let plan: Vec<PlanEntry> = a["plan"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|e| PlanEntry {
                        old: e["old"].as_str().unwrap().into(),
                        new: e["new"].as_str().unwrap().into(),
                    })
                    .collect();
                let dr = a["dir_rename"].as_object().map(|d| PlanEntry {
                    old: d["old"].as_str().unwrap().into(),
                    new: d["new"].as_str().unwrap().into(),
                });
                check_fixes::apply_filename_rename(&mut env, fid("finding_id"), &plan, dr.as_ref())
                    .map(|j| w.nv(&j.to_value()))
            }
            "delete_duplicate_file" => check_fixes::delete_duplicate_file(
                &mut env,
                fid("finding_id"),
                a["filename"].as_str().unwrap(),
            )
            .map(|j| w.nv(&j.to_value())),
            "apply_cluster_sort" => {
                sweep = true;
                check_fixes::apply_cluster_sort(&mut env, fid("finding_id"), a["sort_value"].as_str().unwrap())
                    .map(|j| w.nv(&j.to_value()))
            }
            "quick_fix_all" => {
                sweep = a["check_id"] == json!("inconsistent-artist-sort");
                let mut frames = String::new();
                check_fixes::quick_fix_all(
                    &mut env,
                    a["check_id"].as_str().unwrap(),
                    &mut || {},
                    &mut |mut item| {
                        if let Json::Obj(pairs) = &mut item.data {
                            for (k, v) in pairs.iter_mut() {
                                if k == "finding_id"
                                    && let Json::Int(i) = v {
                                        *i = to_python_app[i];
                                    }
                            }
                        }
                        frames.push_str(&item.sse_frame());
                        ControlFlow::Continue(())
                    },
                )
                .map(|()| json!({"sse": w.n(&frames)}))
            }
            other => panic!("unknown op {other}"),
        };
        let events = w.take_events();
        // The sweep the BulkBatchCompleted subscriber runs after a
        // cluster apply that wrote something (the Python app ran it inside
        // apply_cluster_sort, before publishing; a quick-fix-all's own
        // closing BulkBatchCompleted met no subscriber in the Python app).
        let bulk = events
            .iter()
            .filter(|e| e["type"] == json!("BulkBatchCompleted"))
            .count();
        let cluster_bulk = if st["op"] == json!("quick_fix_all") {
            bulk.saturating_sub(1)
        } else {
            bulk
        };
        if sweep && cluster_bulk > 0 {
            musaic_checks::run_post_scan_checks(&conn, &checks, &lib_s, None);
            // Findings this sweep created: same checks, same order, so the
            // n-th new Rust id is the n-th new Python app id.
            let mut fresh: Vec<i64> = w
                .rows(&conn, "SELECT id FROM check_findings ORDER BY id")
                .iter()
                .map(|r| r[0].as_i64().unwrap())
                .filter(|i| !to_python_app.contains_key(i))
                .collect();
            fresh.sort();
            let mut next = to_python_app.values().max().copied().unwrap_or(0);
            for id in fresh {
                next += 1;
                to_python_app.insert(id, next);
                to_ours.insert(next, id);
            }
        }
        let (status, body) = match res {
            Ok(b) => (200, b),
            Err(e) => (e.status, json!({"detail": e.detail})),
        };
        let state = canon(&w.state(&conn, &ai_jobs));
        let mut want_status = st["status"].clone();
        let mut want_body = st["body"].clone();
        let mut want_events = st["events"].clone();
        let mut want_state = st["state"].clone();

        // ---- enumerated deliberate changes ----
        if st["unknowns"]
            .as_array()
            .unwrap()
            .contains(&json!("delete-duplicate-unreachable"))
        {
            // 409, nothing deleted: the album, its fingerprints, tags and
            // findings stay as before the step; the tree is the step's own.
            used_unreachable = true;
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
        if label == "rename files" {
            // The sidecar follows the moved rows (the Python app's own
            // track_tags rows for the album, rendered as write_sidecar does).
            used_sidecar = true;
            let echo = w.n(&w.id("Echo - Five [FLAC]"));
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
            used_kept_row = true;
            let want = cf_deltas::last_copy_expected(st, &prev_state);
            want_events = want["events"].clone();
            want_state = want["state"].clone();
        }
        if st["op"] == json!("quick_fix_all")
            && let Some(sse) = want_body["sse"].as_str()
        {
            let (text, n) = cf_deltas::no_model_skip_message(
                sse,
                musaic_services::check_fixes::UNRESOLVED_SKIP_MESSAGE,
            );
            used_skip_message += n;
            want_body["sse"] = json!(cf_deltas::progress_in_our_finding_order(&text, |p| {
                cf_deltas::walk_pos(&order, to_ours.get(&p).copied().unwrap_or(p))
            }));
            // The album events follow the same finding order.
            let check = st["args"]["check_id"].as_str().unwrap();
            let our_finding = |e: &Value| -> Option<i64> {
                let id = e.get("album_id")?.as_str()?;
                let dir = id.strip_prefix("<ID:")?.strip_suffix('>')?;
                conn.query_row(
                    "SELECT MIN(id) FROM check_findings WHERE check_id = ?1 AND album_id = ?2",
                    [check, w.id(dir).as_str()],
                    |r| r.get(0),
                )
                .unwrap()
            };
            cf_deltas::events_in_walk_order(&mut want_events, &order, our_finding);
        }
        if want_body["dir_migration"]["albums"] == json!(1) {
            used_blocklist_count = true;
            want_body["dir_migration"]["album_blocked_mb_release_groups"] = json!(0);
        }
        if let Some((path, text)) = &sidecar_delta {
            for e in want_state["tree"].as_array_mut().unwrap() {
                if e[0] == json!(path) {
                    assert_ne!(
                        e[1],
                        json!(text),
                        "sidecar delta must differ from the Python app"
                    );
                    e[1] = json!(text);
                }
            }
        }

        assert_eq!(json!(status), want_status, "{label}: status");
        assert_eq!(body, want_body, "{label}: body");
        assert_eq!(json!(events), want_events, "{label}: events");
        assert_eq!(state, canon(&want_state), "{label}: state");
        if label == cf_deltas::LAST_COPY_STEP {
            // The user removes the Missing album from the library: exactly
            // the Python app's state after its own deletion.
            let juliet = w.id("Juliet - Ten [FLAC]");
            assert!(musaic_db::albums::delete_album(&conn, &juliet).unwrap());
            let mut bridged = want_state.clone();
            for k in ["albums", "track_tags", "track_fingerprints"] {
                bridged[k] = st["state"][k].clone();
            }
            assert_eq!(
                canon(&w.state(&conn, &ai_jobs)),
                canon(&bridged),
                "{label}: after removing the Missing album"
            );
        }
        prev_state = st["state"].clone();
        replayed += 1;
    }
    assert!(
        used_sidecar && used_unreachable && used_blocklist_count && used_kept_row,
        "unused delta"
    );
    assert_eq!(used_skip_message, 11, "the skipped unidentified albums");
    assert!(replayed >= 35, "{replayed}");
}

/// Teardown and early stop: quick-fix-all calls the
/// pacing hook between findings only (the Python app's `asyncio.sleep(1.0)` for
/// i > 0, musaic/api/health.py:650-653), stops forwarding when the client
/// goes away, and still publishes BulkBatchCompleted (its `finally`). A fix
/// attempt always wakes the checks stream (runner.fix_finding's `finally`).
/// With nothing open: start + done and no BulkBatchCompleted
/// (`if len(finding_ids) > 0`).
#[test]
fn quick_fix_all_hooks_and_early_stop() {
    let g = common::load_golden("check_fixes.json");
    let (w, mut conn, checks0, stubs, lib_s) = setup(&g);
    let notified = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let n2 = notified.clone();
    let mut checks = checks0.clone();
    checks.notify = Arc::new(move || {
        n2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    });
    let trash_root = w.cfg.join("trash");
    let now = || NOW.to_string();
    let stamp = || "x".to_string();
    let mut env = FixEnv {
        conn: &mut conn,
        bus: &w.bus,
        checks: &checks,
        sources: stubs.as_ref(),
        covers: &NoCovers,
        library_dir: Some(&lib_s),
        trash_root: &trash_root,
        now: &now,
        trash_stamp: &stamp,
        track_order: TrackOrder::DiscTrack,
        model_unavailable: None,
    };
    let mut between = 0;
    let mut items: Vec<String> = Vec::new();
    check_fixes::quick_fix_all(&mut env, "track-gaps", &mut || between += 1, &mut |it| {
        items.push(it.event.to_string());
        ControlFlow::Continue(())
    })
    .unwrap();
    // One track-gaps finding at setup: start, one progress, done, no pause.
    assert_eq!(items, ["start", "progress", "done"]);
    assert_eq!(between, 0);
    assert_eq!(w.take_events(), vec![json!({"type": "BulkBatchCompleted"})]);

    let mut between = 0;
    let mut items = 0;
    check_fixes::quick_fix_all(&mut env, "no-cover-art", &mut || between += 1, &mut |it| {
        items += 1;
        if it.event == "progress" && items == 3 {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    })
    .unwrap();
    assert_eq!((items, between), (3, 1), "stopped after the second finding");
    assert_eq!(w.take_events(), vec![json!({"type": "BulkBatchCompleted"})]);

    // Nothing open for a check: start + done, and no BulkBatchCompleted.
    check_fixes::quick_fix_all(&mut env, "duplicate-tracks", &mut || {}, &mut |_| {
        ControlFlow::Continue(())
    })
    .unwrap();
    let before = w.take_events();
    assert!(!before.is_empty(), "three duplicate findings at setup");
    conn_resolve_all(env.conn, "duplicate-tracks");
    let mut evs = Vec::new();
    check_fixes::quick_fix_all(&mut env, "duplicate-tracks", &mut || {}, &mut |it| {
        evs.push(it.event);
        ControlFlow::Continue(())
    })
    .unwrap();
    assert_eq!(evs, ["start", "done"]);
    assert!(w.take_events().is_empty());

    let n0 = notified.load(std::sync::atomic::Ordering::SeqCst);
    let f = to_rust_id(env.conn, "duplicate-tracks");
    let _ = check_fixes::fix_finding(&mut env, "duplicate-tracks", f, "");
    assert_eq!(notified.load(std::sync::atomic::Ordering::SeqCst), n0 + 1);
    // A refused fix (wrong check) is answered before the runner: no wake.
    let _ = check_fixes::fix_finding(&mut env, "track-gaps", f, "");
    assert_eq!(notified.load(std::sync::atomic::Ordering::SeqCst), n0 + 1);
}

/// Every fix path panics (a panic inside a fix used to end the
/// stream with no `done` and skip `BulkBatchCompleted`).
struct PanickingSources;

impl FixSources for PanickingSources {
    fn fingerprint_available(&self) -> bool {
        panic!("injected: fingerprint_available")
    }
    fn fingerprint_album(
        &self,
        _c: &mut Connection,
        _a: &str,
        _p: &str,
    ) -> Result<Vec<Fingerprinted>, FingerprintFailure> {
        panic!("injected: fingerprint_album")
    }
    fn resolve_anchor(&self, _c: &mut Connection, _a: &str) -> Result<Option<Anchor>, String> {
        panic!("injected: resolve_anchor")
    }
    fn find_cover(&self, _c: &mut Connection, _a: &str) -> CoverMatch {
        panic!("injected: find_cover")
    }
    fn find_tracklist(&self, _c: &mut Connection, _a: &str) -> TracklistMatch {
        panic!("injected: find_tracklist")
    }
}

/// Also on error: a fix that panics is reported as
/// an `error` progress outcome and the loop goes on to `done`; a sink that
/// panics ends the run with the Python app's fatal `error {detail}` frame. In
/// both cases the bulk-run flag is reset and `BulkBatchCompleted` is still
/// published once (the Python app's `finally`, musaic/api/health.py:713-728).
/// The Python app's frame names are the reference; the `panic: ...` text has no
/// Python app counterpart (a Rust panic is not a Python exception).
#[test]
fn quick_fix_all_survives_a_panicking_fix() {
    let g = common::load_golden("check_fixes.json");
    let (w, mut conn, checks, _stubs, lib_s) = setup(&g);
    let trash_root = w.cfg.join("trash");
    let now = || NOW.to_string();
    let stamp = || "x".to_string();
    let mut env = FixEnv {
        conn: &mut conn,
        bus: &w.bus,
        checks: &checks,
        sources: &PanickingSources,
        covers: &NoCovers,
        library_dir: Some(&lib_s),
        trash_root: &trash_root,
        now: &now,
        trash_stamp: &stamp,
        track_order: TrackOrder::DiscTrack,
        model_unavailable: None,
    };
    let open = |conn: &Connection| -> usize {
        conn.query_row(
            "SELECT COUNT(*) FROM check_findings WHERE check_id = 'no-cover-art' \
             AND resolved_at IS NULL AND ignored_at IS NULL",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap() as usize
    };
    let n = open(env.conn);
    assert!(n >= 2, "no-cover-art findings at setup");
    let mut items: Vec<(String, Value)> = Vec::new();
    check_fixes::quick_fix_all(&mut env, "no-cover-art", &mut || {}, &mut |it| {
        items.push((it.event.to_string(), it.data.to_value()));
        ControlFlow::Continue(())
    })
    .unwrap();
    assert_eq!(items.len(), n + 2, "start, one progress per finding, done");
    assert_eq!(items[0].0, "start");
    for (ev, data) in &items[1..=n] {
        assert_eq!(ev, "progress");
        assert_eq!(data["outcome"], "error", "{data}");
        assert!(
            data["message"]
                .as_str()
                .unwrap()
                .starts_with("panic: injected: "),
            "{data}"
        );
    }
    assert_eq!(items[n + 1].0, "done");
    assert_eq!(items[n + 1].1["errors"], json!(n));
    assert_eq!(w.take_events(), vec![json!({"type": "BulkBatchCompleted"})]);
    assert!(
        !musaic_events::context::current().in_bulk_run,
        "the bulk-run flag is reset"
    );

    // A sink that panics on the first progress frame: fatal `error` frame,
    // flag reset, BulkBatchCompleted still published.
    let mut events: Vec<String> = Vec::new();
    let mut detail = Value::Null;
    check_fixes::quick_fix_all(&mut env, "no-cover-art", &mut || {}, &mut |it| {
        events.push(it.event.to_string());
        if it.event == "progress" {
            panic!("injected: client sink");
        }
        if it.event == "error" {
            detail = it.data.to_value()["detail"].clone();
        }
        ControlFlow::Continue(())
    })
    .unwrap();
    assert_eq!(events, ["start", "progress", "error"]);
    assert_eq!(detail, json!("panic: injected: client sink"));
    assert_eq!(w.take_events(), vec![json!({"type": "BulkBatchCompleted"})]);
    assert!(!musaic_events::context::current().in_bulk_run);
}

fn conn_resolve_all(conn: &Connection, check: &str) {
    conn.execute(
        "UPDATE check_findings SET resolved_at = 'x' WHERE check_id = ?1",
        [check],
    )
    .unwrap();
}

fn to_rust_id(conn: &Connection, check: &str) -> i64 {
    conn.query_row(
        "SELECT id FROM check_findings WHERE check_id = ?1 ORDER BY id LIMIT 1",
        [check],
        |r| r.get(0),
    )
    .unwrap()
}

/// Settled by the Python app's record: the
/// tags-out-of-sync fix and quick fix DO apply a per-track ARTIST that
/// differs from the resolver's canonical artist (the Python app's `_load_album`
/// selects `path`), and the finding clears on the next run. Replays the
/// golden's `per-track-artist` section (own library and database: two
/// anchored albums, one drifting track each), no delta: per-finding fix,
/// quick-fix-all, then a library-wide run.
#[test]
fn tags_out_of_sync_applies_per_track_artist() {
    replay_section("per-track-artist", &|step| step.clone());
}

/// Fixes and quick-fix-all report a finding of
/// a Missing album as not-applicable without attempting it. Replays the
/// golden's `missing-album` section (own library and database: the
/// per-track-artist setup, then Tango's directory
/// moved out of the library and the library rescanned, so Tango is Missing
/// with its finding open). Enumerated deltas, each used once and each
/// changing the Python app's record:
/// - "fix tags-out-of-sync on a Missing album": the body is
///   `{ok: false, message: "Not applicable: <the Missing-album text>"}` (the Python app
///   attempted it and answered "Nothing to fix"); events and state as the
///   Python app's (it changed nothing either);
/// - "quick-fix-all ... with a Missing album": Tango's `progress` outcome is
///   `not-applicable` with that message (the Python app's: `skipped`), and the
///   `done` counts move one from `skipped` to `not_applicable`. Sierra's fix,
///   the events and the state are the Python app's.
#[test]
fn missing_album_findings_are_not_applicable() {
    let used = std::cell::Cell::new(0);
    replay_section("missing-album", &|step| {
        let mut want = step.clone();
        let msg = check_fixes::missing_album_not_applicable(NOW);
        match step["label"].as_str().unwrap() {
            "fix tags-out-of-sync on a Missing album" => {
                let new = json!({"ok": false, "message": msg, "job_id": null});
                assert_ne!(want["body"], new, "not-applicable delta changes nothing");
                want["body"] = new;
                used.set(used.get() + 1);
            }
            "quick-fix-all tags-out-of-sync with a Missing album" => {
                let old = want["body"]["sse"].as_str().unwrap().to_string();
                let tango_old = "{\"finding_id\": 1, \"outcome\": \"skipped\", \"message\": \
                    \"No deterministic fixes available (anchor or canonical missing)\"}";
                let tango_new = format!(
                    "{{\"finding_id\": 1, \"outcome\": \"not-applicable\", \"message\": \"{msg}\"}}"
                );
                let done_old = "{\"total\": 2, \"fixed\": 1, \"skipped\": 1, \"errors\": 0, \"not_applicable\": 0}";
                let done_new = "{\"total\": 2, \"fixed\": 1, \"skipped\": 0, \"errors\": 0, \"not_applicable\": 1}";
                assert!(old.contains(tango_old) && old.contains(done_old), "{old}");
                want["body"]["sse"] = json!(
                    old.replace(tango_old, &tango_new)
                        .replace(done_old, done_new)
                );
                used.set(used.get() + 1);
            }
            other => panic!("unexpected step {other}"),
        }
        want
    });
    assert_eq!(used.get(), 2, "every not-applicable delta used");
}

/// Replay one `sections.<name>` record of check_fixes.json; `expected`
/// turns the Python app's step record into what this build must produce (the
/// enumerated deltas; identity when there are none).
fn replay_section(name: &str, expected: &dyn Fn(&Value) -> Value) {
    let g = common::load_golden("check_fixes.json");
    let sec = &g["sections"][name];
    let td = tempfile::tempdir().unwrap();
    let root = py_resolve(td.path());
    let lib = root.join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    for a in sec["albums"].as_array().unwrap() {
        build_album(&lib, a);
    }
    let db = root.join("library.db");
    drop(Db::open_or_create(&db).unwrap());
    let mut norm = Norm::new(&lib);
    let mut ids: HashMap<String, String> = HashMap::new();
    for a in sec["albums"].as_array().unwrap() {
        let d = a["dir"].as_str().unwrap();
        norm.learn_dir(d);
        ids.insert(
            d.into(),
            musaic_core::album_id(&py_resolve(&lib.join(d)).to_string_lossy()),
        );
    }
    let (bus, log) = recording_bus();
    let lib_s = lib.to_string_lossy().into_owned();
    let scan = || {
        let dbp = db.clone();
        let deps = ScanDeps {
            open_db: Arc::new(move || {
                let c = Connection::open(&dbp).map_err(|e| e.to_string())?;
                c.pragma_update(None, "foreign_keys", "ON")
                    .map_err(|e| e.to_string())?;
                Ok(c)
            }),
            bus: Arc::clone(&bus),
            clock: Arc::new(|| NOW.to_string()),
            track_order: TrackOrder::DiscTrack,
            after_scan: None,
            on_album: None,
            on_worker_done: None,
            log: None,
        };
        match ScanService::new().start_scan(&lib_s, false, deps) {
            StartScan::Started(_, job) => job.run(),
            StartScan::AlreadyRunning(_) => unreachable!(),
        }
    };
    scan();
    let mut conn = connect(&db);
    let mut tracks = HashMap::new();
    for (d, rec) in sec["resolved"].as_object().unwrap() {
        let rg = rec[0].as_str().unwrap().to_string();
        conn.execute(
            "UPDATE albums SET mb_release_group_mbid = ?1 WHERE id = ?2",
            [&rg, &ids[d]],
        )
        .unwrap();
        let t: Vec<(String, String)> = rec[1]
            .as_object()
            .unwrap()
            .iter()
            .map(|(f, a)| (f.clone(), a.as_str().unwrap().to_string()))
            .collect();
        tracks.insert(ids[d].clone(), (rg, t));
    }
    let stubs = Arc::new(Stubs {
        anchored: HashMap::new(),
        tracks,
    });
    let checks = CheckEnv::new(stubs.clone());
    let lib_resolved = py_resolve(&lib).to_string_lossy().into_owned();
    musaic_checks::run_check(
        &conn,
        &checks,
        "tags-out-of-sync",
        &lib_resolved,
        ScopeKind::All,
        None,
    )
    .unwrap();
    if let Some(gone) = sec["moved_out_of_library"].as_str() {
        // missing-album: the album's directory leaves the library; the rescan
        // marks it Missing (its finding stays open).
        std::fs::rename(lib.join(gone), root.join("unmounted")).unwrap();
        scan();
    }
    log.lock().unwrap().clear();

    let n = |s: &str| norm.str(s);
    let nv = |v: &Value| norm.value(v);
    let keys: Vec<String> = sec["tag_keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k.as_str().unwrap().to_string())
        .collect();
    let state = |conn: &Connection| -> Value {
        let mut tree = Vec::new();
        for a in sec["albums"].as_array().unwrap() {
            let d = a["dir"].as_str().unwrap();
            if !lib.join(d).is_dir() {
                continue; // moved out of the library (missing-album)
            }
            let mut files: Vec<_> = std::fs::read_dir(lib.join(d))
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect();
            files.sort();
            for p in files {
                let meta = musaic_scan::flacmeta::read(&p).unwrap();
                let mut tags = serde_json::Map::new();
                for k in &keys {
                    let vals: Vec<&str> = meta.values(k).collect();
                    if !vals.is_empty() {
                        tags.insert(k.clone(), json!(vals));
                    }
                }
                let name = p.file_name().unwrap().to_string_lossy().into_owned();
                tree.push(json!([format!("{d}/{name}"), tags]));
            }
        }
        let rows = |sql: &str| -> Vec<Value> {
            let mut st = conn.prepare(sql).unwrap();
            let cols = st.column_count();
            let mut out: Vec<Value> = st
                .query_map([], |r| {
                    Ok(Value::Array(
                        (0..cols)
                            .map(|i| match r.get::<_, Option<String>>(i).unwrap() {
                                Some(s) => json!(n(&s)),
                                None => Value::Null,
                            })
                            .collect(),
                    ))
                })
                .unwrap()
                .map(Result::unwrap)
                .collect();
            out.sort_by_key(|v| v.to_string());
            out
        };
        json!({
            "tree": tree,
            "albums": rows("SELECT id, dir_name, artist, mb_release_group_mbid, missing_since FROM albums"),
            "ai_jobs": [],
            "open_findings": rows("SELECT check_id, album_id, summary FROM check_findings WHERE resolved_at IS NULL AND ignored_at IS NULL"),
        })
    };
    let canon_rows = |v: &Value| -> Value {
        let mut v = v.clone();
        for k in ["albums", "open_findings"] {
            v[k].as_array_mut().unwrap().sort_by_key(|x| x.to_string());
        }
        v
    };

    // Setup findings: same (check, album, summary, detail); map the ids.
    let mut to_ours: HashMap<i64, i64> = HashMap::new();
    let mut to_python_app: HashMap<i64, i64> = HashMap::new();
    let mut st = conn
        .prepare(
            "SELECT id, check_id, album_id, summary, detail_json FROM check_findings ORDER BY id",
        )
        .unwrap();
    let ours: Vec<(i64, Value)> = st
        .query_map([], |r| {
            let detail: Option<String> = r.get(4)?;
            Ok((
                r.get(0)?,
                json!([
                    r.get::<_, String>(1)?,
                    n(&r.get::<_, String>(2)?),
                    r.get::<_, String>(3)?,
                    nv(
                        &serde_json::from_str::<Value>(detail.as_deref().unwrap_or("null"))
                            .unwrap()
                    ),
                ]),
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    drop(st);
    let setup = sec["setup_findings"].as_array().unwrap();
    assert_eq!(ours.len(), setup.len(), "setup findings");
    for f in setup {
        let rest = json!(f.as_array().unwrap()[1..].to_vec());
        let (id, _) = ours
            .iter()
            .find(|(_, o)| *o == rest)
            .unwrap_or_else(|| panic!("no Rust finding equal to {f}"));
        to_ours.insert(f[0].as_i64().unwrap(), *id);
        to_python_app.insert(*id, f[0].as_i64().unwrap());
    }
    assert_eq!(
        canon_rows(&state(&conn)),
        canon_rows(&sec["initial_state"]),
        "initial state"
    );

    let trash_root = root.join("trash");
    let now = || NOW.to_string();
    let stamp = || "x".to_string();
    for step in sec["steps"].as_array().unwrap() {
        let label = step["label"].as_str().unwrap();
        let a = &step["args"];
        let order = match a["check_id"].as_str() {
            Some(c) if step["op"] == json!("quick_fix_all") => cf_deltas::walk_order(&conn, c),
            _ => Vec::new(),
        };
        let mut env = FixEnv {
            conn: &mut conn,
            bus: &bus,
            checks: &checks,
            sources: stubs.as_ref(),
            covers: &NoCovers,
            library_dir: Some(&lib_s),
            trash_root: &trash_root,
            now: &now,
            trash_stamp: &stamp,
            track_order: TrackOrder::DiscTrack,
            model_unavailable: None,
        };
        let body: Value = match step["op"].as_str().unwrap() {
            "fix_finding" => {
                let fid = to_ours[&a["finding_id"].as_i64().unwrap()];
                match check_fixes::fix_finding(&mut env, a["check_id"].as_str().unwrap(), fid, "")
                    .unwrap()
                {
                    FixOutcome::Done(r) => nv(&r.to_json().to_value()),
                    FixOutcome::NeedsAiJob(_) => panic!("{label}: no AI job expected"),
                }
            }
            "quick_fix_all" => {
                let mut frames = String::new();
                check_fixes::quick_fix_all(
                    &mut env,
                    a["check_id"].as_str().unwrap(),
                    &mut || {},
                    &mut |mut item| {
                        if let Json::Obj(pairs) = &mut item.data {
                            for (k, v) in pairs.iter_mut() {
                                if k == "finding_id"
                                    && let Json::Int(i) = v
                                {
                                    *i = to_python_app[i];
                                }
                            }
                        }
                        frames.push_str(&item.sse_frame());
                        ControlFlow::Continue(())
                    },
                )
                .unwrap();
                json!({"sse": n(&frames)})
            }
            "run_check" => {
                let run = musaic_checks::run_check(
                    env.conn,
                    &checks,
                    a["check_id"].as_str().unwrap(),
                    &lib_resolved,
                    ScopeKind::All,
                    None,
                )
                .unwrap();
                let mut v = nv(&musaic_checks::runner::run_to_json(&run));
                v["duration_ms"] = json!("<MS>");
                // The run row's id counts this database's runs: the setup
                // run was 1 in both.
                v
            }
            other => panic!("unknown op {other}"),
        };
        let events: Vec<Value> = log
            .lock()
            .unwrap()
            .drain(..)
            .map(|e| nv(&event_json(&e)))
            .collect();
        let mut want = expected(step);
        if let Some(sse) = want["body"]["sse"].as_str() {
            want["body"]["sse"] = json!(cf_deltas::progress_in_our_finding_order(sse, |p| {
                cf_deltas::walk_pos(&order, to_ours.get(&p).copied().unwrap_or(p))
            }));
        }
        assert_eq!(body, want["body"], "{label}: body");
        assert_eq!(json!(events), want["events"], "{label}: events");
        assert_eq!(
            canon_rows(&state(&conn)),
            canon_rows(&want["state"]),
            "{label}: state"
        );
    }
}
