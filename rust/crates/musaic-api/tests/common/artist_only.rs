//! Deliberate change: an album with ARTIST but no ALBUMARTIST is an
//! album-level member of its inconsistent-artist-sort cluster. In the
//! synthetic fixture library that is Mono Harbor - Gaps [FLAC] (ARTIST "Mono
//! Harbor", no ALBUMARTIST, no ALBUMARTISTSORT), so the recorded Mono Harbor
//! cluster gains an album-level member for Gaps (sort "", placed before its
//! track-level member, the order the check emits them in) and its summary
//! reads "4 album-level" instead of "3 album-level".

use serde_json::{Value, json};

pub const OLD_SUMMARY: &str = "3 album-level + 1 track-level (4 tracks) for 'Mono Harbor'";
pub const NEW_SUMMARY: &str = "4 album-level + 1 track-level (4 tracks) for 'Mono Harbor'";
const GAPS_DIR: &str = "Mono Harbor - Gaps [FLAC]";

/// Insert Gaps' album-level member into a cluster's `members`; `true` when
/// inserted (the list had Gaps' track-level member and no album-level one).
fn add_member(members: &mut Vec<Value>) -> bool {
    let is_gaps = |m: &Value, kind: &str| m["kind"] == kind && m["dir_name"] == GAPS_DIR;
    if members.iter().any(|m| is_gaps(m, "album")) {
        return false;
    }
    let Some(i) = members.iter().position(|m| is_gaps(m, "track")) else {
        return false;
    };
    let album_id = members[i]["album_id"].clone();
    members.insert(
        i,
        json!({"kind": "album", "album_id": album_id, "dir_name": GAPS_DIR,
               "album_artist": "Mono Harbor", "sort": ""}),
    );
    true
}

/// Apply the change everywhere in `v`: every string equal to the old summary,
/// and the `members` of every finding `detail` (an object) or `detail_json`
/// (Python `json.dumps` text) whose summary is the Mono Harbor one. Returns how
/// many places changed.
pub fn apply(v: &mut Value) -> usize {
    match v {
        Value::String(s) if s == OLD_SUMMARY => {
            *s = NEW_SUMMARY.to_string();
            1
        }
        Value::Array(a) => a.iter_mut().map(apply).sum(),
        Value::Object(o) => {
            let mut n = 0;
            if o.get("summary").and_then(Value::as_str) == Some(OLD_SUMMARY) {
                if let Some(Value::Array(ms)) =
                    o.get_mut("detail").and_then(|d| d.get_mut("members"))
                {
                    n += usize::from(add_member(ms));
                }
                if let Some(Value::String(dj)) = o.get_mut("detail_json") {
                    let mut d: Value = serde_json::from_str(dj).unwrap();
                    if let Some(Value::Array(ms)) = d.get_mut("members")
                        && add_member(ms)
                    {
                        *dj = py_dumps(&d);
                        n += 1;
                    }
                }
            }
            n + o.values_mut().map(apply).sum::<usize>()
        }
        _ => 0,
    }
}

/// Python's `json.dumps` spacing (", " and ": "), keys in the given order.
fn py_dumps(v: &Value) -> String {
    match v {
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(py_dumps).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, x)| format!("{}: {}", serde_json::to_string(k).unwrap(), py_dumps(x)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => serde_json::to_string(other).unwrap(),
    }
}
