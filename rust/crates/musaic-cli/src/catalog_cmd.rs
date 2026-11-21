//! `musaic-cli catalog --check | --write`: the generated tool catalog under
//! `docs/` (`agent-tools.json`, `agent-tools.md`).

use std::path::{Path, PathBuf};

use musaic_commands::catalog::{catalog_json, catalog_markdown};

pub const JSON_FILE: &str = "agent-tools.json";
pub const MD_FILE: &str = "agent-tools.md";

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
}

impl Report {
    pub fn is_clean(&self) -> bool {
        self.differing.is_empty()
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

fn expected() -> [(&'static str, String); 2] {
    [(JSON_FILE, catalog_json()), (MD_FILE, catalog_markdown())]
}

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
    Ok(Report { differing })
}
