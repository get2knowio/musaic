//! FR-020: the operations surface knows no HTTP. Two checks: the dependency
//! closure of `musaic-app` (and a direct-dependency rule for `musaic-services`),
//! and a scan of the source under `src/ops/` for words that belong to a wire.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn metadata() -> serde_json::Value {
    let out = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["metadata", "--format-version", "1", "--locked"])
        .current_dir(manifest_dir())
        .output()
        .expect("run cargo metadata");
    assert!(
        out.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("metadata is JSON")
}

/// Package name by id, and for each package its dependency package names
/// (normal and dev kinds; build dependencies are not part of what ships).
fn graph(meta: &serde_json::Value) -> (BTreeMap<String, String>, BTreeMap<String, Vec<String>>) {
    let mut names = BTreeMap::new();
    for p in meta["packages"].as_array().unwrap() {
        names.insert(
            p["id"].as_str().unwrap().to_string(),
            p["name"].as_str().unwrap().to_string(),
        );
    }
    let mut deps: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for n in meta["resolve"]["nodes"].as_array().unwrap() {
        let id = n["id"].as_str().unwrap().to_string();
        let mut list = Vec::new();
        for d in n["deps"].as_array().unwrap() {
            let kinds = d["dep_kinds"].as_array().unwrap();
            let normal_or_dev = kinds
                .iter()
                .any(|k| k["kind"].is_null() || k["kind"] == "dev");
            if normal_or_dev {
                list.push(d["pkg"].as_str().unwrap().to_string());
            }
        }
        deps.insert(id, list);
    }
    (names, deps)
}

fn package_id(names: &BTreeMap<String, String>, name: &str) -> String {
    names
        .iter()
        .find(|(_, n)| n.as_str() == name)
        .unwrap_or_else(|| panic!("package {name} not in the workspace"))
        .0
        .clone()
}

#[test]
fn musaic_app_has_no_http_in_its_dependency_closure() {
    let meta = metadata();
    let (names, deps) = graph(&meta);
    let root = package_id(&names, "musaic-app");
    let mut seen = BTreeSet::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if seen.insert(id.clone()) {
            stack.extend(deps.get(&id).cloned().unwrap_or_default());
        }
    }
    let found: Vec<&str> = seen
        .iter()
        .map(|id| names[id].as_str())
        .filter(|n| ["axum", "tower", "hyper", "musaic-api"].contains(n))
        .collect();
    assert!(
        found.is_empty(),
        "musaic-app must not depend on an HTTP stack, even in dev: {found:?}"
    );
}

#[test]
fn musaic_app_and_services_do_not_depend_on_http_directly() {
    let meta = metadata();
    let (names, deps) = graph(&meta);
    // `http` itself is allowed transitively (ureq, under musaic-net, brings it).
    for krate in ["musaic-app", "musaic-services"] {
        let id = package_id(&names, krate);
        let direct: Vec<&str> = deps[&id]
            .iter()
            .map(|d| names[d].as_str())
            .filter(|n| ["http", "http-body"].contains(n))
            .collect();
        assert!(direct.is_empty(), "{krate} depends directly on {direct:?}");
    }
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Words that belong to a wire. The bare word `status` is not banned: a job's
/// status and the scan's status are domain terms.
const WIRE_WORDS: &[&str] = &[
    "axum",
    "http::",
    "StatusCode",
    "status_code",
    concat!("Http", "Error"),
    "serde_json",
    "Json",
];

/// Files allowed to hold documents (the allow-list of record newtypes).
const RECORD_FILES: &[&str] = &["record.rs"];

/// The record newtypes the allow-list holds. Adding one is a reviewed change to
/// this list and to `src/ops/record.rs`.
const RECORD_TYPES: &[&str] = &[
    "AlbumRecord",
    "TrackRecord",
    "SettingsRecord",
    "TagSet",
    "FindingRecord",
    "CheckSummaryRecord",
    "CheckDetailRecord",
    "ClusterPreview",
    "ConversionRecord",
    "AbandonRecord",
];

/// The wire words found in `src`, as (file, word) pairs. Pure so the test below
/// can prove it notices one.
fn violations_in(file_name: &str, src: &str) -> Vec<String> {
    if RECORD_FILES.contains(&file_name) {
        return Vec::new();
    }
    WIRE_WORDS
        .iter()
        .filter(|w| src.contains(**w))
        .map(|w| format!("{file_name}: {w}"))
        .collect()
}

#[test]
fn ops_sources_use_no_wire_words() {
    let mut files = Vec::new();
    rust_files(&manifest_dir().join("src/ops"), &mut files);
    assert!(!files.is_empty(), "src/ops is empty or missing");
    let mut found = Vec::new();
    for f in files {
        let name = f.file_name().unwrap().to_string_lossy().into_owned();
        let src = std::fs::read_to_string(&f).unwrap();
        found.extend(
            violations_in(&name, &src)
                .into_iter()
                .map(|v| format!("{}: {v}", f.display())),
        );
    }
    assert!(
        found.is_empty(),
        "wire words under src/ops/:\n{}",
        found.join("\n")
    );
}

#[test]
fn the_scan_notices_a_violation() {
    assert!(!violations_in("system.rs", "fn f() -> StatusCode { todo!() }").is_empty());
    assert!(!violations_in("jobs.rs", "use serde_json::Value;").is_empty());
    assert!(violations_in("record.rs", "use serde_json::Value;").is_empty());
    assert!(violations_in("jobs.rs", "let status = job.status;").is_empty());
}

#[test]
fn record_types_are_defined_in_record_rs_and_nowhere_else() {
    let mut files = Vec::new();
    rust_files(&manifest_dir().join("src/ops"), &mut files);
    for f in files {
        let name = f.file_name().unwrap().to_string_lossy().into_owned();
        let src = std::fs::read_to_string(&f).unwrap();
        for t in RECORD_TYPES {
            let defines = src.contains(&format!("pub struct {t}("));
            assert_eq!(
                defines,
                RECORD_FILES.contains(&name.as_str()),
                "{t} must be defined in record.rs only (seen in {name})"
            );
        }
        if !RECORD_FILES.contains(&name.as_str()) {
            assert!(
                !src.contains("Record(Json)"),
                "{name}: a record newtype outside the allow-list"
            );
        }
    }
}
