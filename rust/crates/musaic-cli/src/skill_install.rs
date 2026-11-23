//! `setup --skill` and `uninstall --skill`: copy the bundled work-delegation
//! skill into a host's skills directory, as the fallback for a host that does
//! not load skills from the MCP server (spec 005, research D4).
//!
//! Only Claude Code's personal skills directory is known and supported. The
//! files written are exactly the bundle the binary embeds, and an install
//! record (`.musaic-skill.json`: version and each file's digest) says which
//! files are ours:
//!
//! * the directory is absent: write it;
//! * it holds our record and every recorded file still has its recorded
//!   digest: replace it in place (an unmodified, owned install);
//! * anything else (no record, a file we wrote that has changed, a symlink):
//!   an existing directory is moved aside to a backup first, and a symlink or
//!   plain file at the path is refused;
//! * uninstall removes only the recorded files whose digest still matches,
//!   then the record, then the directory if it is empty; a file that was
//!   edited, and anything foreign, stays.
//!
//! Installing shows nothing about whether the host has loaded the skill, and
//! every report says so. `--dry-run` writes nothing.

use std::path::{Path, PathBuf};

use musaic_commands::skill::{SKILL_NAME, bundle, digest_of};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::commands::{EXIT_OK, EXIT_PROBLEMS, Outcome, usage};
use crate::setup::{HostPaths, stamp, write_atomic};

/// The record's file name inside the skill directory.
pub const RECORD: &str = ".musaic-skill.json";
/// What every install report ends with.
pub const NOT_LOADED_NOTE: &str =
    "Installed. This does not show that the host has loaded the skill.";

#[derive(Debug, Clone)]
pub struct SkillOptions {
    pub dry_run: bool,
    /// Only `claude-code` supports `--skill`.
    pub host: String,
    pub uninstall: bool,
    pub paths: HostPaths,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Record {
    version: String,
    files: Vec<RecordFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct RecordFile {
    path: String,
    digest: String,
}

fn target(paths: &HostPaths) -> PathBuf {
    paths.claude_code_skills.join(SKILL_NAME)
}

fn backups_dir(paths: &HostPaths) -> PathBuf {
    // Next to the skills directory, not inside it: a second copy of the skill
    // under the skills directory would be loaded as a duplicate.
    paths
        .claude_code_skills
        .parent()
        .map(|p| p.join("musaic-skill-backups"))
        .unwrap_or_else(|| PathBuf::from("musaic-skill-backups"))
}

/// The record, when it is one Musaic could have written: every path it names
/// is relative and stays inside the skill directory. Any other record is
/// treated as foreign, since its paths are joined to the directory and the
/// files they name are removed on update and uninstall.
fn read_record(dir: &Path) -> Option<Record> {
    let record: Record =
        serde_json::from_str(&std::fs::read_to_string(dir.join(RECORD)).ok()?).ok()?;
    let inside = |p: &str| {
        let p = Path::new(p);
        p.components().count() > 0
            && p.components()
                .all(|c| matches!(c, std::path::Component::Normal(_)))
    };
    record
        .files
        .iter()
        .all(|f| inside(&f.path))
        .then_some(record)
}

/// Whether every file the record lists is there with its recorded digest.
fn unmodified(dir: &Path, record: &Record) -> bool {
    record.files.iter().all(|f| {
        std::fs::read(dir.join(&f.path))
            .map(|b| digest_of(&b) == f.digest)
            .unwrap_or(false)
    })
}

fn new_record() -> Record {
    let b = bundle();
    Record {
        version: b.version().to_string(),
        files: b
            .entries
            .iter()
            .map(|e| RecordFile {
                path: e.path.to_string(),
                digest: e.digest.clone(),
            })
            .collect(),
    }
}

/// What is installed, for `doctor`.
pub fn status(paths: &HostPaths) -> Value {
    let dir = target(paths);
    let bundled = bundle().version();
    if !dir.exists() {
        return json!({"installed": false, "bundledVersion": bundled});
    }
    match read_record(&dir) {
        Some(r) => json!({
            "installed": true,
            "ownedByMusaic": true,
            "installedVersion": r.version,
            "bundledVersion": bundled,
            "matchesBundled": r == new_record() && unmodified(&dir, &r),
            "modified": !unmodified(&dir, &r),
        }),
        None => json!({
            "installed": true,
            "ownedByMusaic": false,
            "bundledVersion": bundled,
            "matchesBundled": false,
        }),
    }
}

fn failure(message: &str, dir: &Path) -> Outcome {
    Outcome {
        json: json!({
            "ok": false,
            "skill": {"host": "claude-code", "path": dir.display().to_string(), "action": "refuse", "reason": message},
        }),
        code: EXIT_PROBLEMS,
    }
}

pub fn run(opts: &SkillOptions) -> Outcome {
    if opts.host != "claude-code" {
        return usage(
            "--skill supports only --host claude-code; other hosts load the skill from the MCP server or are not supported yet",
        );
    }
    let dir = target(&opts.paths);
    let meta = std::fs::symlink_metadata(&dir).ok();
    if meta
        .as_ref()
        .is_some_and(|m| m.file_type().is_symlink() || m.is_file())
    {
        return failure(
            "the path is a symlink or a file, not a directory; nothing was changed",
            &dir,
        );
    }
    if opts.uninstall {
        return uninstall(opts, &dir);
    }
    let record = read_record(&dir);
    let existing = meta.is_some();
    let owned_unmodified = record.as_ref().is_some_and(|r| unmodified(&dir, r));
    let want = new_record();
    let (action, backup) = match (&record, existing) {
        (_, false) => ("install", None),
        (Some(r), true) if *r == want && owned_unmodified => ("unchanged", None),
        (Some(_), true) if owned_unmodified => ("update", None),
        (_, true) => (
            "replace_with_backup",
            Some(backups_dir(&opts.paths).join(format!("{SKILL_NAME}.musaic-backup-{}", stamp()))),
        ),
    };
    let mut skill = json!({
        "host": "claude-code",
        "path": dir.display().to_string(),
        "action": action,
        "version": want.version,
        "files": want.files.iter().map(|f| f.path.clone()).collect::<Vec<_>>(),
    });
    if let Some(b) = &backup {
        skill["backup"] = json!(b.display().to_string());
    }
    if !opts.dry_run
        && action != "unchanged"
        && let Err(e) = apply(&dir, backup.as_deref(), record.as_ref(), &want)
    {
        return failure(&format!("could not write the skill ({e})"), &dir);
    }
    Outcome {
        json: json!({
            "ok": true,
            "dryRun": opts.dry_run,
            "skill": skill,
            "note": NOT_LOADED_NOTE,
        }),
        code: EXIT_OK,
    }
}

fn apply(
    dir: &Path,
    backup: Option<&Path>,
    old: Option<&Record>,
    want: &Record,
) -> std::io::Result<()> {
    if let Some(b) = backup {
        if let Some(parent) = b.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(dir, b)?;
    }
    let b = bundle();
    for e in &b.entries {
        let path = dir.join(e.path);
        write_atomic(&path, e.text())?;
    }
    // A file the previous owned install had and this one does not.
    if backup.is_none() {
        for f in old.iter().flat_map(|r| &r.files) {
            if !want.files.iter().any(|w| w.path == f.path) {
                let _ = std::fs::remove_file(dir.join(&f.path));
            }
        }
    }
    let text = serde_json::to_string_pretty(want).expect("a record is JSON") + "\n";
    write_atomic(&dir.join(RECORD), &text)
}

fn uninstall(opts: &SkillOptions, dir: &Path) -> Outcome {
    if !dir.exists() {
        return Outcome {
            json: json!({"ok": true, "dryRun": opts.dry_run,
                "skill": {"host": "claude-code", "path": dir.display().to_string(), "action": "absent"}}),
            code: EXIT_OK,
        };
    }
    let Some(record) = read_record(dir) else {
        return failure(
            "this skill directory was not installed by Musaic (no install record); it was left alone",
            dir,
        );
    };
    let mut removed = Vec::new();
    let mut kept = Vec::new();
    for f in &record.files {
        let path = dir.join(&f.path);
        match std::fs::read(&path) {
            Ok(b) if digest_of(&b) == f.digest => removed.push(f.path.clone()),
            Ok(_) => kept.push(f.path.clone()),
            Err(_) => {}
        }
    }
    if !opts.dry_run {
        for f in &removed {
            let _ = std::fs::remove_file(dir.join(f));
        }
        let _ = std::fs::remove_file(dir.join(RECORD));
        // Remove emptied sub-directories, then the directory if nothing else
        // is in it (std::fs::remove_dir refuses a non-empty one).
        let _ = std::fs::remove_dir(dir.join("references"));
        let _ = std::fs::remove_dir(dir);
    }
    Outcome {
        json: json!({
            "ok": true,
            "dryRun": opts.dry_run,
            "skill": {
                "host": "claude-code",
                "path": dir.display().to_string(),
                "action": "remove",
                "removed": removed,
                "keptBecauseChanged": kept,
            },
        }),
        code: EXIT_OK,
    }
}
