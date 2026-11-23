//! `musaic-cli catalog --check | --write`: the generated tool catalog and event
//! catalog under `docs/` (`agent-tools.{json,md}`, `agent-events.{json,md}`)
//! and the skill assets under `skills/musaic-work-delegation/` (the generated
//! references and the lock that pins every served file's digest and size).

use std::path::{Path, PathBuf};

use musaic_commands::catalog::{catalog_json, catalog_markdown};
use musaic_commands::events::{events_catalog_json, events_catalog_markdown};
use musaic_commands::skill::{self, bundle};
use serde_json::Value;

pub const JSON_FILE: &str = "agent-tools.json";
pub const MD_FILE: &str = "agent-tools.md";
pub const EVENTS_JSON_FILE: &str = "agent-events.json";
pub const EVENTS_MD_FILE: &str = "agent-events.md";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Check,
    Write,
}

/// What a catalog run found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// Files that differ from the code (check) or were rewritten (write).
    pub differing: Vec<String>,
    /// Reasons that are not a plain difference: a refused lock update, a
    /// binary older than the skill files.
    pub notes: Vec<String>,
}

impl Report {
    pub fn is_clean(&self) -> bool {
        self.differing.is_empty() && self.notes.is_empty()
    }
}

/// The docs directory when none is given: the first of `docs`, `../docs` and
/// `../../docs` that looks like this repository's docs (it holds
/// `operations.md`).
pub fn default_docs_dir() -> Option<PathBuf> {
    ["docs", "../docs", "../../docs"]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.join("operations.md").is_file())
}

/// The skill directory that sits beside a docs directory, if it exists.
pub fn skill_dir_beside(docs: &Path) -> Option<PathBuf> {
    let dir = docs.join("..").join("skills").join(skill::SKILL_NAME);
    dir.is_dir().then_some(dir)
}

fn expected() -> [(&'static str, String); 4] {
    [
        (JSON_FILE, catalog_json()),
        (MD_FILE, catalog_markdown()),
        (EVENTS_JSON_FILE, events_catalog_json()),
        (EVENTS_MD_FILE, events_catalog_markdown()),
    ]
}

/// The docs catalogs: tools and events.
pub fn run(mode: Mode, dir: &Path) -> std::io::Result<Report> {
    let mut differing = Vec::new();
    for (name, want) in expected() {
        let path = dir.join(name);
        let have = std::fs::read_to_string(&path).ok();
        if have.as_deref() != Some(want.as_str()) {
            differing.push(name.to_string());
            if mode == Mode::Write {
                std::fs::write(&path, want)?;
            }
        }
    }
    Ok(Report {
        differing,
        notes: Vec::new(),
    })
}

fn lock_files(lock: &Value) -> Vec<(String, String, u64)> {
    lock["files"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|f| {
                    (
                        f["path"].as_str().unwrap_or_default().to_string(),
                        f["digest"].as_str().unwrap_or_default().to_string(),
                        f["size"].as_u64().unwrap_or_default(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The skill assets: the generated references, the hand-written files (the
/// binary embeds them, so a difference means it is older than the tree) and
/// the lock. A write never updates the lock when served bytes changed and the
/// version in `SKILL.md` did not (FR-004).
pub fn run_skill(mode: Mode, dir: &Path) -> std::io::Result<Report> {
    let b = bundle();
    let mut differing = Vec::new();
    let mut notes = Vec::new();
    for e in &b.entries {
        let path = dir.join(e.path);
        let have = std::fs::read(&path).ok();
        if have.as_deref() == Some(e.bytes.as_slice()) {
            continue;
        }
        let generated = skill::GENERATED.contains(&e.path);
        differing.push(format!("{}/{}", skill::SKILL_NAME, e.path));
        if generated {
            if mode == Mode::Write {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&path, &e.bytes)?;
            }
        } else {
            notes.push(format!(
                "{} differs from the bytes this binary embeds; rebuild after editing a skill file",
                e.path
            ));
        }
    }
    let lock_path = dir.join(skill::LOCK_FILE);
    let want = b.lock_json();
    let have = std::fs::read_to_string(&lock_path).ok();
    if have.as_deref() != Some(want.as_str()) {
        differing.push(format!("{}/{}", skill::SKILL_NAME, skill::LOCK_FILE));
        let old: Option<Value> = have.as_deref().and_then(|t| serde_json::from_str(t).ok());
        let new: Value = serde_json::from_str(&want).expect("the lock is JSON");
        let bytes_changed = old
            .as_ref()
            .is_some_and(|o| lock_files(o) != lock_files(&new));
        let same_version = old.as_ref().is_some_and(|o| o["version"] == new["version"]);
        if bytes_changed && same_version {
            notes.push(format!(
                "served skill bytes changed but the version in SKILL.md is still {}; bump metadata.version, then write again",
                b.version()
            ));
        } else if mode == Mode::Write {
            std::fs::write(&lock_path, want)?;
        }
    }
    Ok(Report { differing, notes })
}

/// Everything `catalog` covers: docs, and the skill assets when `skill` names
/// their directory.
pub fn run_all(mode: Mode, docs: &Path, skill: Option<&Path>) -> std::io::Result<Report> {
    let mut report = run(mode, docs)?;
    if let Some(dir) = skill {
        let s = run_skill(mode, dir)?;
        report.differing.extend(s.differing);
        report.notes.extend(s.notes);
    }
    Ok(report)
}
