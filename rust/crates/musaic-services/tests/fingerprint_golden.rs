//! `fingerprint_album` and `best_recording` against
//! testdata/golden/fingerprint/{album,best_recording}.json, recorded by
//! a generator that ran the Python app's own
//! `musaic.fingerprint.fingerprint_album` / `_best_recording` against
//! recorded AcoustID answers.
//!
//! Per call, compared with the Python app's record: the result (or the error code
//! and text), the bus events, every AcoustID request (fingerprint, duration,
//! form fields, how replay served it, in order) and the album's
//! `track_fingerprints` rows afterwards. The same fingerprints go in (the
//! Python app's fpcalc output, see `fp_common`), the same recorded answers come
//! back.
//!
//! Differences are allowed only as enumerated deltas ([`Delta`]: scenario,
//! JSON path, old -> new, decision), each checked to hold the stated Python app
//! value first; an unlisted difference fails the comparison and an unused
//! delta fails the test:
//! - every request: `http://` -> `https://` and form `meta`
//!   `recordings` -> `recordings releasegroups releases tracks compress`.
//!   The recordings of both shapes exist for every
//!   fingerprint, and best_recording.json shows the Python app's own
//!   `_best_recording` picks the same recording from either.
//! - "no AcoustID key": no `no-api-key` error; the files
//!   are fingerprinted and stored with no recording and announced. The next
//!   step, "key restored", then matches the Python app exactly: the stored
//!   fingerprints have no lookup outcome yet, so they are looked up (the
//!   Python app's requests, rows and event), without fingerprinting again
//!   (checked on the fingerprinter's calls).
//! - "force, every lookup fails": the stored recordings are kept
//!   (the Python app overwrote them with NULL); the new value is the Python app's own
//!   previous record of the same rows.
//! - "file removed": the row of the deleted file is gone.
//! - "after failures (cached)": the three lookups that did not
//!   complete (connection error, HTTP 503, HTTP 400) are asked again and
//!   announced; the completed "no match" (a result below the floor) is not.
//!   New values: the Python app's own requests and event for those files, from
//!   the next step ("after failures, force", which asked every file).
//! - "corrupt file again": the rejected file is remembered, so the
//!   call is a cache hit: no request, no event.
//! - "only corrupt file": the row of the deleted good file is gone.

mod fp_common;

use std::fs;

use fp_common::*;
use musaic_db::LOOKUP_DONE;
use musaic_net::{ACOUSTID_META, AcoustIdClient};
use musaic_resolve::LookupResponse;
use musaic_services::fingerprint::{
    AcoustIdLookup, FingerprintEnv, HIGH_CONFIDENCE_SCORE, best_recording, fingerprint_album,
};
use serde_json::{Value, json};

const ROW_TS: &str = "2001-01-01T00:00:00Z";

/// The deliberate request-shape changes applied to one
/// Python app request, after checking the Python app's values.
fn python_request_as_built(r: &Value) -> Value {
    let mut r = r.clone();
    assert_eq!(
        r["url"], "http://api.acoustid.org/v2/lookup",
        "http old value"
    );
    r["url"] = json!("https://api.acoustid.org/v2/lookup");
    assert_eq!(r["form"]["meta"], "recordings", "meta old value");
    r["form"]["meta"] = json!(ACOUSTID_META);
    let o = r.as_object_mut().unwrap();
    o.remove("key"); // the replay key covers the shape changed above
    r
}

fn result_json(
    lib: &Lib,
    out: &Result<
        musaic_services::fingerprint::AlbumFingerprints,
        musaic_services::fingerprint::FingerprintError,
    >,
) -> (Option<Value>, Option<Value>) {
    match out {
        Ok(a) => (
            Some(Value::Array(
                a.tracks
                    .iter()
                    .map(|t| {
                        json!({
                            "filename": t.filename,
                            "fingerprint_sha256": sha256(&t.fingerprint),
                            "duration_s": t.duration_s,
                            "acoustid_recording_mbid": t.acoustid_recording_mbid,
                            "acoustid_score": t.acoustid_score,
                        })
                    })
                    .collect(),
            )),
            None,
        ),
        Err(e) => (
            None,
            Some(json!({"code": e.code(), "detail": lib.norm(&e.to_string())})),
        ),
    }
}

struct Runner<'a> {
    lib: &'a Lib,
    fps: &'a GoldenFingerprints,
    net: &'a TestNet,
    key: bool,
}

impl Runner<'_> {
    /// One Python app call record -> this build's record of the same call.
    fn call(&self, step: &Value) -> Value {
        let conn = self.lib.conn();
        let (bus, events) = recording_bus();
        let client = AcoustIdClient::new(self.net, "golden-test-key");
        let env = FingerprintEnv {
            conn: &conn,
            fingerprinter: self.fps,
            acoustid: self.key.then_some(&client as &dyn AcoustIdLookup),
            bus: &bus,
            now_iso: ROW_TS,
            cancel: None,
        };
        let album_id = self.lib.real(step["album"].as_str().unwrap());
        let path = self.lib.real(step["path"].as_str().unwrap());
        let out = fingerprint_album(&env, &album_id, &path, step["force"].as_bool().unwrap());
        let (result, error) = result_json(self.lib, &out);
        let (rows, impls) = album_rows(&conn, &album_id);
        assert!(
            impls.iter().all(|i| i == musaic_db::FINGERPRINT_IMPL_RUST),
            "every row this build writes is marked {}",
            musaic_db::FINGERPRINT_IMPL_RUST
        );
        let mut rec = json!({
            "events": events.lock().unwrap().iter().map(|e| self.lib.norm_value(&event_json(e))).collect::<Vec<_>>(),
            "requests": self.net.take_log(),
            "rows_after": rows,
        });
        if let Some(r) = result {
            rec["result"] = r;
        }
        if let Some(e) = error {
            rec["error"] = e;
        }
        rec
    }
}

/// The Python app's record of one call, with the request deltas applied.
fn expected(step: &Value) -> Value {
    let mut e = json!({
        "events": step["events"],
        "requests": step["requests"].as_array().unwrap().iter().map(python_request_as_built).collect::<Vec<_>>(),
        "rows_after": step["rows_after"],
    });
    for k in ["result", "error"] {
        if !step[k].is_null() {
            e[k] = step[k].clone();
        }
    }
    e
}

// Every album of the synthetic and the real fixture library, first call
// (computed, looked up, stored, announced) and second call (cache hit:
// nothing asked, nothing announced), plus the real library's forced
// refreshes.
#[test]
fn every_library_call_matches_the_python_app() {
    musaic_testdata::require_private!("replay");
    let g = album_golden();
    let libraries = g["libraries"].as_array().unwrap();
    assert!(!libraries.is_empty());
    let mut compared = 0;
    for l in libraries {
        let name = l["library"].as_str().unwrap();
        let lib = Lib::build(&l["albums"], &l["fpcalc_files"]);
        lib.insert_albums(&lib.conn(), &l["albums"]);
        let fps = GoldenFingerprints::new(&lib.root, &l["fpcalc_files"]);
        let net = TestNet::default();
        let run = Runner {
            lib: &lib,
            fps: &fps,
            net: &net,
            key: true,
        };
        let mut previous_rows: std::collections::HashMap<String, Value> = Default::default();
        let mut deltas: Vec<Delta> = Vec::new();
        for step in l["calls"].as_array().unwrap() {
            let label = step["label"].as_str().unwrap();
            let ctx = format!("{name}: {} / {label}", step["album"]);
            if let Some(injected) = step["injected"].as_str() {
                assert_eq!(injected, "connection-error for every file");
                for r in step["requests"].as_array().unwrap() {
                    net.inject.lock().unwrap().insert(
                        r["fingerprint_sha256"].as_str().unwrap().to_owned(),
                        "connection-error".into(),
                    );
                }
            }
            if label == "force, every lookup fails" {
                // The Python app stored NULL over the matches; this
                // build keeps them until a lookup completes. New value: the
                // Python app's own record of the rows before this call, with
                // this call's timestamp (the same fixed one).
                let before = &previous_rows[step["album"].as_str().unwrap()];
                let keep = |old: &Value| {
                    let mut new = old.clone();
                    for (i, row) in new.as_array_mut().unwrap().iter_mut().enumerate() {
                        row["acoustid_recording_mbid"] =
                            before[i]["acoustid_recording_mbid"].clone();
                        row["acoustid_score"] = before[i]["acoustid_score"].clone();
                    }
                    new
                };
                for path in ["/rows_after", "/result"] {
                    let old = step[&path[1..]].clone();
                    deltas.push(Delta::new(
                        ctx.clone(),
                        path,
                        Some(old.clone()),
                        Some(keep(&old)),
                        "recomputed-fingerprints",
                    ));
                }
            }
            let got = run.call(step);
            net.inject.lock().unwrap().clear();
            let mut want = expected(step);
            let mut d = Deltas::new(std::mem::take(&mut deltas));
            d.apply(&ctx, &mut want);
            d.assert_all_used();
            assert_eq!(got, want, "{ctx}");
            previous_rows.insert(
                step["album"].as_str().unwrap().to_owned(),
                step["rows_after"].clone(),
            );
            compared += 1;
        }
    }
    // 13 synthetic + 5 real albums, two calls each, plus two forced calls.
    assert_eq!(compared, if libraries.len() == 2 { 38 } else { 26 });
}

/// The deliberate differences from the Python app's edge steps (see the module
/// documentation), built from the Python app's own records.
fn edge_deltas(e: &Value) -> Vec<Delta> {
    let steps = e["steps"].as_array().unwrap();
    let step = |label: &str| steps.iter().find(|s| s["label"] == label).unwrap();
    let ctx = |s: &Value| format!("{} / {}", s["album"], s["label"].as_str().unwrap());
    let mut d = Vec::new();

    // No key is not an error; the files are stored
    // without a recording and announced. Values from the Python app's record of
    // the next step, which computed the same files.
    let nokey = step("no AcoustID key");
    let next = step("key restored");
    assert!(
        next["result"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["acoustid_recording_mbid"].is_null())
    );
    d.push(Delta::new(
        ctx(nokey),
        "/error",
        Some(nokey["error"].clone()),
        None,
        "no-key-not-an-error",
    ));
    d.push(Delta::new(
        ctx(nokey),
        "/result",
        None,
        Some(next["result"].clone()),
        "no-key-not-an-error",
    ));
    for k in ["rows_after", "events"] {
        d.push(Delta::new(
            ctx(nokey),
            &format!("/{k}"),
            Some(nokey[k].clone()),
            Some(next[k].clone()),
            "no-key-not-an-error",
        ));
    }

    // Rows of deleted files are deleted on recompute.
    let without = |rows: &Value, file: &str| {
        Value::Array(
            rows.as_array()
                .unwrap()
                .iter()
                .filter(|r| r["filename"] != file)
                .cloned()
                .collect(),
        )
    };
    for (label, file) in [
        ("file removed", "99 - Added Copy.flac"),
        ("only corrupt file", "01 - Good.flac"),
    ] {
        let s = step(label);
        d.push(Delta::new(
            ctx(s),
            "/rows_after",
            Some(s["rows_after"].clone()),
            Some(without(&s["rows_after"], file)),
            "rejected-files",
        ));
    }

    // The rejected file is remembered; the next call is a cache hit.
    let again = step("corrupt file again");
    for k in ["requests", "events"] {
        let old: Vec<Value> = match k {
            "requests" => again[k]
                .as_array()
                .unwrap()
                .iter()
                .map(python_request_as_built)
                .collect(),
            _ => again[k].as_array().unwrap().clone(),
        };
        d.push(Delta::new(
            ctx(again),
            &format!("/{k}"),
            Some(json!(old)),
            Some(json!([])),
            "rejected-files",
        ));
    }

    // The lookups that did not complete are asked again on the next
    // call; the completed no-match is not. The Python app's requests and event
    // for those files come from its forced step right after.
    let failed = step("lookup failures");
    let cached = step("after failures (cached)");
    let forced = step("after failures, force");
    let retried: Vec<String> = failed["injected"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(_, kind)| *kind != "ok-below-floor")
        .map(|(f, _)| f.clone())
        .collect();
    assert_eq!(retried.len(), 3);
    let fp_of = |f: &str| {
        e["fpcalc_files"][format!("Mono Harbor - Gaps [FLAC]/{f}")]["fingerprint_sha256"].clone()
    };
    let retried_prints: Vec<Value> = retried.iter().map(|f| fp_of(f)).collect();
    let new_requests: Vec<Value> = forced["requests"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| retried_prints.contains(&r["fingerprint_sha256"]))
        .map(python_request_as_built)
        .collect();
    assert_eq!(new_requests.len(), 3);
    let mut event = forced["events"][0].clone();
    event["filenames"] = json!(
        event["filenames"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| retried.iter().any(|r| *f == r.as_str()))
            .cloned()
            .collect::<Vec<_>>()
    );
    d.push(Delta::new(
        ctx(cached),
        "/requests",
        Some(json!([])),
        Some(json!(new_requests)),
        "lookup-retry",
    ));
    d.push(Delta::new(
        ctx(cached),
        "/events",
        Some(json!([])),
        Some(json!([event])),
        "lookup-retry",
    ));
    d
}

// The edge scenarios, in order on one
// library: force; a file added then removed; four kinds of failed lookup,
// then the call after them (failures asked again, no-match not)
// and a forced one; a file the fingerprinter rejects, twice (remembered);
// only rejected files; `._` files; no FLAC; a missing directory; no
// AcoustID key, then the key restored (stored fingerprints looked up
// without fingerprinting again).
#[test]
fn edge_scenarios_match_the_python_app() {
    musaic_testdata::require_private!("replay");
    let e = &album_golden()["edges"];
    let lib = Lib::build(&e["albums"], &e["fpcalc_files"]);
    lib.insert_albums(&lib.conn(), &e["albums"]);
    let fps = GoldenFingerprints::new(&lib.root, &e["fpcalc_files"]);
    let net = TestNet::default();
    let mut run = Runner {
        lib: &lib,
        fps: &fps,
        net: &net,
        key: true,
    };
    let second = "Aurora Fields - Second Light [FLAC]";
    let corrupt = "Corrupt Band - Half Broken [FLAC]";
    let mut deltas = Deltas::new(edge_deltas(e));
    for step in e["steps"].as_array().unwrap() {
        let label = step["label"].as_str().unwrap();
        let change = step["change"].as_str().unwrap();
        let ctx = format!("{} / {label}", step["album"]);
        // Reproduce the generator's change before the call.
        match label {
            "file added" => {
                let src = change
                    .strip_prefix("copied ")
                    .and_then(|c| c.strip_suffix(" to 99 - Added Copy.flac"))
                    .unwrap();
                fs::write(lib.root.join(second).join("99 - Added Copy.flac"), b"").unwrap();
                fps.alias(
                    &format!("{second}/99 - Added Copy.flac"),
                    &format!("{second}/{src}"),
                );
            }
            "file removed" => {
                fs::remove_file(lib.root.join(second).join("99 - Added Copy.flac")).unwrap()
            }
            "lookup failures" => {
                let album = "Mono Harbor - Gaps [FLAC]";
                for (file, kind) in step["injected"].as_object().unwrap() {
                    let sha = e["fpcalc_files"][format!("{album}/{file}")]["fingerprint_sha256"]
                        .as_str()
                        .unwrap();
                    net.inject
                        .lock()
                        .unwrap()
                        .insert(sha.to_owned(), kind.as_str().unwrap().to_owned());
                }
            }
            "after failures (cached)" => net.inject.lock().unwrap().clear(),
            "only corrupt file" => {
                fs::remove_file(lib.root.join(corrupt).join("01 - Good.flac")).unwrap()
            }
            "no flac files" => {
                fs::create_dir(lib.root.join("Empty Dir")).unwrap();
                fs::write(lib.root.join("Empty Dir/notes.txt"), b"no audio\n").unwrap();
            }
            "no AcoustID key" => run.key = false,
            "key restored" => run.key = true,
            _ => assert!(
                change == "none"
                    || change == "none (directory holds ._ files)"
                    || change == "injections removed"
                    || change == "path does not exist"
                    || change.starts_with("02 - Bad.flac"),
                "{ctx}: unhandled change {change:?}"
            ),
        }
        let calls_before = fps.calls.lock().unwrap().len();
        let got = run.call(step);
        let mut want = expected(step);
        deltas.apply(&ctx, &mut want);
        assert_eq!(got, want, "{ctx}");

        let album = lib.real(step["album"].as_str().unwrap());
        let states = lookup_states(&lib.conn(), &album);
        let state_of = |f: &str| states.iter().find(|s| s.0 == f).unwrap().clone();
        match label {
            // Three lookups failed, one completed with no match.
            "lookup failures" => {
                for (f, kind) in step["injected"].as_object().unwrap() {
                    let s = state_of(f);
                    if kind == "ok-below-floor" {
                        assert_eq!((s.1.as_deref(), s.2), (Some(LOOKUP_DONE), 0), "{ctx}: {f}");
                    } else {
                        assert_eq!(
                            (s.1.as_deref(), s.2, s.3.as_deref()),
                            (Some(musaic_db::LOOKUP_FAILED), 1, Some(ROW_TS)),
                            "{ctx}: {f}: due again at once after a first failure"
                        );
                    }
                }
            }
            "after failures (cached)" | "key restored" => {
                assert!(
                    states
                        .iter()
                        .all(|s| s.1.as_deref() == Some(LOOKUP_DONE) && s.2 == 0)
                );
                // Looked up from the stored prints: nothing fingerprinted again.
                assert_eq!(fps.calls.lock().unwrap().len(), calls_before, "{ctx}");
            }
            "no AcoustID key" => {
                assert!(
                    states.iter().all(|s| s.1.is_none()),
                    "{ctx}: no outcome yet"
                );
            }
            "corrupt file" | "corrupt file again" => {
                assert_eq!(
                    reject_names(&lib.conn(), &album),
                    ["02 - Bad.flac"],
                    "{ctx}"
                );
            }
            _ => {}
        }
    }
    deltas.assert_all_used();
}

// `_best_recording` over constructed answers and over every
// recorded AcoustID answer in the recorded replay set (private), both request shapes.
#[test]
fn best_recording_matches_the_python_app() {
    let g = golden("best_recording.json");
    assert_eq!(g["floor"].as_f64(), Some(HIGH_CONFIDENCE_SCORE));
    let best_json = |b: Option<(String, f64)>| match b {
        Some((m, s)) => json!({"mbid": m, "score": s}),
        None => Value::Null,
    };
    for c in g["constructed"].as_array().unwrap() {
        let body = serde_json::to_vec(&c["body"]).unwrap();
        let parsed = LookupResponse::from_json(&body).unwrap();
        assert_eq!(
            best_json(best_recording(&parsed.results)),
            c["best"],
            "{}",
            c["name"]
        );
    }
    // The recorded answers (and their cases, the private half) are private.
    if !musaic_testdata::private_available(&["replay", "golden/fingerprint/best_recording.json"]) {
        musaic_testdata::skip_private_cases();
        return;
    }
    let replay = musaic_testdata::private_required("replay");
    let mut by_print: std::collections::BTreeMap<(String, String), Vec<Value>> = Default::default();
    for r in g["recorded"].as_array().unwrap() {
        let rec: Value = serde_json::from_str(
            &fs::read_to_string(replay.join(format!("{}.json", r["key"].as_str().unwrap())))
                .unwrap(),
        )
        .unwrap();
        use base64::Engine as _;
        let body = base64::engine::general_purpose::STANDARD
            .decode(rec["content_b64"].as_str().unwrap())
            .unwrap();
        let parsed = LookupResponse::from_json(&body).unwrap();
        assert_eq!(
            best_json(best_recording(&parsed.results)),
            r["best"],
            "{}",
            r["key"]
        );
        by_print
            .entry((
                r["fingerprint_sha256"].as_str().unwrap().into(),
                r["duration"].as_str().unwrap().into(),
            ))
            .or_default()
            .push(r["best"].clone());
    }
    // The Python app's answer with meta=recordings and this build's with
    // release-group metadata give the same best recording for every
    // recorded fingerprint.
    assert_eq!(by_print.len(), 93);
    for (k, v) in &by_print {
        assert_eq!(v.len(), 2, "{k:?}: both shapes recorded");
        assert_eq!(v[0], v[1], "{k:?}");
    }
}

// A rejected file is remembered and not decoded again while it is
// unchanged; once it changes (here rewritten, and now fingerprinting like
// the good file) the next call recomputes the album with it, and it is no
// longer remembered. A remembered reject from another fingerprinter does
// not count. Requests and rows: the Python app's record of the good file.
#[test]
fn a_rejected_file_is_picked_up_once_it_changes() {
    musaic_testdata::require_private!("replay");
    let e = &album_golden()["edges"];
    let lib = Lib::build(&e["albums"], &e["fpcalc_files"]);
    lib.insert_albums(&lib.conn(), &e["albums"]);
    let fps = GoldenFingerprints::new(&lib.root, &e["fpcalc_files"]);
    let net = TestNet::default();
    let run = Runner {
        lib: &lib,
        fps: &fps,
        net: &net,
        key: true,
    };
    let steps = e["steps"].as_array().unwrap();
    let first = steps.iter().find(|s| s["label"] == "corrupt file").unwrap();
    let album = lib.real(first["album"].as_str().unwrap());
    let corrupt = "Corrupt Band - Half Broken [FLAC]";
    let bad = format!("{corrupt}/02 - Bad.flac");
    let bad_calls = || {
        fps.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| **c == bad)
            .count()
    };

    assert_eq!(run.call(first), expected(first));
    assert_eq!(bad_calls(), 1);
    assert_eq!(reject_names(&lib.conn(), &album), ["02 - Bad.flac"]);
    let again = run.call(first);
    assert_eq!(again["requests"], json!([]));
    assert_eq!(bad_calls(), 1, "unchanged: not decoded again");

    // Rewritten: it now fingerprints like the good file.
    fs::write(lib.root.join(&bad), b"now a readable file").unwrap();
    fps.alias(&bad, &format!("{corrupt}/01 - Good.flac"));
    let got = run.call(first);
    assert_eq!(bad_calls(), 2);
    assert!(reject_names(&lib.conn(), &album).is_empty());
    let want_req = python_request_as_built(&first["requests"][0]);
    assert_eq!(got["requests"], json!([want_req, want_req]));
    let good = &first["rows_after"][0];
    let mut bad_row = good.clone();
    bad_row["filename"] = json!("02 - Bad.flac");
    assert_eq!(got["rows_after"], json!([good, bad_row]));

    // Broken again: the cache rule (filenames) does not see a changed file,
    // as in the Python app; a forced call does, rejects it and drops its old
    // row, after which unforced calls are cache hits again.
    fps.files.lock().unwrap().insert(bad.clone(), None);
    fs::write(lib.root.join(&bad), b"broken again").unwrap();
    let mut forced = first.clone();
    forced["force"] = json!(true);
    let got = run.call(&forced);
    assert_eq!(bad_calls(), 3);
    assert_eq!(reject_names(&lib.conn(), &album), ["02 - Bad.flac"]);
    assert_eq!(got["rows_after"], first["rows_after"]);
    let got = run.call(first);
    assert_eq!(got["requests"], json!([]));
    assert_eq!(bad_calls(), 3);
    lib.conn()
        .execute(
            "UPDATE fingerprint_rejects SET fingerprint_impl = 'fpcalc'",
            [],
        )
        .unwrap();
    run.call(first);
    assert_eq!(
        bad_calls(),
        4,
        "another fingerprinter's reject is tried again"
    );
}

// Negative: rows are deleted only for files a working listing no
// longer shows. A directory that cannot be listed deletes nothing.
#[cfg(unix)]
#[test]
fn an_unlistable_directory_deletes_no_rows() {
    musaic_testdata::require_private!("replay");
    use std::os::unix::fs::PermissionsExt;
    let e = &album_golden()["edges"];
    let lib = Lib::build(&e["albums"], &e["fpcalc_files"]);
    lib.insert_albums(&lib.conn(), &e["albums"]);
    let fps = GoldenFingerprints::new(&lib.root, &e["fpcalc_files"]);
    let net = TestNet::default();
    let run = Runner {
        lib: &lib,
        fps: &fps,
        net: &net,
        key: true,
    };
    let first = &e["steps"][0];
    let album = lib.real(first["album"].as_str().unwrap());
    let dir = lib.root.join("Aurora Fields - First Light [FLAC]");
    run.call(first);
    let rows = album_rows(&lib.conn(), &album).0;
    assert_eq!(rows.len(), 5);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o000)).unwrap();
    let listable = fs::read_dir(&dir).is_ok(); // root can list anyway
    let got = run.call(first);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    if !listable {
        assert_eq!(got["rows_after"], json!(rows));
    }
}

// Negative: a fingerprinter whose refusals are not about the file
// (one that cannot run, as the app's "unavailable" stand-in) remembers no
// reject and keeps the rows the files already have.
#[test]
fn a_refusing_fingerprinter_keeps_rows_and_remembers_nothing() {
    musaic_testdata::require_private!("replay");
    struct Refuse;
    impl musaic_services::fingerprint::Fingerprinter for Refuse {
        fn fingerprint(&self, _p: &std::path::Path) -> Result<(String, u64), String> {
            Err("cannot run".into())
        }
        fn implementation(&self) -> &str {
            musaic_db::FINGERPRINT_IMPL_RUST
        }
    }
    let e = &album_golden()["edges"];
    let lib = Lib::build(&e["albums"], &e["fpcalc_files"]);
    lib.insert_albums(&lib.conn(), &e["albums"]);
    let fps = GoldenFingerprints::new(&lib.root, &e["fpcalc_files"]);
    let net = TestNet::default();
    let run = Runner {
        lib: &lib,
        fps: &fps,
        net: &net,
        key: true,
    };
    let first = &e["steps"][0];
    let album = lib.real(first["album"].as_str().unwrap());
    let path = lib.real(first["path"].as_str().unwrap());
    run.call(first);
    let conn = lib.conn();
    let before = album_rows(&conn, &album).0;
    assert_eq!(before.len(), 5);
    let (bus, _) = recording_bus();
    let env = FingerprintEnv {
        conn: &conn,
        fingerprinter: &Refuse,
        acoustid: None,
        bus: &bus,
        now_iso: ROW_TS,
        cancel: None,
    };
    let out = fingerprint_album(&env, &album, &path, true);
    assert_eq!(out.unwrap_err().code(), "no-fingerprints");
    assert_eq!(album_rows(&conn, &album).0, before);
    assert!(reject_names(&conn, &album).is_empty());
}
