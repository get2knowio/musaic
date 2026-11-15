//! Replay testdata/golden/api/cases.json
//! (recorded from the Python app's FastAPI app driven through
//! TestClient, no lifespan, network off) against the router on the same
//! starting state, request by request, and compare status, the recorded
//! headers and the body after the generator's normalisation.
//!
//! Every difference from the Python app is listed in `DELTAS` (one entry per
//! case and JSON location, with the reason) or `RULES` (a rule with the
//! exact number of responses it touches). An unlisted difference, or a
//! listed one that does not occur, fails the test.

mod common;

use axum::body::Body;
use axum::http::Request;
use common::{Norm, Sandbox, call, golden, send};
use http_body_util::BodyExt;
use musaic_api::{AppState, Network};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime};
use tower::ServiceExt;

/// How a recorded Python app response is turned into the expected Rust one.
#[derive(Clone)]
enum Change {
    /// The value at a JSON pointer.
    Set(&'static str, Value, Value),
    /// Remove the filename-mismatch finding of the NFD album from the array
    /// at the pointer.
    DropNfdFinding(&'static str),
    /// The track-order change applied to the Night Mix album inside the SSE text or the
    /// album list at the pointer.
    TrackOrder(&'static str),
    /// Albums in this build's discovery order (name order) instead of the
    /// recording's APFS listing order: the `album` frames of the SSE text,
    /// or the ties (same sort key) of the album list at the pointer.
    ScanOrder(&'static str),
    /// The findings at the pointer carry this run's ids: each recorded
    /// finding takes the id of the finding with the same (check, album) in
    /// this run's response, and the list is put in id order
    /// (the recorded list was in id order; `last_seen_at` order is applied
    /// afterwards, as for every finding list). Cluster members follow the
    /// stored album order as well, so they are put in name order.
    FindingIds(&'static str),
    /// The inconsistent-artist-sort finding at the pointer sits on the
    /// alphabetically first album id of its members. Album ids hash the
    /// library path, so which album that is depends on where the library
    /// lives (a quirk of the Python app, reproduced); recomputed from the real ids.
    Representative(&'static str),
}

/// (case label, change, why).
type Delta = (&'static str, Change, &'static str);

fn set(
    label: &'static str,
    ptr: &'static str,
    python: Value,
    rust: Value,
    why: &'static str,
) -> Delta {
    (label, Change::Set(ptr, python, rust), why)
}

const NFD_WHY: &str = "deliberate change: no rename is proposed when names differ only in NFC/NFD form, so the Cafe Orchestra dir finding does not exist";
const SCAN_ORDER_WHY: &str = "deliberate change: albums are discovered in name order on every filesystem, not in the filesystem's listing order (the recording's is APFS's)";
const SCAN_ORDER_FINDINGS_WHY: &str = "deliberate change (name-order discovery): a check run creates its findings album by album in stored order, so finding ids (and the id order of ties) follow name order, not the recording's APFS order";
const TRACK_ORDER_WHY: &str = "deliberate change: album fields and track order follow DISCNUMBER/TRACKNUMBER; only Various Artists - Night Mix changes";

fn deltas() -> Vec<Delta> {
    let v17 = "this build writes and reports its own schema version \
               (v17: fingerprint_impl; v18: AcoustID lookup outcome and rejected files)";
    let sv = musaic_db::SCHEMA_VERSION;
    let trav = "deliberate change: the Python app's SPA fallback served a file outside the frontend dir; refused here (index.html)";
    let index = json!("<!doctype html><html><body><div id=\"root\"></div></body></html>\n");
    vec![
        (
            "scan stream first",
            Change::ScanOrder("/sse"),
            SCAN_ORDER_WHY,
        ),
        (
            "scan stream again (new incremental scan)",
            Change::ScanOrder("/sse"),
            SCAN_ORDER_WHY,
        ),
        ("albums", Change::ScanOrder("/json/albums"), SCAN_ORDER_WHY),
        (
            "scan stream first",
            Change::TrackOrder("/sse"),
            TRACK_ORDER_WHY,
        ),
        (
            "scan stream again (new incremental scan)",
            Change::TrackOrder("/sse"),
            TRACK_ORDER_WHY,
        ),
        (
            "albums",
            Change::TrackOrder("/json/albums"),
            TRACK_ORDER_WHY,
        ),
        set(
            "resolution offline",
            "/json/notes",
            json!(["No anchor found via Tiers 0a-0c; AI escalation may be required"]),
            json!([
                "Tier 0b: MusicBrainz search failed (network error: could not resolve host musicbrainz.org)",
                "No anchor found via Tiers 0a-0c; AI escalation may be required"
            ]),
            "resolution notes: the cascade adds a trace line for a failed search",
        ),
        set("db info", "/json/current_schema", json!(16), json!(sv), v17),
        set("db info", "/json/schema_version", json!(16), json!(sv), v17),
        set(
            "snapshot create",
            "/json/filename",
            json!("library-<TAG>-v16.db"),
            json!(format!("library-<TAG>-v{sv}.db")),
            v17,
        ),
        set(
            "snapshot create",
            "/json/schema_version",
            json!(16),
            json!(sv),
            v17,
        ),
        set(
            "snapshots one",
            "/json/snapshots/0/filename",
            json!("library-<TAG>-v16.db"),
            json!(format!("library-<TAG>-v{sv}.db")),
            v17,
        ),
        set(
            "snapshots one",
            "/json/snapshots/0/schema_version",
            json!(16),
            json!(sv),
            v17,
        ),
        set(
            "backup download",
            "/headers/content-disposition",
            json!("attachment; filename=\"musaic-v16-<TAG>.db\""),
            json!(format!("attachment; filename=\"musaic-v{sv}-<TAG>.db\"")),
            v17,
        ),
        set(
            "settings provider unknown",
            "/json/detail",
            json!(
                "Unknown or not-installed provider 'bedrock'. Installed: claude, github-copilot, opencode. Install its airframe extra to add more."
            ),
            json!(
                "Unknown or not-installed provider 'bedrock'. Installed: claude. Install its airframe extra to add more."
            ),
            "no other provider in this build: only 'claude' is known; the Python app lists the airframe extras installed on the recording machine",
        ),
        set(
            "run filename-mismatch",
            "/json/issue_count",
            json!(3),
            json!(2),
            NFD_WHY,
        ),
        (
            "check detail",
            Change::DropNfdFinding("/json/findings"),
            NFD_WHY,
        ),
        set(
            "check detail",
            "/json/runs/0/issue_count",
            json!(3),
            json!(2),
            NFD_WHY,
        ),
        (
            "findings",
            Change::DropNfdFinding("/json/findings"),
            NFD_WHY,
        ),
        (
            "findings",
            Change::Representative("/json/findings/15"),
            "reproduced: representative = smallest album id; ids hash the library path, so it differs between the Python app's run and this one",
        ),
        (
            "findings",
            Change::FindingIds("/json/findings"),
            SCAN_ORDER_FINDINGS_WHY,
        ),
        (
            "findings by album",
            Change::FindingIds("/json/findings"),
            SCAN_ORDER_FINDINGS_WHY,
        ),
        set(
            "plan-preview 20",
            "",
            json!(null),
            json!({"status": 404, "headers": {"content-type": "application/json"}, "json": {"detail": "Finding not found"}}),
            NFD_WHY,
        ),
        set(
            "checks after runs",
            "/json/checks/6/open_count",
            json!(3),
            json!(2),
            NFD_WHY,
        ),
        set(
            "checks after runs",
            "/json/checks/6/last_run/issue_count",
            json!(3),
            json!(2),
            NFD_WHY,
        ),
        set(
            "spa traversal encoded",
            "/headers/content-type",
            json!("text/plain; charset=utf-8"),
            json!("text/html; charset=utf-8"),
            trav,
        ),
        set(
            "spa traversal encoded",
            "/text",
            json!("outside the frontend dir\n"),
            index.clone(),
            trav,
        ),
        set(
            "spa traversal double encoded",
            "/headers/content-type",
            json!("text/plain; charset=utf-8"),
            json!("text/html; charset=utf-8"),
            trav,
        ),
        set(
            "spa traversal double encoded",
            "/text",
            json!("outside the frontend dir\n"),
            index,
            trav,
        ),
    ]
}

/// A rule touching every response it matches; `count` is asserted.
struct Rule {
    why: &'static str,
    count: usize,
    applies: fn(&Value) -> bool,
    apply: fn(&mut Value),
}

fn rules() -> Vec<Rule> {
    vec![
        Rule {
            why: "deliberate change: the settings body gains keyring_available, and its \
              sibling keys_pending_removal (false: no secret row is left to scrub)",
            count: 15,
            applies: |c| c["request"]["path"] == "/api/settings" && c["response"]["status"] == 200,
            apply: |r| {
                r["json"]["keyring_available"] = json!(true);
                r["json"]["keys_pending_removal"] = json!(false);
            },
        },
        Rule {
            why: "no AI provider is ready (the default \
              Unconfigured provider), so check descriptions, the unanchored trace note and \
              unanchored tags-out-of-sync summaries are shown without AI wording \
              (tests/common/ai_wording.rs)",
            count: AI_WORDING_RESPONSES,
            applies: |c| {
                let mut r = c["response"].clone();
                ai_wording_response(&mut r) > 0
            },
            apply: |r| {
                assert!(ai_wording_response(r) > 0);
            },
        },
    ]
}

/// Responses of cases.json the AI-wording rule rewrites.
const AI_WORDING_RESPONSES: usize = 4;

/// The AI-wording rule applied to a recorded response (its JSON body or SSE text).
fn ai_wording_response(r: &mut Value) -> usize {
    let mut n = 0;
    if let Some(j) = r.get_mut("json") {
        n += common::ai_wording::json(j);
    }
    if let Some(Value::String(t)) = r.get_mut("sse") {
        let (t2, m) = common::ai_wording::text(t);
        *t = t2;
        n += m;
    }
    n
}

fn multipart(field: &str, filename: &str, ctype: &str, data: &[u8]) -> (String, Vec<u8>) {
    let boundary = "musaicgoldenboundary";
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"{filename}\"\r\nContent-Type: {ctype}\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(data);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

fn upload_bytes(label: &str) -> Vec<u8> {
    match label {
        "restore tiny" => b"tiny".to_vec(),
        "restore not sqlite" => vec![b'x'; 100],
        other => panic!("no upload bytes for {other}"),
    }
}

fn sha(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

fn query_string(params: &serde_json::Map<String, Value>) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{k}={}", v.as_str().unwrap()))
        .collect::<Vec<_>>()
        .join("&")
}

/// Finding lists (GET /api/checks/findings and the `findings` of
/// GET /api/checks/{id}) are ordered `last_seen_at DESC` and nothing else:
/// Python app musaic/checks/runner.py:477 and :426, Rust
/// musaic-db/src/findings.rs `list_findings`. `last_seen_at` is stamped per
/// upsert at one-second resolution (Python app runner.py:62 `_now_iso`, :274;
/// Rust musaic-checks/src/runner.rs `emit`), and ties come out in SQLite's
/// order, which is insertion (id) order. So the order is a function of
/// wall time in the Python app too: the recording has ids 1..20 ascending only
/// because all seven explicit runs landed in one second; had a second
/// boundary fallen before `run inconsistent-artist-sort`, the Python app would
/// have listed 16, 17, 18, 19, 20, 1, ... (an open question: the order
/// could be made deterministic).
///
/// Known difference, enumerated: the Python app's list is re-sorted, stably, by
/// the `last_seen_at` the Rust build actually stamped on the finding with
/// the same id, newest first. Equal stamps keep the Python app's recorded order,
/// so the tie order, the set of findings, their ids and every field are
/// still compared exactly; only the second each finding happened to be
/// touched in is taken from this run. Returns whether the order changed.
fn order_by_rust_last_seen(want: &mut Value, raw: &Value) -> Option<bool> {
    let rust = raw.get("findings")?.as_array()?;
    let arr = want.pointer_mut("/json/findings")?.as_array_mut()?;
    let stamp = |id: &Value| -> String {
        rust.iter()
            .find(|f| f["id"] == *id)
            .and_then(|f| f["last_seen_at"].as_str())
            .unwrap_or("")
            .to_string()
    };
    let before: Vec<Value> = arr.iter().map(|f| f["id"].clone()).collect();
    arr.sort_by_key(|f| std::cmp::Reverse(stamp(&f["id"])));
    Some(arr.iter().map(|f| &f["id"]).ne(before.iter()))
}

/// Number of recorded responses carrying a finding list
/// (check detail, findings, by check, by album, include ignored).
const FINDING_LISTS: usize = 5;

/// A wall clock the test moves: frozen at the start, stepped one second
/// at chosen requests, and ticking one second per reading during another.
/// It forces, deterministically, the second boundaries that under load
/// fall at random inside the explicit check runs.
struct ScriptedClock {
    base: SystemTime,
    offset: AtomicU64,
    ticking: AtomicBool,
}

impl ScriptedClock {
    fn now(&self) -> SystemTime {
        let off = if self.ticking.load(Ordering::SeqCst) {
            self.offset.fetch_add(1, Ordering::SeqCst)
        } else {
            self.offset.load(Ordering::SeqCst)
        };
        self.base + Duration::from_secs(off)
    }
    fn before(&self, label: &str) {
        match label {
            // Between two runs: the failure seen under load
            // (inconsistent-artist-sort id 16 first).
            "run inconsistent-artist-sort" => {
                self.offset.fetch_add(1, Ordering::SeqCst);
            }
            // Inside one run: its findings get distinct seconds.
            "run filename-mismatch" => self.ticking.store(true, Ordering::SeqCst),
            _ => {}
        }
    }
    fn after(&self, label: &str) {
        if label == "run filename-mismatch" {
            self.ticking.store(false, Ordering::SeqCst);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn golden_cases_replay() {
    // System clock: whether any finding list is reordered depends on where
    // the second boundaries fall (none, usually).
    replay(None).await;
}

/// The finding-list order under second boundaries placed by the test
/// (see `order_by_rust_last_seen`); without that known difference this
/// fails exactly as the system-clock replay fails under load.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn golden_cases_replay_across_second_boundaries() {
    let clock = Arc::new(ScriptedClock {
        base: SystemTime::now(),
        offset: AtomicU64::new(0),
        ticking: AtomicBool::new(false),
    });
    let reordered = replay(Some(clock)).await;
    assert_eq!(reordered, ["check detail", "findings"]);
}

/// Replays every case; returns the labels whose finding list was re-sorted
/// by `order_by_rust_last_seen`.
async fn replay(clock: Option<Arc<ScriptedClock>>) -> Vec<String> {
    let g = golden("cases.json");
    let cases = g["cases"].as_array().unwrap();
    let sb = Sandbox::new();
    let state = match &clock {
        None => sb.state_no_lifespan(),
        // As Sandbox::state_no_lifespan, plus the clock.
        Some(c) => {
            let c = c.clone();
            AppState::builder(&sb.cfg)
                .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
                .network(Network::Offline)
                .fingerprinting(common::no_key_fingerprinting())
                .frontend_dir(Some(sb.front.clone()))
                .subscribers(false)
                .auto_snapshot(false)
                .clock(Arc::new(move || c.now()))
                .build()
                .unwrap()
        }
    };
    let app = musaic_api::router(state);
    let mut finding_lists = 0usize;
    let mut reordered: Vec<String> = Vec::new();
    let mut norm = Norm::new(&sb);
    // "library-gone" normalises as "<LIB>-gone" in the Python app run too.
    let deltas = deltas();
    let mut used = vec![false; deltas.len()];
    let mut bytes_422 = 0;
    let rules = rules();
    let mut rule_hits = vec![0usize; rules.len()];
    let mut failures: Vec<String> = Vec::new();

    for case in cases {
        let label = case["label"].as_str().unwrap();
        let req = &case["request"];
        let method = req["method"].as_str().unwrap();
        let mut path = norm.unnorm(req["path"].as_str().unwrap());
        if let Some(p) = req.get("params").and_then(Value::as_object) {
            path = format!("{path}?{}", norm.unnorm(&query_string(p)));
        }
        if let Some(c) = &clock {
            c.before(label);
        }
        let got = if let Some(files) = req.get("files").and_then(Value::as_object) {
            let (name, f) = files.iter().next().unwrap();
            let data = upload_bytes(label);
            assert_eq!(
                sha(&data),
                f["sha256"].as_str().unwrap(),
                "{label}: upload bytes"
            );
            let (ct, body) = multipart(
                name,
                f["filename"].as_str().unwrap(),
                f["content_type"].as_str().unwrap(),
                &data,
            );
            send(
                &app,
                Request::builder()
                    .method(method)
                    .uri(&path)
                    .header("content-type", ct)
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
        } else if let Some(raw) = req.get("raw_body") {
            send(
                &app,
                Request::builder()
                    .method(method)
                    .uri(&path)
                    .header("content-type", "application/json")
                    .body(Body::from(norm.unnorm_json_text(raw.as_str().unwrap())))
                    .unwrap(),
            )
            .await
        } else {
            let body = req.get("json").map(|j| norm.unnorm_value(j));
            call(&app, method, &path, body.as_ref()).await
        };

        if let Some(c) = &clock {
            c.after(label);
        }
        // Side effects the generator performed between requests.
        if label == "config gone dir" {
            std::fs::remove_dir(&sb.lib_gone).unwrap();
        }
        if label == "scan stream first" {
            let albums = call(&app, "GET", "/api/albums", None).await.json();
            for a in albums["albums"].as_array().unwrap() {
                let id = a["id"].as_str().unwrap();
                let dn = a["dir_name"].as_str().unwrap();
                norm.add(id, &format!("<ID:{dn}>"));
            }
        }

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

        // The Rust response, recorded like the generator records.
        let mut rec = json!({"status": got.status, "headers": {}});
        for h in [
            "content-type",
            "cache-control",
            "x-accel-buffering",
            "connection",
            "content-disposition",
            "etag",
            "location",
            "allow",
        ] {
            if let Some(v) = got.headers.get(h) {
                rec["headers"][h] = json!(if h == "etag" {
                    "<ETAG>".to_string()
                } else {
                    norm.text(v)
                });
            }
        }
        let ct = got.ctype().to_string();
        if ct.starts_with("application/json") {
            rec["json"] = norm.value(&got.json(), None);
        } else if ct.starts_with("text/event-stream") {
            rec["sse"] = json!(norm.raw_json_text(&got.text()));
        } else if ct.starts_with("text/") {
            rec["text"] = json!(norm.text(&got.text()));
        } else if ct.starts_with("application/octet-stream") {
            rec["binary"] = json!({"sqlite_header": got.body.starts_with(b"SQLite format 3\0")});
        } else {
            rec["binary"] = json!({"sha256": sha(&got.body), "size": got.body.len()});
        }

        let mut want = case["response"].clone();
        for (i, (l, change, why)) in deltas.iter().enumerate() {
            if *l != label {
                continue;
            }
            apply_change(label, change, why, &mut want, &rec, &norm);
            used[i] = true;
        }
        if ct.starts_with("application/json")
            && let Some(changed) = order_by_rust_last_seen(&mut want, &got.json())
        {
            finding_lists += 1;
            if changed {
                reordered.push(label.to_string());
            }
        }
        for (i, r) in rules.iter().enumerate() {
            if (r.applies)(case) {
                (r.apply)(&mut want);
                rule_hits[i] += 1;
            }
        }
        if rec != want {
            if let Some(d) = std::env::var_os("MUSAIC_API_DIFF_DIR") {
                let d = std::path::PathBuf::from(d);
                std::fs::create_dir_all(&d).unwrap();
                let slug: String = label
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                    .collect();
                std::fs::write(
                    d.join(format!("{slug}.python.json")),
                    serde_json::to_string_pretty(&want).unwrap(),
                )
                .unwrap();
                std::fs::write(
                    d.join(format!("{slug}.rust.json")),
                    serde_json::to_string_pretty(&rec).unwrap(),
                )
                .unwrap();
            }
            failures.push(format!(
                "== {label} ({method} {path})\n-- python: {}\n-- rust:   {}",
                truncate(&want),
                truncate(&rec)
            ));
        }
    }
    first_frames(&app, &norm).await;
    // Every 422 the golden records (cases.json, Python app at 6902c23).
    assert_eq!(bytes_422, 16, "422 cases compared byte for byte");
    for (i, d) in deltas.iter().enumerate() {
        assert!(used[i], "unused delta: {} ({})", d.0, d.2);
    }
    for (i, r) in rules.iter().enumerate() {
        assert_eq!(rule_hits[i], r.count, "rule '{}' touched", r.why);
    }
    assert_eq!(
        finding_lists, FINDING_LISTS,
        "responses with a finding list"
    );
    assert!(
        failures.is_empty(),
        "{} of {} cases differ (finding lists re-sorted by last_seen_at: {reordered:?}):\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
    reordered
}

fn truncate(v: &Value) -> String {
    let s = v.to_string();
    if s.len() > 6000 {
        format!("{}...", &s[..6000])
    } else {
        s
    }
}

fn apply_change(
    label: &str,
    change: &Change,
    why: &str,
    want: &mut Value,
    rec: &Value,
    norm: &Norm,
) {
    match change {
        Change::Representative(ptr) => {
            let f = want.pointer_mut(ptr).unwrap();
            assert_eq!(f["check_id"], "inconsistent-artist-sort", "{label} ({why})");
            let mut tokens: Vec<String> = f["detail"]["members"]
                .as_array()
                .unwrap()
                .iter()
                .map(|m| m["album_id"].as_str().unwrap().to_string())
                .collect();
            tokens.sort_by_key(|t| norm.unnorm(t));
            f["album_id"] = json!(tokens[0]);
        }
        Change::Set(ptr, python, rust) => {
            let slot = want
                .pointer_mut(ptr)
                .unwrap_or_else(|| panic!("{label}: delta path {ptr} missing ({why})"));
            if !ptr.is_empty() {
                assert_eq!(
                    slot, python,
                    "{label}: {ptr} no longer matches the Python app ({why})"
                );
            }
            *slot = rust.clone();
        }
        Change::DropNfdFinding(ptr) => {
            let arr = want.pointer_mut(ptr).and_then(Value::as_array_mut).unwrap();
            let before = arr.len();
            arr.retain(|f| {
                !(f["check_id"] == "filename-mismatch"
                    && f["album_id"]
                        .as_str()
                        .is_some_and(|a| a.starts_with("<ID:Cafe\u{301}")))
            });
            assert_eq!(
                arr.len(),
                before - 1,
                "{label}: exactly one NFD finding ({why})"
            );
        }
        Change::ScanOrder("/sse") => {
            let w = want["sse"].as_str().unwrap().to_string();
            let mut frames: Vec<String> = w.split("\n\n").map(str::to_string).collect();
            let head = "event: album\ndata: ";
            let slots: Vec<usize> = (0..frames.len())
                .filter(|&i| frames[i].starts_with(head))
                .collect();
            let path = |f: &String| -> String {
                let v: Value = serde_json::from_str(&f[head.len()..]).unwrap();
                v["path"].as_str().unwrap().to_string()
            };
            let mut albums: Vec<String> = slots.iter().map(|&i| frames[i].clone()).collect();
            albums.sort_by_key(path);
            let mut changed = false;
            for (&i, f) in slots.iter().zip(albums) {
                changed |= frames[i] != f;
                frames[i] = f;
            }
            assert!(
                changed,
                "{label}: the recording is already in name order ({why})"
            );
            want["sse"] = json!(frames.join("\n\n"));
        }
        Change::FindingIds(ptr) => {
            let ours = rec
                .pointer(ptr)
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let arr = want.pointer_mut(ptr).and_then(Value::as_array_mut).unwrap();
            let mut changed = false;
            // (check, album) is unique in these lists; the summary is not
            // part of the key because the AI-wording rule rewrites some.
            let key = |f: &Value| (f["check_id"].clone(), f["album_id"].clone());
            for f in arr.iter_mut() {
                let mut same = ours.iter().filter(|o| key(o) == key(f));
                let o = same
                    .next()
                    .unwrap_or_else(|| panic!("{label}: no finding like {f} in this run ({why})"));
                assert!(
                    same.next().is_none(),
                    "{label}: (check, album) not unique ({why})"
                );
                changed |= f["id"] != o["id"];
                f["id"] = o["id"].clone();
                // Cluster members are listed in stored album order too.
                if let Some(m) = f
                    .pointer_mut("/detail/members")
                    .and_then(Value::as_array_mut)
                {
                    m.sort_by(|a, b| a["dir_name"].as_str().cmp(&b["dir_name"].as_str()));
                }
            }
            arr.sort_by_key(|f| f["id"].as_i64());
            assert!(changed, "{label}: ids already as recorded ({why})");
        }
        Change::ScanOrder(ptr) => {
            let arr = want.pointer_mut(ptr).and_then(Value::as_array_mut).unwrap();
            let before = arr.clone();
            arr.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
            arr.sort_by_key(sort_key);
            assert_ne!(
                *arr, before,
                "{label}: the recording is already in name order ({why})"
            );
        }
        Change::TrackOrder("/sse") => {
            let w = want["sse"].as_str().unwrap().to_string();
            let r = rec["sse"].as_str().unwrap_or("").to_string();
            let wf: Vec<&str> = w.split("\n\n").collect();
            let rf: Vec<&str> = r.split("\n\n").collect();
            assert_eq!(wf.len(), rf.len(), "{label}: frame count");
            let mut out = Vec::new();
            let mut hits = 0;
            for (a, b) in wf.iter().zip(&rf) {
                if a.contains("\"dir_name\": \"Various Artists - Night Mix [FLAC]\"")
                    && a.starts_with("event: album\n")
                {
                    let oa: Value =
                        serde_json::from_str(&a["event: album\ndata: ".len()..]).unwrap();
                    let ra: Value =
                        serde_json::from_str(&b["event: album\ndata: ".len()..]).unwrap();
                    assert_eq!(
                        first_file_order(&oa, &ra),
                        ra,
                        "{label}: Night Mix after the track-order change ({why})"
                    );
                    hits += 1;
                    out.push((*b).to_string());
                } else {
                    out.push((*a).to_string());
                }
            }
            assert_eq!(hits, 1, "{label}: one Night Mix frame");
            want["sse"] = json!(out.join("\n\n"));
        }
        Change::TrackOrder(ptr) => {
            let ra = rec
                .pointer(ptr)
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let arr = want.pointer_mut(ptr).and_then(Value::as_array_mut).unwrap();
            let i = arr
                .iter()
                .position(|a| a["dir_name"] == "Various Artists - Night Mix [FLAC]")
                .unwrap();
            let r = ra
                .iter()
                .find(|a| a["dir_name"] == "Various Artists - Night Mix [FLAC]")
                .unwrap();
            arr[i] = first_file_order(&arr[i], r);
            // sort_albums: the artist moved, so may the album.
            arr.sort_by_key(sort_key);
        }
    }
}

/// `sort_albums`' key (stable sort: ties keep the stored order).
fn sort_key(a: &Value) -> (String, String, String) {
    let s = |k: &str| a[k].as_str().unwrap_or("").to_string();
    let aa = s("album_artist");
    let artist = if aa.is_empty() { s("artist") } else { aa };
    (artist.to_lowercase(), s("year"), s("title").to_lowercase())
}

/// The track-order change applied to the Python app's album dict: per-track lists in
/// (disc, track) order, album-level fields from the first track in that
/// order. The genre union keeps its members (order taken from Rust after
/// checking the set).
fn first_file_order(python: &Value, rust: &Value) -> Value {
    let mut e = python.clone();
    let nums: Vec<Option<i64>> = python["track_numbers"]
        .as_array()
        .unwrap()
        .iter()
        .map(Value::as_i64)
        .collect();
    let mut order: Vec<usize> = (0..nums.len()).collect();
    order.sort_by_key(|i| (nums[*i].is_none(), nums[*i]));
    let ta = python["track_artists"].as_array().unwrap();
    let new_ta: Vec<Value> = order.iter().map(|i| ta[*i].clone()).collect();
    e["track_numbers"] = json!(order.iter().map(|i| nums[*i]).collect::<Vec<_>>());
    let first = &new_ta[0];
    e["artist"] = first["artist"].clone();
    let dir = python["path"].as_str().unwrap();
    e["sample_track"] = json!(format!("{dir}/{}", first["filename"].as_str().unwrap()));
    e["track_artists"] = json!(new_ta);
    let set = |v: &Value| {
        let mut g: Vec<String> = v
            .as_str()
            .unwrap_or("")
            .split("; ")
            .map(str::to_string)
            .collect();
        g.sort();
        g
    };
    assert_eq!(
        set(&python["genre"]),
        set(&rust["genre"]),
        "the track-order change keeps the genre members"
    );
    e["genre"] = rust["genre"].clone();
    e
}

/// The three never-ending App-level streams, at the end of the
/// sequence: response headers and the first frame, byte for byte after
/// normalisation (testdata/golden/api/sse_first_frames.json).
async fn first_frames(app: &axum::Router, norm: &Norm) {
    let g = golden("sse_first_frames.json");
    for (path, want) in g["streams"].as_object().unwrap() {
        let resp = app.clone().oneshot(common::get(path)).await.unwrap();
        assert_eq!(resp.status(), 200, "{path}");
        for (h, v) in want["headers"].as_object().unwrap() {
            assert_eq!(
                resp.headers().get(h).and_then(|x| x.to_str().ok()),
                v.as_str(),
                "{path}: header {h}"
            );
        }
        let mut body = resp.into_body();
        let frame = body.frame().await.unwrap().unwrap().into_data().unwrap();
        let got = norm.raw_json_text(&String::from_utf8_lossy(&frame));
        let mut expected = want["first_frame"].as_str().unwrap().to_string();
        match path.as_str() {
            "/api/checks/stream" => {
                // No NFD dir finding (see NFD_WHY).
                let tail_o = "\"open_count\": 3, \"last_run\": {\"id\": 7, \"check_id\": \"filename-mismatch\"";
                let tail_r = "\"open_count\": 2, \"last_run\": {\"id\": 7, \"check_id\": \"filename-mismatch\"";
                assert!(expected.contains(tail_o), "NFD delta still applies");
                expected = expected.replace(tail_o, tail_r);
                let ic_o = "\"scope\": \"all\", \"issue_count\": 3, \"error\": null, \"duration_ms\": \"<MS>\"}}]}";
                let ic_r = "\"scope\": \"all\", \"issue_count\": 2, \"error\": null, \"duration_ms\": \"<MS>\"}}]}";
                assert!(expected.contains(ic_o), "NFD delta still applies");
                expected = expected.replace(ic_o, ic_r);
                // No AI provider
                // is ready, so the three descriptions that name AI are the
                // neutral ones.
                let (e, n) = common::ai_wording::text(&expected);
                assert_eq!(n, 3, "AI-wording delta applies");
                expected = e;
            }
            "/api/fingerprint-pool/stream" => {
                // No delta: the pool ran after the sequence's scans and,
                // as in the Python app's recording, reported `unavailable` with
                // the Python app's reason (harness configuration, see
                // common::PYTHON_NO_KEY).
                assert!(expected.contains("\"status\": \"unavailable\""));
            }
            _ => {}
        }
        assert_eq!(got, expected, "{path}: first frame");
        drop(body);
    }
}
