//! `setup` and `uninstall` against a temporary home (T060, SC-007).

use std::path::{Path, PathBuf};

use musaic_cli::setup::{HostPaths, Options, run};
use serde_json::Value;

struct Home {
    tmp: tempfile::TempDir,
    paths: HostPaths,
}

const DESKTOP: &str = r#"{
  "globalShortcut":   "Ctrl+Space",
  "mcpServers": {
    "other":  {"command": "npx", "args": ["-y",   "some-server"], "env": {"A":"1"}},
    "second": { "command": "/bin/second" }
  },
  "theme" : "dark"
}
"#;

const CODE: &str = r#"{"numStartups": 7, "projects": {"/x": {"allowedTools": []}}, "mcpServers": {"keep": {"type": "stdio", "command": "keep"}}}"#;

fn home() -> Home {
    let tmp = tempfile::tempdir().unwrap();
    let paths = HostPaths::under(tmp.path(), Some(&tmp.path().join("AppData/Roaming")));
    std::fs::create_dir_all(paths.claude_desktop.parent().unwrap()).unwrap();
    std::fs::write(&paths.claude_desktop, DESKTOP).unwrap();
    std::fs::write(&paths.claude_code, CODE).unwrap();
    Home { tmp, paths }
}

fn exe(h: &Home) -> PathBuf {
    let p = h.tmp.path().join("bin/musaic-cli");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, "").unwrap();
    p
}

fn opts(h: &Home) -> Options {
    let mut o = Options::new(h.paths.clone());
    o.executable = Some(exe(h));
    o
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap()
}

fn actions(v: &Value) -> Vec<(String, String)> {
    v["hosts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| {
            (
                h["id"].as_str().unwrap().to_string(),
                h["action"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn backups(p: &Path) -> Vec<PathBuf> {
    let prefix = format!(
        "{}.musaic-backup-",
        p.file_name().unwrap().to_string_lossy()
    );
    std::fs::read_dir(p.parent().unwrap())
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|f| {
            f.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(&prefix)
        })
        .collect()
}

#[test]
fn a_dry_run_lists_the_changes_and_writes_nothing() {
    let h = home();
    let mut o = opts(&h);
    o.dry_run = true;
    let out = run(&o);
    assert_eq!(out.code, 0, "{}", out.json);
    assert_eq!(out.json["dryRun"], true);
    assert_eq!(
        actions(&out.json),
        [
            ("claude-desktop".into(), "add".into()),
            ("claude-code".into(), "add".into())
        ]
    );
    assert_eq!(out.json["files"].as_array().unwrap().len(), 2);
    assert_eq!(out.json["skills"], serde_json::json!([]));
    assert_eq!(read(&h.paths.claude_desktop), DESKTOP, "byte for byte");
    assert_eq!(read(&h.paths.claude_code), CODE);
    assert!(
        backups(&h.paths.claude_desktop).is_empty() && backups(&h.paths.claude_code).is_empty()
    );
    assert!(out.json["manualEntry"]["mcpServers"]["musaic"]["args"] == serde_json::json!(["mcp"]));
}

#[test]
fn setup_adds_the_entry_keeps_everything_else_verbatim_and_backs_up_first() {
    let h = home();
    let o = opts(&h);
    let out = run(&o);
    assert_eq!(out.code, 0, "{}", out.json);
    for (path, original) in [
        (&h.paths.claude_desktop, DESKTOP),
        (&h.paths.claude_code, CODE),
    ] {
        let text = read(path);
        let v: Value = serde_json::from_str(&text).expect("still valid JSON");
        let entry = &v["mcpServers"]["musaic"];
        assert_eq!(entry["type"], "stdio");
        assert_eq!(entry["args"], serde_json::json!(["mcp"]));
        assert!(entry["command"].as_str().unwrap().ends_with("musaic-cli"));
        assert!(Path::new(entry["command"].as_str().unwrap()).is_absolute());
        // Unrelated values keep their exact text, and their order.
        let before: Value = serde_json::from_str(original).unwrap();
        for k in before
            .as_object()
            .unwrap()
            .keys()
            .filter(|k| *k != "mcpServers")
        {
            assert_eq!(v[k], before[k], "{k}");
        }
        for k in before["mcpServers"].as_object().unwrap().keys() {
            assert_eq!(v["mcpServers"][k], before["mcpServers"][k], "{k}");
        }
        // One backup, equal to the original.
        let b = backups(path);
        assert_eq!(b.len(), 1);
        assert_eq!(read(&b[0]), original);
        // No temp file is left.
        assert!(
            !std::fs::read_dir(path.parent().unwrap())
                .unwrap()
                .flatten()
                .any(|e| e.file_name().to_string_lossy().contains("musaic-tmp"))
        );
    }
    let text = read(&h.paths.claude_desktop);
    // Whitespace inside a value is part of its text and stays; the layout of
    // the keys around it is setup's own.
    for needle in [
        r#"["-y",   "some-server"]"#,
        r#"{ "command": "/bin/second" }"#,
        r#"{"A":"1"}"#,
        r#""Ctrl+Space""#,
    ] {
        assert!(text.contains(needle), "{needle} is not verbatim in\n{text}");
    }
    assert!(text.find("globalShortcut").unwrap() < text.find("mcpServers").unwrap());
    assert!(text.find("\"other\"").unwrap() < text.find("\"second\"").unwrap());
    assert!(
        text.find("\"second\"").unwrap() < text.find("\"musaic\"").unwrap(),
        "appended last"
    );
    assert!(text.find("mcpServers").unwrap() < text.find("\"theme\"").unwrap());

    // Again: nothing to do, nothing written, no new backup.
    let again = run(&o);
    assert_eq!(
        actions(&again.json),
        [
            ("claude-desktop".into(), "none".into()),
            ("claude-code".into(), "none".into())
        ]
    );
    assert_eq!(backups(&h.paths.claude_desktop).len(), 1);
}

#[test]
fn an_owned_entry_is_updated_in_place() {
    let h = home();
    std::fs::write(
        &h.paths.claude_desktop,
        r#"{"mcpServers": {"first": {"command": "a"}, "musaic": {"type": "stdio", "command": "/old/place/musaic-cli", "args": ["mcp"], "env": {}}, "last": {"command": "z"}}}"#,
    )
    .unwrap();
    let mut o = opts(&h);
    o.host = "claude-desktop".into();
    o.config_dir = Some(PathBuf::from("/some/config"));
    let out = run(&o);
    assert_eq!(out.code, 0, "{}", out.json);
    assert_eq!(
        actions(&out.json),
        [("claude-desktop".into(), "update".into())]
    );
    let text = read(&h.paths.claude_desktop);
    let v: Value = serde_json::from_str(&text).unwrap();
    assert!(
        v["mcpServers"]["musaic"]["command"]
            .as_str()
            .unwrap()
            .ends_with("bin/musaic-cli")
    );
    assert_eq!(
        v["mcpServers"]["musaic"]["args"],
        serde_json::json!(["mcp", "--config-dir", "/some/config"])
    );
    assert!(
        text.find("\"first\"").unwrap() < text.find("\"musaic\"").unwrap()
            && text.find("\"musaic\"").unwrap() < text.find("\"last\"").unwrap(),
        "kept its place"
    );
    assert_eq!(
        read(&h.paths.claude_code),
        CODE,
        "the other host was not asked about"
    );
}

#[test]
fn a_foreign_entry_or_a_malformed_file_refuses_and_nothing_changes_anywhere() {
    let h = home();
    std::fs::write(
        &h.paths.claude_code,
        r#"{"mcpServers": {"musaic": {"command": "node", "args": ["server.js"]}}}"#,
    )
    .unwrap();
    let before_code = read(&h.paths.claude_code);
    let out = run(&opts(&h));
    assert_eq!(out.code, 1);
    assert_eq!(out.json["ok"], false);
    assert_eq!(
        actions(&out.json)[1],
        ("claude-code".into(), "refuse".into())
    );
    assert!(
        out.json["hosts"][1]["reason"]
            .as_str()
            .unwrap()
            .contains("Musaic did not write")
    );
    assert_eq!(
        read(&h.paths.claude_desktop),
        DESKTOP,
        "the good host is untouched too"
    );
    assert_eq!(read(&h.paths.claude_code), before_code);
    assert!(backups(&h.paths.claude_desktop).is_empty());

    for bad in [
        "{\"mcpServers\": ",
        "not json at all",
        "[1, 2]",
        r#"{"mcpServers": 5}"#,
    ] {
        let h = home();
        std::fs::write(&h.paths.claude_desktop, bad).unwrap();
        let mut o = opts(&h);
        o.host = "claude-desktop".into();
        let out = run(&o);
        assert_eq!(out.code, 1, "{bad}: {}", out.json);
        assert_eq!(
            actions(&out.json),
            [("claude-desktop".into(), "refuse".into())]
        );
        assert_eq!(read(&h.paths.claude_desktop), bad);
        assert!(backups(&h.paths.claude_desktop).is_empty());
    }
}

#[test]
fn an_interrupted_setup_restores_every_file_it_had_written() {
    let h = home();
    let mut o = opts(&h);
    o.fail_write_number = Some(1);
    let out = run(&o);
    assert_eq!(out.code, 1, "{}", out.json);
    assert_eq!(out.json["ok"], false);
    assert_eq!(
        read(&h.paths.claude_desktop),
        DESKTOP,
        "the first file was written, then restored exactly"
    );
    assert_eq!(read(&h.paths.claude_code), CODE);
    // A host file that did not exist is removed again, not left empty.
    let h = home();
    std::fs::remove_file(&h.paths.claude_code).unwrap();
    std::fs::write(&h.paths.claude_code, "{}").unwrap();
    let mut o = opts(&h);
    o.fail_write_number = Some(1);
    assert_eq!(run(&o).code, 1);
    assert_eq!(read(&h.paths.claude_desktop), DESKTOP);
    assert_eq!(read(&h.paths.claude_code), "{}");
}

#[test]
fn uninstall_removes_only_the_entry_setup_wrote() {
    let h = home();
    run(&opts(&h));
    let mut o = opts(&h);
    o.uninstall = true;
    let out = run(&o);
    assert_eq!(out.code, 0, "{}", out.json);
    assert_eq!(
        actions(&out.json),
        [
            ("claude-desktop".into(), "remove".into()),
            ("claude-code".into(), "remove".into())
        ]
    );
    for (path, original) in [
        (&h.paths.claude_desktop, DESKTOP),
        (&h.paths.claude_code, CODE),
    ] {
        let v: Value = serde_json::from_str(&read(path)).unwrap();
        let before: Value = serde_json::from_str(original).unwrap();
        assert_eq!(v, before, "back to what it was, value for value");
    }
    // A foreign entry is left alone; nothing to remove is not an error.
    let h = home();
    std::fs::write(
        &h.paths.claude_code,
        r#"{"mcpServers": {"musaic": {"command": "node", "args": ["x"]}}}"#,
    )
    .unwrap();
    let before = read(&h.paths.claude_code);
    let mut o = opts(&h);
    o.uninstall = true;
    let out = run(&o);
    assert_eq!(out.code, 0);
    assert_eq!(
        actions(&out.json),
        [
            ("claude-desktop".into(), "none".into()),
            ("claude-code".into(), "none".into())
        ]
    );
    assert_eq!(read(&h.paths.claude_code), before);
    assert!(backups(&h.paths.claude_code).is_empty());
}

#[test]
fn an_executable_inside_an_appimage_is_refused_with_a_remedy() {
    let h = home();
    let mut o = opts(&h);
    o.executable = Some(PathBuf::from("/tmp/.mount_MusaicAbc123/usr/bin/musaic-cli"));
    let out = run(&o);
    assert_eq!(out.code, 1);
    let reason = out.json["hosts"][0]["reason"].as_str().unwrap();
    assert!(
        reason.contains("AppImage") && reason.contains("copy it out"),
        "{reason}"
    );
    assert_eq!(read(&h.paths.claude_desktop), DESKTOP);
}

#[test]
fn a_host_that_is_not_installed_is_skipped_and_the_manual_entry_is_printed() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = HostPaths::under(tmp.path(), None);
    let mut o = Options::new(paths.clone());
    o.executable = Some(tmp.path().join("musaic-cli"));
    let out = run(&o);
    assert_eq!(out.code, 0);
    assert!(
        out.json["hosts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|h| h["detected"] == false && h["action"] == "none")
    );
    assert!(!paths.claude_code.exists() && !paths.claude_desktop.exists());
    assert_eq!(
        out.json["manualEntry"]["mcpServers"]["musaic"]["type"],
        "stdio"
    );
    // An unknown host name is a usage error.
    o.host = "vscode".into();
    assert_eq!(run(&o).code, 64);
}

#[test]
fn a_desktop_config_that_does_not_exist_yet_is_created_when_the_app_is_installed() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = HostPaths::under(tmp.path(), Some(&tmp.path().join("AppData/Roaming")));
    std::fs::create_dir_all(paths.claude_desktop.parent().unwrap()).unwrap();
    let mut o = Options::new(paths.clone());
    o.executable = Some(tmp.path().join("musaic-cli"));
    o.host = "claude-desktop".into();
    let out = run(&o);
    assert_eq!(out.code, 0, "{}", out.json);
    let v: Value = serde_json::from_str(&read(&paths.claude_desktop)).unwrap();
    assert_eq!(
        v["mcpServers"]["musaic"]["args"],
        serde_json::json!(["mcp"])
    );
    assert!(
        backups(&paths.claude_desktop).is_empty(),
        "there was nothing to back up"
    );
}
