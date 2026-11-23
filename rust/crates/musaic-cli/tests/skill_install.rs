//! `setup --skill` and `uninstall --skill` against a temporary home only
//! (research D4). Nothing here touches the real `~/.claude`.

use std::path::{Path, PathBuf};

use musaic_cli::setup::HostPaths;
use musaic_cli::skill_install::{self, NOT_LOADED_NOTE, RECORD, SkillOptions};
use musaic_commands::skill::{SKILL_NAME, bundle, digest_of};
use serde_json::Value;

struct Home {
    tmp: tempfile::TempDir,
    paths: HostPaths,
}

fn home() -> Home {
    let tmp = tempfile::tempdir().unwrap();
    let paths = HostPaths::under(tmp.path(), None);
    Home { tmp, paths }
}

fn opts(h: &Home) -> SkillOptions {
    SkillOptions {
        dry_run: false,
        host: "claude-code".into(),
        uninstall: false,
        paths: h.paths.clone(),
    }
}

fn dir(h: &Home) -> PathBuf {
    h.paths.claude_code_skills.join(SKILL_NAME)
}

fn action(v: &Value) -> &str {
    v["skill"]["action"].as_str().unwrap()
}

fn files_under(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(base: &Path, p: &Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(p).unwrap().flatten() {
            if e.file_type().unwrap().is_dir() {
                walk(base, &e.path(), out);
            } else {
                out.push(
                    e.path()
                        .strip_prefix(base)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
    if root.exists() {
        walk(root, root, &mut out);
    }
    out.sort();
    out
}

#[test]
fn a_fresh_install_writes_the_bundle_and_a_record_and_says_it_proves_nothing() {
    let h = home();
    let out = skill_install::run(&opts(&h));
    assert_eq!(out.code, 0, "{}", out.json);
    assert_eq!(action(&out.json), "install");
    assert_eq!(out.json["note"], NOT_LOADED_NOTE);
    assert!(NOT_LOADED_NOTE.contains("does not show that the host has loaded the skill"));
    for e in &bundle().entries {
        assert_eq!(
            std::fs::read(dir(&h).join(e.path)).unwrap(),
            e.bytes,
            "{}",
            e.path
        );
    }
    assert!(dir(&h).join(RECORD).is_file());
    // Running it again changes nothing.
    let again = skill_install::run(&opts(&h));
    assert_eq!(action(&again.json), "unchanged");
    let status = skill_install::status(&h.paths);
    assert_eq!(status["matchesBundled"], true, "{status}");
}

#[test]
fn an_unmodified_older_install_is_updated_in_place_without_a_backup() {
    let h = home();
    skill_install::run(&opts(&h));
    // An older owned install: one file with other content and its own digest
    // in the record, and a file the new bundle no longer has.
    let d = dir(&h);
    let old_text = "old guide\n";
    std::fs::write(d.join("references/recovery.md"), old_text).unwrap();
    std::fs::write(d.join("references/retired.md"), "retired\n").unwrap();
    let mut rec: Value =
        serde_json::from_str(&std::fs::read_to_string(d.join(RECORD)).unwrap()).unwrap();
    rec["version"] = "0.9.0".into();
    for f in rec["files"].as_array_mut().unwrap() {
        if f["path"] == "references/recovery.md" {
            f["digest"] = digest_of(old_text.as_bytes()).into();
        }
    }
    rec["files"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "path": "references/retired.md", "digest": digest_of(b"retired\n")
        }));
    std::fs::write(d.join(RECORD), rec.to_string()).unwrap();

    let out = skill_install::run(&opts(&h));
    assert_eq!(action(&out.json), "update", "{}", out.json);
    assert!(out.json["skill"].get("backup").is_none());
    assert_eq!(
        std::fs::read_to_string(d.join("references/recovery.md")).unwrap(),
        bundle()
            .lookup("skill://musaic-work-delegation/references/recovery.md")
            .unwrap()
            .text()
    );
    assert!(!d.join("references/retired.md").exists());
    assert!(
        !h.paths
            .claude_code_skills
            .parent()
            .unwrap()
            .join("musaic-skill-backups")
            .exists()
    );
}

#[test]
fn a_modified_file_sends_the_directory_to_a_backup_first() {
    let h = home();
    skill_install::run(&opts(&h));
    let d = dir(&h);
    std::fs::write(d.join("SKILL.md"), "my own edits\n").unwrap();
    std::fs::write(d.join("notes.txt"), "mine\n").unwrap();
    let out = skill_install::run(&opts(&h));
    assert_eq!(action(&out.json), "replace_with_backup", "{}", out.json);
    let backup = PathBuf::from(out.json["skill"]["backup"].as_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(backup.join("SKILL.md")).unwrap(),
        "my own edits\n"
    );
    assert_eq!(
        std::fs::read_to_string(backup.join("notes.txt")).unwrap(),
        "mine\n"
    );
    assert_eq!(
        std::fs::read(d.join("SKILL.md")).unwrap(),
        bundle().skill_md().bytes
    );
    assert!(!d.join("notes.txt").exists());
    // The backup is not under the skills directory (it would load twice).
    assert!(!backup.starts_with(&h.paths.claude_code_skills));
}

#[test]
fn a_foreign_directory_with_no_record_is_backed_up_not_overwritten() {
    let h = home();
    std::fs::create_dir_all(dir(&h)).unwrap();
    std::fs::write(dir(&h).join("SKILL.md"), "someone else's skill\n").unwrap();
    let out = skill_install::run(&opts(&h));
    assert_eq!(action(&out.json), "replace_with_backup");
    let backup = PathBuf::from(out.json["skill"]["backup"].as_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(backup.join("SKILL.md")).unwrap(),
        "someone else's skill\n"
    );
}

#[test]
fn uninstall_removes_only_what_it_wrote_and_still_matches() {
    let h = home();
    skill_install::run(&opts(&h));
    let d = dir(&h);
    std::fs::write(d.join("references/work-lifecycle.md"), "edited\n").unwrap();
    std::fs::write(d.join("foreign.txt"), "not ours\n").unwrap();
    let mut o = opts(&h);
    o.uninstall = true;
    let out = skill_install::run(&o);
    assert_eq!(out.code, 0, "{}", out.json);
    assert_eq!(action(&out.json), "remove");
    assert_eq!(
        out.json["skill"]["keptBecauseChanged"],
        serde_json::json!(["references/work-lifecycle.md"])
    );
    assert_eq!(
        files_under(&d),
        ["foreign.txt", "references/work-lifecycle.md"],
        "the edited file and the foreign file stay"
    );
    // A clean install and uninstall leaves nothing.
    let h2 = home();
    skill_install::run(&opts(&h2));
    let mut o2 = opts(&h2);
    o2.uninstall = true;
    skill_install::run(&o2);
    assert!(!dir(&h2).exists());
    // Uninstalling what was never installed is a no-op; a foreign directory
    // with no record is left alone.
    let out = skill_install::run(&o2);
    assert_eq!(action(&out.json), "absent");
    std::fs::create_dir_all(dir(&h2)).unwrap();
    std::fs::write(dir(&h2).join("SKILL.md"), "foreign\n").unwrap();
    let out = skill_install::run(&o2);
    assert_eq!(out.code, 1);
    assert!(dir(&h2).join("SKILL.md").is_file());
}

#[test]
fn a_dry_run_writes_nothing() {
    let h = home();
    let mut o = opts(&h);
    o.dry_run = true;
    let out = skill_install::run(&o);
    assert_eq!(out.json["dryRun"], true);
    assert_eq!(action(&out.json), "install");
    assert!(!h.paths.claude_code_skills.exists());
    assert!(!h.tmp.path().join(".claude").exists());
    // And an uninstall dry run keeps an install intact.
    skill_install::run(&opts(&h));
    let before = files_under(&dir(&h));
    o.uninstall = true;
    skill_install::run(&o);
    assert_eq!(files_under(&dir(&h)), before);
}

#[test]
fn a_symlink_or_file_at_the_path_and_other_hosts_are_refused() {
    let h = home();
    std::fs::create_dir_all(&h.paths.claude_code_skills).unwrap();
    std::fs::write(dir(&h), "a file\n").unwrap();
    let out = skill_install::run(&opts(&h));
    assert_eq!(out.code, 1);
    assert_eq!(action(&out.json), "refuse");
    assert_eq!(std::fs::read_to_string(dir(&h)).unwrap(), "a file\n");
    #[cfg(unix)]
    {
        let h = home();
        let elsewhere = h.tmp.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::create_dir_all(&h.paths.claude_code_skills).unwrap();
        std::os::unix::fs::symlink(&elsewhere, dir(&h)).unwrap();
        let out = skill_install::run(&opts(&h));
        assert_eq!(out.code, 1);
        assert!(files_under(&elsewhere).is_empty());
    }
    let mut o = opts(&home());
    o.host = "claude-desktop".into();
    assert_eq!(skill_install::run(&o).code, 64);
}

#[test]
fn status_reports_missing_foreign_and_modified() {
    let h = home();
    assert_eq!(skill_install::status(&h.paths)["installed"], false);
    skill_install::run(&opts(&h));
    std::fs::write(dir(&h).join("SKILL.md"), "x\n").unwrap();
    let s = skill_install::status(&h.paths);
    assert_eq!(
        (s["modified"].clone(), s["matchesBundled"].clone()),
        (true.into(), false.into())
    );
    std::fs::remove_file(dir(&h).join(RECORD)).unwrap();
    assert_eq!(skill_install::status(&h.paths)["ownedByMusaic"], false);
}
