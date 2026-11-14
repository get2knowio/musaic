//! Replay
//! testdata/golden/api/wired.json (recorded from the Python app's FastAPI app through TestClient, no lifespan, network off, no
//! AcoustID key) against this router on the same starting state: the tag,
//! tag-vocabulary, fingerprint-pool status and check-fix routes on the
//! fixture library, after a scan and a run of every check.
//!
//! Finding ids in paths and SSE frames are mapped through the golden's
//! `findings_after_checks` by (check, album, summary) (album masked for the
//! artist-sort cluster: the finding sits on the cluster's smallest album id).
//! Every difference from the Python app is an
//! entry of `deltas()` (case, JSON pointer, Python value, Rust value, why)
//! and must be used; anything else fails.

mod common;

use std::collections::HashMap;

use common::{Norm, Sandbox, call, golden};
use regex::Regex;
use serde_json::{Value, json};

/// (case, pointer, python, rust-or-null-to-take-ours, why).
fn deltas() -> Vec<(
    &'static str,
    &'static str,
    Value,
    Option<Value>,
    &'static str,
)> {
    let mut v = vec![(
        "rename dir",
        "/json/dir_migration/album_blocked_mb_release_groups",
        Value::Null,
        Some(json!(0)),
        "deliberate change: the directory rename also repoints the blocklist rows and counts them",
    )];
    // No model backend in this build, so where
    // the Python app queued an AI job the fix answers ok:false with the job's
    // not-available message and no job id.
    for (case, python_msg) in [
        (
            "fix track-gaps",
            "Asking Claude to verify the track sequence and propose a fix",
        ),
        (
            "fix unidentified",
            "Asking Claude to identify this album on MusicBrainz",
        ),
    ] {
        let why = "no model backend; the fix queues no AI job";
        v.push((case, "/json/ok", json!(true), Some(json!(false)), why));
        v.push((
            case,
            "/json/message",
            json!(python_msg),
            Some(json!(not_available())),
            why,
        ));
        v.push((case, "/json/job_id", json!("<JOB>"), Some(Value::Null), why));
    }
    v
}

/// The not-available message of this build's provider.
fn not_available() -> String {
    musaic_ai::model_unavailable(&musaic_ai::Unconfigured).expect("no backend")
}

/// The Python app's finding id -> ours, and back.
struct Ids {
    to_ours: HashMap<i64, i64>,
    to_python: HashMap<i64, i64>,
}

fn finding_key(check: &Value, album: &Value, summary: &Value) -> String {
    if check == "inconsistent-artist-sort" {
        format!("{check}|<cluster>|{summary}")
    } else {
        format!("{check}|{album}|{summary}")
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wired_routes_replay_the_python_app() {
    let g = golden("wired.json");
    let sb = Sandbox::new();
    let app = musaic_api::router(sb.state_no_lifespan());
    let mut norm = Norm::new(&sb);
    let deltas = deltas();
    let mut used = vec![false; deltas.len()];
    let mut bytes_422 = 0;
    let mut ids = Ids {
        to_ours: HashMap::new(),
        to_python: HashMap::new(),
    };
    let mut failures = Vec::new();
    let mut configured = false;
    let mut checked = false;
    // How often the no-provider wording delta applied.
    let mut ai_wording_hits = 0;
    let path_id = Regex::new(r"/findings/(\d+)/").unwrap();
    let frame_id = Regex::new(r#""finding_id": (\d+)"#).unwrap();

    for case in g["cases"].as_array().unwrap() {
        let label = case["label"].as_str().unwrap();
        // The generator's unrecorded steps, at the same points.
        if !configured && label == "pool status after scan" {
            configured = true;
            let lib = Sandbox::s(&sb.lib);
            assert_eq!(
                call(
                    &app,
                    "POST",
                    "/api/config",
                    Some(&json!({"library_dir": lib}))
                )
                .await
                .status,
                200
            );
            let s = call(&app, "GET", "/api/scan/stream", None).await;
            assert!(s.text().contains("event: done"));
            for a in call(&app, "GET", "/api/albums", None).await.json()["albums"]
                .as_array()
                .unwrap()
            {
                norm.add(
                    a["id"].as_str().unwrap(),
                    &format!("<ID:{}>", a["dir_name"].as_str().unwrap()),
                );
            }
            // The directory the "rename dir" case creates.
            let new_dir = "Quiet Atlas - Old Rips [FLAC]";
            let id = musaic_core::album_id(
                &musaic_scan::py_resolve(&sb.lib.join(new_dir)).to_string_lossy(),
            );
            norm.add(&id, &format!("<ID:{new_dir}>"));
        }
        if !checked && label.starts_with("fix ") {
            checked = true;
            let list = call(&app, "GET", "/api/checks", None).await.json();
            for c in list["checks"].as_array().unwrap() {
                let id = c["meta"]["id"].as_str().unwrap();
                let r = call(
                    &app,
                    "POST",
                    &format!("/api/checks/{id}/run"),
                    Some(&json!({"scope": "all"})),
                )
                .await;
                assert_eq!(r.status, 200, "{id}");
            }
            let ours = call(&app, "GET", "/api/checks/findings", None).await.json();
            let mut by_key: HashMap<String, i64> = HashMap::new();
            for f in ours["findings"].as_array().unwrap() {
                let f = norm.value(f, None);
                by_key.insert(
                    finding_key(&f["check_id"], &f["album_id"], &f["summary"]),
                    f["id"].as_i64().unwrap(),
                );
            }
            for f in g["findings_after_checks"].as_array().unwrap() {
                // No AI provider
                // is ready, so the summaries are shown in the neutral wording.
                let mut shown = json!({"check_id": f[1], "summary": f[3]});
                ai_wording_hits += common::ai_wording::json(&mut shown);
                // No NFC/NFD dir finding here.
                if let Some(o) = by_key.get(&finding_key(&f[1], &f[2], &shown["summary"])) {
                    ids.to_ours.insert(f[0].as_i64().unwrap(), *o);
                    ids.to_python.insert(*o, f[0].as_i64().unwrap());
                } else {
                    assert!(
                        f[2].as_str().unwrap().starts_with("<ID:Cafe\u{301}"),
                        "no Rust finding for {f}"
                    );
                }
            }
            assert_eq!(
                ids.to_ours.len() + 1,
                g["findings_after_checks"].as_array().unwrap().len()
            );
        }

        let req = &case["request"];
        let method = req["method"].as_str().unwrap();
        let mut path = norm.unnorm(req["path"].as_str().unwrap());
        if req.get("finding").is_some() {
            path = path_id
                .replace(&path, |c: &regex::Captures| {
                    let o: i64 = c[1].parse().unwrap();
                    format!("/findings/{}/", ids.to_ours[&o])
                })
                .into_owned();
        }
        if let Some(p) = req.get("params").and_then(Value::as_object) {
            let q: Vec<String> = p
                .iter()
                .map(|(k, v)| format!("{k}={}", v.as_str().unwrap()))
                .collect();
            path = format!("{path}?{}", q.join("&"));
        }
        let body = req.get("json").map(|j| norm.unnorm_value(j));
        let got = call(&app, method, &path, body.as_ref()).await;

        // FastAPI's 422 bytes (pydantic's error key order), not
        // only the parsed body the golden records.
        if case["response"]["status"] == 422 {
            assert_eq!(
                norm.text(&got.text()),
                common::fastapi_422_text(&case["response"]["json"]),
                "{label}: 422 bytes"
            );
            bytes_422 += 1;
        }
        let mut rec = json!({"status": got.status});
        if got.ctype().starts_with("application/json") {
            rec["json"] = norm.value(&got.json(), None);
            // The AI job a fix enqueued (the golden records its id as
            // <JOB>).
            if rec["json"].get("job_id").is_some_and(Value::is_string) {
                rec["json"]["job_id"] = json!("<JOB>");
            }
        } else if got.ctype().starts_with("text/event-stream") {
            let text = frame_id
                .replace_all(&got.text(), |c: &regex::Captures| {
                    let i: i64 = c[1].parse().unwrap();
                    format!("\"finding_id\": {}", ids.to_python[&i])
                })
                .into_owned();
            rec["sse"] = json!(norm.raw_json_text(&text));
        } else {
            rec["text"] = json!(norm.text(&got.text()));
        }
        let mut want = json!({"status": case["response"]["status"]});
        for k in ["json", "sse", "text"] {
            if let Some(v) = case["response"].get(k) {
                want[k] = v.clone();
            }
        }
        // The Python app's response in the no-provider wording.
        if let Some(j) = want.get_mut("json") {
            ai_wording_hits += common::ai_wording::json(j);
        }
        if let Some(Value::String(t)) = want.get_mut("sse") {
            let (t2, n) = common::ai_wording::text(t);
            *t = t2;
            ai_wording_hits += n;
        }
        for (i, (l, ptr, python, rust, why)) in deltas.iter().enumerate() {
            if *l != label {
                continue;
            }
            used[i] = true;
            let slot = match want.pointer_mut(ptr) {
                Some(s) => s,
                None => {
                    // An added key (the Python app has none).
                    let (parent, key) = ptr.rsplit_once('/').unwrap();
                    want.pointer_mut(parent).unwrap()[key] = Value::Null;
                    want.pointer_mut(ptr).unwrap()
                }
            };
            assert_eq!(
                slot, python,
                "{label}: {ptr} no longer the Python app's ({why})"
            );
            *slot = match rust {
                Some(v) => v.clone(),
                None => rec.pointer(ptr).cloned().unwrap_or(Value::Null),
            };
        }
        // Lists the Python app sorts by album id: ids hash the absolute library
        // path, so the order depends on where the library lives (the same
        // normalisation as musaic-services' tags/vocabulary golden test).
        // Sorted by the normalised names on both sides.
        for v in [&mut rec, &mut want] {
            id_order(v);
        }
        if label == "albums after" {
            // The scanner's track order (disc, then track number; a
            // deliberate change) changes Night Mix's track-order fields
            // (golden_cases.rs pins them); compare what this sequence
            // changed: ids, dirs and the sort fields the fixes wrote.
            let project = |v: &Value| -> Value {
                let mut a: Vec<Value> = v["json"]["albums"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|a| {
                        json!([
                            a["id"],
                            a["dir_name"],
                            a["album_artist_sort"],
                            a["artist_sort"],
                            a["path"]
                        ])
                    })
                    .collect();
                a.sort_by_key(|x| x.to_string());
                json!(a)
            };
            if project(&rec) != project(&want) {
                failures.push(format!(
                    "== {label}\n-- python: {}\n-- rust:   {}",
                    project(&want),
                    project(&rec)
                ));
            }
            continue;
        }
        if rec != want {
            failures.push(format!(
                "== {label} ({method} {path})\n-- python: {want}\n-- rust:   {rec}"
            ));
        }
    }
    // Every 422 the golden records (wired.json, Python app at 6902c23).
    assert_eq!(bytes_422, 7, "422 cases compared byte for byte");
    for (i, d) in deltas.iter().enumerate() {
        assert!(used[i], "unused delta: {} {} ({})", d.0, d.1, d.4);
    }
    assert!(ai_wording_hits > 0, "AI-wording delta unused");
    assert!(
        failures.is_empty(),
        "{} of {} cases differ:\n{}",
        failures.len(),
        g["cases"].as_array().unwrap().len(),
        failures.join("\n")
    );
}

fn id_order(v: &mut Value) {
    let Some(j) = v.get_mut("json") else { return };
    for k in ["rename_result", "merge_result", "delete_result"] {
        if let Some(Value::Array(a)) = j.get_mut(k).and_then(|r| r.get_mut("albums")) {
            a.sort_by_key(|x| x.as_str().unwrap_or_default().to_string());
        }
    }
    if let Some(Value::Array(t)) = j.get_mut("tracks") {
        t.sort_by_key(|x| (x["album_id"].to_string(), x["filename"].to_string()));
    }
}
