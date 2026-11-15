//! Backend text names AI only when a provider is ready: the worker scenarios of testdata/golden/ai/worker.json
//! that a build with no ready provider can reach, replayed with this build's
//! provider (`musaic_ai::Unconfigured`, never ready) instead of the replay
//! provider (ready; golden_worker.rs compares those byte for byte).
//!
//! Reachable without a provider: every scenario the resolver covers (no
//! provider call: the synthesizers), and the cover job whose provider call
//! fails and falls back to MusicBrainz (here the provider refuses at once).
//! Each must match the Python app's record (job dict, stored `result_json` text,
//! ai_jobs row, album anchor columns, queue-paused setting, anchor events)
//! with these enumerated deltas, each of which must occur:
//!
//! - DERIVED: `synth::DERIVED_NOTES` reads `synth::DERIVED_NOTES_NEUTRAL`;
//! - FALLBACK: `worker::FALLBACK_COVER_PREFIX` reads
//!   `worker::FALLBACK_COVER_PREFIX_NEUTRAL`.
//!
//! The Python app's provider request of the fallback scenario is not compared
//! (no provider call reaches a model here). No result text names AI or
//! Claude afterwards.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use common::{StubEnv, golden, job_id, norm, norm_value, rows};
use musaic_ai::synth::{DERIVED_NOTES, DERIVED_NOTES_NEUTRAL};
use musaic_ai::worker::{FALLBACK_COVER_PREFIX, FALLBACK_COVER_PREFIX_NEUTRAL};
use musaic_ai::{Provider, Py, RunOptions};
use musaic_db::ai_jobs::{self, NewJob};
use serde_json::{Value, json};

const SCENARIOS: [&str; 6] = [
    "skip-synth-changes",
    "skip-synth-noop-autoack",
    "skip-edge-0.85",
    "cover-job-resolver-cover",
    "metadata+cover-both-resolved",
    "cover-job-provider-error-mb-fallback",
];

fn setup(g: &Value, dir: &std::path::Path) -> StubEnv {
    let mut albums = BTreeMap::new();
    let mut tracks = BTreeMap::new();
    let mut ids = Vec::new();
    for (d, a) in g["album_dicts"].as_object().unwrap() {
        let id = a["id"].as_str().unwrap().to_string();
        let path = a["path"].as_str().unwrap().to_string();
        albums.insert(id.clone(), Py::from_value(a));
        tracks.insert(path, Py::from_value(&g["track_dicts"][d]));
        ids.push(id);
    }
    let db = common::fresh_db(dir, &ids);
    let mut env = StubEnv::new(db, albums, tracks);
    env.thumb = Some(("<ID:Alpha - One [FLAC]>".into(), common::thumb_bytes(g)));
    env
}

/// Applies `from -> to` to every string in `v`; returns how many changed.
fn replace_all(v: &mut Value, from: &str, to: &str) -> usize {
    match v {
        Value::String(x) if x.contains(from) => {
            *x = x.replace(from, to);
            1
        }
        Value::Array(a) => a.iter_mut().map(|x| replace_all(x, from, to)).sum(),
        Value::Object(o) => o.values_mut().map(|x| replace_all(x, from, to)).sum(),
        _ => 0,
    }
}

#[test]
fn no_provider_scenarios_replay_the_python_app_with_neutral_notes() {
    let g = golden("worker.json");
    let tmp = tempfile::tempdir().unwrap();
    let env = setup(&g, tmp.path());
    let provider: Arc<dyn Provider> = Arc::new(musaic_ai::Unconfigured);
    assert!(!provider.info().ready);
    let mut used: BTreeMap<&str, usize> = BTreeMap::from([("DERIVED", 0), ("FALLBACK", 0)]);
    let mut failures = Vec::new();
    for label in SCENARIOS {
        let s = g["scenarios"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == label)
            .unwrap_or_else(|| panic!("{label}"));
        assert!(!s["no_cli"].as_bool().unwrap());
        let conn = env.open_conn();
        conn.execute_batch(
            "DELETE FROM ai_jobs; DELETE FROM album_blocked_mb_release_groups; \
             UPDATE albums SET mb_release_group_mbid = NULL, mb_release_mbid = NULL, \
             mb_match_score = NULL, mb_last_checked_at = NULL, metadata_ai_acted_on_at = NULL;",
        )
        .unwrap();
        musaic_ai::jobs::set_queue_paused(&conn, false).unwrap();
        let album_id = s["album_id"].as_str().unwrap().to_string();
        assert!(s["blocked"].as_array().unwrap().is_empty(), "{label}");
        *env.resolution.lock().unwrap() = Some(s["resolution"].clone());
        *env.firecrawl.lock().unwrap() = s["firecrawl"].as_str().unwrap().into();
        *env.find_cover.lock().unwrap() = s["find_cover"].clone();
        *env.mb_status.lock().unwrap() = s["mb_status"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        env.anchor_events.lock().unwrap().clear();

        let want_row = &s["job_row"];
        let mut conn = conn;
        let job = ai_jobs::enqueue(
            &mut conn,
            &NewJob {
                album_id: &album_id,
                album_path: want_row["album_path"].as_str().unwrap(),
                album_title: want_row["album_title"].as_str().unwrap(),
                job_type: s["job_type"].as_str().unwrap(),
                hint: s["hint"].as_str().unwrap(),
                attach_cover: s["attach_cover"].as_bool().unwrap(),
            },
            || job_id(want_row["id"].as_str().unwrap()),
            &env.clock.now(),
        )
        .unwrap()
        .unwrap();
        musaic_ai::run_one_job(&env, &provider, &job, &RunOptions::default());

        let conn = env.open_conn();
        let row = ai_jobs::get_job(&conn, &job.id).unwrap().unwrap();
        let got = json!({
            "job": norm_value(&musaic_ai::job_to_py(&row).to_value()),
            "result_json": row.result_json.as_deref().map(norm),
            "job_row": rows(&conn, "SELECT * FROM ai_jobs")[0],
            "album_row": rows(&conn, &format!(
                "SELECT id, mb_release_group_mbid, mb_release_mbid, mb_match_score, \
                 mb_last_checked_at, metadata_ai_acted_on_at FROM albums WHERE id = '{}'",
                album_id.replace('\'', "''"))),
            "ai_queue_paused": musaic_db::settings::get_raw(&conn, "ai_queue_paused").unwrap(),
            "events": env.anchor_events.lock().unwrap().iter()
                .map(|a| json!({"type": "AlbumMbAnchorSet", "album_id": a, "source": "ai"}))
                .collect::<Vec<_>>(),
        });
        let mut want = json!({
            "job": s["job"],
            "result_json": s["result_json"],
            "job_row": s["job_row"],
            "album_row": s["album_row"],
            "ai_queue_paused": s["ai_queue_paused"],
            "events": s["events"],
        });
        *used.get_mut("DERIVED").unwrap() +=
            replace_all(&mut want, DERIVED_NOTES, DERIVED_NOTES_NEUTRAL);
        *used.get_mut("FALLBACK").unwrap() += replace_all(
            &mut want,
            FALLBACK_COVER_PREFIX,
            FALLBACK_COVER_PREFIX_NEUTRAL,
        );
        let text = got.to_string();
        assert!(
            !text.contains("Claude") && !text.contains(" AI "),
            "{label}: {text}"
        );
        if got != want {
            for k in want.as_object().unwrap().keys() {
                if got[k] != want[k] {
                    failures.push(format!(
                        "== {label}: {k}\n-- python+delta: {}\n-- rust:   {}",
                        want[k], got[k]
                    ));
                }
            }
        }
    }
    assert!(used.values().all(|n| *n > 0), "unused delta: {used:?}");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

trait OpenConn {
    fn open_conn(&self) -> rusqlite::Connection;
}

impl OpenConn for StubEnv {
    fn open_conn(&self) -> rusqlite::Connection {
        musaic_db::open_connection(&self.db).unwrap()
    }
}
