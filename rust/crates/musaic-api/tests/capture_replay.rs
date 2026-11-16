//! Replay the request sequence of the recorded API run in
//! captures/api-offline (offline mode, the Python app
//! running as a server with its lifespan) against the router, from the same
//! starting state (fresh config dir, fresh copy of testdata/synthetic-library/library, a
//! dummy AcoustID key), and compare every recorded response and SSE stream.
//!
//! Stand-in for an unbuilt subsystem: the Python app's fingerprint pool ran
//! after the scan (fpcalc locally, lookups blocked). The Rust pool is not
//! built, so the test copies the Python app's own `track_fingerprints` rows from
//! testdata/db/v16-synthetic-offline.db (written by this very capture) at the
//! point where the capture waited for the pool. Its `TrackFingerprinted`
//! events are therefore absent (listed below).
//!
//! Normalisation (both sides; the capture tool normalised only paths and
//! ids): ISO timestamps, `dir_mtime`, `duration_ms`, `size_bytes`, snapshot
//! file-name tags, and the ids of findings and check runs (the
//! Rust build deliberately runs the post-scan checks after the scan, so its rows are
//! created earlier and numbered differently; findings are matched by
//! (check, album, summary) instead and preview requests are re-targeted).
//! Every other difference is listed in `DELTAS` / handled by a named rule.

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use common::{Norm, Sandbox, call, golden};
use http_body_util::BodyExt;
use musaic_api::{AppState, Network, router};
use regex::Regex;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

const CAPTURE: &str = "captures/api-offline";
/// 2026-11-09T17:39:25Z, the second every finding and check run of the
/// capture carries.
const CAPTURE_EPOCH_S: u64 = 1_794_245_965;

fn cap(rel: &str) -> std::path::PathBuf {
    musaic_testdata::public(CAPTURE).join(rel)
}

fn read_json(rel: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(cap(rel)).unwrap()).unwrap()
}

fn read_jsonl(rel: &str) -> Vec<Value> {
    std::fs::read_to_string(cap(rel))
        .unwrap()
        .lines()
        .skip(1)
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

struct Extra {
    iso: Regex,
    tag: Regex,
}

impl Extra {
    fn new() -> Self {
        Extra {
            iso: Regex::new(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?(?:Z|\+00:00)").unwrap(),
            tag: Regex::new(r"\d{8}-\d{6}Z").unwrap(),
        }
    }

    /// Timestamps, mtimes, durations, sizes, finding / run ids.
    fn v(&self, v: &Value, key: Option<&str>) -> Value {
        match (key, v) {
            (Some("dir_mtime"), Value::Number(_)) => json!("<MTIME>"),
            (Some("duration_ms"), Value::Number(_)) => json!("<MS>"),
            (Some("size_bytes"), Value::Number(_)) => json!("<SIZE>"),
            (_, Value::String(s)) => {
                let s = self.iso.replace_all(s, "<NOW>").into_owned();
                json!(self.tag.replace_all(&s, "<TAG>").into_owned())
            }
            (_, Value::Array(a)) => Value::Array(a.iter().map(|x| self.v(x, None)).collect()),
            (_, Value::Object(m)) => {
                let is_row = m.contains_key("check_id")
                    && (m.contains_key("summary") || m.contains_key("scope"));
                Value::Object(
                    m.iter()
                        .map(|(k, x)| {
                            if is_row && k == "id" {
                                (k.clone(), json!("<ROW_ID>"))
                            } else {
                                (k.clone(), self.v(x, Some(k)))
                            }
                        })
                        .collect(),
                )
            }
            (_, other) => other.clone(),
        }
    }
}

fn sha(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

/// The Python app's fingerprint rows, mapped onto this run's album ids by
/// directory name (the pool stand-in).
fn seed_fingerprints(db: &Path) {
    let tmp = tempfile::tempdir().unwrap();
    let fixture = tmp.path().join("fixture.db");
    std::fs::copy(
        musaic_testdata::public("db/v16-synthetic-offline.db"),
        &fixture,
    )
    .unwrap();
    let src = rusqlite::Connection::open(&fixture).unwrap();
    let dst = rusqlite::Connection::open(db).unwrap();
    let mut stmt = src
        .prepare(
            "SELECT a.dir_name, f.filename, f.fingerprint, f.duration_s, \
             f.acoustid_recording_mbid, f.acoustid_score, f.last_computed_at \
             FROM track_fingerprints f JOIN albums a ON a.id = f.album_id",
        )
        .unwrap();
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, f64>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<f64>>(5)?,
                r.get::<_, String>(6)?,
            ))
        })
        .unwrap();
    let mut n = 0;
    for row in rows {
        let (dir, file, fp, dur, mbid, score, at) = row.unwrap();
        let id: String = dst
            .query_row("SELECT id FROM albums WHERE dir_name = ?1", [&dir], |r| {
                r.get(0)
            })
            .unwrap();
        dst.execute(
            "INSERT INTO track_fingerprints(album_id, filename, fingerprint, duration_s, \
             acoustid_recording_mbid, acoustid_score, last_computed_at) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            rusqlite::params![id, file, fp, dur, mbid, score, at],
        )
        .unwrap();
        n += 1;
    }
    assert_eq!(n, 42, "the capture's fingerprint rows");
}

/// Parse an SSE body into (event, data) pairs.
fn sse_events(text: &str) -> Vec<(String, Value)> {
    text.split("\n\n")
        .filter(|f| !f.trim().is_empty() && !f.starts_with(':'))
        .map(|f| {
            let mut ev = String::new();
            let mut data = String::new();
            for l in f.lines() {
                if let Some(e) = l.strip_prefix("event: ") {
                    ev = e.to_string();
                } else if let Some(d) = l.strip_prefix("data: ") {
                    data = d.to_string();
                }
            }
            (ev, serde_json::from_str(&data).unwrap_or(Value::Null))
        })
        .collect()
}

struct Run {
    app: axum::Router,
    norm: Norm,
    extra: Extra,
    failures: Vec<String>,
    deltas_used: BTreeMap<&'static str, usize>,
}

impl Run {
    /// The Rust response recorded the way the capture recorded the Python app's.
    async fn record(&mut self, method: &str, path: &str, body: Option<&Value>) -> Value {
        let got = call(&self.app, method, path, body).await;
        let ct = got.ctype().to_string();
        let mut resp = json!({"status": got.status, "content_type": ct});
        if ct.contains("json") {
            resp["json"] = got.json();
        } else if ct.starts_with("text/") && !ct.contains("html") {
            resp["text"] = json!(got.text());
        } else {
            resp["binary"] = json!({"sha256": sha(&got.body), "size": got.body.len()});
        }
        self.extra.v(&self.norm.value(&resp, None), None)
    }

    fn check(&mut self, name: &str, mut want: Value, got: Value) {
        want = self.extra.v(&want, None);
        apply_deltas(name, &mut want, &got, &mut self.deltas_used);
        if want != got {
            self.failures.push(format!(
                "== {name}\n-- python: {}\n-- rust:   {}",
                cut(&want),
                cut(&got)
            ));
        }
    }
}

fn cut(v: &Value) -> String {
    let s = v.to_string();
    if s.len() > 3000 {
        format!("{}...", &s[..3000])
    } else {
        s
    }
}

/// Every recorded difference: (record name prefix, why). The changes are in
/// `apply_deltas`; each id must be used.
const DELTAS: &[(&str, &str)] = &[
    (
        "keyring",
        "deliberate change: settings bodies gain keyring_available and keys_pending_removal",
    ),
    (
        "track_order",
        "deliberate change: Night Mix album fields and track order (disc, then track number)",
    ),
    (
        "thumb",
        "thumbnail format kept, bytes differ (Pillow vs the image crate)",
    ),
    (
        "providers",
        "no other provider in this build: GET /api/ai/providers lists this build's provider (Unconfigured), not the installed airframe adapters; `selected` is the Python app's",
    ),
    (
        "resolution",
        "resolution notes: the cascade traces the failed search",
    ),
    (
        "v17",
        "this build's schema (v17, plus later additive steps)",
    ),
    (
        "reruns",
        "deliberate change: post-scan reruns really run the checks after the scan and after the apply",
    ),
    (
        "pool",
        "PLACEHOLDER: fingerprint pool not built (always unavailable)",
    ),
    ("nfd", "deliberate change: no NFC/NFD-only dir rename"),
    (
        "scan_order",
        "deliberate change: albums are discovered in name order, not in the filesystem's listing order (the capture's order is APFS's)",
    ),
    (
        "sort_cluster",
        "reproduced: representative album = smallest album id (path-dependent)",
    ),
    (
        "artist_only",
        "deliberate change: the ARTIST-only Mono Harbor - Gaps is an album-level member of the Mono Harbor artist-sort cluster (tests/common/artist_only.rs)",
    ),
    (
        "track_album",
        "deliberate change: each track of GET /api/albums/{id}/tracks carries its ALBUM tag (the album panel compares a proposed title with it)",
    ),
    (
        "ai_wording",
        "no AI provider is ready, so check descriptions, the unanchored trace note and unanchored tags-out-of-sync summaries are shown without AI wording",
    ),
    (
        "settings_changed",
        "deliberate addition (issue #187): a settings write publishes SettingsChanged (setting names only), which the library-events stream relays",
    ),
];

fn use_delta(used: &mut BTreeMap<&'static str, usize>, id: &'static str) {
    assert!(DELTAS.iter().any(|(d, _)| *d == id), "unknown delta {id}");
    *used.entry(id).or_default() += 1;
}

const RUNS_DURING_SCAN: [&str; 5] = [
    "unidentified-album",
    "track-gaps",
    "inconsistent-artist-sort",
    "no-cover-art",
    "duplicate-tracks",
];

fn is_nfd_finding(f: &Value) -> bool {
    f["check_id"] == "filename-mismatch"
        && f["album_id"]
            .as_str()
            .is_some_and(|a| a.starts_with("<ID:Cafe\u{301}"))
}

fn album(o: &Value, r: &Value) -> Value {
    let mut e = o.clone();
    let nums: Vec<Option<i64>> = o["track_numbers"]
        .as_array()
        .unwrap()
        .iter()
        .map(Value::as_i64)
        .collect();
    let mut order: Vec<usize> = (0..nums.len()).collect();
    order.sort_by_key(|i| (nums[*i].is_none(), nums[*i]));
    let ta = o["track_artists"].as_array().unwrap();
    let new_ta: Vec<Value> = order.iter().map(|i| ta[*i].clone()).collect();
    e["track_numbers"] = json!(order.iter().map(|i| nums[*i]).collect::<Vec<_>>());
    e["artist"] = new_ta[0]["artist"].clone();
    e["sample_track"] = json!(format!(
        "{}/{}",
        o["path"].as_str().unwrap(),
        new_ta[0]["filename"].as_str().unwrap()
    ));
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
        set(&o["genre"]),
        set(&r["genre"]),
        "the track-order change keeps genre members"
    );
    e["genre"] = r["genre"].clone();
    e
}

fn is_night_mix(a: &Value) -> bool {
    a["dir_name"] == "Various Artists - Night Mix [FLAC]"
}

fn sort_albums(arr: &mut [Value]) {
    arr.sort_by_key(|a| {
        let s = |k: &str| a[k].as_str().unwrap_or("").to_string();
        let aa = s("album_artist");
        let artist = if aa.is_empty() { s("artist") } else { aa };
        (artist.to_lowercase(), s("year"), s("title").to_lowercase())
    });
}

/// The capture's album rows were stored in APFS listing order; this build
/// stores them in discovery (name) order, so everything ordered by album
/// row follows name order: ties in the album list (same sort key), the
/// findings of one check run, and cluster members. All albums sit directly
/// under the library, so name order is path order is `<ID:dir name>` order.
fn scan_order(want: &mut Value, used: &mut BTreeMap<&'static str, usize>) {
    let before = want.clone();
    if let Some(albums) = want
        .pointer_mut("/json/albums")
        .and_then(Value::as_array_mut)
    {
        albums.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
        sort_albums(albums);
    }
    if let Some(findings) = want
        .pointer_mut("/json/findings")
        .and_then(Value::as_array_mut)
    {
        for f in findings.iter_mut() {
            if let Some(m) = f
                .pointer_mut("/detail/members")
                .and_then(Value::as_array_mut)
            {
                m.sort_by(|a, b| a["dir_name"].as_str().cmp(&b["dir_name"].as_str()));
            }
        }
        // Within one check, in the slots that check's findings occupy.
        let checks: std::collections::BTreeSet<String> = findings
            .iter()
            .filter_map(|f| f["check_id"].as_str().map(str::to_string))
            .collect();
        for c in checks {
            let slots: Vec<usize> = (0..findings.len())
                .filter(|&i| findings[i]["check_id"] == c.as_str())
                .collect();
            let mut vals: Vec<Value> = slots.iter().map(|&i| findings[i].clone()).collect();
            vals.sort_by(|a, b| a["album_id"].as_str().cmp(&b["album_id"].as_str()));
            for (&i, v) in slots.iter().zip(vals) {
                findings[i] = v;
            }
        }
    }
    if *want != before {
        use_delta(used, "scan_order");
    }
}

fn apply_deltas(
    name: &str,
    want: &mut Value,
    got: &Value,
    used: &mut BTreeMap<&'static str, usize>,
) {
    // This build's provider is never ready.
    let mut n = common::ai_wording::json(want);
    if let Some(Value::String(t)) = want.get_mut("text") {
        let (t2, m) = common::ai_wording::text(t);
        *t = t2;
        n += m;
    }
    if n > 0 {
        use_delta(used, "ai_wording");
    }
    if common::artist_only::apply(want) > 0 {
        use_delta(used, "artist_only");
    }
    // Track listings gain each file's ALBUM tag, read from the same files.
    if Regex::new(r"^\d{3}-tracks-").unwrap().is_match(name)
        && want["status"] == 200
        && let Some(tracks) = want
            .pointer_mut("/json/tracks")
            .and_then(Value::as_array_mut)
    {
        let got_tracks = got["json"]["tracks"].as_array().unwrap();
        assert_eq!(tracks.len(), got_tracks.len(), "{name}");
        for (t, g) in tracks.iter_mut().zip(got_tracks) {
            assert_eq!(t["filename"], g["filename"], "{name}");
            assert!(g["album"].is_string(), "{name}: {g}");
            t["album"] = g["album"].clone();
        }
        use_delta(used, "track_album");
    }
    // Settings bodies.
    if (name.contains("settings") && !name.contains("db"))
        && want["status"] == 200
        && want["json"].get("ai_max_parallel").is_some()
    {
        want["json"]["keyring_available"] = json!(true);
        want["json"]["keys_pending_removal"] = json!(false);
        use_delta(used, "keyring");
    }
    // Night Mix.
    if let Some(albums) = want
        .pointer_mut("/json/albums")
        .and_then(Value::as_array_mut)
        && let Some(i) = albums.iter().position(is_night_mix)
    {
        let r = got["json"]["albums"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| is_night_mix(a))
            .unwrap();
        albums[i] = album(&albums[i], r);
        sort_albums(albums);
        use_delta(used, "track_order");
    }
    if name.contains("album-various-artists-night-mix") && want["status"] == 200 {
        want["json"] = album(&want["json"], &got["json"]);
        use_delta(used, "track_order");
    }
    scan_order(want, used);
    // Thumbnails (default size=thumb): status and type only.
    let thumb = Regex::new(r"^\d{3}-cover-").unwrap();
    if thumb.is_match(name) && !name.contains("cover-full") && want["status"] == 200 {
        assert_eq!(got["content_type"], "image/jpeg", "{name}: {got}");
        want["binary"] = got["binary"].clone();
        use_delta(used, "thumb");
    }
    // The AI routes are wired (GET /api/ai/jobs is compared like any
    // other); the provider list is this build's.
    if name.contains("get-api-ai-providers") && want["status"] == 200 {
        assert_eq!(got["status"], 200, "{name}");
        assert_eq!(got["json"]["selected"], want["json"]["selected"], "{name}");
        assert_eq!(
            got["json"]["providers"][0]["id"],
            musaic_ai::provider::UNCONFIGURED_ID,
            "{name}"
        );
        *want = got.clone();
        use_delta(used, "providers");
    }
    if name.contains("resolution-") {
        let notes = want["json"]["notes"].as_array_mut().unwrap();
        assert_eq!(notes.len(), 1);
        notes.insert(
            0,
            json!("Tier 0b: MusicBrainz search failed (network error: could not resolve host musicbrainz.org)"),
        );
        use_delta(used, "resolution");
    }
    if name.contains("get-api-db-info") {
        assert_eq!(want["json"]["current_schema"], 16);
        // This build's schema (v17, later steps additive).
        want["json"]["current_schema"] = json!(musaic_db::SCHEMA_VERSION);
        want["json"]["schema_version"] = json!(musaic_db::SCHEMA_VERSION);
        use_delta(used, "v17");
        // Findings exist already: the post-scan rerun ran.
        assert_eq!(want["json"]["counts"]["check_findings_open"], 0);
        want["json"]["counts"]["check_findings_open"] =
            got["json"]["counts"]["check_findings_open"].clone();
        assert!(
            got["json"]["counts"]["check_findings_open"]
                .as_i64()
                .unwrap()
                > 0
        );
        use_delta(used, "reruns");
    }
    if name.contains("get-api-db-snapshots") {
        let s = &mut want["json"]["snapshots"][0];
        assert_eq!(s["filename"], "library-<TAG>-v16.db");
        s["filename"] = json!(format!("library-<TAG>-v{}.db", musaic_db::SCHEMA_VERSION));
        s["schema_version"] = json!(musaic_db::SCHEMA_VERSION);
        use_delta(used, "v17");
    }
    if name.contains("fingerprint-pool") && want["status"] == 200 {
        *want = got.clone();
        assert_eq!(got["json"]["status"], "unavailable");
        use_delta(used, "pool");
    }
    // Findings / check summaries before the explicit runs: the reruns ran.
    if name.contains("079-get-api-checks-findings") {
        assert_eq!(want["json"]["findings"], json!([]));
        let got_checks: Vec<&str> = got["json"]["findings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["check_id"].as_str().unwrap())
            .collect();
        assert!(!got_checks.is_empty());
        assert!(
            got_checks.iter().all(|c| RUNS_DURING_SCAN.contains(c)),
            "{got_checks:?}"
        );
        want["json"] = got["json"].clone();
        use_delta(used, "reruns");
    }
    if name.contains("078-get-api-checks") {
        for (w, g) in want["json"]["checks"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .zip(got["json"]["checks"].as_array().unwrap())
        {
            if RUNS_DURING_SCAN.contains(&w["meta"]["id"].as_str().unwrap()) {
                assert_eq!(w["last_run"], Value::Null);
                assert_eq!(g["last_run"]["status"], "completed");
                w["last_run"] = g["last_run"].clone();
                w["open_count"] = g["open_count"].clone();
            }
        }
        use_delta(used, "reruns");
    }
    // Check detail: the runs list carries the earlier rerun(s) too.
    if name.contains("check-detail-") {
        let id = want["json"]["meta"]["id"].as_str().unwrap().to_string();
        let w_runs = want["json"]["runs"].as_array().unwrap().len();
        let g_runs = got["json"]["runs"].as_array().unwrap().len();
        if RUNS_DURING_SCAN.contains(&id.as_str()) {
            assert_eq!(
                g_runs,
                w_runs + 1,
                "{name}: one post-scan rerun before the explicit run"
            );
            want["json"]["runs"]
                .as_array_mut()
                .unwrap()
                .push(got["json"]["runs"][w_runs].clone());
            use_delta(used, "reruns");
        }
    }
    // NFD finding.
    if let Some(arr) = want
        .pointer_mut("/json/findings")
        .and_then(Value::as_array_mut)
    {
        let before = arr.len();
        arr.retain(|f| !is_nfd_finding(f));
        if arr.len() != before {
            use_delta(used, "nfd");
        }
    }
    if name.contains("filename-mismatch") && want["json"]["issue_count"] == 3 {
        want["json"]["issue_count"] = json!(2);
        use_delta(used, "nfd");
    }
    if name.contains("check-detail-filename-mismatch") {
        want["json"]["runs"][0]["issue_count"] = json!(2);
    }
    if let Some(checks) = want
        .pointer_mut("/json/checks")
        .and_then(Value::as_array_mut)
    {
        for c in checks.iter_mut() {
            if c["meta"]["id"] == "filename-mismatch" && c["open_count"] == 3 {
                c["open_count"] = json!(2);
                c["last_run"]["issue_count"] = json!(2);
                use_delta(used, "nfd");
            }
        }
    }
    // Finding lists are ordered by last_seen_at; the extra reruns
    // touch rows at other times, so the order (not the content) differs.
    if name.starts_with("104-") || name.starts_with("112-") {
        let garr = got["json"]["findings"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let key = |f: &Value| {
            (
                f["check_id"].clone(),
                f["summary"].clone(),
                f["detail"]["dir_name"].clone(),
                f["detail"]["headline"].clone(),
            )
        };
        let arr = want["json"]["findings"].as_array_mut().unwrap();
        arr.sort_by_key(|f| {
            garr.iter()
                .position(|g| key(g) == key(f))
                .unwrap_or(usize::MAX)
        });
        use_delta(used, "reruns");
    }
    // Inconsistent-artist-sort representative: take the album the Rust
    // build chose after checking it is a member of the same cluster.
    if let Some(arr) = want
        .pointer_mut("/json/findings")
        .and_then(Value::as_array_mut)
    {
        let garr = got["json"]["findings"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        for f in arr.iter_mut() {
            if f["check_id"] == "inconsistent-artist-sort" {
                let g = garr
                    .iter()
                    .find(|g| g["check_id"] == "inconsistent-artist-sort")
                    .unwrap();
                let members: Vec<&Value> = f["detail"]["members"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|m| &m["album_id"])
                    .collect();
                assert!(members.contains(&&g["album_id"]));
                // Which member represents the cluster depends on the album
                // ids, and those hash the (random) temp library path. So the
                // ids may or may not differ in a given run: the rule checked
                // is the membership assert above, and the delta counts as
                // exercised either way.
                use_delta(used, "sort_cluster");
                if f["album_id"] != g["album_id"] {
                    f["album_id"] = g["album_id"].clone();
                }
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn api_offline_capture_replays() {
    let meta = read_json("meta.json");
    assert_eq!(meta["mode"], "offline");
    let sb = Sandbox::new();
    let st = AppState::builder(&sb.cfg)
        .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
        .network(Network::Offline)
        .fingerprinting(common::no_key_fingerprinting())
        .frontend_dir(Some(sb.front.clone()))
        // Every finding and check run the Python app stamped in this
        // capture carries the same second (2026-11-09T17:39:25Z; e.g. all
        // three findings of 103-check-detail-filename-mismatch). Findings
        // are listed `ORDER BY last_seen_at DESC, id ASC` (the Python app's
        // order), so on a live clock a check run that crosses a second
        // boundary lists its findings in another order (reproduced with a
        // clock that advances one second per reading: 091 and 103 flip).
        // Pinned to that second, every finding row has the capture's
        // timestamps; every other timestamp is normalised to <NOW> anyway,
        // so the comparison stays exactly as strict.
        .clock(Arc::new(|| {
            UNIX_EPOCH + Duration::from_secs(CAPTURE_EPOCH_S)
        }))
        .build()
        .unwrap();
    let app = router(st.clone());
    let mut run = Run {
        app: app.clone(),
        norm: Norm::new(&sb),
        extra: Extra::new(),
        failures: Vec::new(),
        deltas_used: BTreeMap::new(),
    };

    // Background listener on /api/library-events/stream.
    let events: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
    let ev2 = events.clone();
    let resp = app
        .clone()
        .oneshot(common::get("/api/library-events/stream"))
        .await
        .unwrap();
    let listener = tokio::spawn(async move {
        let mut body = resp.into_body();
        let mut buf = String::new();
        while let Some(Ok(f)) = body.frame().await {
            if let Ok(d) = f.into_data() {
                buf.push_str(&String::from_utf8_lossy(&d));
                while let Some(i) = buf.find("\n\n") {
                    let frame: String = buf.drain(..i + 2).collect();
                    ev2.lock().unwrap().extend(sse_events(&frame));
                }
            }
        }
    });

    let index = read_json("api/index.json");
    let mut fid_map: BTreeMap<i64, Option<i64>> = BTreeMap::new();
    let re = Regex::new(r"/findings/(\d+)/").unwrap();
    for entry in index.as_array().unwrap() {
        let file = entry["file"].as_str().unwrap();
        let name = file.trim_start_matches("api/").trim_end_matches(".json");
        let rec = read_json(file);
        let method = rec["request"]["method"].as_str().unwrap();
        let mut path = rec["request"]["path"].as_str().unwrap().to_string();
        if name.starts_with("001-") {
            // FastAPI's generated /openapi.json is not part of the contract;
            // the inventory is pinned by testdata/golden/api/routes.json.
            continue;
        }
        if name.starts_with("009-") {
            // The capture streamed the scan here (events/scan-stream.jsonl).
            let got = call(&app, "GET", "/api/scan/stream", None).await;
            let got_ev: Vec<Value> = sse_events(&got.text())
                .into_iter()
                .map(|(e, d)| json!({"event": e, "data": d}))
                .collect();
            let albums = call(&app, "GET", "/api/albums", None).await.json();
            for a in albums["albums"].as_array().unwrap() {
                run.norm.add(
                    a["id"].as_str().unwrap(),
                    &format!("<ID:{}>", a["dir_name"].as_str().unwrap()),
                );
            }
            let want_ev: Vec<Value> = read_jsonl("events/scan-stream.jsonl")
                .into_iter()
                .map(|l| json!({"event": l["event"], "data": l["data"]}))
                .collect();
            let mut want_ev = run.extra.v(&json!(want_ev), None);
            let got_ev = run.extra.v(&run.norm.value(&json!(got_ev), None), None);
            // The capture streamed albums in APFS listing order; this build
            // discovers them in name order on every filesystem.
            let slots: Vec<usize> = (0..want_ev.as_array().unwrap().len())
                .filter(|&i| want_ev[i]["event"] == "album")
                .collect();
            let mut sorted: Vec<Value> = slots.iter().map(|&i| want_ev[i].clone()).collect();
            sorted.sort_by(|a, b| {
                a["data"]["path"]
                    .as_str()
                    .unwrap()
                    .cmp(b["data"]["path"].as_str().unwrap())
            });
            if slots.iter().zip(&sorted).any(|(&i, w)| want_ev[i] != *w) {
                for (&i, w) in slots.iter().zip(sorted) {
                    want_ev[i] = w;
                }
                use_delta(&mut run.deltas_used, "scan_order");
            }
            for (w, g) in want_ev
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .zip(got_ev.as_array().unwrap())
            {
                if w["event"] == "album" && is_night_mix(&w["data"]) {
                    w["data"] = album(&w["data"], &g["data"]);
                    use_delta(&mut run.deltas_used, "track_order");
                }
            }
            if want_ev != got_ev {
                run.failures.push(format!(
                    "== scan stream\n-- python: {}\n-- rust:   {}",
                    cut(&want_ev),
                    cut(&got_ev)
                ));
            }
            // The post-scan rerun runs in the background.
            assert!(st.bus().wait_background_idle(Duration::from_secs(30)));
            // Pool stand-in (see the module docs).
            seed_fingerprints(st.db_path());
        }
        if name.starts_with("105-")
            || name.starts_with("106-")
            || name.starts_with("107-")
            || name.starts_with("108-")
        {
            let oid: i64 = re.captures(&path).unwrap()[1].parse().unwrap();
            let nid = fid_map.get(&oid).copied().flatten().unwrap_or(999_999);
            path = re
                .replace(&path, format!("/findings/{nid}/").as_str())
                .into_owned();
        }
        let body = rec["request"]["json"].clone();
        let body = (!body.is_null()).then(|| run.norm.unnorm_value(&body));
        let mut url = run.norm.unnorm(&path);
        if let Some(p) = rec["request"]["params"].as_object() {
            let q: Vec<String> = p
                .iter()
                .map(|(k, v)| format!("{k}={}", v.as_str().unwrap()))
                .collect();
            url = format!("{url}?{}", q.join("&"));
        }
        let got = run.record(method, &url, body.as_ref()).await;
        let mut want = rec["response"].clone();
        if name.starts_with("004-") && cfg!(windows) {
            // "/no/such/dir" has no drive on Windows; str(Path) there (and
            // ours) spells it with backslashes, as Python does on Windows.
            if let Some(d) = want["json"]["detail"].as_str() {
                want["json"]["detail"] = Value::String(d.replace('/', "\\"));
            }
        }
        if name.starts_with("089-") {
            // body=False in the capture: status and type only.
            want["binary"] = got["binary"].clone();
        }
        if name.starts_with("104-") {
            // Match findings by identity: Python id -> Rust id.
            let o = read_json(file)["response"]["json"]["findings"].clone();
            let g = call(&app, "GET", "/api/checks/findings", None).await.json();
            let key = |f: &Value, norm: &Norm| {
                (
                    f["check_id"].as_str().unwrap().to_string(),
                    norm.text(f["album_id"].as_str().unwrap_or("")),
                    f["summary"].as_str().unwrap().to_string(),
                )
            };
            for f in o.as_array().unwrap() {
                let k = key(f, &run.norm);
                let hit = g["findings"].as_array().unwrap().iter().find(|x| {
                    let kx = key(x, &run.norm);
                    kx.0 == k.0 && (kx.1 == k.1 || k.0 == "inconsistent-artist-sort") && kx.2 == k.2
                });
                fid_map.insert(
                    f["id"].as_i64().unwrap(),
                    hit.map(|h| h["id"].as_i64().unwrap()),
                );
            }
        }
        let tagged = name.to_string();
        if name.starts_with("106-") || name.starts_with("107-") || name.starts_with("108-") {
            // A plan-preview of the NFD finding has no Rust counterpart.
            if want["json"]["dir_name"]
                .as_str()
                .is_some_and(|d| d.starts_with("Cafe\u{301}"))
            {
                assert_eq!(got["status"], 404);
                want = got.clone();
                use_delta(&mut run.deltas_used, "nfd");
            }
        }
        run.check(&tagged, want, got);

        if name.starts_with("109-") {
            mutation_phase(&mut run, &st).await;
        }
        // The capture's last step: the analyses ran after "findings
        // after apply" (112) and before the end-of-run status records.
        if name.starts_with("112-") {
            analysis_phase(&mut run, &st).await;
        }
    }

    assert!(st.bus().wait_background_idle(Duration::from_secs(30)));
    // Library events: the Python app's minus the ones of unbuilt subsystems.
    let mut want: Vec<Value> = Vec::new();
    for l in read_jsonl("events/library-events.jsonl") {
        if l["event"] == "TrackFingerprinted" {
            use_delta(&mut run.deltas_used, "pool");
            continue;
        }
        want.push(json!({"event": l["event"], "data": l["data"]}));
    }
    // Every event is queued for the stream by now; give the listener task
    // time to read them all (generous: a loaded runner can be slow), then a
    // little longer so an extra event would show up too.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while events.lock().unwrap().len() < want.len() && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    listener.abort();
    let got: Vec<Value> = events
        .lock()
        .unwrap()
        .iter()
        .filter(|(e, d)| {
            if e != "SettingsChanged" {
                return true;
            }
            // Names only, never a value.
            assert!(d["keys"].is_array(), "{d}");
            use_delta(&mut run.deltas_used, "settings_changed");
            false
        })
        .map(|(e, d)| json!({"event": e, "data": d}))
        .collect();
    let got = run.norm.value(&json!(got), None);
    if json!(want) != got {
        run.failures.push(format!(
            "== library events\n-- python: {}\n-- rust:   {}",
            cut(&json!(want)),
            cut(&got)
        ));
    }

    for (id, why) in DELTAS {
        assert!(
            run.deltas_used.contains_key(id),
            "delta {id} never applied ({why})"
        );
    }
    println!("deltas applied: {:?}", run.deltas_used);
    assert!(
        run.failures.is_empty(),
        "{} differences:\n{}",
        run.failures.len(),
        run.failures.join("\n")
    );
    let _ = golden;
}

/// The capture's mutation step: dry run, genre=Ambient (a no-op
/// on this album), genre=Drone.
async fn mutation_phase(run: &mut Run, st: &AppState) {
    let target = run.norm.unnorm("<ID:Aurora Fields - First Light [FLAC]>");
    for (file, body) in [
        (
            "events/apply-dry-run.jsonl",
            json!({"suggestions": {"genre": "Ambient"}, "track_suggestions": {}, "cover_url": null, "dry_run": true}),
        ),
        (
            "events/apply-genre.jsonl",
            json!({"suggestions": {"genre": "Ambient"}, "track_suggestions": {}, "cover_url": null, "dry_run": false}),
        ),
        (
            "events/apply-genre-change.jsonl",
            json!({"suggestions": {"genre": "Drone"}, "track_suggestions": {}, "cover_url": null, "dry_run": false}),
        ),
    ] {
        let got = call(
            &run.app,
            "POST",
            &format!("/api/albums/{target}/apply/stream"),
            Some(&body),
        )
        .await;
        assert_eq!(got.status, 200, "{file}");
        let got_ev: Vec<Value> = sse_events(&got.text())
            .into_iter()
            .map(|(e, d)| json!({"event": e, "data": d}))
            .collect();
        let got_ev = run.extra.v(&run.norm.value(&json!(got_ev), None), None);
        let want_ev: Vec<Value> = read_jsonl(file)
            .into_iter()
            .map(|l| json!({"event": l["event"], "data": l["data"]}))
            .collect();
        let want_ev = run.extra.v(&json!(want_ev), None);
        if want_ev != got_ev {
            run.failures.push(format!(
                "== {file}\n-- python: {}\n-- rust:   {}",
                cut(&want_ev),
                cut(&got_ev)
            ));
        }
        assert!(st.bus().wait_background_idle(Duration::from_secs(30)));
    }
}

/// The capture's last step: the analysis streams, compared like
/// the applies (the real analyzers, musaic-bpm and musaic-dsp).
async fn analysis_phase(run: &mut Run, st: &AppState) {
    let target = run.norm.unnorm("<ID:Aurora Fields - First Light [FLAC]>");
    for what in ["replaygain", "bpm"] {
        let file = format!("events/analyze-{what}.jsonl");
        let got = call(
            &run.app,
            "POST",
            &format!("/api/albums/{target}/analyze/stream"),
            Some(&json!({"what": what})),
        )
        .await;
        assert_eq!(got.status, 200, "{file}: {}", got.text());
        let got_ev: Vec<Value> = sse_events(&got.text())
            .into_iter()
            .map(|(e, d)| json!({"event": e, "data": d}))
            .collect();
        let got_ev = run.extra.v(&run.norm.value(&json!(got_ev), None), None);
        let want_ev: Vec<Value> = read_jsonl(&file)
            .into_iter()
            .map(|l| json!({"event": l["event"], "data": l["data"]}))
            .collect();
        let want_ev = run.extra.v(&json!(want_ev), None);
        if want_ev != got_ev {
            run.failures.push(format!(
                "== {file}\n-- python: {}\n-- rust:   {}",
                cut(&want_ev),
                cut(&got_ev)
            ));
        }
        assert!(st.bus().wait_background_idle(Duration::from_secs(30)));
    }
}
