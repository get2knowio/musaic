//! The enumerated deliberate changes against
//! testdata/golden/services/tags_vocab.json, shared by the service-level
//! replay (musaic-services tests/golden_tags_vocab.rs) and the HTTP replay
//! (musaic-api tests/tags_vocab_http.rs).
//!
//! Each [`Delta`] replaces the value at a JSON pointer of one recorded step
//! (`status`, `body`, `state`) with a new value built from the Python app's
//! record (its previous state / previous step) by the deliberate rule. The
//! Python app's value must differ from the new one (a delta that changes
//! nothing fails), and every delta must be used once (see [`Tracker`]).
//!
//! After a step whose new state differs from the Python app's, the world adopts
//! the Python app's recorded state ([`ADOPT`], [`adopt`]: its `track_tags` and
//! `tag_vocabulary` rows and every sidecar, checked equal afterwards), so
//! each later step starts where the Python app's did and is compared unchanged.
//! This is how rows that only an older build could write (a tag on
//! `../escape.flac`, on a Missing album, two vocabulary entries "Été" /
//! "été") reach the later steps: the Python app wrote them, the Rust build
//! refuses to, and the later steps still exercise them.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use musaic_db::track_tags::fold;
use musaic_services::apply::missing_album_detail;
use musaic_services::track_tags::bad_filename_detail;
use rusqlite::{Connection, params};
use serde_json::{Value, json};

/// Inputs a new value is built from.
pub struct Inputs<'a> {
    /// The Python app's record of the step.
    pub step: &'a Value,
    /// The Python app's state before the step.
    pub prev_state: &'a Value,
    /// The Python app's record of the previous step of the same section.
    pub prev_step: Option<&'a Value>,
}

pub struct Delta {
    /// `"steps"` or `"extra_steps"`.
    pub section: &'static str,
    pub label: &'static str,
    /// JSON pointer into the step record; the last segment may be a new key.
    pub path: &'static str,
    pub why: &'static str,
    pub new: fn(&Inputs) -> Value,
}

/// Steps after which the world adopts the Python app's recorded state.
pub const ADOPT: &[(&str, &str)] = &[
    ("steps", "add album tag unknown filename"),
    ("steps", "add album tag missing album"),
    ("steps", "replace album tags"),
    ("steps", "vocab rename case only"),
    ("steps", "vocab rename to itself"),
    ("steps", "vocab merge"),
    ("extra_steps", "missing album add track tag"),
    ("extra_steps", "missing album remove track tag"),
    ("extra_steps", "missing album add track tag again"),
    ("extra_steps", "missing album remove album tag"),
    ("extra_steps", "missing album replace"),
    ("extra_steps", "track tag unknown filename"),
    ("extra_steps", "album tag path separator"),
    ("extra_steps", "replace with subdirectory name"),
    ("extra_steps", "unicode vocab add case dup"),
    ("extra_steps", "unicode vocab merge case twins"),
    ("extra_steps", "unicode vocab rename with case twin present"),
    ("extra_steps", "unicode vocab patch other case"),
    ("extra_steps", "unicode vocab rename case only"),
    ("extra_steps", "unicode vocab delete strip other case"),
];

/// The setup rescan stamped Gamma Missing with the clock: the generator's
/// `<NOW>`, the test worlds' fixed 2030-01-01T00:00:00Z.
const MISSING_SINCE: &str = "2030-01-01T00:00:00Z";
const ALPHA: &str = "<ID:Alpha - One [FLAC]>";
const GAMMA: &str = "<ID:Gamma - Three [FLAC]>";

fn prev_state(i: &Inputs) -> Value {
    i.prev_state.clone()
}

fn detail(d: String) -> Value {
    json!({ "detail": d })
}

// ---------- the deliberate rules, applied to the Python app's rows ----------

/// `[album, filename, tag, created_at]` rows after respelling every row
/// whose tag folds to `old` (and is not already `new`) as `new`; returns the
/// rows, the number of rows rewritten, the number inserted, and the albums.
fn respell(rows: &Value, old: &str, new: &str) -> (Vec<Value>, usize, usize, BTreeSet<String>) {
    let mut out: Vec<Value> = Vec::new();
    let mut moved = Vec::new();
    for r in rows.as_array().unwrap() {
        let tag = r[2].as_str().unwrap();
        if fold(tag) == fold(old) && tag != new {
            moved.push(r.clone());
        } else {
            out.push(r.clone());
        }
    }
    let mut albums = BTreeSet::new();
    let mut inserted = 0;
    for r in &moved {
        albums.insert(r[0].as_str().unwrap().to_string());
        let exists = out
            .iter()
            .any(|o| o[0] == r[0] && o[1] == r[1] && o[2] == json!(new));
        if !exists {
            out.push(json!([r[0], r[1], new, r[3]]));
            inserted += 1;
        }
    }
    sort_rows(&mut out);
    (out, moved.len(), inserted, albums)
}

/// Rows without every row whose tag folds to `tag`.
fn strip_rows(rows: &Value, tag: &str) -> (Vec<Value>, usize, BTreeSet<String>) {
    let mut out = Vec::new();
    let mut albums = BTreeSet::new();
    let mut n = 0;
    for r in rows.as_array().unwrap() {
        if fold(r[2].as_str().unwrap()) == fold(tag) {
            n += 1;
            albums.insert(r[0].as_str().unwrap().to_string());
        } else {
            out.push(r.clone());
        }
    }
    (out, n, albums)
}

fn sort_rows(rows: &mut [Value]) {
    rows.sort_by_key(|r| {
        (
            r[0].as_str().unwrap().to_string(),
            r[1].as_str().unwrap().to_string(),
            r[2].as_str().unwrap().to_string(),
        )
    });
}

/// `dirs` with the sidecar of every album in `albums` rendered from `rows`
/// (as `write_sidecar` does: deleted when the album has no row).
fn dirs_for(prev_dirs: &Value, rows: &[Value], albums: &BTreeSet<String>) -> Value {
    let mut dirs = prev_dirs.clone();
    for a in albums {
        let dir = a.trim_start_matches("<ID:").trim_end_matches('>');
        let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for r in rows.iter().filter(|r| r[0] == json!(a)) {
            map.entry(r[1].as_str().unwrap().into())
                .or_default()
                .push(r[2].as_str().unwrap().into());
        }
        for tags in map.values_mut() {
            tags.sort();
        }
        let d = dirs[dir].as_object_mut().expect("album dir present");
        let mut dots: Vec<String> = d["dotfiles"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap().to_string())
            .filter(|x| x != ".media-manager-tags.json")
            .collect();
        if map.is_empty() {
            d.insert("sidecar".into(), Value::Null);
        } else {
            d.insert(
                "sidecar".into(),
                json!(musaic_fsops::sidecar::render_sidecar(&map)),
            );
            dots.push(".media-manager-tags.json".into());
            dots.sort();
        }
        d.insert("dotfiles".into(), json!(dots));
    }
    dirs
}

fn vocab_without(list: &Value, tag: &str) -> Value {
    json!(
        list.as_array()
            .unwrap()
            .iter()
            .filter(|e| e["tag"] != json!(tag))
            .cloned()
            .collect::<Vec<_>>()
    )
}

/// The spelling of the vocabulary entry a merge's `into_tag`
/// addresses (in the Python app's state before the step).
fn merge_spelling(i: &Inputs) -> String {
    let into = args(i, "into_tag");
    let entries: Vec<&str> = i.prev_state["vocabulary"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["tag"].as_str().unwrap())
        .filter(|t| fold(t) == fold(into))
        .collect();
    assert_eq!(entries.len(), 1, "one entry addresses {into}");
    assert_ne!(entries[0], into, "merge spelling: typed in another case");
    entries[0].to_string()
}

fn albums_json(a: &BTreeSet<String>) -> Value {
    json!(a.iter().collect::<Vec<_>>())
}

fn args<'a>(i: &'a Inputs, k: &str) -> &'a str {
    i.step["args"][k].as_str().unwrap()
}

/// The state after a rename / merge of the step's names.
fn respelled_state(i: &Inputs, old: &str, new: &str) -> (Value, usize, usize, BTreeSet<String>) {
    let (rows, rewritten, inserted, albums) = respell(&i.prev_state["track_tags"], old, new);
    let state = json!({
        "track_tags": rows,
        "vocabulary": i.step["state"]["vocabulary"],
        "dirs": dirs_for(&i.prev_state["dirs"], &rows, &albums),
    });
    (state, rewritten, inserted, albums)
}

fn refusal(
    section: &'static str,
    label: &'static str,
    why: &'static str,
    d: fn(&Inputs) -> Value,
    status: u16,
    out: &mut Vec<Delta>,
) {
    let st: fn(&Inputs) -> Value = match status {
        400 => |_| json!(400),
        409 => |_| json!(409),
        _ => unreachable!(),
    };
    out.push(Delta {
        section,
        label,
        path: "/status",
        why,
        new: st,
    });
    out.push(Delta {
        section,
        label,
        path: "/body",
        why,
        new: d,
    });
}

/// Every deliberate change, in step order.
pub fn deltas() -> Vec<Delta> {
    let mut d = Vec::new();
    let missing: fn(&Inputs) -> Value = |_| detail(missing_album_detail(MISSING_SINCE));
    // A filename that is not a FLAC of the album is refused (400);
    // nothing changes.
    refusal(
        "steps",
        "add album tag unknown filename",
        "listed-flacs-only",
        |_| detail(bad_filename_detail("nope.flac")),
        400,
        &mut d,
    );
    d.push(Delta {
        section: "steps",
        label: "add album tag unknown filename",
        path: "/state",
        why: "listed-flacs-only",
        new: prev_state,
    });
    // A Missing album's tags are not changed (409).
    refusal(
        "steps",
        "add album tag missing album",
        "missing-album-tags",
        missing,
        409,
        &mut d,
    );
    d.push(Delta {
        section: "steps",
        label: "add album tag missing album",
        path: "/state",
        why: "missing-album-tags",
        new: prev_state,
    });
    refusal(
        "steps",
        "replace album tags",
        "listed-flacs-only",
        |_| detail(bad_filename_detail("extra.flac")),
        400,
        &mut d,
    );
    d.push(Delta {
        section: "steps",
        label: "replace album tags",
        path: "/state",
        why: "listed-flacs-only",
        new: prev_state,
    });
    // A case-only rename respells the rows in place.
    d.push(Delta {
        section: "steps",
        label: "vocab rename case only",
        path: "/state",
        why: "case-only-rename",
        new: |i| respelled_state(i, args(i, "tag"), args(i, "new_tag")).0,
    });
    d.push(Delta {
        section: "steps",
        label: "vocab rename case only",
        path: "/body/rename_result/tag_rows",
        why: "case-only-rename",
        new: |i| json!(respelled_state(i, args(i, "tag"), args(i, "new_tag")).1),
    });
    d.push(Delta {
        section: "steps",
        label: "vocab rename case only",
        path: "/body/vocabulary",
        why: "case-only-rename",
        new: |i| {
            let (state, ..) = respelled_state(i, args(i, "tag"), args(i, "new_tag"));
            let mut v = i.step["body"]["vocabulary"].clone();
            for e in v.as_array_mut().unwrap() {
                let t = fold(e["tag"].as_str().unwrap());
                e["count"] = json!(
                    state["track_tags"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|r| fold(r[2].as_str().unwrap()) == t)
                        .count()
                );
            }
            v
        },
    });
    // An identical rename is a no-op.
    d.push(Delta {
        section: "steps",
        label: "vocab rename to itself",
        path: "/state",
        why: "case-only-rename",
        new: prev_state,
    });
    d.push(Delta {
        section: "steps",
        label: "vocab rename to itself",
        path: "/body/vocabulary",
        why: "case-only-rename",
        new: |i| i.prev_step.unwrap()["body"]["vocabulary"].clone(),
    });
    d.push(Delta {
        section: "steps",
        label: "vocab rename to itself",
        path: "/body/rename_result",
        why: "case-only-rename",
        new: |_| json!({"vocab": false, "tag_rows": 0, "albums": []}),
    });
    // Gamma (Missing, folder gone) loses its row; its sidecar is
    // not written, and the response says so.
    d.push(Delta {
        section: "steps",
        label: "vocab delete unknown strip",
        path: "/body/delete_result/sidecars_not_written",
        why: "missing-album-tags",
        new: |_| json!([GAMMA]),
    });

    // ---- extra_steps ----
    for label in [
        "missing album add track tag",
        "missing album remove track tag",
        "missing album add track tag again",
        "missing album remove album tag",
        "missing album replace",
    ] {
        refusal(
            "extra_steps",
            label,
            "missing-album-tags",
            missing,
            409,
            &mut d,
        );
        d.push(Delta {
            section: "extra_steps",
            label,
            path: "/state",
            why: "missing-album-tags",
            new: prev_state,
        });
    }
    refusal(
        "extra_steps",
        "track tag unknown filename",
        "listed-flacs-only",
        |_| detail(bad_filename_detail("nope.flac")),
        400,
        &mut d,
    );
    d.push(Delta {
        section: "extra_steps",
        label: "track tag unknown filename",
        path: "/state",
        why: "listed-flacs-only",
        new: prev_state,
    });
    // (no state change: the Python app's removal of an absent row changed nothing)
    refusal(
        "extra_steps",
        "remove track tag unknown filename no row",
        "listed-flacs-only",
        |_| detail(bad_filename_detail("ghost.flac")),
        400,
        &mut d,
    );
    refusal(
        "extra_steps",
        "album tag path separator",
        "listed-flacs-only",
        |_| detail(bad_filename_detail("../escape.flac")),
        400,
        &mut d,
    );
    d.push(Delta {
        section: "extra_steps",
        label: "album tag path separator",
        path: "/state",
        why: "listed-flacs-only",
        new: prev_state,
    });
    refusal(
        "extra_steps",
        "replace with subdirectory name",
        "listed-flacs-only",
        |_| detail(bad_filename_detail("sub/x.flac")),
        400,
        &mut d,
    );
    d.push(Delta {
        section: "extra_steps",
        label: "replace with subdirectory name",
        path: "/state",
        why: "listed-flacs-only",
        new: prev_state,
    });
    // "été" is the existing "Été": not added again.
    d.push(Delta {
        section: "extra_steps",
        label: "unicode vocab add case dup",
        path: "/state",
        why: "case-fold",
        new: prev_state,
    });
    d.push(Delta {
        section: "extra_steps",
        label: "unicode vocab add case dup",
        path: "/body",
        why: "case-fold",
        new: |i| i.prev_step.unwrap()["body"].clone(),
    });
    // Twins written under the old rules are reported, not merged.
    d.push(Delta {
        section: "extra_steps",
        label: "unicode vocab list with case twins",
        path: "/body/case_conflicts",
        why: "case-fold",
        new: |_| json!([["Été", "été"]]),
    });
    // Merging "xmas" into "HOLIDAY" while the
    // entry is spelled "Holiday" writes "Holiday" (the Python app wrote the name
    // as typed and left "HOLIDAY" / "Holiday" case variants). A row that
    // already carries the entry's spelling is a union-merge: not counted.
    d.push(Delta {
        section: "steps",
        label: "vocab merge",
        path: "/state",
        why: "merge-spelling",
        new: |i| respelled_state(i, args(i, "tag"), &merge_spelling(i)).0,
    });
    d.push(Delta {
        section: "steps",
        label: "vocab merge",
        path: "/body/merge_result/moved",
        why: "merge-spelling",
        new: |i| json!(respelled_state(i, args(i, "tag"), &merge_spelling(i)).2),
    });
    d.push(Delta {
        section: "steps",
        label: "vocab merge",
        path: "/body/vocabulary",
        why: "merge-spelling",
        new: |i| {
            let (state, ..) = respelled_state(i, args(i, "tag"), &merge_spelling(i));
            let mut v = i.step["body"]["vocabulary"].clone();
            for e in v.as_array_mut().unwrap() {
                let t = fold(e["tag"].as_str().unwrap());
                e["count"] = json!(
                    state["track_tags"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|r| fold(r[2].as_str().unwrap()) == t)
                        .count()
                );
            }
            v
        },
    });
    // ... and a merge of one twin into the other resolves them.
    d.push(Delta {
        section: "extra_steps",
        label: "unicode vocab merge case twins",
        path: "/status",
        why: "case-fold",
        new: |_| json!(200),
    });
    d.push(Delta {
        section: "extra_steps",
        label: "unicode vocab merge case twins",
        path: "/body",
        why: "case-fold",
        new: |i| {
            let (_, _, inserted, albums) = respelled_state(i, args(i, "tag"), args(i, "into_tag"));
            let prev = &i.prev_step.unwrap()["body"];
            json!({
                "vocabulary": vocab_without(&prev["vocabulary"], args(i, "tag")),
                "orphans": prev["orphans"],
                "merge_result": {"moved": inserted, "albums": albums_json(&albums)},
            })
        },
    });
    d.push(Delta {
        section: "extra_steps",
        label: "unicode vocab merge case twins",
        path: "/state",
        why: "case-fold",
        new: |i| {
            let (mut state, ..) = respelled_state(i, args(i, "tag"), args(i, "into_tag"));
            state["vocabulary"] = vocab_without(&i.prev_state["vocabulary"], args(i, "tag"));
            state
        },
    });
    // Renaming one twin onto the other's fold is a clash.
    d.push(Delta {
        section: "extra_steps",
        label: "unicode vocab rename with case twin present",
        path: "/status",
        why: "case-fold",
        new: |_| json!(409),
    });
    d.push(Delta {
        section: "extra_steps",
        label: "unicode vocab rename with case twin present",
        path: "/body",
        why: "case-fold",
        new: |i| {
            detail(format!(
                "vocabulary entry '{}' already exists; use merge instead",
                args(i, "new_tag")
            ))
        },
    });
    d.push(Delta {
        section: "extra_steps",
        label: "unicode vocab rename with case twin present",
        path: "/state",
        why: "case-fold",
        new: prev_state,
    });
    // "été" addresses the entry "ÉTÉ".
    d.push(Delta {
        section: "extra_steps",
        label: "unicode vocab patch other case",
        path: "/body/vocabulary",
        why: "case-fold",
        new: |i| {
            let mut v = i.step["body"]["vocabulary"].clone();
            for e in v.as_array_mut().unwrap() {
                if fold(e["tag"].as_str().unwrap()) == fold(args(i, "tag")) {
                    e["description"] = i.step["args"]["description"].clone();
                }
            }
            v
        },
    });
    d.push(Delta {
        section: "extra_steps",
        label: "unicode vocab patch other case",
        path: "/state/vocabulary",
        why: "case-fold",
        new: |i| {
            let mut v = i.prev_state["vocabulary"].clone();
            for e in v.as_array_mut().unwrap() {
                if fold(e["tag"].as_str().unwrap()) == fold(args(i, "tag")) {
                    e["description"] = i.step["args"]["description"].clone();
                }
            }
            v
        },
    });
    // Every case variant of the tag is respelled.
    d.push(Delta {
        section: "extra_steps",
        label: "unicode vocab rename case only",
        path: "/state",
        why: "case-fold",
        new: |i| respelled_state(i, args(i, "tag"), args(i, "new_tag")).0,
    });
    d.push(Delta {
        section: "extra_steps",
        label: "unicode vocab rename case only",
        path: "/body/rename_result/tag_rows",
        why: "case-fold",
        new: |i| json!(respelled_state(i, args(i, "tag"), args(i, "new_tag")).1),
    });
    // A strip by another case removes every variant and the entry.
    d.push(Delta { section: "extra_steps", label: "unicode vocab delete strip other case", path: "/state", why: "case-fold",
        new: |i| {
            let (rows, _, albums) = strip_rows(&i.prev_state["track_tags"], args(i, "tag"));
            let tag = args(i, "tag");
            json!({
                "track_tags": rows,
                "vocabulary": json!(i.prev_state["vocabulary"].as_array().unwrap().iter()
                    .filter(|e| fold(e["tag"].as_str().unwrap()) != fold(tag)).cloned().collect::<Vec<_>>()),
                "dirs": dirs_for(&i.prev_state["dirs"], &rows, &albums),
            })
        } });
    d.push(Delta {
        section: "extra_steps",
        label: "unicode vocab delete strip other case",
        path: "/body/delete_result",
        why: "case-fold",
        new: |i| {
            let (_, n, albums) = strip_rows(&i.prev_state["track_tags"], args(i, "tag"));
            json!({"vocab": true, "tag_rows": n, "albums": albums_json(&albums)})
        },
    });
    d.push(Delta {
        section: "extra_steps",
        label: "unicode vocab delete strip other case",
        path: "/body/vocabulary",
        why: "case-fold",
        new: |i| {
            let tag = args(i, "tag");
            json!(
                i.step["body"]["vocabulary"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|e| fold(e["tag"].as_str().unwrap()) != fold(tag))
                    .cloned()
                    .collect::<Vec<_>>()
            )
        },
    });
    // A case-only rename where a track already has the new
    // spelling (kept) and another two variants (one row left).
    let l = "vocab rename case only with variants";
    d.push(Delta {
        section: "extra_steps",
        label: l,
        path: "/state",
        why: "case-only-rename",
        new: |i| respelled_state(i, args(i, "tag"), args(i, "new_tag")).0,
    });
    d.push(Delta {
        section: "extra_steps",
        label: l,
        path: "/body/rename_result/tag_rows",
        why: "case-only-rename",
        new: |i| json!(respelled_state(i, args(i, "tag"), args(i, "new_tag")).1),
    });
    d.push(Delta {
        section: "extra_steps",
        label: l,
        path: "/body/vocabulary",
        why: "case-only-rename",
        new: |i| {
            let (state, ..) = respelled_state(i, args(i, "tag"), args(i, "new_tag"));
            let mut v = i.step["body"]["vocabulary"].clone();
            for e in v.as_array_mut().unwrap() {
                let t = fold(e["tag"].as_str().unwrap());
                e["count"] = json!(
                    state["track_tags"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|r| fold(r[2].as_str().unwrap()) == t)
                        .count()
                );
            }
            v
        },
    });
    let _ = ALPHA;
    d
}

/// Counts every delta's use; [`Tracker::finish`] fails on an unused one.
pub struct Tracker {
    all: Vec<Delta>,
    used: Vec<usize>,
}

impl Default for Tracker {
    fn default() -> Self {
        Self::new()
    }
}

impl Tracker {
    pub fn new() -> Tracker {
        let all = deltas();
        let used = vec![0; all.len()];
        Tracker { all, used }
    }

    /// The Python app's record of `step` (`{status, body, state}` + the rest)
    /// with its deltas applied, and whether the world must adopt the
    /// Python app's state afterwards.
    pub fn expected(&mut self, section: &str, i: &Inputs) -> (Value, bool) {
        let label = i.step["label"].as_str().unwrap();
        let mut out = i.step.clone();
        for (k, d) in self.all.iter().enumerate() {
            if d.section != section || d.label != label {
                continue;
            }
            let new = (d.new)(i);
            let (parent, key) = d.path.rsplit_once('/').unwrap();
            let slot = out
                .pointer_mut(parent)
                .and_then(Value::as_object_mut)
                .unwrap_or_else(|| panic!("{label}: no {parent} in the Python app's record"));
            assert_ne!(
                slot.get(key),
                Some(&new),
                "{label} {} ({}): delta changes nothing",
                d.path,
                d.why
            );
            slot.insert(key.to_string(), new);
            self.used[k] += 1;
        }
        let adopt = ADOPT.iter().any(|(s, l)| *s == section && *l == label);
        (out, adopt)
    }

    pub fn finish(&self) {
        for (d, n) in self.all.iter().zip(&self.used) {
            assert_eq!(
                *n, 1,
                "delta {} {} {} ({}) used {n} times",
                d.section, d.label, d.path, d.why
            );
        }
    }
}

/// Make the world's tag rows, vocabulary and sidecars the Python app's recorded
/// `state` (`id` maps an album directory name to this world's album id;
/// `now` replaces `<NOW>`).
pub fn adopt(conn: &Connection, lib: &Path, state: &Value, id: &dyn Fn(&str) -> String, now: &str) {
    let un = |s: &str| {
        if s == "<NOW>" {
            now.to_string()
        } else {
            s.to_string()
        }
    };
    conn.execute("DELETE FROM track_tags", []).unwrap();
    for r in state["track_tags"].as_array().unwrap() {
        let a = r[0].as_str().unwrap();
        let dir = a.trim_start_matches("<ID:").trim_end_matches('>');
        conn.execute(
            "INSERT INTO track_tags (album_id, filename, tag, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![
                id(dir),
                r[1].as_str().unwrap(),
                r[2].as_str().unwrap(),
                un(r[3].as_str().unwrap())
            ],
        )
        .unwrap();
    }
    conn.execute("DELETE FROM tag_vocabulary", []).unwrap();
    for e in state["vocabulary"].as_array().unwrap() {
        conn.execute(
            "INSERT INTO tag_vocabulary (tag, description, color, sort_order, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                e["tag"].as_str().unwrap(),
                e["description"].as_str().unwrap(),
                e["color"].as_str(),
                e["sort_order"].as_i64().unwrap(),
                un(e["created_at"].as_str().unwrap())
            ],
        )
        .unwrap();
    }
    for (dir, d) in state["dirs"].as_object().unwrap() {
        if d.is_null() {
            continue;
        }
        let p = lib.join(dir).join(".media-manager-tags.json");
        match d["sidecar"].as_str() {
            Some(text) => std::fs::write(&p, text).unwrap(),
            None => {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
}
