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

/// The Python app's quick-fix-all SSE text with the verified count
/// (deliberate change): `done` also carries `still_open`, the
/// findings whose fix reported success but which a rerun of the check over
/// the touched albums still finds, and `fixed` no longer counts them. The
/// recorded `done` frame gains `"still_open": <still_open>` and its `fixed`
/// drops by that much; every other frame is unchanged.
pub fn done_with_still_open(sse: &str, still_open: i64) -> String {
    let mut out = String::new();
    let mut changed = 0;
    for frame in sse.split_inclusive("\n\n") {
        let Some(data) = frame
            .strip_prefix("event: done\ndata: ")
            .and_then(|d| d.strip_suffix("}\n\n"))
        else {
            out.push_str(frame);
            continue;
        };
        let (head, rest) = data.split_once("\"fixed\": ").unwrap();
        let (n, tail) = rest.split_once(',').unwrap();
        let fixed: i64 = n.parse().unwrap();
        assert!(fixed >= still_open, "{frame}");
        out.push_str(&format!(
            "event: done\ndata: {head}\"fixed\": {},{tail}, \"still_open\": {still_open}}}\n\n",
            fixed - still_open
        ));
        changed += 1;
    }
    assert_eq!(changed, 1, "one done frame: {sse}");
    out
}

/// quick-fix-all steps whose `done` frame reports findings still open after
/// a fix that reported success ([`done_with_still_open`]); every other
/// quick-fix-all step reports 0. "quick-fix-all artist-sort": the
/// propagation writes ALBUMARTISTSORT on The Kilo - Twelve, whose track
/// (ARTIST "The Kilo", no ARTISTSORT) then differs from its album credit and
/// joins the cluster as a track-level member, so the cluster is still
/// propagatable and the Python app's own sweep after the step left the
/// finding open too (its record's state).
/// "quick-fix-all tags-out-of-sync": both fixes write the canonical value
/// MusicBrainz has (a sort tag, a title) but the albums still lack a genre,
/// so both findings stay open under a shorter summary (the case the
/// change is about: the Python app reported "fixed 2").
/// "quick-fix-all unidentified": November is anchored (from the resolver's
/// cache) but fingerprinting fails without an AcoustID key, so its "Not yet
/// fingerprinted" finding stays open.
pub const STILL_OPEN: &[(&str, i64)] = &[
    ("quick-fix-all artist-sort", 1),
    ("quick-fix-all tags-out-of-sync", 2),
    ("quick-fix-all unidentified", 1),
];

/// [`STILL_OPEN`]'s count for a step label (0 when not listed).
pub fn still_open_for(label: &str) -> i64 {
    STILL_OPEN
        .iter()
        .find(|(l, _)| *l == label)
        .map_or(0, |(_, n)| *n)
}

/// A state's `open_findings` after quick-fix-all's rerun of its check
/// (deliberate change; the Python app left the findings as they were until
/// the check's next run): `resolved` rows ((check, album, summary) as the
/// state lists them) are removed, `opened` rows (a partly fixed album's
/// finding under its new summary) added when not already listed.
pub fn rerun_open_findings(state: &mut Value, resolved: &[Value], opened: &[Value]) {
    if let Some(Value::Array(open)) = state.get_mut("open_findings") {
        open.retain(|r| !resolved.contains(r));
        for r in opened {
            if !open.contains(r) {
                open.push(r.clone());
            }
        }
    }
}
