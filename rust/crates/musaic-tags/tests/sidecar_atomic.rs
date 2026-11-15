//! The `cover.jpg` sidecar is written as `.cover.jpg.tmp` +
//! rename, observed through what only that mechanism produces, against
//! testdata/golden/tags/sidecar-atomic.json (the Python app's own
//! `tags.save_cover_to_album_dir`):
//!
//! * a hard link to the old `cover.jpg` keeps the old bytes (an in-place
//!   rewrite would change it), and `cover.jpg` is a new inode;
//! * a directory squatting on `.cover.jpg.tmp` makes the write fail before
//!   `cover.jpg` is touched (returns `None`; the old bytes stay);
//! * a stale `.cover.jpg.tmp` hard-linked elsewhere is truncated and
//!   rewritten in place before the rename (the other link sees the new
//!   bytes), as in the Python app;
//! * samefile cleanup: a competing `cover.*` that is a hard link to
//!   `folder.jpg` is deleted (not the same file as the new `cover.jpg`),
//!   `folder.*` / `front.*` are kept.
//!
//! No scenario depends on case folding, so the golden holds on either kind
//! of volume.

use std::fs;
use std::path::Path;

use musaic_tags::sidecar::save_cover_to_album_dir;
use serde_json::{Value, json};

fn golden() -> Value {
    musaic_testdata::golden("tags/sidecar-atomic.json")
}

/// Build the scenario's starting state exactly as the generator does.
fn setup(name: &str, d: &Path, old: &[u8]) {
    fs::write(d.join("cover.jpg"), old).unwrap();
    match name {
        "hardlink-to-old-cover" => fs::hard_link(d.join("cover.jpg"), d.join("keep.jpg")).unwrap(),
        "tmp-squatted-by-dir" => fs::create_dir(d.join(".cover.jpg.tmp")).unwrap(),
        "stale-tmp-hardlinked" => {
            fs::write(d.join(".cover.jpg.tmp"), b"STALE-TMP").unwrap();
            fs::hard_link(d.join(".cover.jpg.tmp"), d.join("other.bin")).unwrap();
        }
        "samefile-and-stale-siblings" => {
            fs::write(d.join("cover.png"), b"PNG-SIBLING").unwrap();
            fs::write(d.join("folder.jpg"), b"FOLDER").unwrap();
            fs::hard_link(d.join("folder.jpg"), d.join("cover.JPEG")).unwrap();
            fs::write(d.join("front.jpg"), b"FRONT").unwrap();
        }
        other => panic!("unknown scenario {other}"),
    }
}

/// The generator's `observe`: sorted entries; files with bytes and link
/// count (link counts compared on unix only).
fn observe(d: &Path) -> Vec<Value> {
    let mut names: Vec<String> = fs::read_dir(d)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|name| {
            let p = d.join(&name);
            if p.is_dir() {
                json!({"name": name, "kind": "dir"})
            } else {
                json!({
                    "name": name,
                    "kind": "file",
                    "bytes": String::from_utf8(fs::read(&p).unwrap()).unwrap(),
                    "nlink": nlink(&p),
                })
            }
        })
        .collect()
}

#[cfg(unix)]
fn nlink(p: &Path) -> Value {
    use std::os::unix::fs::MetadataExt;
    json!(fs::symlink_metadata(p).unwrap().nlink())
}

#[cfg(not(unix))]
fn nlink(_: &Path) -> Value {
    Value::Null
}

fn strip_nlink(v: &[Value]) -> Vec<Value> {
    v.iter()
        .map(|e| {
            let mut e = e.clone();
            if cfg!(not(unix)) {
                e.as_object_mut().unwrap().remove("nlink");
            }
            e
        })
        .collect()
}

// The sidecar is written as a temp file and renamed, as the Python app does.
#[test]
fn sidecar_write_is_temp_file_then_rename_as_in_the_python_app() {
    let g = golden();
    let old = g["old_bytes"].as_str().unwrap().as_bytes();
    let new = g["new_bytes"].as_str().unwrap().as_bytes();
    let scenarios = g["scenarios"].as_array().unwrap();
    assert_eq!(scenarios.len(), 4);
    for s in scenarios {
        let name = s["scenario"].as_str().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join(name);
        fs::create_dir(&d).unwrap();
        setup(name, &d, old);
        assert_eq!(
            strip_nlink(&observe(&d)),
            strip_nlink(s["before"].as_array().unwrap()),
            "{name}: starting state"
        );
        let got = save_cover_to_album_dir(&d, new);
        let got_name = got.map(|p| p.file_name().unwrap().to_str().unwrap().to_string());
        assert_eq!(
            got_name.as_deref(),
            s["returned"].as_str(),
            "{name}: return value"
        );
        assert_eq!(
            strip_nlink(&observe(&d)),
            strip_nlink(s["after"].as_array().unwrap()),
            "{name}: directory afterwards"
        );
        if let Some(want) = s["cover_jpg_samefile_keep_jpg"].as_bool() {
            let same = same_file::is_same_file(d.join("cover.jpg"), d.join("keep.jpg")).unwrap();
            assert_eq!(same, want, "{name}: cover.jpg vs keep.jpg");
        }
    }
}

// The hard-link observation is not vacuous: the golden's
// hard-link scenario is the one where an in-place rewrite and a
// write-then-rename disagree, and it records the rename's outcome.
#[test]
fn golden_hardlink_scenario_records_the_old_bytes_on_the_link() {
    let g = golden();
    let s = g["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["scenario"] == "hardlink-to-old-cover")
        .unwrap();
    let keep = s["after"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "keep.jpg")
        .unwrap();
    assert_eq!(keep["bytes"], g["old_bytes"]);
    assert_eq!(s["cover_jpg_samefile_keep_jpg"], json!(false));
}
