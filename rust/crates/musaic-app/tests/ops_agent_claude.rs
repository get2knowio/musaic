//! Connect Claude Desktop (spec 007): the readiness report in every state, the
//! bundle and the skill archive. Every test injects a scratch home and a
//! scratch temporary root; nothing reads a real Claude directory.

mod common;

use std::io::Read;
use std::path::{Path, PathBuf};

use common::{AgentWorld, agent_world};
use musaic_app::agent::claude_desktop::{
    self as cd, BundleShape, CliState, Command, HostEnv, Route, Unstable,
};
use musaic_app::ops::agent::HandOffError;
use musaic_commands::PermissionKind as K;
use musaic_commands::claude_desktop::{EntryOwner, Platform, config_file};
use musaic_commands::events::HostObservation;

struct Setup {
    tmp: tempfile::TempDir,
}

impl Setup {
    fn new() -> Self {
        Self {
            tmp: tempfile::tempdir().unwrap(),
        }
    }
    fn p(&self, rel: &str) -> PathBuf {
        self.tmp.path().join(rel)
    }
    fn env(&self, platform: Platform) -> HostEnv {
        let exe = self.p("app/Musaic");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(exe.parent().unwrap().join(program_name(platform)), b"cli").unwrap();
        std::fs::create_dir_all(self.p("tmproot")).unwrap();
        HostEnv {
            platform,
            home: Some(self.p("home")),
            appdata: Some(self.p("home/AppData/Roaming")),
            local_appdata: Some(self.p("home/AppData/Local")),
            application_dirs: vec![self.p("Applications")],
            executable: Some(exe),
            temp_root: self.p("tmproot"),
            allow_temporary_program: false,
            non_default_config_dir: None,
            cleanup: false,
            // Scratch trees are never under `/Volumes/`; a fixed answer keeps
            // the real mount out of the tests.
            mount: |_| cd::MountKind::Unknown,
        }
    }
    /// Claude Desktop "installed": its configuration directory exists.
    fn claude(&self, env: &HostEnv, config: Option<&str>) {
        let file = env.config_file().unwrap();
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        if let Some(text) = config {
            std::fs::write(file, text).unwrap();
        }
    }
}

fn program_name(platform: Platform) -> &'static str {
    if platform == Platform::Win32 {
        "musaic-cli.exe"
    } else {
        "musaic-cli"
    }
}

fn app_with_env(env: HostEnv) -> (musaic_app::App, common::Scratch) {
    common::app_with(move |b| b.host_env(env))
}

fn canonical(p: &Path) -> String {
    musaic_core::config::resolve_path(p)
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

const OURS: &str = r#"{"mcpServers":{"musaic":{"command":"/x/musaic-cli","args":["mcp"]}}}"#;
const FOREIGN: &str = r#"{"mcpServers":{"musaic":{"command":"/x/other","args":["serve"]}}}"#;

fn read_zip(bytes: &[u8]) -> Vec<(String, Vec<u8>, Option<u32>)> {
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).unwrap();
    (0..z.len())
        .map(|i| {
            let mut f = z.by_index(i).unwrap();
            let mut out = Vec::new();
            f.read_to_end(&mut out).unwrap();
            (f.name().unwrap().into_owned(), out, f.unix_mode())
        })
        .collect()
}

fn manifest_of(bundle: &Path) -> serde_json::Value {
    let entries = read_zip(&std::fs::read(bundle).unwrap());
    let m = entries.iter().find(|e| e.0 == "manifest.json").unwrap();
    serde_json::from_slice(&m.1).unwrap()
}

#[test]
fn every_readiness_state_is_reported() {
    // Claude Desktop absent.
    let s = Setup::new();
    let env = s.env(Platform::Darwin);
    let (app, _g) = app_with_env(env);
    let st = app.ops().agent().claude_desktop_status();
    assert!(!st.claude_found);
    assert_eq!(st.route, Route::Blocked);
    assert!(st.bundle_supported);

    // Linux: configuration file only.
    let s = Setup::new();
    let env = s.env(Platform::Linux);
    s.claude(&env, None);
    let (app, _g) = app_with_env(env);
    let st = app.ops().agent().claude_desktop_status();
    assert!(!st.bundle_supported);
    assert_eq!(st.route, Route::ConfigFileOnly);
    let setup = st.setup_command.expect("the configuration-file command");
    assert_eq!(setup.args, ["setup", "--host", "claude-desktop"]);

    // Our own entry, a foreign entry, an unreadable file.
    for (text, owner, route) in [
        (OURS, EntryOwner::Ours, Route::ConfigFileOnly),
        (FOREIGN, EntryOwner::Foreign, Route::Blocked),
        ("{not json", EntryOwner::Unreadable, Route::Blocked),
    ] {
        let s = Setup::new();
        let env = s.env(Platform::Darwin);
        s.claude(&env, Some(text));
        let (app, _g) = app_with_env(env);
        let st = app.ops().agent().claude_desktop_status();
        assert_eq!(st.config_entry, owner, "{text}");
        assert_eq!(st.route, route, "{text}");
        assert_eq!(st.uninstall_command.is_some(), owner == EntryOwner::Ours);
    }

    // Program missing.
    let s = Setup::new();
    let env = s.env(Platform::Darwin);
    s.claude(&env, None);
    std::fs::remove_file(s.p("app/musaic-cli")).unwrap();
    let (app, _g) = app_with_env(env);
    let st = app.ops().agent().claude_desktop_status();
    assert_eq!(st.cli, CliState::Missing);
    assert_eq!(st.route, Route::Blocked);

    // Unstable location: the program sits under the temporary root.
    let s = Setup::new();
    let mut env = s.env(Platform::Darwin);
    let exe = s.p("tmproot/run/Musaic");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(exe.parent().unwrap().join("musaic-cli"), b"cli").unwrap();
    env.executable = Some(exe);
    s.claude(&env, None);
    let (app, _g) = app_with_env(env.clone());
    let st = app.ops().agent().claude_desktop_status();
    assert_eq!(st.cli, CliState::Unstable(Unstable::Temporary));
    assert_eq!(st.route, Route::Blocked);
    // The environment may allow it (tests of a bundle written from a scratch tree).
    env.allow_temporary_program = true;
    let (app, _g) = app_with_env(env);
    assert_eq!(
        app.ops().agent().claude_desktop_status().route,
        Route::Bundle
    );

    // Ready.
    let s = Setup::new();
    let env = s.env(Platform::Darwin);
    s.claude(&env, None);
    let (app, _g) = app_with_env(env);
    let st = app.ops().agent().claude_desktop_status();
    assert_eq!(st.route, Route::Bundle);
    assert_eq!(st.shape, BundleShape::Installed);
    assert_eq!(st.app_version, env!("CARGO_PKG_VERSION"));
}

#[test]
fn the_disk_image_and_app_image_locations_are_refused() {
    let s = Setup::new();
    let env = s.env(Platform::Darwin);
    // The path text decides (and, under `/Volumes/`, the mount: the module's
    // own tests cover that); build the answer through the public rule.
    for (dir, want) in [
        ("Volumes/Musaic", Unstable::DiskImage),
        (".mount_Musaic1/usr/bin", Unstable::AppImage),
        ("AppTranslocation/ABC/d/Musaic.app", Unstable::Translocated),
    ] {
        let mut e = env.clone();
        // These locations are not creatable; the rule is exercised on the
        // canonical path of a directory whose name carries the marker.
        let root = s.p(&format!("x/{dir}"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("musaic-cli"), b"cli").unwrap();
        e.executable = Some(root.join("Musaic"));
        let got = e.cli_location();
        if dir.starts_with("Volumes") {
            // Not at the start of the path in a scratch tree: found.
            assert!(matches!(got, cd::CliLocation::Found(_)), "{dir}");
        } else {
            assert_eq!(got, cd::CliLocation::Unstable(want), "{dir}");
        }
    }
}

#[test]
fn the_command_shown_is_the_command_written_for_both_shapes() {
    let s = Setup::new();
    let env = s.env(Platform::Darwin);
    s.claude(&env, None);
    let (app, _g) = app_with_env(env.clone());
    let st = app.ops().agent().claude_desktop_status();
    let CliState::Found(shown) = st.cli else {
        panic!("found")
    };
    let prepared = app.ops().agent().claude_desktop_bundle().unwrap();
    assert_eq!(prepared.command, shown);
    let m = manifest_of(&prepared.path);
    assert_eq!(m["server"]["mcp_config"]["command"], shown.program.as_str());
    assert_eq!(
        m["server"]["mcp_config"]["args"],
        serde_json::json!(shown.args)
    );
    assert_eq!(m["server"]["entry_point"], shown.program.as_str());
    assert_eq!(shown.program, canonical(&s.p("app/musaic-cli")));
    app.ops().agent().discard_claude_desktop_bundle(&prepared);

    // The self-contained shape: the command is inside the bundle.
    for platform in [Platform::Darwin, Platform::Win32] {
        let env = s.env(platform);
        let CliState::Found(_) = cd::status(&env, "1.0.0").cli else {
            panic!("found")
        };
        let program = s.p(&format!("app/{}", program_name(platform)));
        let command = cd::bundle_command(&env, BundleShape::SelfContained, &program);
        let bytes = cd::write_bundle(
            &command,
            BundleShape::SelfContained,
            "1.0.0",
            platform,
            &program,
        )
        .unwrap();
        let entries = read_zip(&bytes);
        let names: Vec<&str> = entries.iter().map(|e| e.0.as_str()).collect();
        let entry = if platform == Platform::Win32 {
            "server/musaic-cli.exe"
        } else {
            "server/musaic-cli"
        };
        assert_eq!(names, ["manifest.json", "icon.png", entry]);
        assert_eq!(
            entries[2].2.map(|m| m & 0o111),
            Some(0o111),
            "executable bit"
        );
        assert_eq!(entries[2].1, b"cli");
        let m: serde_json::Value = serde_json::from_slice(&entries[0].1).unwrap();
        assert_eq!(m["server"]["entry_point"], entry);
        assert_eq!(
            m["server"]["mcp_config"]["command"],
            format!("${{__dirname}}/{entry}")
        );
        assert_eq!(
            m["server"]["mcp_config"]["command"],
            command.program.as_str()
        );
    }
}

#[test]
fn config_dir_appears_only_for_a_non_default_directory() {
    let s = Setup::new();
    let mut env = s.env(Platform::Darwin);
    s.claude(&env, None);
    let program = s.p("app/musaic-cli");
    let plain = cd::bundle_command(&env, BundleShape::Installed, &program);
    assert_eq!(plain.args, ["mcp"]);
    assert_eq!(
        cd::status(&env, "1").setup_command.unwrap().args,
        ["setup", "--host", "claude-desktop"]
    );
    env.non_default_config_dir = Some(s.p("my config"));
    let with = cd::bundle_command(&env, BundleShape::Installed, &program);
    assert_eq!(
        with.args,
        [
            "mcp".to_string(),
            "--config-dir".into(),
            s.p("my config").to_string_lossy().into_owned()
        ]
    );
    assert_eq!(
        cd::status(&env, "1").setup_command.unwrap().args[3],
        "--config-dir"
    );
}

#[test]
fn a_program_location_with_spaces_quotes_and_non_ascii_is_one_argument() {
    let s = Setup::new();
    let mut env = s.env(Platform::Darwin);
    let dir = s.p("Mü sic \"Q\"/App");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("musaic-cli"), b"cli").unwrap();
    env.executable = Some(dir.join("Musaic"));
    env.non_default_config_dir = Some(s.p("dir with space/é"));
    s.claude(&env, None);
    let (app, _g) = app_with_env(env);
    let prepared = app.ops().agent().claude_desktop_bundle().unwrap();
    let m = manifest_of(&prepared.path);
    assert_eq!(
        m["server"]["mcp_config"]["command"],
        canonical(&dir.join("musaic-cli")).as_str()
    );
    let args = m["server"]["mcp_config"]["args"].as_array().unwrap();
    assert_eq!(args.len(), 3);
    assert_eq!(args[2], s.p("dir with space/é").to_string_lossy().as_ref());
    app.ops().agent().discard_claude_desktop_bundle(&prepared);
}

#[test]
fn the_manifest_has_the_required_fields_and_nothing_configurable() {
    let s = Setup::new();
    let env = s.env(Platform::Darwin);
    s.claude(&env, None);
    let (app, _g) = app_with_env(env);
    let prepared = app.ops().agent().claude_desktop_bundle().unwrap();
    let m = manifest_of(&prepared.path);
    assert_eq!(m["manifest_version"], "0.3");
    assert_eq!(m["name"], "musaic");
    assert_eq!(m["version"], env!("CARGO_PKG_VERSION"));
    assert!(m["description"].as_str().is_some_and(|d| !d.is_empty()));
    assert!(m["author"]["name"].as_str().is_some());
    assert_eq!(m["server"]["type"], "binary");
    assert_eq!(m["icon"], "icon.png");
    assert_eq!(
        m["compatibility"]["platforms"],
        serde_json::json!(["darwin"])
    );
    assert!(m.get("user_config").is_none());
    assert!(m["server"]["mcp_config"].get("env").is_none());
    let names: Vec<String> = read_zip(&std::fs::read(&prepared.path).unwrap())
        .into_iter()
        .map(|e| e.0)
        .collect();
    assert_eq!(names, ["manifest.json", "icon.png"]);
    app.ops().agent().discard_claude_desktop_bundle(&prepared);
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn no_generated_byte_holds_a_secret_of_the_fixture() {
    let s = Setup::new();
    let env = s.env(Platform::Darwin);
    s.claude(&env, None);
    // Same app, with the scratch machine.
    let world_env = env.clone();
    let clock = common::FakeClock::new();
    let store = std::sync::Arc::new(musaic_secrets::MemoryStore::new());
    let (c2, s2) = (clock.clone(), store.clone());
    let (app, _g) =
        common::app_with(move |b| b.clock(c2.wall()).agent_secrets(s2).host_env(world_env));
    let pairing = app
        .ops()
        .agent()
        .create_pairing("Claude", &[K::LibraryRead])
        .unwrap()
        .pairing
        .id;
    let secret_hex =
        musaic_secrets::agent::get_secret(store.as_ref(), &common::config_id(&app), &pairing)
            .unwrap()
            .unwrap();
    let secret_raw = musaic_commands::auth::hex_decode(&secret_hex).unwrap();
    let prepared = app.ops().agent().claude_desktop_bundle().unwrap();
    let file = std::fs::read(&prepared.path).unwrap();
    let mut all = vec![file.clone()];
    all.extend(read_zip(&file).into_iter().map(|e| e.1));
    let sha = {
        use sha2::{Digest, Sha256};
        musaic_commands::auth::hex_encode(&Sha256::digest(&secret_raw))
    };
    let dir_name = prepared
        .path
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    for bytes in &all {
        for needle in [
            secret_hex.as_bytes(),
            &secret_raw[..],
            sha.as_bytes(),
            pairing.as_bytes(),
        ] {
            assert!(
                !contains(bytes, needle),
                "a secret or the pairing id is in the bundle"
            );
        }
    }
    assert!(!contains(dir_name.as_bytes(), secret_hex.as_bytes()));
    app.ops().agent().discard_claude_desktop_bundle(&prepared);
}

fn snapshot(root: &Path) -> Vec<(String, u64)> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, u64)>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            let md = std::fs::symlink_metadata(&p).unwrap();
            out.push((rel, if md.is_file() { md.len() } else { 0 }));
            if md.is_dir() {
                walk(&p, root, out);
            }
        }
    }
    let mut v = Vec::new();
    walk(root, root, &mut v);
    v.sort();
    v
}

#[test]
fn a_hand_off_writes_only_under_the_temporary_root() {
    let s = Setup::new();
    let env = s.env(Platform::Darwin);
    s.claude(&env, Some(r#"{"mcpServers":{"other":{"command":"x"}}}"#));
    let (app, scratch) = app_with_env(env);
    let before_home = snapshot(&s.p("home"));
    let before_config = snapshot(&scratch.config);
    let before_app = snapshot(&s.p("app"));
    let prepared = app.ops().agent().claude_desktop_bundle().unwrap();
    assert!(canonical_or(&prepared.path).starts_with(canonical_or(&s.p("tmproot"))));
    assert_eq!(prepared.path.file_name().unwrap(), "Musaic.mcpb");
    assert!(
        prepared
            .path
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("musaic-connect-")
    );
    assert_eq!(
        snapshot(&s.p("home")),
        before_home,
        "Claude's files untouched"
    );
    assert_eq!(
        snapshot(&scratch.config),
        before_config,
        "the configuration directory untouched"
    );
    assert_eq!(snapshot(&s.p("app")), before_app);
    let after_tmp = snapshot(&s.p("tmproot"));
    assert_eq!(
        after_tmp.len(),
        2,
        "one directory holding one file: {after_tmp:?}"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let dir = prepared.path.parent().unwrap();
        assert_eq!(
            std::fs::metadata(dir).unwrap().permissions().mode() & 0o077,
            0,
            "private directory"
        );
        assert_eq!(
            std::fs::metadata(&prepared.path)
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
    }
    app.ops().agent().discard_claude_desktop_bundle(&prepared);
    assert!(
        snapshot(&s.p("tmproot")).is_empty(),
        "the directory is removed with the file"
    );
}

fn canonical_or(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

#[test]
fn a_hand_off_is_refused_unless_the_route_is_the_bundle_and_one_at_a_time() {
    let s = Setup::new();
    let env = s.env(Platform::Darwin);
    s.claude(&env, Some(OURS));
    let (app, _g) = app_with_env(env);
    let err = app.ops().agent().claude_desktop_bundle().unwrap_err();
    assert!(matches!(err, HandOffError::NotReady(_)), "{err:?}");
    assert!(snapshot(&s.p("tmproot")).is_empty(), "nothing created");

    let s = Setup::new();
    let env = s.env(Platform::Darwin);
    s.claude(&env, None);
    let (app, _g) = app_with_env(env);
    app.0
        .hand_off_busy
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        app.ops().agent().claude_desktop_bundle().unwrap_err(),
        HandOffError::Busy
    );
    assert!(snapshot(&s.p("tmproot")).is_empty());
    app.0
        .hand_off_busy
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let p = app.ops().agent().claude_desktop_bundle().unwrap();
    app.ops().agent().discard_claude_desktop_bundle(&p);
}

#[test]
fn sweep_removes_only_what_it_made_and_only_when_old_enough() {
    let s = Setup::new();
    let root = s.p("root");
    let mk = |name: &str, files: &[&str]| {
        let d = root.join(name);
        std::fs::create_dir_all(&d).unwrap();
        for f in files {
            std::fs::write(d.join(f), b"x").unwrap();
        }
    };
    mk("musaic-connect-a", &["Musaic.mcpb"]);
    mk("musaic-connect-b", &["Musaic.mcpb", "extra.txt"]);
    mk("other-a", &["Musaic.mcpb"]);
    mk("musaic-connect-empty", &[]);
    let now = std::time::SystemTime::now();
    // Too young: nothing goes.
    assert_eq!(cd::sweep(&root, Some(cd::MAX_AGE), now), 0);
    // Old enough: only the prefix directories holding nothing but the file.
    let later = now + std::time::Duration::from_secs(2 * 60 * 60);
    assert_eq!(cd::sweep(&root, Some(cd::MAX_AGE), later), 2);
    let left: Vec<String> = snapshot(&root).into_iter().map(|e| e.0).collect();
    assert!(left.contains(&"musaic-connect-b".to_string()));
    assert!(left.contains(&"other-a".to_string()));
    assert!(!left.contains(&"musaic-connect-a".to_string()));
    // At startup everything of ours goes regardless of age.
    mk("musaic-connect-c", &["Musaic.mcpb"]);
    assert_eq!(cd::sweep(&root, None, now), 1);
    assert!(root.join("musaic-connect-b/extra.txt").exists());
}

#[test]
fn the_skill_archive_is_stable_and_holds_exactly_the_served_files() {
    let (app, _g) = common::app();
    let a = app.ops().agent().skill_archive().unwrap();
    let b = app.ops().agent().skill_archive().unwrap();
    assert_eq!(a.bytes, b.bytes, "the same bytes every time");
    let version = musaic_commands::skill::bundle().version().to_string();
    assert_eq!(a.file_name, format!("musaic-work-delegation-{version}.zip"));
    let entries = read_zip(&a.bytes);
    assert_eq!(entries.len(), 9);
    assert_eq!(entries[0].0, "musaic-work-delegation/SKILL.md");
    let lock: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../skills/musaic-work-delegation/skill.lock.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let files = lock["files"].as_array().unwrap();
    assert_eq!(files.len(), entries.len());
    for (name, bytes, _) in &entries {
        let path = name
            .strip_prefix("musaic-work-delegation/")
            .expect("one folder");
        let want = files
            .iter()
            .find(|f| f["path"] == path)
            .unwrap_or_else(|| panic!("{path}"));
        assert_eq!(
            want["digest"],
            musaic_commands::skill::digest_of(bytes).as_str(),
            "{path}"
        );
        assert_eq!(want["size"], bytes.len());
    }
}

fn observe(w: &AgentWorld, id: &str, cli: Option<&str>) {
    let p = w.login(id);
    w.app.ops().agent().observe_host(
        &p,
        &HostObservation {
            client_name: "Claude".into(),
            client_version: "1".into(),
            profile: "2026-07-28".into(),
            tasks: false,
            skills: false,
            events: false,
            cli_version: cli.map(str::to_string),
        },
    );
}

#[test]
fn the_reported_program_version_is_shown_and_compared() {
    let w = agent_world();
    let id = w.pair("Claude", &[K::LibraryRead]).pairing.id;
    let diag = |w: &AgentWorld| {
        w.app
            .ops()
            .agent()
            .diagnostics()
            .unwrap()
            .into_iter()
            .find(|d| d.pairing_id == id)
            .unwrap()
    };
    assert_eq!(diag(&w).cli_version, None);
    observe(&w, &id, Some("0.1.0"));
    let d = diag(&w);
    assert_eq!(
        (d.cli_version.as_deref(), d.cli_older),
        (Some("0.1.0"), Some(true))
    );
    observe(&w, &id, Some(env!("CARGO_PKG_VERSION")));
    assert_eq!(diag(&w).cli_older, Some(false));
    observe(&w, &id, Some("99.0.0"));
    assert_eq!(diag(&w).cli_older, Some(false));
    observe(&w, &id, Some("dev build"));
    let d = diag(&w);
    assert_eq!(
        (d.cli_version.as_deref(), d.cli_older),
        (Some("dev build"), None)
    );
    // A program that does not report leaves nothing shown.
    observe(&w, &id, None);
    assert_eq!(diag(&w).cli_version, None);
    observe(&w, &id, Some("0.1.0"));
    w.app.ops().agent().revoke_pairing(&id).unwrap();
    assert_eq!(diag(&w).cli_version, None, "cleared on revoke");
}

#[test]
fn the_shared_config_location_rule_matches_the_environment() {
    let s = Setup::new();
    let env = s.env(Platform::Win32);
    assert_eq!(
        env.config_file().unwrap(),
        config_file(
            Platform::Win32,
            &s.p("home"),
            Some(&s.p("home/AppData/Roaming"))
        )
    );
    let _ = Command {
        program: String::new(),
        args: vec![],
    };
}

#[test]
fn the_extension_is_detected_from_its_folder_and_does_not_change_the_route() {
    use musaic_commands::claude_desktop::{
        BUNDLE_AUTHOR, BUNDLE_NAME, ExtensionState, extension_dir,
    };
    let s = Setup::new();
    let env = s.env(Platform::Darwin);
    s.claude(&env, None);
    let home = env.home.clone().unwrap();
    let (app, _g) = app_with_env(env.clone());
    let st = app.ops().agent().claude_desktop_status();
    assert_eq!(st.extension, ExtensionState::NotDetected);
    assert_eq!(st.route, Route::Bundle);

    std::fs::create_dir_all(extension_dir(Platform::Darwin, &home, None)).unwrap();
    let st = app.ops().agent().claude_desktop_status();
    assert_eq!(st.extension, ExtensionState::Detected);
    assert_eq!(
        st.route,
        Route::Bundle,
        "an installed extension does not block reinstalling"
    );
    let p = app.ops().agent().claude_desktop_bundle().unwrap();
    app.ops().agent().discard_claude_desktop_bundle(&p);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let support = env.config_file().unwrap().parent().unwrap().to_path_buf();
        std::fs::set_permissions(&support, std::fs::Permissions::from_mode(0o000)).unwrap();
        let got = app.ops().agent().claude_desktop_status().extension;
        std::fs::set_permissions(&support, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            matches!(got, ExtensionState::Unknown | ExtensionState::Detected),
            "{got:?}"
        );
    }

    // The folder name and the manifest cannot drift: both come from the constants.
    let manifest: serde_json::Value = serde_json::from_str(&cd::manifest(
        &Command {
            program: "/x".into(),
            args: vec!["mcp".into()],
        },
        BundleShape::Installed,
        "1.0.0",
        Platform::Darwin,
    ))
    .unwrap();
    assert_eq!(manifest["name"], BUNDLE_NAME);
    assert_eq!(manifest["author"]["name"], BUNDLE_AUTHOR);
    let folder = extension_dir(Platform::Darwin, &home, None);
    assert_eq!(
        folder.file_name().unwrap().to_string_lossy(),
        format!(
            "local.mcpb.{}.{}",
            manifest["author"]["name"].as_str().unwrap().to_lowercase(),
            manifest["name"].as_str().unwrap()
        )
    );
}
