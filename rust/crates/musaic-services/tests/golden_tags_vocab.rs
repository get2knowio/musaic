//! Track tags and the tag vocabulary replayed against the Python app's record
//! (testdata/golden/services/tags_vocab.json, written by a generator that
//! drove the Python app's
//! real routes through TestClient; two runs byte-identical).
//!
//! Every service-layer step is replayed in order on the same evolving state:
//! response status and body (parsed JSON), the `track_tags` and
//! `tag_vocabulary` rows, every album directory's sidecar text and dot-files,
//! and the bus events (none, as in the Python app). Steps marked `layer: "http"`
//! are 422 request-validation cases for the HTTP layer and are skipped. Then
//! drop_snapshot, then the golden's `extra_steps` the same way.
//!
//! Differences are allowed only as the enumerated deltas of
//! tests/deltas/tags_vocab.rs (case-only / identical rename, Missing album,
//! filenames that are not FLACs of the album, one Unicode case fold), each
//! used exactly once; after a step whose state differs the world adopts the
//! Python app's recorded state (checked) so later steps start where the Python app's
//! did.
//!
//! Covered: the tags and vocabulary routes' bodies, the 404 / 409 album
//! lookups, the 400 / 409 refusals, the sidecar rewrite after tag
//! mutations, and drop_snapshot keeping everything.

mod common;
#[path = "deltas/tags_vocab.rs"]
mod tv_deltas;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{Norm, build_album, connect, event_json, recording_bus};
use musaic_db::Db;
use musaic_scan::{TrackOrder, py_resolve};
use musaic_services::track_tags::{self, TagEnv};
use musaic_services::vocabulary;
use musaic_services::{HttpError, Json, ScanDeps, ScanService, StartScan};
use rusqlite::Connection;
use serde_json::{Value, json};

const NOW: &str = "2030-01-01T00:00:00Z";

fn golden() -> Value {
    common::load_golden("tags_vocab.json")
}

struct World {
    _td: tempfile::TempDir,
    lib: PathBuf,
    db: PathBuf,
    norm: Norm,
    ids: Vec<(String, String)>,
    bus: Arc<musaic_events::Bus>,
    log: Arc<std::sync::Mutex<Vec<musaic_events::Event>>>,
}

impl World {
    fn new(g: &Value) -> World {
        let td = tempfile::tempdir().unwrap();
        let root = py_resolve(td.path());
        let lib = root.join("lib");
        std::fs::create_dir(&lib).unwrap();
        for a in g["albums"].as_array().unwrap() {
            build_album(&lib, a);
        }
        let cfg = root.join("cfg");
        std::fs::create_dir(&cfg).unwrap();
        let db = cfg.join("library.db");
        drop(Db::open_or_create(&db).unwrap());
        let mut norm = Norm::new(&lib);
        let mut ids = Vec::new();
        for a in g["albums"].as_array().unwrap() {
            let d = a["dir"].as_str().unwrap();
            norm.learn_dir(d);
            ids.push((
                d.to_string(),
                musaic_core::album_id(&py_resolve(&lib.join(d)).to_string_lossy()),
            ));
        }
        let (bus, log) = recording_bus();
        World {
            _td: td,
            lib,
            db,
            norm,
            ids,
            bus,
            log,
        }
    }

    fn id(&self, dir: &str) -> String {
        self.ids
            .iter()
            .find(|(d, _)| d == dir)
            .map(|(_, i)| i.clone())
            .unwrap_or_else(|| dir.to_string())
    }

    fn scan(&self) {
        let db = self.db.clone();
        let deps = ScanDeps {
            open_db: Arc::new(move || {
                let c = Connection::open(&db).map_err(|e| e.to_string())?;
                c.pragma_update(None, "foreign_keys", "ON")
                    .map_err(|e| e.to_string())?;
                Ok(c)
            }),
            bus: Arc::clone(&self.bus),
            clock: Arc::new(|| NOW.to_string()),
            track_order: TrackOrder::DiscTrack,
            after_scan: None,
            on_album: None,
            on_worker_done: None,
            log: None,
        };
        match ScanService::new().start_scan(&self.lib.to_string_lossy(), false, deps) {
            StartScan::Started(_, job) => job.run(),
            StartScan::AlreadyRunning(_) => unreachable!(),
        }
    }

    fn state(&self, g: &Value) -> Value {
        let c = connect(&self.db);
        let mut st = c
            .prepare("SELECT album_id, filename, tag, created_at FROM track_tags")
            .unwrap();
        let mut tt: Vec<Value> = st
            .query_map([], |r| {
                Ok(json!([
                    self.norm.str(&r.get::<_, String>(0)?),
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    self.norm.str(&r.get::<_, String>(3)?),
                ]))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        tt.sort_by_key(|r| {
            (
                r[0].as_str().unwrap().to_string(),
                r[1].as_str().unwrap().to_string(),
                r[2].as_str().unwrap().to_string(),
            )
        });
        let mut st = c
            .prepare("SELECT tag, description, color, sort_order, created_at FROM tag_vocabulary")
            .unwrap();
        let mut voc: Vec<Value> = st
            .query_map([], |r| {
                Ok(json!({
                    "tag": r.get::<_, String>(0)?,
                    "description": r.get::<_, String>(1)?,
                    "color": r.get::<_, Option<String>>(2)?,
                    "sort_order": r.get::<_, i64>(3)?,
                    "created_at": self.norm.str(&r.get::<_, String>(4)?),
                }))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        voc.sort_by_key(|r| r["tag"].as_str().unwrap().to_string());
        let mut dirs = serde_json::Map::new();
        for a in g["albums"].as_array().unwrap() {
            let name = a["dir"].as_str().unwrap();
            let d = self.lib.join(name);
            if !d.is_dir() {
                dirs.insert(name.into(), Value::Null);
                continue;
            }
            let sc = d.join(".media-manager-tags.json");
            let mut dots: Vec<String> = std::fs::read_dir(&d)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with('.'))
                .collect();
            dots.sort();
            dirs.insert(
                name.into(),
                json!({
                    "sidecar": std::fs::read_to_string(&sc).ok(),
                    "dotfiles": dots,
                }),
            );
        }
        json!({"track_tags": tt, "vocabulary": voc, "dirs": dirs})
    }

    fn take_events(&self) -> Vec<Value> {
        let mut l = self.log.lock().unwrap();
        let out = l.iter().map(|e| self.norm.value(&event_json(e))).collect();
        l.clear();
        out
    }
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap()
}

fn opt_s<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().to_string())
        .collect()
}

/// `rename_result.albums` & co. are `sorted(album ids)`; ids hash the
/// absolute path, so their order depends on where the library lives. Sort
/// the normalised names on both sides.
fn sort_album_lists(mut v: Value) -> Value {
    for k in ["rename_result", "merge_result", "delete_result"] {
        if let Some(Value::Array(a)) = v.get_mut(k).and_then(|r| r.get_mut("albums")) {
            a.sort_by_key(|x| x.as_str().unwrap_or_default().to_string());
        }
    }
    v
}

/// Run one golden step through the service layer.
fn dispatch(
    w: &World,
    conn: &mut Connection,
    lib: Option<&str>,
    op: &str,
    a: &Value,
) -> Result<Json, HttpError> {
    let album = |a: &Value| w.id(s(a, "album"));
    let mut env = TagEnv {
        conn,
        library_dir: lib,
        order: TrackOrder::DiscTrack,
        now: NOW,
    };
    match op {
        "get_album_tags" => track_tags::get_album_tags(&mut env, &album(a)),
        "add_album_tag" => {
            let f = a["filenames"].as_array().map(|_| strings(&a["filenames"]));
            track_tags::add_album_tag(&mut env, &album(a), s(a, "tag"), f.as_deref())
        }
        "remove_album_tag" => track_tags::remove_album_tag(&mut env, &album(a), s(a, "tag")),
        "add_track_tag" => {
            track_tags::add_track_tag(&mut env, &album(a), s(a, "filename"), s(a, "tag"))
        }
        "remove_track_tag" => {
            track_tags::remove_track_tag(&mut env, &album(a), s(a, "filename"), s(a, "tag"))
        }
        "replace_album_tags" => {
            let pairs: Vec<(String, Vec<String>)> = a["track_tags"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| (p[0].as_str().unwrap().to_string(), strings(&p[1])))
                .collect();
            track_tags::replace_album_tags(&mut env, &album(a), &pairs)
        }
        "list_tags" => track_tags::list_tags(env.conn),
        "tracks_for_tag" => track_tags::tracks_for_tag(env.conn, s(a, "tag")),
        "list_vocabulary" => vocabulary::list_vocabulary(env.conn),
        "add_vocabulary_entry" => vocabulary::add_vocabulary_entry(
            env.conn,
            NOW,
            s(a, "tag"),
            opt_s(a, "description"),
            opt_s(a, "color"),
            a["sort_order"].as_i64(),
        ),
        "patch_vocabulary_entry" => vocabulary::patch_vocabulary_entry(
            env.conn,
            s(a, "tag"),
            opt_s(a, "description"),
            opt_s(a, "color"),
            a["sort_order"].as_i64(),
        ),
        "rename_vocabulary_entry" => {
            vocabulary::rename_vocabulary_entry(env.conn, s(a, "tag"), s(a, "new_tag"))
        }
        "merge_vocabulary_entry" => {
            vocabulary::merge_vocabulary_entry(env.conn, s(a, "tag"), s(a, "into_tag"))
        }
        "delete_vocabulary_entry" => vocabulary::delete_vocabulary_entry(
            env.conn,
            s(a, "tag"),
            a["strip_from_tracks"].as_bool().unwrap(),
        ),
        "promote_orphan_tag" => vocabulary::promote_orphan_tag(env.conn, NOW, s(a, "tag")),
        other => panic!("unknown op {other}"),
    }
}

/// Every service step equals the
/// Python app's, up to the enumerated deltas.
#[test]
fn tags_and_vocabulary_replay_the_python_app() {
    let g = golden();
    let w = World::new(&g);
    let lib_s = w.lib.to_string_lossy().into_owned();
    let mut conn = connect(&w.db);
    let steps = g["steps"].as_array().unwrap();
    let mut tracker = tv_deltas::Tracker::new();
    let mut replayed = 0;
    let mut configured = false;
    let mut prev_state = g["initial_state"].clone();
    let mut prev_step: Option<&Value> = None;
    for st in steps {
        if !configured && st["request"]["path"] != json!("/api/tags") && st["status"] != json!(409)
        {
            // First configured step: scan, then the setup the generator did.
            configured = true;
            w.scan();
            assert_eq!(
                w.state(&g),
                g["initial_state"],
                "initial state after the first scan"
            );
            std::fs::remove_dir_all(w.lib.join("Gamma - Three [FLAC]")).unwrap();
            musaic_db::albums::drop_snapshot(&mut conn, &lib_s).unwrap();
            w.scan();
            for e in std::fs::read_dir(w.lib.join("Delta - Four [FLAC]")).unwrap() {
                let p = e.unwrap().path();
                if p.extension().is_some_and(|x| x == "flac") {
                    std::fs::remove_file(p).unwrap();
                }
            }
            let mut st2 = conn
                .prepare("SELECT id, missing_since FROM albums")
                .unwrap();
            let presence: serde_json::Map<String, Value> = st2
                .query_map([], |r| {
                    Ok((
                        w.norm.str(&r.get::<_, String>(0)?),
                        Value::Bool(r.get::<_, Option<String>>(1)?.is_some()),
                    ))
                })
                .unwrap()
                .map(Result::unwrap)
                .collect();
            drop(st2);
            assert_eq!(Value::Object(presence), g["presence_after_setup"]);
            w.take_events();
        }
        if st["layer"] == json!("http") {
            prev_state = st["state"].clone();
            prev_step = Some(st);
            continue;
        }
        let lib = configured.then_some(lib_s.as_str());
        replay_step(
            &w,
            &g,
            &mut conn,
            lib,
            "steps",
            st,
            &prev_state,
            prev_step,
            &mut tracker,
        );
        prev_state = st["state"].clone();
        prev_step = Some(st);
        replayed += 1;
    }
    let http = steps.iter().filter(|s| s["layer"] == json!("http")).count();
    assert_eq!(replayed + http, steps.len());
    assert!(replayed >= 60, "{replayed}");

    // Drop_snapshot as the Python app recorded it.
    musaic_db::albums::drop_snapshot(&mut conn, &lib_s).unwrap();
    let d = &g["drop_snapshot"];
    let mut st = conn
        .prepare("SELECT id, dir_mtime, missing_since FROM albums ORDER BY id")
        .unwrap();
    let albums: Vec<Value> = st
        .query_map([], |r| {
            let m: f64 = r.get(1)?;
            Ok(json!([
                w.norm.str(&r.get::<_, String>(0)?),
                if m == 0.0 {
                    json!(0.0)
                } else {
                    json!("<MTIME>")
                },
                r.get::<_, Option<String>>(2)?.map(|s| w.norm.str(&s)),
            ]))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut sorted = albums.clone();
    sorted.sort_by_key(|r| r[0].as_str().unwrap().to_string());
    let mut expected = d["albums"].as_array().unwrap().clone();
    expected.sort_by_key(|r| r[0].as_str().unwrap().to_string());
    assert_eq!(sorted, expected, "drop_snapshot albums");
    let libs: Vec<Value> = conn
        .prepare("SELECT path, scanned_at FROM libraries")
        .unwrap()
        .query_map([], |r| {
            Ok(json!([
                w.norm.str(&r.get::<_, String>(0)?),
                r.get::<_, String>(1)?
            ]))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(json!(libs), d["libraries"]);
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM track_tags", [], |r| r.get(0))
        .unwrap();
    assert_eq!(json!(n), d["track_tags_count"]);

    drop(st);
    // The extra steps, recorded after drop_snapshot on the state it left.
    let mut prev_step: Option<&Value> = None;
    let extra = g["extra_steps"].as_array().unwrap();
    for st in extra {
        replay_step(
            &w,
            &g,
            &mut conn,
            Some(&lib_s),
            "extra_steps",
            st,
            &prev_state,
            prev_step,
            &mut tracker,
        );
        prev_state = st["state"].clone();
        prev_step = Some(st);
    }
    assert!(extra.len() >= 24, "{}", extra.len());
    tracker.finish();
    let _ = Path::new("");
}

/// Replay one recorded step and compare it with the Python app's record (deltas
/// applied); adopt the Python app's state afterwards when the deltas say so.
#[allow(clippy::too_many_arguments)]
fn replay_step(
    w: &World,
    g: &Value,
    conn: &mut Connection,
    lib: Option<&str>,
    section: &str,
    st: &Value,
    prev_state: &Value,
    prev_step: Option<&Value>,
    tracker: &mut tv_deltas::Tracker,
) {
    let label = s(st, "label");
    let res = dispatch(w, conn, lib, s(st, "op"), &st["args"]);
    let (status, body) = match res {
        Ok(j) => (200, j.to_value()),
        Err(e) => (e.status, json!({"detail": e.detail})),
    };
    let (want, adopt) = tracker.expected(
        section,
        &tv_deltas::Inputs {
            step: st,
            prev_state,
            prev_step,
        },
    );
    assert_eq!(json!(status), want["status"], "{label}: status ({body})");
    assert_eq!(
        sort_album_lists(w.norm.value(&body)),
        sort_album_lists(want["body"].clone()),
        "{label}: body"
    );
    assert_eq!(w.state(g), want["state"], "{label}: state");
    assert_eq!(json!(w.take_events()), want["events"], "{label}: events");
    if adopt {
        tv_deltas::adopt(conn, &w.lib, &st["state"], &|d| w.id(d), NOW);
        assert_eq!(w.state(g), st["state"], "{label}: adopted state");
    }
}
