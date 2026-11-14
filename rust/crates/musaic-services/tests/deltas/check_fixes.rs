//! Enumerated deliberate changes against
//! testdata/golden/services/check_fixes.json shared by the service-level
//! replay (musaic-services tests/golden_check_fixes.rs) and the HTTP replay
//! (musaic-api tests/check_fixes_http.rs).
#![allow(dead_code)]

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
