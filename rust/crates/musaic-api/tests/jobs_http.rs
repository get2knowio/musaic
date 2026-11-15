//! The 11
//! AI-queue routes replayed against testdata/golden/ai/http.json
//! (recorded from the Python app's real routes through
//! TestClient, no lifespan, network off, the AI layer driven by scripted
//! airframe events; two runs byte-identical).
//!
//! Same library (built from the golden's recipes), configured and scanned,
//! the Python app's thumbnail for "Alpha - One"; job ids and the clock scripted
//! as the generator's (`0000000000000001`, ... and one second per call), so
//! queue order and ids line up. Then every case in order:
//! - `http`: status, content type, body text byte for byte (Starlette's
//!   compact rendering), FastAPI's 422 bodies included (pydantic's error
//!   key order); bus events; the
//!   `ai_jobs` rows, the albums' anchor / acted-on columns and the paused
//!   setting after the step.
//! - `run_next`: where the generator ran `worker.run_one_job` on the oldest
//!   queued job, this test runs `AppState::ai_run_next()` with the replay
//!   provider playing the same scripts; the provider requests (system
//!   prompt, user message built from THIS build's offline resolution, image,
//!   MCP servers, max turns) and the state after are compared.
//! - `sse`: the stream's first frame, and the next after the recorded
//!   action, byte for byte.
//! - `setup`: the generator's direct calls (`mark_running`,
//!   `reset_stuck_running`).
//!
//! Enumerated deltas (each must be used; anything else fails):
//! - PROVIDERS (this build installs no other provider): `GET
//!   /api/ai/providers` lists this build's provider (Unconfigured / the
//!   test's replay provider) instead of the installed airframe adapters,
//!   and `uninstalled` is empty; `selected` is the Python app's.
//! - PROVIDER-MODEL: the Python app's request names a model and two hosted web
//!   tools; this build's request carries neither (see musaic-ai's
//!   golden_worker.rs).
//! - RETRY-ACTIVE: `retry C failed`
//!   retries a failed job while the same album has another queued job; the
//!   Python app queued it (two active jobs for one album), this build answers
//!   409 in the enqueue conflict's shape naming the active job and changes
//!   nothing. The replay then puts job C where the Python app's retry left it
//!   (`musaic_db::ai_jobs::retry_failed`, the Python app's own rule, excerpt
//!   kept) so the later cases still compare against the recording.

mod common;
mod http_world;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use http_world::World;
use musaic_ai::Provider;
use musaic_ai::provider::replay::ReplayProvider;
use regex::Regex;
use serde_json::{Value, json};
use sha2::Digest;
use tower::ServiceExt;

fn golden_ai(name: &str) -> Value {
    let p = musaic_testdata::public("golden/ai").join(name);
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

/// The recorded scripts as replay scripts (the musaic-ai harness's rules).
mod scripts {
    use musaic_ai::Py;
    use musaic_ai::provider::replay::{Ending, Script};
    use musaic_ai::provider::{ProviderError, ProviderEvent};
    use serde_json::Value;

    pub fn script(s: &Value) -> Script {
        let events = s["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| match e["type"].as_str().unwrap() {
                "text" => ProviderEvent::Text(e["text"].as_str().unwrap().into()),
                "reasoning" => ProviderEvent::Reasoning(e["text"].as_str().unwrap().into()),
                "tool_start" => ProviderEvent::ToolCallStart {
                    tool_name: e["name"].as_str().unwrap().into(),
                    arguments_preview: e["args"].as_str().unwrap().into(),
                },
                "tool_result" => ProviderEvent::ToolCallResult {
                    output: Py::from_value(&e["output"]),
                    is_error: e["is_error"].as_bool().unwrap(),
                },
                "complete" => ProviderEvent::TurnComplete {
                    text: e["text"].as_str().unwrap().into(),
                },
                other => panic!("{other}"),
            })
            .collect();
        let ending = match s.get("raise") {
            None => Ending::Done,
            Some(r) => match r["kind"].as_str().unwrap() {
                "auth" => Ending::Fail(ProviderError::Auth(r["message"].as_str().unwrap().into())),
                "transient" => Ending::Fail(ProviderError::Transient(
                    r["message"].as_str().unwrap().into(),
                )),
                other => panic!("unscripted raise {other}"),
            },
        };
        Script { events, ending }
    }
}

struct Run {
    w: World,
    replay: ReplayProvider,
    ids: Vec<(String, String)>,
    iso: Regex,
    job: Regex,
}

impl Run {
    fn n(&self, s: &str) -> String {
        let mut s = s.replace(&self.w.lib_str(), "<LIB>");
        for (id, d) in &self.ids {
            s = s.replace(id, &format!("<ID:{d}>"));
        }
        let s = self
            .job
            .replace_all(&s, |c: &regex::Captures| {
                format!("<JOB:{}>", u64::from_str_radix(&c[0], 16).unwrap())
            })
            .into_owned();
        self.iso.replace_all(&s, "<NOW>").into_owned()
    }

    fn un(&self, s: &str) -> String {
        let mut s = self.w.unnorm(s);
        let re = Regex::new(r"<JOB:(\d+)>").unwrap();
        s = re
            .replace_all(&s, |c: &regex::Captures| {
                format!("{:016x}", c[1].parse::<u64>().unwrap())
            })
            .into_owned();
        s
    }

    fn rows(&self, sql: &str) -> Vec<Value> {
        let conn = self.w.conn();
        let mut st = conn.prepare(sql).unwrap();
        let names: Vec<String> = st.column_names().iter().map(|s| s.to_string()).collect();
        st.query_map([], |r| {
            let mut m = serde_json::Map::new();
            for (i, n) in names.iter().enumerate() {
                let v = match r.get_ref(i).unwrap() {
                    rusqlite::types::ValueRef::Null => Value::Null,
                    rusqlite::types::ValueRef::Integer(x) => json!(x),
                    rusqlite::types::ValueRef::Real(x) => json!(x),
                    rusqlite::types::ValueRef::Text(t) => {
                        Value::String(self.n(&String::from_utf8_lossy(t)))
                    }
                    rusqlite::types::ValueRef::Blob(_) => unreachable!(),
                };
                m.insert(n.clone(), v);
            }
            Ok(Value::Object(m))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    }

    fn state(&self) -> Value {
        let mut albums = self.rows(
            "SELECT id, mb_release_group_mbid, mb_release_mbid, mb_match_score, \
             mb_last_checked_at, metadata_ai_acted_on_at FROM albums",
        );
        albums.sort_by_key(|a| a["id"].to_string());
        let paused = musaic_db::settings::get_raw(&self.w.conn(), "ai_queue_paused").unwrap();
        json!({
            "ai_jobs": self.rows("SELECT * FROM ai_jobs ORDER BY rowid"),
            "albums": albums,
            "ai_queue_paused": paused,
        })
    }

    async fn request(&self, req: &Value) -> common::Got {
        let method = req["method"].as_str().unwrap();
        let mut path = self.un(req["path"].as_str().unwrap());
        if let Some(p) = req.get("params").and_then(Value::as_object) {
            let q: Vec<String> = p
                .iter()
                .map(|(k, v)| format!("{k}={}", v.as_str().unwrap().replace(' ', "%20")))
                .collect();
            path = format!("{path}?{}", q.join("&"));
        }
        let mut b = Request::builder().method(method).uri(path);
        let body = if let Some(j) = req.get("json") {
            b = b.header("content-type", "application/json");
            let j = self.w.unnorm_value(j);
            Body::from(serde_json::to_vec(&j).unwrap())
        } else if let Some(r) = req.get("raw") {
            b = b.header("content-type", "application/json");
            Body::from(r.as_str().unwrap().to_string())
        } else {
            Body::empty()
        };
        common::send(&self.w.app, b.body(body).unwrap()).await
    }
}

fn setup() -> (Run, Value) {
    let g = golden_ai("http.json");
    // The recipes' raw-byte files are written after the build.
    let mut recipes = g.clone();
    let mut raw_files = Vec::new();
    for a in recipes["albums"].as_array_mut().unwrap() {
        let dir = a["dir"].as_str().unwrap().to_string();
        let files = a["files"].as_array_mut().unwrap();
        files.retain(|f| {
            if let Some(b) = f.get("b64") {
                use base64::Engine;
                raw_files.push((
                    dir.clone(),
                    f["name"].as_str().unwrap().to_string(),
                    base64::engine::general_purpose::STANDARD
                        .decode(b.as_str().unwrap())
                        .unwrap(),
                ));
                false
            } else {
                true
            }
        });
    }
    let replay = ReplayProvider::new("claude");
    let r2 = replay.clone();
    let next_id = Arc::new(AtomicU64::new(0));
    let tick = Arc::new(AtomicU64::new(0));
    let w = World::new(&recipes, &[], move |b| {
        let provider: Arc<dyn Provider> = Arc::new(r2);
        b.ai_provider(provider)
            .ai_job_ids(Arc::new(move || {
                format!("{:016x}", next_id.fetch_add(1, Ordering::SeqCst) + 1)
            }))
            .clock(Arc::new(move || {
                UNIX_EPOCH
                    + Duration::from_secs(1_767_225_600 + tick.fetch_add(1, Ordering::SeqCst) + 1)
            }))
    });
    for (d, name, bytes) in raw_files {
        std::fs::write(w.lib.join(d).join(name), bytes).unwrap();
    }
    let ids: Vec<(String, String)> = w
        .ids
        .iter()
        .map(|(d, id)| (id.clone(), d.clone()))
        .collect();
    let run = Run {
        w,
        replay,
        ids,
        iso: Regex::new(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?(?:Z|\+00:00)").unwrap(),
        job: Regex::new(r"\b0{8}[0-9a-f]{8}\b").unwrap(),
    };
    (run, g)
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

/// Read one SSE frame (`event: ...\n\n`) from a streaming body.
async fn next_frame(body: &mut Body) -> String {
    let mut buf = String::new();
    loop {
        let f = tokio::time::timeout(Duration::from_secs(10), body.frame())
            .await
            .expect("a frame within 10 s")
            .unwrap()
            .unwrap();
        if let Ok(d) = f.into_data() {
            buf.push_str(&String::from_utf8_lossy(&d));
        }
        if buf.ends_with("\n\n") {
            return buf;
        }
    }
}

// Every AI-queue route (+ the enumerated deltas).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ai_queue_routes_replay_the_python_app() {
    let (run, g) = setup();
    run.w.configure_and_scan().await;
    assert!(!run.w.state.ai_worker_running(), "no lifespan: no worker");
    let alpha = run.w.id("Alpha - One [FLAC]");
    let thumbs = run.w.cfg.join("thumbs");
    std::fs::create_dir_all(&thumbs).unwrap();
    {
        use base64::Engine;
        std::fs::write(
            thumbs.join(format!("{alpha}.jpg")),
            base64::engine::general_purpose::STANDARD
                .decode(g["_meta"]["thumb_b64"].as_str().unwrap())
                .unwrap(),
        )
        .unwrap();
    }
    run.w.take_events();
    let mut used: BTreeMap<&str, usize> =
        BTreeMap::from([("PROVIDERS", 0), ("PROVIDER-MODEL", 0), ("RETRY-ACTIVE", 0)]);
    let mut failures = Vec::new();
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    for case in g["cases"].as_array().unwrap() {
        let label = case["label"].as_str().unwrap();
        let kind = case["kind"].as_str().unwrap();
        *kinds.entry(kind.to_string()).or_default() += 1;
        let mut got = json!({});
        let mut want = json!({});
        match kind {
            "http" => {
                let r = run.request(&case["request"]).await;
                let resp = &case["response"];
                got["status"] = json!(r.status);
                want["status"] = resp["status"].clone();
                got["content_type"] = json!(r.ctype());
                want["content_type"] = resp["content_type"].clone();
                if label == "retry C failed" {
                    *used.get_mut("RETRY-ACTIVE").unwrap() += 1;
                    assert_eq!(resp["status"], 200, "the Python app queued it");
                    assert_eq!(r.status, 409, "{label}: {}", r.text());
                    let d = &r.json()["detail"];
                    assert_eq!(d["error"], "active_job_exists");
                    assert_eq!(d["existing_status"], "queued");
                    let c = case["request"]["path"].as_str().unwrap();
                    let job_c = ids_of(&run, c.split('/').nth(4).unwrap());
                    let other = d["existing_job_id"].as_str().unwrap();
                    assert_ne!(other, job_c);
                    // Nothing changed: C is still failed. Then align with
                    // the Python app's state for the rest of the replay.
                    let mut conn = run.w.conn();
                    let row = musaic_db::ai_jobs::get_job(&conn, &job_c).unwrap().unwrap();
                    assert_eq!(row.status, "failed");
                    // The Python app's retry took the clock's next tick, after
                    // every job created so far.
                    let last: String = conn
                        .query_row("SELECT max(created_at) FROM ai_jobs", [], |r| r.get(0))
                        .unwrap();
                    let now = one_second_after(&last);
                    musaic_db::ai_jobs::retry_failed(&mut conn, &job_c, &now)
                        .unwrap()
                        .unwrap();
                    got["status"] = resp["status"].clone();
                    got["text"] = resp["text"].clone();
                    want["text"] = resp["text"].clone();
                } else if label == "providers" {
                    *used.get_mut("PROVIDERS").unwrap() += 1;
                    let j = r.json();
                    assert_eq!(j["selected"], resp["json"]["selected"], "{label}");
                    assert_eq!(j["uninstalled"], json!([]));
                    let p = &j["providers"][0];
                    assert_eq!(p["id"], "claude", "the test's replay provider");
                    assert_eq!(j["providers"].as_array().unwrap().len(), 1);
                } else {
                    got["text"] = json!(run.n(&r.text()));
                    want["text"] = resp["text"].clone();
                }
            }
            "run_next" => {
                for s in case["scripts"].as_array().unwrap() {
                    run.replay.push(scripts::script(s));
                }
                let st = run.w.state.clone();
                let id = tokio::task::spawn_blocking(move || st.ai_run_next())
                    .await
                    .unwrap();
                assert_eq!(
                    id.map(|i| run.n(&i)),
                    Some(case["job_id"].as_str().unwrap().into())
                );
                assert_eq!(run.replay.pending(), 0, "{label}: unused scripts");
                let calls: Vec<Value> = run
                    .replay
                    .take_requests()
                    .iter()
                    .map(|r| {
                        json!({
                            "system": system_name(&r.system_prompt),
                            "mcp_servers": r.mcp_servers.iter().map(|m| json!({
                                "name": m.name, "transport": m.transport, "url": m.url})).collect::<Vec<_>>(),
                            "image": r.image.as_ref().map(|i| json!({
                                "sha256": sha2::Sha256::digest(&i.bytes).iter().map(|b| format!("{b:02x}")).collect::<String>(),
                                "media_type": i.media_type, "first": true})),
                            "user_message": run.n(&r.user_message),
                            "max_turns": r.max_turns,
                        })
                    })
                    .collect();
                let mut want_calls = case["provider_calls"].as_array().unwrap().clone();
                for c in &mut want_calls {
                    let o = c.as_object_mut().unwrap();
                    assert!(o.remove("model").is_some());
                    assert!(o.remove("native_tools").is_some());
                    *used.get_mut("PROVIDER-MODEL").unwrap() += 1;
                }
                got["provider_calls"] = json!(calls);
                want["provider_calls"] = json!(want_calls);
            }
            "sse" => {
                let resp = run
                    .w
                    .app
                    .clone()
                    .oneshot(common::get("/api/ai/jobs/stream"))
                    .await
                    .unwrap();
                assert_eq!(resp.status(), 200);
                assert_eq!(
                    resp.headers()["content-type"],
                    musaic_api::sse::CONTENT_TYPE
                );
                let mut body = resp.into_body();
                let mut frames = vec![next_frame(&mut body).await];
                match case["action"].as_str() {
                    None => {}
                    Some("POST /api/ai/queue/resume") => {
                        let r = run.w.post("/api/ai/queue/resume", None).await;
                        assert_eq!(r.status, 200);
                        frames.push(next_frame(&mut body).await);
                    }
                    Some("POST /api/ai/jobs (B metadata)") => {
                        let b = run.w.id("Bravo - Two [FLAC]");
                        let r = run
                            .w
                            .post(
                                "/api/ai/jobs",
                                Some(&json!({"album_id": b, "job_type": "metadata"})),
                            )
                            .await;
                        assert_eq!(r.status, 200, "{}", r.text());
                        frames.push(next_frame(&mut body).await);
                    }
                    Some(other) => panic!("unknown action {other}"),
                }
                drop(body);
                got["frames"] = json!(frames.iter().map(|f| run.n(f)).collect::<Vec<_>>());
                want["frames"] = case["frames"].clone();
            }
            "setup" => {
                let conn = run.w.conn();
                match case["op"].as_str().unwrap() {
                    "mark_running D" => {
                        let d = ids_of(&run, "<JOB:4>");
                        musaic_db::ai_jobs::mark_running(&conn, &d, &run.w.state_now()).unwrap();
                    }
                    "reset_stuck_running" => {
                        let n = musaic_db::ai_jobs::reset_stuck_running(&conn).unwrap();
                        got["returned"] = json!(n);
                        want["returned"] = case["returned"].clone();
                    }
                    other => panic!("unknown op {other}"),
                }
            }
            other => panic!("unknown case kind {other}"),
        }
        got["events"] = json!(run.w.take_events());
        want["events"] = case["events"].clone();
        got["state"] = run.state();
        want["state"] = case["state"].clone();
        if got != want {
            for k in want.as_object().unwrap().keys() {
                if got[k] != want[k] {
                    failures.push(format!(
                        "== {label} ({kind}): {k}\n-- python: {}\n-- rust:   {}",
                        want[k], got[k]
                    ));
                }
            }
        }
    }
    assert_eq!(
        kinds,
        BTreeMap::from([
            ("http".to_string(), 57),
            ("run_next".to_string(), 3),
            ("setup".to_string(), 2),
            ("sse".to_string(), 4),
        ])
    );
    assert!(used.values().all(|n| *n > 0), "unused delta {used:?}");
    assert!(
        failures.is_empty(),
        "{} differences:\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert_eq!(run.w.state.gate().snapshot(), (0, false), "gate released");
}

/// `YYYY-MM-DDTHH:MM:SS...` plus one second (within the day; the scripted
/// clock starts at midnight and ticks once per call).
fn one_second_after(ts: &str) -> String {
    let (day, rest) = ts.split_at(11);
    let h: u32 = rest[0..2].parse().unwrap();
    let m: u32 = rest[3..5].parse().unwrap();
    let sec: u32 = rest[6..8].parse().unwrap();
    let t = h * 3600 + m * 60 + sec + 1;
    format!(
        "{day}{:02}:{:02}:{:02}{}",
        t / 3600,
        t / 60 % 60,
        t % 60,
        &rest[8..]
    )
}

fn ids_of(run: &Run, job: &str) -> String {
    run.un(job)
}

trait Now {
    fn state_now(&self) -> String;
}

impl Now for World {
    fn state_now(&self) -> String {
        "2026-01-01T00:00:00Z".to_string()
    }
}
