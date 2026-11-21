//! The CLI must not own the database (constitution II), and the contract crate
//! must stay a leaf: no db, app, HTTP server or async runtime (T004).

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

fn metadata() -> serde_json::Value {
    let out = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["metadata", "--format-version", "1", "--locked"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run cargo metadata");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

/// The names in the normal (non-dev, non-build) dependency closure of `krate`.
fn closure(meta: &serde_json::Value, krate: &str) -> BTreeSet<String> {
    let mut names = BTreeMap::new();
    for p in meta["packages"].as_array().unwrap() {
        names.insert(
            p["id"].as_str().unwrap().to_string(),
            p["name"].as_str().unwrap().to_string(),
        );
    }
    let mut deps: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for n in meta["resolve"]["nodes"].as_array().unwrap() {
        let list = n["deps"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|d| {
                d["dep_kinds"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|k| k["kind"].is_null())
            })
            .map(|d| d["pkg"].as_str().unwrap().to_string())
            .collect();
        deps.insert(n["id"].as_str().unwrap().to_string(), list);
    }
    let root = names
        .iter()
        .find(|(_, n)| n.as_str() == krate)
        .unwrap()
        .0
        .clone();
    let mut seen = BTreeSet::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if seen.insert(id.clone()) {
            stack.extend(deps.get(&id).cloned().unwrap_or_default());
        }
    }
    seen.iter().map(|i| names[i].clone()).collect()
}

#[test]
fn cli_links_no_database_or_app() {
    let meta = metadata();
    let c = closure(&meta, "musaic-cli");
    for banned in ["musaic-db", "rusqlite", "musaic-app", "musaic-api"] {
        assert!(
            !c.contains(banned),
            "musaic-cli must not depend on {banned}"
        );
    }
}

#[test]
fn contract_crate_is_a_leaf() {
    let meta = metadata();
    let c = closure(&meta, "musaic-commands");
    for banned in ["musaic-db", "musaic-app", "axum", "tokio"] {
        assert!(
            !c.contains(banned),
            "musaic-commands must not depend on {banned}"
        );
    }
}
