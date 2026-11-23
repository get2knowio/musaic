//! SC-005: the checked-in catalog is the generated one, and drift is caught.

use musaic_cli::catalog_cmd::{self, Mode};

fn docs() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../docs")
}

#[test]
fn checked_in_catalog_matches_the_code() {
    let r = catalog_cmd::run(Mode::Check, &docs()).unwrap();
    assert!(
        r.is_clean(),
        "{:?} differ; run `cargo run -p musaic-cli -- catalog --write --dir ../docs`",
        r.differing
    );
}

#[test]
fn a_modified_copy_fails_the_check_and_write_repairs_it() {
    let tmp = tempfile::tempdir().unwrap();
    for f in [
        catalog_cmd::JSON_FILE,
        catalog_cmd::MD_FILE,
        catalog_cmd::EVENTS_JSON_FILE,
        catalog_cmd::EVENTS_MD_FILE,
    ] {
        std::fs::copy(docs().join(f), tmp.path().join(f)).unwrap();
    }
    assert!(
        catalog_cmd::run(Mode::Check, tmp.path())
            .unwrap()
            .is_clean()
    );
    let md = tmp.path().join(catalog_cmd::MD_FILE);
    let mut text = std::fs::read_to_string(&md).unwrap();
    text.push_str("drift\n");
    std::fs::write(&md, text).unwrap();
    let r = catalog_cmd::run(Mode::Check, tmp.path()).unwrap();
    assert_eq!(r.differing, vec![catalog_cmd::MD_FILE.to_string()]);
    // A missing file counts as drift too.
    std::fs::remove_file(tmp.path().join(catalog_cmd::JSON_FILE)).unwrap();
    assert_eq!(
        catalog_cmd::run(Mode::Check, tmp.path())
            .unwrap()
            .differing
            .len(),
        2
    );
    let events = tmp.path().join(catalog_cmd::EVENTS_MD_FILE);
    let mut text = std::fs::read_to_string(&events).unwrap();
    text.push_str("drift\n");
    std::fs::write(&events, text).unwrap();
    assert_eq!(
        catalog_cmd::run(Mode::Check, tmp.path())
            .unwrap()
            .differing
            .len(),
        3
    );
    catalog_cmd::run(Mode::Write, tmp.path()).unwrap();
    assert!(
        catalog_cmd::run(Mode::Check, tmp.path())
            .unwrap()
            .is_clean()
    );
}

fn skill_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../skills/musaic-work-delegation")
}

fn copy_skill() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    for e in &musaic_commands::skill::bundle().entries {
        let to = tmp.path().join(e.path);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(skill_dir().join(e.path), to).unwrap();
    }
    std::fs::copy(
        skill_dir().join(musaic_commands::skill::LOCK_FILE),
        tmp.path().join(musaic_commands::skill::LOCK_FILE),
    )
    .unwrap();
    tmp
}

#[test]
fn the_checked_in_skill_assets_match_the_code() {
    let r = catalog_cmd::run_skill(Mode::Check, &skill_dir()).unwrap();
    assert!(r.is_clean(), "{r:?}; run `catalog --write --dir ../docs`");
}

#[test]
fn a_drifted_generated_reference_is_caught_and_rewritten() {
    let tmp = copy_skill();
    let tools = tmp.path().join("references/tools.md");
    std::fs::write(&tools, "stale\n").unwrap();
    let r = catalog_cmd::run_skill(Mode::Check, tmp.path()).unwrap();
    assert_eq!(r.differing, ["musaic-work-delegation/references/tools.md"]);
    // The reference is rewritten; the lock never was out of date, so it stays.
    let r = catalog_cmd::run_skill(Mode::Write, tmp.path()).unwrap();
    assert_eq!(r.notes, Vec::<String>::new());
    assert!(
        catalog_cmd::run_skill(Mode::Check, tmp.path())
            .unwrap()
            .is_clean()
    );
}

#[test]
fn a_drifted_events_reference_is_caught() {
    let tmp = copy_skill();
    std::fs::write(tmp.path().join("references/events.md"), "stale\n").unwrap();
    let r = catalog_cmd::run_skill(Mode::Check, tmp.path()).unwrap();
    assert_eq!(r.differing, ["musaic-work-delegation/references/events.md"]);
}

#[test]
fn a_missing_or_changed_lock_is_caught() {
    let tmp = copy_skill();
    let lock = tmp.path().join(musaic_commands::skill::LOCK_FILE);
    std::fs::remove_file(&lock).unwrap();
    let r = catalog_cmd::run_skill(Mode::Check, tmp.path()).unwrap();
    assert_eq!(r.differing, ["musaic-work-delegation/skill.lock.json"]);
    // A missing lock is simply written.
    assert!(
        catalog_cmd::run_skill(Mode::Write, tmp.path())
            .unwrap()
            .notes
            .is_empty()
    );
    assert!(
        catalog_cmd::run_skill(Mode::Check, tmp.path())
            .unwrap()
            .is_clean()
    );
}

#[test]
fn changed_bytes_without_a_version_change_refuse_the_lock_update() {
    let tmp = copy_skill();
    let lock = tmp.path().join(musaic_commands::skill::LOCK_FILE);
    // A lock that records other bytes under the same version: the code's
    // bytes are "new", the version is the same.
    let mut doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&lock).unwrap()).unwrap();
    doc["files"][0]["digest"] = serde_json::json!("sha256:00");
    std::fs::write(&lock, serde_json::to_string_pretty(&doc).unwrap() + "\n").unwrap();
    let before = std::fs::read_to_string(&lock).unwrap();
    let r = catalog_cmd::run_skill(Mode::Write, tmp.path()).unwrap();
    assert_eq!(r.notes.len(), 1, "{r:?}");
    assert!(r.notes[0].contains("bump metadata.version"));
    assert_eq!(std::fs::read_to_string(&lock).unwrap(), before);
    // The same drift with a different recorded version is accepted.
    doc["version"] = serde_json::json!("0.9.0");
    std::fs::write(&lock, serde_json::to_string_pretty(&doc).unwrap() + "\n").unwrap();
    let r = catalog_cmd::run_skill(Mode::Write, tmp.path()).unwrap();
    assert!(r.notes.is_empty());
    assert!(
        catalog_cmd::run_skill(Mode::Check, tmp.path())
            .unwrap()
            .is_clean()
    );
}

#[test]
fn a_hand_written_file_that_differs_from_the_binary_is_a_note() {
    let tmp = copy_skill();
    std::fs::write(tmp.path().join("references/recovery.md"), "edited\n").unwrap();
    let r = catalog_cmd::run_skill(Mode::Check, tmp.path()).unwrap();
    assert!(!r.is_clean());
    assert!(r.notes[0].contains("rebuild"));
}

#[test]
fn an_invalid_lock_is_caught_and_rewritten_without_a_panic() {
    let tmp = copy_skill();
    let lock = tmp.path().join(musaic_commands::skill::LOCK_FILE);
    std::fs::write(&lock, "{ this is not json").unwrap();
    let r = catalog_cmd::run_skill(Mode::Check, tmp.path()).unwrap();
    assert_eq!(r.differing, ["musaic-work-delegation/skill.lock.json"]);
    let r = catalog_cmd::run_skill(Mode::Write, tmp.path()).unwrap();
    assert!(r.notes.is_empty(), "{r:?}");
    assert!(
        catalog_cmd::run_skill(Mode::Check, tmp.path())
            .unwrap()
            .is_clean()
    );
}
