//! The analyze stream against testdata/golden/services/analyze.json, recorded
//! by a generator that drove the Python app's own
//! `POST /api/albums/{id}/analyze/stream` with real librosa / pyloudnorm.
//!
//! Per scenario: the same album copied from testdata/synthetic-library/library (or
//! the real-audio set (private), skipped with a message when absent), the same
//! starting database (this build's scan + save_snapshot, compared with the
//! Python app's `before`), the same out-of-band changes (Missing row, removed
//! directory, deleted FLACs, read-only file, a non-FLAC `.flac`), then the
//! real analyzers (musaic-bpm, musaic-dsp) and compared with the Python app:
//! every item the Python app's generator produced, with the number of bus
//! events published before it; the SSE body for the runs that went to the
//! end; the bus events; every file of the library (bytes and parsed layout,
//! so BPM / ReplayGain tag strings are compared exactly); the database rows.
//!
//! Consumer stops (the ASGI-harness scenarios): the sink returns `Break` on
//! the last item the Python app's generator produced, which is what the Python app's
//! disconnect amounts to.
//!
//! Differences are allowed only where listed in [`record_deltas`] (scenario,
//! JSON pointer, reason, new value; the Python app's value must differ and every
//! delta is used), plus one comparison rule:
//! - a stopped run still goes to completion: files, rows and bus
//!   events are the Python app's own full run of the same album and analysis
//!   (same starting point, checked); the items handed out are unchanged.
//! - a Missing album is refused with 409 before anything happens.
//! - a failed file counts once in `errors` and gets no album gain.
//! - rule: a per-file `error` is compared for presence only,
//!   except the Python app's reproducible OS text (`[Errno 13] Permission
//!   denied`), which must match exactly.
//!
//! DSP tolerances (+/-0.1 dB ReplayGain, +/-2 BPM) are not needed: on
//! these files the Rust values produce the Python app's tag strings exactly.

mod common;

use std::ops::ControlFlow;
use std::path::Path;
use std::sync::OnceLock;

use common::*;
use musaic_scan::TrackOrder;
use musaic_services::analyze::{
    AnalysisKind, AnalyzeEnv, AnalyzeOutcome, AnalyzeRequest, Analyzer, DspAnalyzer, analyze_album,
    resolve_album,
};
use musaic_services::apply::ApplyItem;
use serde_json::{Value, json};

fn golden() -> &'static Value {
    static G: OnceLock<Value> = OnceLock::new();
    G.get_or_init(|| load_golden("analyze.json"))
}

fn album_columns() -> Vec<String> {
    golden()["album_columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap().to_string())
        .collect()
}

fn scenario(id: &str) -> &'static Value {
    golden()["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == id)
        .unwrap_or_else(|| panic!("no scenario {id}"))
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap().flatten() {
        let p = e.path();
        if e.file_type().unwrap().is_dir() {
            copy_dir(&p, &dst.join(e.file_name()));
        } else {
            std::fs::copy(&p, dst.join(e.file_name())).unwrap();
        }
    }
}

fn make_writable(dir: &Path) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            make_writable(&p);
        } else {
            let mut perm = std::fs::metadata(&p).unwrap().permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perm.set_readonly(false);
            let _ = std::fs::set_permissions(&p, perm);
        }
    }
}

/// `"dir_mtime": <number>` -> `"dir_mtime": "<MTIME>"`, as the generator does
/// to the raw SSE body.
fn mask_mtime(body: &str) -> String {
    const KEY: &str = "\"dir_mtime\": ";
    let mut out = String::new();
    let mut rest = body;
    while let Some(i) = rest.find(KEY) {
        out.push_str(&rest[..i + KEY.len()]);
        rest = &rest[i + KEY.len()..];
        let end = rest
            .find(|c: char| !(c.is_ascii_digit() || "+-.e".contains(c)))
            .unwrap_or(rest.len());
        if end > 0 {
            out.push_str("\"<MTIME>\"");
            rest = &rest[end..];
        }
    }
    out.push_str(rest);
    out
}

struct Run {
    /// The router's refusal, if any.
    refused: Option<musaic_services::HttpError>,
    /// Its detail with the library root as `<LIB>`.
    refused_detail_normalized: String,
    items: Vec<Value>,
    sse_body: String,
    outcome: AnalyzeOutcome,
    bus: Vec<Value>,
    before: Value,
    after: Value,
}

/// Prepare the library and database as the generator did, run the analysis
/// with `analyzer`, record what came out. `None` when the scenario needs the
/// real-audio fixture set and it is absent.
fn run(sc: &Value, analyzer: &dyn Analyzer) -> Option<Run> {
    // Real-audio scenarios come from the private half of the golden and need
    // the local copy of the real-audio set (private, never committed).
    let src_root = if sc["source"] == "real" {
        match musaic_testdata::real_library() {
            Some(p) => p,
            None => {
                musaic_testdata::skip_private_cases();
                return None;
            }
        }
    } else {
        musaic_testdata::synthetic_library()
    };
    let album_name = sc["album"].as_str().unwrap();
    let (td, lib) = temp_lib(golden());
    let album_dir = lib.join(album_name);
    copy_dir(&src_root.join(album_name), &album_dir);
    for (name, text) in sc["extra_files"].as_object().into_iter().flatten() {
        std::fs::write(album_dir.join(name), text.as_str().unwrap()).unwrap();
    }
    for (name, tags) in sc["extra_flacs"].as_object().into_iter().flatten() {
        let tags: Vec<(String, String)> = tags
            .as_array()
            .unwrap()
            .iter()
            .map(|kv| {
                (
                    kv[0].as_str().unwrap().into(),
                    kv[1].as_str().unwrap().into(),
                )
            })
            .collect();
        std::fs::write(album_dir.join(name), flac_bytes(&tags, 3, 0)).unwrap();
    }
    let mut norm = Norm::new(&lib);
    norm.learn_all();
    let db = fresh_db(td.path());
    let mut conn = connect(&db);
    let albums = musaic_scan::scan_library(&lib, TrackOrder::DiscTrack);
    let seed_ts = golden()["seed_ts"].as_str().unwrap();
    musaic_db::albums::save_snapshot(&mut conn, &lib.to_string_lossy(), seed_ts, seed_ts, &albums)
        .unwrap();
    let album_id = musaic_core::album_id(&musaic_scan::py_resolve(&album_dir).to_string_lossy());
    if sc["mark_missing"] == true {
        musaic_db::albums::mark_album_missing(&conn, &album_id, "2001-02-01T00:00:00Z").unwrap();
    }
    if sc["remove_dir"] == true {
        std::fs::remove_dir_all(&album_dir).unwrap();
    }
    if sc["delete_flacs"] == true {
        for e in std::fs::read_dir(&album_dir).unwrap().flatten() {
            if e.path().extension().is_some_and(|x| x == "flac") {
                std::fs::remove_file(e.path()).unwrap();
            }
        }
    }
    for rel in sc["readonly"].as_array().into_iter().flatten() {
        let p = album_dir.join(rel.as_str().unwrap());
        let mut perm = std::fs::metadata(&p).unwrap().permissions();
        perm.set_readonly(true);
        std::fs::set_permissions(&p, perm).unwrap();
    }
    let before =
        json!({"files": describe_lib(&lib), "db": dump_db(&conn, &norm, &album_columns())});

    let lib_str = lib.to_string_lossy().into_owned();
    let what = AnalysisKind::parse(sc["what"].as_str().unwrap()).unwrap();
    let lookup = resolve_album(&conn, Some(&lib_str), &album_id, TrackOrder::DiscTrack);
    let refused = lookup.as_ref().err().cloned();
    // A refused album is still handed to the pipeline (as a direct caller
    // could): it must refuse too.
    let (id, path) = match lookup {
        Ok(a) => (a.id, a.path),
        Err(_) => (
            album_id.clone(),
            musaic_scan::py_resolve(&album_dir)
                .to_string_lossy()
                .into_owned(),
        ),
    };
    let req = AnalyzeRequest {
        album_id: id,
        album_path: path,
        library_dir: Some(lib_str),
        what,
    };
    // Disconnect scenarios: stop on the last item the Python app produced.
    let stop_at = (sc["driver"] == "asgi" && sc["exception"].is_null())
        .then(|| sc["produced"].as_array().unwrap().len());
    let (bus, log) = recording_bus();
    let now = || "2030-01-01T00:00:00Z".to_string();
    let mut env = AnalyzeEnv {
        conn: &mut conn,
        bus: &bus,
        analyzer,
        now: &now,
        track_order: TrackOrder::DiscTrack,
    };
    let mut items = Vec::new();
    let mut sse = String::new();
    let outcome = analyze_album(&mut env, &req, &mut |item: ApplyItem| {
        sse.push_str(&item.sse_frame());
        items.push(json!({"event": item.event, "data": item.data.to_value(),
            "bus_events_so_far": log.lock().unwrap().len()}));
        if stop_at == Some(items.len()) {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });
    norm.learn_all();
    let bus_events: Vec<Value> = log
        .lock()
        .unwrap()
        .iter()
        .map(|e| norm.value(&event_json(e)))
        .collect();
    let after = json!({"files": describe_lib(&lib), "db": dump_db(&conn, &norm, &album_columns())});
    make_writable(&lib);
    let mut sse_body = mask_mtime(&sse);
    for (real, label) in &norm.ids {
        sse_body = sse_body.replace(real.as_str(), &format!("<ID:{label}>"));
    }
    let sse_body = sse_body.replace(&norm.lib, "<LIB>");
    drop(td);
    let outcome = match outcome {
        AnalyzeOutcome::Aborted(d) => AnalyzeOutcome::Aborted(d.replace(&norm.lib, "<LIB>")),
        o => o,
    };
    Some(Run {
        refused_detail_normalized: refused
            .as_ref()
            .map(|e| e.detail.replace(&norm.lib, "<LIB>"))
            .unwrap_or_default(),
        refused,
        items: items.iter().map(|i| norm.value(i)).collect(),
        sse_body,
        outcome,
        bus: bus_events,
        before,
        after: norm.value(&after),
    })
}

// ---------- deltas ----------

/// Where the new value of a [`RecordDelta`] comes from.
enum NewValue {
    /// The same place in another scenario's record (the Python app's run of the
    /// same analysis on the same album, to completion).
    Scenario(&'static str),
    /// Another place in the same scenario's record.
    Path(&'static str),
    /// Built in the test from the scenario's record.
    Value(fn(&Value) -> Value),
}

/// One deliberate change: in `scenario`, the value at JSON pointer `path`
/// is replaced. The Python app's value must differ from the new one.
struct RecordDelta {
    scenario: &'static str,
    path: &'static str,
    why: &'static str,
    new: NewValue,
}

/// The text of the Missing-album refusal: the generator seeded missing_since.
const MISSING_SINCE_SEED: &str = "2001-02-01T00:00:00Z";

/// `errors` of the final `done` item: each failed file once.
fn errors_once(sc: &Value) -> Value {
    json!(
        sc["produced"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["event"] == "file_done" && p["data"].get("error").is_some())
            .count()
    )
}

fn record_deltas() -> Vec<RecordDelta> {
    let mut d = Vec::new();
    // A run whose consumer is gone still goes
    // to completion. Old: what the Python app's closed generator left behind.
    // New: the Python app's own full run of the same analysis on the same album
    // (identical `before`, checked in `stop_scenarios_share_the_full_runs_input`).
    for (scenario, full, parts) in [
        (
            "disconnect-after-file-done-0",
            "bpm-first-light",
            &["/after/files", "/bus_events"][..],
        ),
        (
            "disconnect-after-file-started-1",
            "bpm-first-light",
            &["/after/files", "/bus_events"],
        ),
        (
            "disconnect-during-file-1",
            "bpm-first-light",
            &["/after/files", "/bus_events"],
        ),
        (
            "disconnect-rg-after-last-file-done",
            "rg-first-light",
            &["/after/files", "/bus_events"],
        ),
        (
            "disconnect-rg-after-album-done",
            "rg-first-light",
            &["/bus_events"],
        ),
        (
            "disconnect-rg-after-updated-album",
            "rg-first-light",
            &["/bus_events"],
        ),
        (
            "disconnect-during-updated-album",
            "rg-first-light",
            &["/bus_events"],
        ),
    ] {
        for path in parts {
            d.push(RecordDelta {
                scenario,
                path,
                why: "analyze-completes",
                new: NewValue::Scenario(full),
            });
        }
    }
    // A Missing album is refused with 409
    // before anything is read or written: no item, no event, nothing
    // changed. (missing-dir-gone: the Python app changed nothing either; it
    // raised inside the stream instead.)
    for scenario in ["missing-dir-present", "missing-dir-gone"] {
        d.push(RecordDelta {
            scenario,
            path: "/status",
            why: "analyze-refuses-missing",
            new: NewValue::Value(|_| json!(409)),
        });
    }
    d.push(RecordDelta {
        scenario: "missing-dir-present",
        path: "/produced",
        why: "analyze-refuses-missing",
        new: NewValue::Value(|_| json!([])),
    });
    d.push(RecordDelta {
        scenario: "missing-dir-present",
        path: "/after",
        why: "analyze-refuses-missing",
        new: NewValue::Path("/before"),
    });
    d.push(RecordDelta {
        scenario: "missing-dir-present",
        path: "/bus_events",
        why: "analyze-refuses-missing",
        new: NewValue::Value(|_| json!([])),
    });
    d.push(RecordDelta {
        scenario: "missing-dir-gone",
        path: "/exception",
        why: "analyze-refuses-missing",
        new: NewValue::Value(|_| Value::Null),
    });
    // A Present row whose directory is gone
    // is refused with 409 before streaming (delete-duplicate's status for an
    // unreachable album directory); the Python app answered 200 and raised
    // inside the stream. Nothing changed in either.
    d.push(RecordDelta {
        scenario: "present-dir-gone",
        path: "/status",
        why: "unreachable-album",
        new: NewValue::Value(|_| json!(409)),
    });
    d.push(RecordDelta {
        scenario: "present-dir-gone",
        path: "/exception",
        why: "unreachable-album",
        new: NewValue::Value(|_| Value::Null),
    });
    // A file whose track step failed counts
    // once and gets no album gain. Where its album-tag write failed anyway
    // (not FLAC, read-only) the files are as recorded and only the count
    // differs; for a valid FLAC whose measurement failed the file stays as
    // it was before the run.
    for scenario in ["rg-broken-file", "rg-readonly"] {
        d.push(RecordDelta {
            scenario,
            path: "/produced/LAST/data/errors",
            why: "album-gain-once",
            new: NewValue::Value(errors_once),
        });
        d.push(RecordDelta {
            scenario,
            path: "/sse_body",
            why: "album-gain-once",
            new: NewValue::Value(|sc| {
                let body = sc["sse_body"].as_str().unwrap();
                let done = body.rfind("event: done").unwrap();
                let (head, tail) = body.split_at(done);
                let old = format!(
                    "\"errors\": {}",
                    sc["produced"].as_array().unwrap().last().unwrap()["data"]["errors"]
                );
                assert_eq!(tail.matches(&old).count(), 1);
                json!(format!(
                    "{head}{}",
                    tail.replace(&old, &format!("\"errors\": {}", errors_once(sc)))
                ))
            }),
        });
    }
    d.push(RecordDelta {
        scenario: "rg-metadata-only-file",
        path: "/after/files",
        why: "album-gain-once",
        new: NewValue::Value(|sc| {
            let failed: Vec<&str> = sc["produced"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|p| p["event"] == "file_done" && p["data"].get("error").is_some())
                .map(|p| p["data"]["filename"].as_str().unwrap())
                .collect();
            let before = sc["before"]["files"].as_array().unwrap();
            json!(
                sc["after"]["files"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|f| {
                        let name = f["path"].as_str().unwrap().rsplit('/').next().unwrap();
                        if failed.contains(&name) {
                            before
                                .iter()
                                .find(|b| b["path"] == f["path"])
                                .unwrap()
                                .clone()
                        } else {
                            f.clone()
                        }
                    })
                    .collect::<Vec<_>>()
            )
        }),
    });
    d
}

/// `/produced/LAST/...` addresses the last produced item.
fn resolve_pointer(sc: &Value, path: &str) -> String {
    match path.strip_prefix("/produced/LAST") {
        Some(rest) => format!(
            "/produced/{}{rest}",
            sc["produced"].as_array().unwrap().len() - 1
        ),
        None => path.to_string(),
    }
}

/// The Python app's record of `sc` with its [`record_deltas`] applied.
fn expected_record(sc: &Value) -> Value {
    let id = sc["id"].as_str().unwrap();
    let mut out = sc.clone();
    for d in record_deltas().iter().filter(|d| d.scenario == id) {
        let path = resolve_pointer(sc, d.path);
        let new = match &d.new {
            NewValue::Scenario(other) => scenario(other).pointer(&path).unwrap().clone(),
            NewValue::Path(p) => sc.pointer(p).unwrap().clone(),
            NewValue::Value(f) => f(sc),
        };
        let slot = out
            .pointer_mut(&path)
            .unwrap_or_else(|| panic!("{id}: no {path} in the Python app's record"));
        assert_ne!(*slot, new, "{id} {path} ({}): delta changes nothing", d.why);
        *slot = new;
    }
    out
}

/// A per-file error's text is compared only where the Python app's is
/// reproducible (an OS error, e.g. `[Errno 13] Permission denied: ...`).
fn reproducible_error(text: &str) -> bool {
    text.starts_with("[Errno ")
}

/// Items with every non-reproducible `error` text masked.
fn mask_item_errors(items: &Value, recorded: &Value) -> Value {
    let mut out = items.clone();
    for (i, item) in out.as_array_mut().unwrap().iter_mut().enumerate() {
        let theirs = recorded[i]["data"]["error"].as_str();
        if let (Some(e), Some(t)) = (item["data"].get_mut("error"), theirs)
            && !reproducible_error(t)
        {
            *e = json!("<ERROR>");
        }
    }
    out
}

/// An SSE body with every non-reproducible `"error": "..."` masked.
fn mask_sse_errors(body: &str) -> String {
    const KEY: &str = "\"error\": \"";
    let mut out = String::new();
    let mut rest = body;
    while let Some(i) = rest.find(KEY) {
        out.push_str(&rest[..i + KEY.len()]);
        rest = &rest[i + KEY.len()..];
        let mut end = 0;
        let bytes = rest.as_bytes();
        while bytes[end] != b'"' {
            end += if bytes[end] == b'\\' { 2 } else { 1 };
        }
        let text = &rest[..end];
        out.push_str(if reproducible_error(text) {
            text
        } else {
            "<ERROR>"
        });
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

fn check(id: &str) {
    // The real-audio scenarios are in the private half of the golden.
    if id.starts_with("real-") && !musaic_testdata::json("golden/services/analyze.json").private {
        musaic_testdata::skip();
        return;
    }
    let sc = scenario(id);
    let Some(r) = run(sc, &DspAnalyzer) else {
        return;
    };
    let want = expected_record(sc);
    assert_eq!(
        r.before, want["before"],
        "{id}: starting state differs from the Python app's"
    );
    if want["status"] == 409 {
        // The router refuses; the pipeline refused as well.
        let e = r.refused.as_ref().expect("refused");
        assert_eq!(e.status, 409, "{id}");
        let want_detail = if sc["mark_missing"] == true {
            musaic_services::apply::missing_album_detail(MISSING_SINCE_SEED)
        } else {
            // The Python app's own FileNotFoundError names the path.
            assert_eq!(sc["exception"]["type"], "FileNotFoundError", "{id}");
            musaic_services::analyze::unreachable_album_detail(
                Path::new(&format!("<LIB>/{}", sc["album"].as_str().unwrap())),
                &std::io::Error::from_raw_os_error(2), // ENOENT
            )
        };
        assert_eq!(r.refused_detail_normalized, want_detail, "{id}");
        assert_eq!(r.outcome, AnalyzeOutcome::Refused(e.clone()), "{id}");
    } else {
        assert!(r.refused.is_none(), "{id}: refused {:?}", r.refused);
    }
    let produced = &want["produced"];
    assert_eq!(
        mask_item_errors(&Value::Array(r.items.clone()), produced),
        mask_item_errors(produced, produced),
        "{id}: items"
    );
    assert_eq!(
        Value::Array(r.bus.clone()),
        want["bus_events"],
        "{id}: bus events"
    );
    assert_eq!(r.after["db"], want["after"]["db"], "{id}: database");
    let ours = r.after["files"].as_array().unwrap();
    let theirs = want["after"]["files"].as_array().unwrap();
    assert_eq!(ours.len(), theirs.len(), "{id}: file count");
    for (o, t) in ours.iter().zip(theirs) {
        assert_eq!(o, t, "{id}: file {}", t["path"]);
    }
    if want["status"] == 409 {
        return;
    }
    match sc["driver"].as_str() {
        // TestClient: the whole body went out; byte-identical SSE (error
        // texts compared for presence only, as above).
        None => {
            assert_eq!(r.outcome, AnalyzeOutcome::Finished, "{id}");
            assert_eq!(
                sc["frames"],
                Value::Array(
                    sc["produced"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|p| json!({"event": p["event"], "data": p["data"]}))
                        .collect()
                )
            );
            assert_eq!(
                mask_sse_errors(&r.sse_body),
                mask_sse_errors(want["sse_body"].as_str().unwrap()),
                "{id}: SSE body"
            );
        }
        Some("asgi") if !want["exception"].is_null() => {
            let AnalyzeOutcome::Aborted(detail) = &r.outcome else {
                panic!("{id}: expected Aborted, got {:?}", r.outcome);
            };
            assert!(
                r.items.is_empty() && sc["frames"] == json!([]),
                "{id}: no frame"
            );
            assert_eq!(sc["exception"]["type"], "FileNotFoundError", "{id}");
            assert_eq!(
                json!(detail),
                sc["exception"]["str"],
                "{id}: exception text"
            );
        }
        Some("asgi") => {
            assert_eq!(r.outcome, AnalyzeOutcome::Stopped, "{id}");
            // What the client actually received is a prefix of what was produced.
            let frames = sc["frames"].as_array().unwrap();
            for (f, p) in frames.iter().zip(r.items.iter()) {
                assert_eq!(f["event"], p["event"]);
                assert_eq!(f["data"], p["data"]);
            }
        }
        other => panic!("{id}: unknown driver {other:?}"),
    }
}

/// Each stop scenario starts exactly where its full run did, so
/// the full run's end state is the right expectation; and every delta's
/// scenario exists and is used.
#[test]
fn stop_scenarios_share_the_full_runs_input() {
    for d in record_deltas() {
        let sc = scenario(d.scenario);
        if let NewValue::Scenario(full) = d.new {
            let f = scenario(full);
            assert_eq!(
                sc["before"], f["before"],
                "{} vs {full}: before",
                d.scenario
            );
            assert_eq!(sc["what"], f["what"]);
            assert_eq!(sc["album"], f["album"]);
        }
        let _ = expected_record(sc); // panics on a delta that changes nothing
    }
}

macro_rules! scenarios {
    ($($name:ident => $id:literal),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                check($id);
            }
        )*

        /// Every scenario in the golden has a test above.
        #[test]
        fn every_golden_scenario_is_tested() {
            let tested = [$($id),*];
            for s in golden()["scenarios"].as_array().unwrap() {
                assert!(tested.contains(&s["id"].as_str().unwrap()), "untested scenario {}", s["id"]);
            }
        }
    };
}

// The item sequence, the bpm tag and payload, AlbumTagsChanged
// (fields=["bpm"]), and `._` files and notes.txt skipped.
scenarios! {
    bpm_dotfiles => "bpm-dotfiles",
    rg_dotfiles => "rg-dotfiles",
    bpm_first_light => "bpm-first-light",
    rg_first_light => "rg-first-light",
    rg_non_ascii_names => "rg-cafe",
    bpm_broken_file => "bpm-broken-file",
    rg_broken_file_counted_once => "rg-broken-file",
    bpm_readonly_file => "bpm-readonly",
    rg_readonly_file_counted_once => "rg-readonly",
    bpm_all_readonly_still_publishes => "bpm-all-readonly",
    no_flacs_left => "no-flacs-left",
    missing_album_dir_present_refused => "missing-dir-present",
    missing_album_dir_gone_refused => "missing-dir-gone",
    stop_after_file_done_0 => "disconnect-after-file-done-0",
    stop_after_file_started_1 => "disconnect-after-file-started-1",
    stop_rg_after_last_file_done => "disconnect-rg-after-last-file-done",
    stop_rg_after_album_done => "disconnect-rg-after-album-done",
    stop_rg_after_updated_album => "disconnect-rg-after-updated-album",
    stop_while_file_1_runs => "disconnect-during-file-1",
    stop_while_rescan_runs => "disconnect-during-updated-album",
    real_bpm => "real-bpm",
    real_rg => "real-rg",
    rg_failed_file_gets_no_album_gain => "rg-metadata-only-file",
    present_album_dir_gone_refused => "present-dir-gone",
}

/// Negative: the router's lookup, compared with the Python app's
/// `http` cases: 404 for an unknown id, 409 without a library. (422 for a bad
/// `what` is request validation in the HTTP layer: [`AnalysisKind::parse`]
/// accepts exactly the Python app's Literal values.)
#[test]
fn router_lookup_refusals() {
    let http = golden()["http"].as_array().unwrap();
    let case = |id: &str| http.iter().find(|c| c["id"] == id).unwrap();
    let td = tempfile::tempdir().unwrap();
    let db = fresh_db(td.path());
    let conn = connect(&db);
    let lib = td.path().join("lib");
    std::fs::create_dir(&lib).unwrap();
    let lib = lib.to_string_lossy().into_owned();
    let e =
        resolve_album(&conn, Some(&lib), "0123456789abcdef", TrackOrder::DiscTrack).unwrap_err();
    assert_eq!(json!(e.status), case("unknown-album")["status"]);
    assert_eq!(json!(e.detail), case("unknown-album")["json"]["detail"]);
    let e = resolve_album(&conn, None, "0123456789abcdef", TrackOrder::DiscTrack).unwrap_err();
    assert_eq!(json!(e.status), case("no-library")["status"]);
    assert_eq!(json!(e.detail), case("no-library")["json"]["detail"]);
    let bad = case("bad-what");
    assert_eq!(bad["status"], 422);
    assert_eq!(
        bad["json"]["detail"][0]["ctx"]["expected"],
        "'bpm' or 'replaygain'"
    );
    assert_eq!(AnalysisKind::parse("bpm"), Some(AnalysisKind::Bpm));
    assert_eq!(
        AnalysisKind::parse("replaygain"),
        Some(AnalysisKind::ReplayGain)
    );
    assert_eq!(AnalysisKind::parse("loudness"), None);
}
