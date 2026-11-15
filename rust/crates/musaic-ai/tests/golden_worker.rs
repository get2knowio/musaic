//! Every worker
//! scenario of testdata/golden/ai/worker.json (recorded from the Python app:
//! the Python app's `services.worker.run_one_job` with a scripted airframe
//! runtime, a stubbed resolver / cover check / MusicBrainz, network off; two
//! runs byte-identical) replayed through `musaic_ai::run_one_job` with the
//! replay provider playing the same scripts and a stub env feeding the same
//! outside world: the job dict, the stored `result_json` text, the ai_jobs
//! row, the album's anchor columns, the queue-paused setting, the
//! `AlbumMbAnchorSet` events and every provider request (system prompt,
//! user message, image, MCP servers, max turns).
//!
//! Enumerated deltas (each must be used):
//! - PROVIDER-MODEL: the Python app's request names a model
//!   (`["claude", "claude-sonnet-4-6"]`) and negotiates two hosted web
//!   tools; the Rust request carries neither (model and tools are the
//!   provider's concern).
//! - PREFLIGHT: "provider-not-ready" fails in the Python app's preflight before
//!   any session is opened (no request recorded); here the provider itself
//!   answers `NotReady` with the Python app's requirement / remedy text, so one
//!   request is recorded. Job row, message and pause are the Python app's.
//! - TIMEOUT: the generator raised `TimeoutError` from the stream; here the
//!   replay provider hangs and the worker's cap (shortened for the test)
//!   fires. Same message. The excerpt differs in one way, in the generator's
//!   simulation and not in the Python app: ending the stream with an exception
//!   runs the recorder's `finally: recorder.finish()`, which flushes the
//!   text still buffered ("waiting (never flushed)"). A real wall-clock cap
//!   (`asyncio.wait_for`, worker.py:304-314) cancels the await while the
//!   call's thread is still streaming, so the excerpt is read (worker.py:433)
//!   before any flush, as in a recorded real run, whose excerpt ends on a
//!   tool line. This build does the latter: the trailing line is
//!   removed from the Python app's expectation.
//! - UNACKED (deliberate change): "provider-mbid-404",
//!   "provider-mbid-network-error" and "provider-mbid-blocklisted" end with
//!   no anchor stored, so the job stays `completed` for the user to review;
//!   the Python app auto-acknowledged them (`acted_on` / `applied`). Only the
//!   acknowledgement fields of `job` / `job_row` change; everything else is
//!   compared as recorded.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use common::{StubEnv, golden, job_id, norm, norm_value, rows, script};
use musaic_ai::provider::ProviderError;
use musaic_ai::provider::replay::{Ending, Script};
use musaic_ai::{Provider, Py, RunOptions};
use musaic_db::ai_jobs::{self, NewJob};
use serde_json::{Value, json};
use sha2::Digest;

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

fn system_name(s: &str) -> String {
    use musaic_ai::prompts::*;
    for (name, txt) in [
        ("metadata", SYSTEM_PROMPT_METADATA),
        ("cover", SYSTEM_PROMPT_COVER),
    ] {
        if s == txt {
            return name.into();
        }
        if s == format!("{txt}{FIRECRAWL_PROMPT_HINT}") {
            return format!("{name}+firecrawl");
        }
    }
    "<other>".into()
}

#[test]
fn worker_scenarios_replay_the_python_app() {
    let g = golden("worker.json");
    let units = golden("units.json");
    let tmp = tempfile::tempdir().unwrap();
    let env = setup(&g, tmp.path());
    let mut used: BTreeMap<&str, usize> = BTreeMap::from([
        ("PROVIDER-MODEL", 0),
        ("PREFLIGHT", 0),
        ("TIMEOUT", 0),
        ("UNACKED", 0),
    ]);
    let mut failures = Vec::new();
    let scenarios = g["scenarios"].as_array().unwrap();
    assert_eq!(scenarios.len(), 29);
    for s in scenarios {
        let label = s["id"].as_str().unwrap();
        let conn = env.open_conn();
        conn.execute_batch(
            "DELETE FROM ai_jobs; DELETE FROM album_blocked_mb_release_groups; \
             UPDATE albums SET mb_release_group_mbid = NULL, mb_release_mbid = NULL, \
             mb_match_score = NULL, mb_last_checked_at = NULL, metadata_ai_acted_on_at = NULL;",
        )
        .unwrap();
        musaic_ai::jobs::set_queue_paused(&conn, false).unwrap();
        let album_id = s["album_id"].as_str().unwrap().to_string();
        for m in s["blocked"].as_array().unwrap() {
            musaic_db::mb_anchor::add_blocked_mb_release_group(
                &conn,
                &album_id,
                m.as_str().unwrap(),
                "2001-01-01T00:00:00Z",
            )
            .unwrap();
        }
        *env.resolution.lock().unwrap() = Some(s["resolution"].clone());
        *env.firecrawl.lock().unwrap() = s["firecrawl"].as_str().unwrap().into();
        *env.verify.lock().unwrap() = s["verify"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(u, v)| {
                (
                    u.clone(),
                    (v[0].as_bool().unwrap(), v[1].as_str().unwrap().to_string()),
                )
            })
            .collect();
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

        let replay = common::replay();
        if s["no_cli"].as_bool().unwrap() {
            // PREFLIGHT delta: the provider reports what the Python app's
            // preflight reported for a missing `claude` CLI.
            let st = &units["provider_runtime_status"]["claude_missing"];
            replay.push(Script {
                events: vec![],
                ending: Ending::Fail(ProviderError::NotReady {
                    requirement: st["requirement"].as_str().unwrap().into(),
                    remedy: st["remedy"].as_str().unwrap().into(),
                }),
            });
            *used.get_mut("PREFLIGHT").unwrap() += 1;
        }
        let mut hangs = false;
        for sc in s["scripts"].as_array().unwrap() {
            let sc = script(sc);
            hangs |= sc.ending == Ending::Hang;
            replay.push(sc);
        }
        if hangs {
            *used.get_mut("TIMEOUT").unwrap() += 1;
        }
        let provider: Arc<dyn Provider> = Arc::new(replay.clone());
        let opts = RunOptions {
            call_timeout: if hangs {
                Duration::from_millis(300)
            } else {
                Duration::from_secs(30)
            },
            ..RunOptions::default()
        };
        musaic_ai::run_one_job(&env, &provider, &job, &opts);
        replay.release();
        assert_eq!(replay.pending(), 0, "{label}: unused scripts");

        let conn = env.open_conn();
        let row = ai_jobs::get_job(&conn, &job.id).unwrap().unwrap();
        let mut got = json!({
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
        let mut calls: Vec<Value> = replay
            .take_requests()
            .iter()
            .map(|r| {
                json!({
                    "system": system_name(&r.system_prompt),
                    "mcp_servers": r.mcp_servers.iter().map(|m| json!({
                        "name": m.name, "transport": m.transport, "url": m.url})).collect::<Vec<_>>(),
                    "image": r.image.as_ref().map(|i| json!({
                        "sha256": hex(&sha2::Sha256::digest(&i.bytes)),
                        "media_type": i.media_type, "first": true})),
                    "user_message": r.user_message,
                    "max_turns": r.max_turns,
                })
            })
            .collect();
        let mut want_calls: Vec<Value> = s["provider_calls"].as_array().unwrap().clone();
        for c in &mut want_calls {
            // PROVIDER-MODEL delta.
            let o = c.as_object_mut().unwrap();
            assert_eq!(
                o.remove("model"),
                Some(json!(["claude", "claude-sonnet-4-6"]))
            );
            assert_eq!(o.remove("native_tools"), Some(json!(2)));
            *used.get_mut("PROVIDER-MODEL").unwrap() += 1;
        }
        if s["no_cli"].as_bool().unwrap() {
            assert_eq!(calls.len(), 1, "{label}");
            assert!(want_calls.is_empty());
            calls.clear();
        }
        got["provider_calls"] = json!(calls);
        let mut want = json!({
            "job": s["job"],
            "result_json": s["result_json"],
            "job_row": s["job_row"],
            "album_row": s["album_row"],
            "ai_queue_paused": s["ai_queue_paused"],
            "events": s["events"],
            "provider_calls": want_calls,
        });
        if hangs {
            // Structural: plain strings carry a newline, stored JSON text
            // carries the two characters backslash-n.
            fn strip(v: &mut Value, n: &mut usize) {
                match v {
                    Value::String(x) => {
                        for pat in ["\nwaiting (never flushed)", "\\nwaiting (never flushed)"] {
                            if x.contains(pat) {
                                *x = x.replace(pat, "");
                                *n += 1;
                            }
                        }
                    }
                    Value::Array(a) => a.iter_mut().for_each(|x| strip(x, n)),
                    Value::Object(o) => o.values_mut().for_each(|x| strip(x, n)),
                    _ => {}
                }
            }
            let mut n = 0;
            strip(&mut want, &mut n);
            if n > 0 {
                *used.get_mut("TIMEOUT").unwrap() += 1;
            }
        }
        if [
            "provider-mbid-404",
            "provider-mbid-network-error",
            "provider-mbid-blocklisted",
        ]
        .contains(&label)
        {
            *used.get_mut("UNACKED").unwrap() += 1;
            for k in ["job", "job_row"] {
                assert_eq!(
                    want[k]["status"], "acted_on",
                    "{label}: the Python app acknowledged it"
                );
                want[k]["status"] = json!("completed");
                want[k]["acted_on_action"] = Value::Null;
                want[k]["acted_on_at"] = Value::Null;
            }
        }
        if got != want {
            for k in want.as_object().unwrap().keys() {
                if got[k] != want[k] {
                    failures.push(format!(
                        "== {label}: {k}\n-- python: {}\n-- rust:   {}",
                        want[k], got[k]
                    ));
                }
            }
        }
    }
    assert!(used.values().all(|n| *n > 0), "unused delta: {used:?}");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

impl StubEnv {
    fn open_conn(&self) -> rusqlite::Connection {
        musaic_db::open_connection(&self.db).unwrap()
    }
}
