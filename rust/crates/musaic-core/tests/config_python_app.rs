//! Config dir resolution and config.json, against
//! testdata/golden/persistence/config.json (exported from the Python app's
//! config.py, recorded by running it).

use std::ffi::OsStr;
use std::path::PathBuf;

use musaic_core::config::{AppConfig, ConfigDir, resolve_config_dir};
use serde_json::Value;

fn golden() -> Value {
    let p = musaic_testdata::public("golden/persistence/config.json");
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn canonical_tempdir() -> (tempfile::TempDir, PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().canonicalize().unwrap();
    (t, root)
}

/// MUSAIC_CONFIG_DIR wins when non-empty, else
/// ~/.musaic. The two legacy cases are a deliberate change: the
/// Python app honours MEDIA_MANAGER_CONFIG_DIR and renames ~/.media_manager; we
/// ignore both and never touch the home directory.
#[test]
fn config_dir_resolution_matches_the_python_app() {
    let g = golden();
    let (_t, root) = canonical_tempdir();
    let root_s = root.to_str().unwrap();
    let cases = g["resolution"].as_array().unwrap();
    assert_eq!(cases.len(), 9);
    for (i, case) in cases.iter().enumerate() {
        let label = case["label"].as_str().unwrap();
        let home = root.join(format!("home-{i}"));
        std::fs::create_dir(&home).unwrap();
        for d in case["home_dirs"].as_array().unwrap() {
            std::fs::create_dir(home.join(d.as_str().unwrap())).unwrap();
        }
        let env = case["env"]["MUSAIC_CONFIG_DIR"]
            .as_str()
            .map(|s| s.replace("<ROOT>", root_s));
        let got = resolve_config_dir(env.as_deref().map(OsStr::new), Some(&home)).unwrap();
        let got = got
            .to_str()
            .unwrap()
            .replace(home.to_str().unwrap(), "<HOME>")
            .replace(root_s, "<ROOT>");
        let python = case["result"].as_str().unwrap();
        let mut home_after: Vec<String> = std::fs::read_dir(&home)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        home_after.sort();
        match label {
            // Legacy env var dropped.
            "legacy env only" => {
                assert_eq!(python, "<ROOT>/legacy-env");
                assert_eq!(got, "<HOME>/.musaic", "{label}");
            }
            // ~/.media_manager is neither used nor renamed.
            "legacy dir only" => {
                assert_eq!(python, "<HOME>/.musaic");
                assert_eq!(case["home_after"], serde_json::json!([".musaic"]));
                assert_eq!(got, python, "{label}");
                assert_eq!(home_after, vec![".media_manager".to_owned()], "{label}");
            }
            _ => {
                assert_eq!(got, python, "{label}");
                assert_eq!(serde_json::json!(home_after), case["home_after"], "{label}");
            }
        }
    }
}

/// Unreadable / odd files load as the default config.
#[test]
fn load_matches_the_python_app() {
    let g = golden();
    let (_t, root) = canonical_tempdir();
    let cfg = ConfigDir::new(root.join("cfg"));
    std::fs::create_dir(cfg.dir()).unwrap();
    for case in g["load"].as_array().unwrap() {
        let label = case["label"].as_str().unwrap();
        let _ = std::fs::remove_file(cfg.config_file());
        if let Some(text) = case["file"].as_str() {
            std::fs::write(cfg.config_file(), text).unwrap();
        } else if let Some(hex) = case["file_bytes_hex"].as_str() {
            let bytes: Vec<u8> = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect();
            std::fs::write(cfg.config_file(), bytes).unwrap();
        }
        let got = cfg.load().library_dir;
        match label {
            // Deliberate: a number is not a path (Python passes 5 through).
            "number" => {
                assert_eq!(case["result"]["library_dir"], 5);
                assert_eq!(got, None);
            }
            // Deliberate: Python raises AttributeError (HTTP 500); we use the default.
            "array" => {
                assert_eq!(case["result"]["raises"], "AttributeError");
                assert_eq!(got, None);
            }
            _ => assert_eq!(
                serde_json::json!(got),
                case["result"]["library_dir"],
                "{label}"
            ),
        }
    }
}

/// set_library_dir normalises like Path.expanduser().resolve()
/// and writes byte-identical config.json (indent=2, ASCII escapes, no newline).
#[test]
fn set_library_dir_matches_the_python_app() {
    let g = golden();
    let s = &g["set_library_dir"];
    let (_t, root) = canonical_tempdir();
    let fs = root.join("fs");
    for d in s["fs_setup"]["dirs"].as_array().unwrap() {
        std::fs::create_dir_all(fs.join(d.as_str().unwrap())).unwrap();
    }
    #[cfg(unix)]
    for (link, target) in s["fs_setup"]["symlinks"].as_object().unwrap() {
        std::os::unix::fs::symlink(fs.join(target.as_str().unwrap()), fs.join(link)).unwrap();
    }
    let fs_s = fs.to_str().unwrap();
    let cfg = ConfigDir::new(root.join("cfg"));
    std::fs::create_dir(cfg.dir()).unwrap();
    std::fs::write(
        cfg.config_file(),
        s["fs_setup"]["config_before"].as_str().unwrap(),
    )
    .unwrap();
    for case in s["cases"].as_array().unwrap() {
        let label = case["label"].as_str().unwrap();
        let arg = case["arg"].as_str().unwrap().replace("<FS>", fs_s);
        let got = cfg.set_library_dir(&arg, Some(&fs)).unwrap();
        assert_eq!(
            got.library_dir.unwrap().replace(fs_s, "<FS>"),
            case["library_dir"].as_str().unwrap(),
            "{label}"
        );
        let text = std::fs::read_to_string(cfg.config_file()).unwrap();
        assert_eq!(
            text.replace(fs_s, "<FS>"),
            case["file_text"].as_str().unwrap(),
            "{label}"
        );
        assert_eq!(cfg.load(), AppConfig::parse(text.as_bytes()));
    }
    // Missing config dir is created.
    std::fs::remove_dir_all(cfg.dir()).unwrap();
    cfg.set_library_dir(&format!("{fs_s}/music/real"), Some(&fs))
        .unwrap();
    assert_eq!(
        cfg.config_file().is_file(),
        s["creates_missing_config_dir"].as_bool().unwrap()
    );
}

/// A config with no library dir serialises as Python's `null`.
#[test]
fn unset_library_dir_is_null() {
    // Shape taken from the Python app's file_text cases: same layout, null value.
    let g = golden();
    let sample = g["set_library_dir"]["cases"][0]["file_text"]
        .as_str()
        .unwrap();
    let prefix = sample.split(": ").next().unwrap();
    assert_eq!(
        AppConfig::default().to_json(),
        format!("{prefix}: null\n}}")
    );
}
