//! Enumerated deliberate changes against
//! testdata/golden/services/check_fixes.json shared by the service-level
//! replay (musaic-services tests/golden_check_fixes.rs) and the HTTP replay
//! (musaic-api tests/check_fixes_http.rs).
#![allow(dead_code)]

use rusqlite::Connection;
use serde_json::{Value, json};

/// The step the kept-row change applies to: deleting the last copy leaves
/// the album without FLACs. (Item names are shared with musaic-api's
/// tests/check_fixes_http.rs.)
pub const LAST_COPY_STEP: &str = "delete last copy (album empties)";

/// The Python app's record of
/// [`LAST_COPY_STEP`] with the deliberate change applied. The rescan found no
/// FLAC left in Juliet's (reachable) directory: the album row is kept and
/// marked Missing (`missing_since` = the clock, `<NOW>`; nothing else of the
/// row changes), `AlbumMarkedMissing` is published instead of
/// `AlbumDeleted`, its track tags and the fingerprint of the file that was
/// unlinked out of band survive (the Python app's previous state); only the
/// trashed file's own fingerprint row goes, as in the Python app. Body, tree,
/// trash and findings are the Python app's. Each replaced part differs from the
/// Python app's (checked).
pub fn last_copy_expected(step: &Value, prev_state: &Value) -> Value {
    assert_eq!(step["label"], LAST_COPY_STEP);
    let album = "<ID:Juliet - Ten [FLAC]>";
    let trashed = step["args"]["filename"].as_str().unwrap();
    let mut want = step.clone();
    let mut set = |path: &str, new: Value| {
        let slot = want.pointer_mut(path).unwrap();
        assert_ne!(*slot, new, "kept row {path}: delta changes nothing");
        *slot = new;
    };
    set(
        "/events",
        json!([{"type": "AlbumMarkedMissing", "album_id": album, "missing_since": "<NOW>"}]),
    );
    let mut albums = prev_state["albums"].clone();
    for r in albums.as_array_mut().unwrap() {
        if r[0] == json!(album) {
            assert!(r[5].is_null(), "Juliet was Present before the step");
            r[5] = json!("<NOW>");
        }
    }
    set("/state/albums", albums);
    set("/state/track_tags", prev_state["track_tags"].clone());
    let fps: Vec<Value> = prev_state["track_fingerprints"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| !(r[0] == json!(album) && r[1] == json!(trashed)))
        .cloned()
        .collect();
    assert!(
        fps.iter().any(|r| r[0] == json!(album)),
        "a fingerprint of Juliet survives"
    );
    set("/state/track_fingerprints", json!(fps));
    want
}

/// The advice dropped from the unidentified-album skip message when no
/// model backend exists (the Python app's `_try_resolver` text ends with it).
pub const PYTHON_AI_ADVICE: &str = ", or use the per-finding Fix to escalate to AI";

/// The Python app's quick-fix-all SSE text with the deliberate change
/// applied.
/// Without a model backend the per-finding Fix answers ok:false (not
/// available) instead of queuing an AI job, so every skip message that ends
/// with [`PYTHON_AI_ADVICE`] loses it; nothing else changes. Returns the
/// text and how many messages changed. Every changed message decodes to a
/// text ending with `want_tail` (the Rust constant), so the delta is pinned
/// to the Python app's own words less the advice.
pub fn no_model_skip_message(sse: &str, want_tail: &str) -> (String, usize) {
    let quoted = format!("{PYTHON_AI_ADVICE}\"");
    let n = sse.matches(&quoted).count();
    let out = sse.replace(&quoted, "\"");
    let mut checked = 0;
    for line in out.lines().filter_map(|l| l.strip_prefix("data: ")) {
        let v: Value = serde_json::from_str(line).unwrap();
        if let Some(m) = v["message"].as_str() {
            assert!(!m.contains("escalate to AI"), "no AI advice: {m}");
            if m.contains("couldn't identify the album") {
                assert!(m.ends_with(want_tail), "no AI advice: {m}");
                checked += 1;
            }
        }
    }
    assert_eq!(checked, n, "every changed message is a skip message");
    (out, n)
}

/// The Python app's quick-fix-all SSE text in this build's finding order.
/// quick-fix-all walks the open findings by id, and finding ids follow the
/// order the album rows were stored in. The Python app stored them in the
/// filesystem's listing order (APFS's, for this golden); this build stores
/// them in name order on every filesystem (a deliberate change), so the same
/// findings arrive in a different order. `ours` maps a Python app finding
/// id to this build's id for the same (check, album, summary); the progress
/// frames are put in that order, in the slots they occupied. Every frame,
/// its fields and the closing counts stay the Python app's.
pub fn progress_in_our_finding_order(sse: &str, ours: impl Fn(i64) -> i64) -> String {
    let frames: Vec<&str> = sse.split_inclusive("\n\n").collect();
    let fid = |f: &str| -> i64 {
        let data = f.lines().find_map(|l| l.strip_prefix("data: ")).unwrap();
        let v: Value = serde_json::from_str(data).unwrap();
        v["finding_id"].as_i64().unwrap()
    };
    let slots: Vec<usize> = (0..frames.len())
        .filter(|&i| frames[i].starts_with("event: progress\n"))
        .collect();
    let mut progress: Vec<&str> = slots.iter().map(|&i| frames[i]).collect();
    progress.sort_by_key(|f| ours(fid(f)));
    let mut out: Vec<&str> = frames.clone();
    for (&i, f) in slots.iter().zip(progress) {
        out[i] = f;
    }
    out.concat()
}

/// The order quick-fix-all walks a check's open findings
/// (`list_findings`: `last_seen_at DESC, id ASC`), read with an independent
/// query before the run. The Python app's record has the same rule, but its
/// finding ids follow its album rows, which it stored in APFS listing order
/// (this build: name order), so the recorded frames are compared in this
/// order ([`progress_in_our_finding_order`]).
pub fn walk_order(conn: &Connection, check: &str) -> Vec<i64> {
    let mut st = conn
        .prepare(
            "SELECT id FROM check_findings WHERE check_id = ?1 AND resolved_at IS NULL \
             AND ignored_at IS NULL ORDER BY last_seen_at DESC, id ASC",
        )
        .unwrap();
    st.query_map([check], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// Position of one of our finding ids in [`walk_order`] (unknown ids last).
pub fn walk_pos(order: &[i64], ours: i64) -> i64 {
    order
        .iter()
        .position(|&o| o == ours)
        .map_or(i64::MAX, |i| i as i64)
}

/// The recorded bus events of a quick-fix-all in [`walk_order`]: the events
/// that belong to one of the check's findings (`finding` maps an event to
/// our finding id) are put in that finding order, in the slots they
/// occupied; the others stay where they are.
pub fn events_in_walk_order(
    events: &mut Value,
    order: &[i64],
    finding: impl Fn(&Value) -> Option<i64>,
) {
    let evs = events.as_array_mut().unwrap();
    let slots: Vec<usize> = (0..evs.len())
        .filter(|&i| finding(&evs[i]).is_some())
        .collect();
    let mut moved: Vec<Value> = slots.iter().map(|&i| evs[i].clone()).collect();
    moved.sort_by_key(|e| walk_pos(order, finding(e).unwrap()));
    for (&i, e) in slots.iter().zip(moved) {
        evs[i] = e;
    }
}
