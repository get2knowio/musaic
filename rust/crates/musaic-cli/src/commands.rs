//! The CLI's commands as functions from input to output, so tests run them in
//! process. Every command but `mcp` ends in one JSON document on stdout.

use std::path::{Path, PathBuf};

use musaic_commands::{ToolError, ToolErrorCode, find_by_cli_name};
use serde_json::{Value, json};

use crate::client::Client;
use crate::credentials::{CredentialSource, DevFile, OsKeyring};

/// Exit codes.
pub const EXIT_OK: i32 = 0;
pub const EXIT_PROBLEMS: i32 = 1;
pub const EXIT_TOOL_ERROR: i32 = 2;
pub const EXIT_TRANSPORT: i32 = 3;
pub const EXIT_USAGE: i32 = 64;

/// What a command prints and how it exits.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub json: Value,
    pub code: i32,
}

pub fn failure(e: &ToolError) -> Outcome {
    let mut err = json!({"code": e.code.as_str(), "message": e.message});
    if let Some(d) = &e.details {
        err["details"] = json!(d);
    }
    if e.code == ToolErrorCode::AppNotRunning {
        err["remedy"] = json!("Start Musaic, then try again. The CLI never starts Musaic itself.");
    }
    Outcome {
        json: json!({"ok": false, "error": err}),
        code: if e.code.is_transport() {
            EXIT_TRANSPORT
        } else {
            EXIT_TOOL_ERROR
        },
    }
}

pub fn usage(message: &str) -> Outcome {
    Outcome {
        json: json!({"ok": false, "error": {"code": "INVALID_INPUT", "message": message}}),
        code: EXIT_USAGE,
    }
}

/// The config dir: `--config-dir`, then `MUSAIC_CONFIG_DIR`, then `~/.musaic`.
pub fn resolve_config_dir(flag: Option<&Path>) -> Option<PathBuf> {
    flag.map(Path::to_path_buf)
        .or_else(musaic_core::config::config_dir)
}

/// The credential source: the OS keyring, or the scratch-dir file with
/// `--dev-credentials`.
pub fn credential_source(
    config_dir: &Path,
    dev: bool,
) -> Result<Box<dyn CredentialSource>, ToolError> {
    if dev {
        Ok(Box::new(DevFile::new(config_dir)?))
    } else {
        Ok(Box::new(OsKeyring))
    }
}

/// `call <cli-name>`: the tool's result, or its error.
pub fn call(client: &Client, cli_name: &str, input: Option<Value>) -> Outcome {
    let Some(tool) = find_by_cli_name(cli_name) else {
        return usage(&format!(
            "no tool is called {cli_name}; `musaic-cli tools` lists them"
        ));
    };
    let input = input.unwrap_or_else(|| json!({}));
    if !input.is_object() {
        return usage("the input must be a JSON object");
    }
    match client.call(tool.id, &input) {
        Ok(result) => Outcome {
            json: json!({"ok": true, "result": result}),
            code: EXIT_OK,
        },
        Err(e) => failure(&e),
    }
}

/// `tools`: the catalog, from the code (no app needed).
pub fn tools() -> Outcome {
    Outcome {
        json: musaic_commands::catalog::catalog_with_digest(),
        code: EXIT_OK,
    }
}

/// `doctor`.
pub fn doctor(client: &Client, config_dir: &Path) -> Outcome {
    doctor_with(client, config_dir, &crate::setup::HostPaths::from_env())
}

/// `doctor` with the host locations given (tests use a temporary home).
pub fn doctor_with(client: &Client, config_dir: &Path, paths: &crate::setup::HostPaths) -> Outcome {
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "musaic-cli".into());
    let (json, healthy) = crate::doctor::report(client, config_dir, &exe, paths);
    Outcome {
        json,
        code: if healthy { EXIT_OK } else { EXIT_PROBLEMS },
    }
}
