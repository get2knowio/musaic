//! The six album / track tag
//! routes, `/api/tags`, `/api/tags/{tag}/tracks` and the seven
//! `/api/vocabulary` routes, replayed through this router against
//! testdata/golden/services/tags_vocab.json (recorded from the Python app's
//! real routes driven through TestClient, no lifespan, network
//! off; two runs byte-identical).
//!
//! Every step, including the four recorded 422 request-validation cases
//! (`layer: "http"`), is sent as recorded, then (after drop_snapshot) the
//! golden's `extra_steps`; status, body, the `track_tags` /
//! `tag_vocabulary` rows, every album directory's sidecar text and dot-files,
//! and the bus events (none) must equal the Python app's, up to the enumerated
//! deltas shared with the service-level test
//! (musaic-services tests/deltas/tags_vocab.rs: a case-only vocabulary
//! rename, a Missing album's tags, unknown filenames and ASCII-only case
//! folding, each used once; after a step whose state differs the world
//! adopts the Python app's recorded state, checked).
//!
//! Every mutating tag / vocabulary route is refused with
//! 409 while a restore holds the gate; the reads are not.

mod common;
mod http_world;
#[path = "../../musaic-services/tests/deltas/tags_vocab.rs"]
mod tv_deltas;

use http_world::World;
use http_world::svc;
use serde_json::{Value, json};

fn golden() -> Value {
    svc::load_golden("tags_vocab.json")
}

/// The generator's `state()`.
fn state(w: &World, g: &Value) -> Value {
    let c = w.conn();
    let mut st = c
        .prepare("SELECT album_id, filename, tag, created_at FROM track_tags")
        .unwrap();
    let mut tt: Vec<Value> = st
        .query_map([], |r| {
            Ok(json!([
                w.norm.str(&r.get::<_, String>(0)?),
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                w.norm.str(&r.get::<_, String>(3)?),
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
                "created_at": w.norm.str(&r.get::<_, String>(4)?),
            }))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    voc.sort_by_key(|r| r["tag"].as_str().unwrap().to_string());
    let mut dirs = serde_json::Map::new();
    for a in g["albums"].as_array().unwrap() {
        let name = a["dir"].as_str().unwrap();
        let d = w.lib.join(name);
        if !d.is_dir() {
            dirs.insert(name.into(), Value::Null);
            continue;
        }
        let mut dots: Vec<String> = std::fs::read_dir(&d)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with('.'))
            .collect();
        dots.sort();
        dirs.insert(
            name.into(),
            json!({
                "sidecar": std::fs::read_to_string(d.join(".media-manager-tags.json")).ok(),
                "dotfiles": dots,
            }),
        );
    }
    json!({"track_tags": tt, "vocabulary": voc, "dirs": dirs})
}

/// `*_result.albums` are `sorted(album ids)`; ids hash the absolute path,
/// so the order depends on where the library lives (as in the services
/// test): sort the normalised names on both sides.
fn sort_album_lists(mut v: Value) -> Value {
    for k in ["rename_result", "merge_result", "delete_result"] {
        if let Some(Value::Array(a)) = v.get_mut(k).and_then(|r| r.get_mut("albums")) {
            a.sort_by_key(|x| x.as_str().unwrap_or_default().to_string());
        }
    }
    v
}

/// The generator's setup after the unconfigured steps: configure + scan,
/// remove Gamma and rescan (Missing), delete Delta's FLACs (cached row, no
/// tracks on disk).
async fn setup(w: &World, g: &Value) {
    w.configure_and_scan().await;
    assert_eq!(
        state(w, g),
        g["initial_state"],
        "state after the first scan"
    );
    std::fs::remove_dir_all(w.lib.join("Gamma - Three [FLAC]")).unwrap();
    assert_eq!(w.post("/api/scan/rescan", None).await.status, 200);
    let s = w.get("/api/scan/stream").await;
    assert!(s.text().contains("event: done"), "{}", s.text());
    for e in std::fs::read_dir(w.lib.join("Delta - Four [FLAC]")).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "flac") {
            std::fs::remove_file(p).unwrap();
        }
    }
    let c = w.conn();
    let mut st = c.prepare("SELECT id, missing_since FROM albums").unwrap();
    let presence: serde_json::Map<String, Value> = st
        .query_map([], |r| {
            Ok((
                w.norm.str(&r.get::<_, String>(0)?),
                Value::Bool(r.get::<_, Option<String>>(1)?.is_some()),
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(Value::Object(presence), g["presence_after_setup"]);
    w.take_events();
}

// Every recorded step, through HTTP.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tags_and_vocabulary_routes_replay_the_python_app() {
    let g = golden();
    let w = World::new(&g, &[], |b| b);
    let steps = g["steps"].as_array().unwrap();
    let mut configured = false;
    let mut http_layer = 0;
    let mut tracker = tv_deltas::Tracker::new();
    let mut prev_state = g["initial_state"].clone();
    let mut prev_step: Option<&Value> = None;
    for st in steps {
        let label = st["label"].as_str().unwrap();
        if !configured && !label.ends_with("unconfigured") {
            setup(&w, &g).await;
            configured = true;
        }
        replay(&w, &g, "steps", st, &prev_state, prev_step, &mut tracker).await;
        if st["layer"] == "http" {
            http_layer += 1;
        }
        prev_state = st["state"].clone();
        prev_step = Some(st);
    }
    assert_eq!(steps.len(), 71);
    assert_eq!(http_layer, 4, "the recorded 422 cases");

    // `GET /api/albums?refresh=true` drops the snapshot
    // (musaic_db::albums::drop_snapshot) exactly as the Python app's
    // `cache.drop_snapshot` left it: every album row of the library re-read
    // on the next scan (dir_mtime 0), scanned_at cleared, nothing deleted.
    let r = w.get("/api/albums?refresh=true").await;
    assert_eq!(r.status, 200);
    let d = &g["drop_snapshot"];
    let c = w.conn();
    let mut albums: Vec<Value> = c
        .prepare("SELECT id, dir_mtime, missing_since FROM albums")
        .unwrap()
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
    albums.sort_by_key(|r| r[0].as_str().unwrap().to_string());
    let mut want = d["albums"].as_array().unwrap().clone();
    want.sort_by_key(|r| r[0].as_str().unwrap().to_string());
    assert_eq!(albums, want, "drop_snapshot albums");
    let libs: Vec<Value> = c
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
    let n: i64 = c
        .query_row("SELECT COUNT(*) FROM track_tags", [], |r| r.get(0))
        .unwrap();
    assert_eq!(json!(n), d["track_tags_count"]);
    drop(c);

    // The extra steps, recorded after drop_snapshot.
    let mut prev_step: Option<&Value> = None;
    for st in g["extra_steps"].as_array().unwrap() {
        replay(
            &w,
            &g,
            "extra_steps",
            st,
            &prev_state,
            prev_step,
            &mut tracker,
        )
        .await;
        prev_state = st["state"].clone();
        prev_step = Some(st);
    }
    tracker.finish();
}

/// Send one recorded step and compare with the Python app's record (deltas
/// applied); adopt the Python app's state afterwards when the deltas say so.
async fn replay(
    w: &World,
    g: &Value,
    section: &str,
    st: &Value,
    prev_state: &Value,
    prev_step: Option<&Value>,
    tracker: &mut tv_deltas::Tracker,
) {
    let label = st["label"].as_str().unwrap();
    let (want, adopt) = tracker.expected(
        section,
        &tv_deltas::Inputs {
            step: st,
            prev_state,
            prev_step,
        },
    );
    let got = w.send(&st["request"]).await;
    let body = w.body(&got);
    assert_eq!(
        json!(got.status),
        want["status"],
        "{label}: status ({body})"
    );
    assert_eq!(
        sort_album_lists(body),
        sort_album_lists(want["body"].clone()),
        "{label}: body"
    );
    assert_eq!(state(w, g), want["state"], "{label}: state");
    assert_eq!(json!(w.take_events()), want["events"], "{label}: events");
    if adopt {
        tv_deltas::adopt(
            &w.conn(),
            &w.lib,
            &st["state"],
            &|d| w.id(d),
            "2030-01-01T00:00:00Z",
        );
        assert_eq!(state(w, g), st["state"], "{label}: adopted state");
    }
}

/// The mutating steps of the golden, one per route (method, path shape).
fn mutating_routes(g: &Value) -> Vec<Value> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for st in g["steps"].as_array().unwrap() {
        let r = &st["request"];
        if r["method"] == "GET" || st["status"] != 200 {
            continue;
        }
        if seen.insert(st["op"].as_str().unwrap().to_string()) {
            out.push(r.clone());
        }
    }
    out
}

// Mutations refused while a restore holds the gate;
// nothing changes. The reads still answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mutating_tag_routes_take_the_restore_gate() {
    let g = golden();
    let w = World::new(&g, &[], |b| b);
    w.configure_and_scan().await;
    let routes = mutating_routes(&g);
    let ops: Vec<String> = routes
        .iter()
        .map(|r| format!("{} {}", r["method"], r["path"]))
        .collect();
    // add/remove album tag, add/remove track tag, replace; vocabulary add,
    // patch, promote, rename, merge, delete.
    assert_eq!(routes.len(), 11, "{ops:#?}");
    // Work the scan set off in the background (post-scan check reruns) may
    // still hold the gate for a moment; a restore waits for it, as the
    // route's caller would retry.
    let restore = {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            match w.state.gate().begin_restore() {
                Ok(g) => break g,
                Err(_) if std::time::Instant::now() < deadline => {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
                Err(e) => panic!("gate still busy after 30 s: {e}"),
            }
        }
    };
    let before = state(&w, &g);
    for r in &routes {
        let got = w.send(r).await;
        assert_eq!(got.status, 409, "{r}: {}", got.text());
        assert_eq!(got.json()["detail"], musaic_api::gate::WORK_REFUSED, "{r}");
    }
    assert_eq!(state(&w, &g), before, "nothing changed");
    for path in ["/api/tags", "/api/vocabulary"] {
        assert_eq!(w.get(path).await.status, 200, "{path}");
    }
    let a = w.id("Alpha - One [FLAC]");
    assert_eq!(w.get(&format!("/api/albums/{a}/tags")).await.status, 200);
    drop(restore);
    // Released: the first mutation goes through again, and holds nothing.
    let got = w.send(&routes[0]).await;
    assert_eq!(got.status, 200, "{}", got.text());
    assert_eq!(w.state.gate().snapshot(), (0, false));
}
