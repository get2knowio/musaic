//! Where the CLI finds the pairing secret: the OS keyring (the real thing), a
//! credential file under a scratch directory (a development run, opt-in), or
//! memory (tests). The secret is read, used to prove possession and never
//! printed, logged or put in arguments.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use musaic_commands::auth::{AGENT_SERVICE, agent_account, hex_decode, index_account};
use musaic_commands::{ToolError, ToolErrorCode};

pub trait CredentialSource: Send + Sync {
    /// The pairing ids that may have a credential here.
    fn pairings(&self, config_dir_id: &str) -> Result<Vec<String>, ToolError>;
    /// The secret bytes of a pairing, if there is one.
    fn secret(&self, config_dir_id: &str, pairing_id: &str) -> Result<Option<Vec<u8>>, ToolError>;
}

fn unavailable(why: impl std::fmt::Display) -> ToolError {
    ToolError::new(
        ToolErrorCode::NotPaired,
        format!("the credential store is not available: {why}"),
    )
}

/// The OS keyring under service `musaic-agent`.
pub struct OsKeyring;

impl OsKeyring {
    fn read(account: &str) -> Result<Option<String>, ToolError> {
        let entry = keyring::Entry::new(AGENT_SERVICE, account).map_err(unavailable)?;
        match entry.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(unavailable(e)),
        }
    }
}

impl CredentialSource for OsKeyring {
    fn pairings(&self, config_dir_id: &str) -> Result<Vec<String>, ToolError> {
        Ok(Self::read(&index_account(config_dir_id))?
            .map(|v| {
                v.split(',')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default())
    }

    fn secret(&self, config_dir_id: &str, pairing_id: &str) -> Result<Option<Vec<u8>>, ToolError> {
        Ok(Self::read(&agent_account(config_dir_id, pairing_id))?
            .and_then(|h| hex_decode(h.trim())))
    }
}

/// `<config dir>/agent-dev-credentials/<pairing id>`: only for a config dir
/// under the system temp dir or a directory named `work`, and only when asked
/// for with `--dev-credentials`.
pub struct DevFile {
    dir: PathBuf,
}

impl DevFile {
    /// Refused (`NOT_PAIRED`) outside a scratch directory.
    pub fn new(config_dir: &Path) -> Result<Self, ToolError> {
        if !scratch_dir_ok(config_dir) {
            return Err(ToolError::new(
                ToolErrorCode::NotPaired,
                "--dev-credentials is only allowed for a config dir under the temp dir or a directory named `work`",
            ));
        }
        Ok(Self {
            dir: config_dir.join("agent-dev-credentials"),
        })
    }
}

impl CredentialSource for DevFile {
    fn pairings(&self, _config_dir_id: &str) -> Result<Vec<String>, ToolError> {
        let mut out: Vec<String> = std::fs::read_dir(&self.dir)
            .map(|rd| {
                rd.flatten()
                    .filter(|e| e.path().is_file())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        out.sort();
        Ok(out)
    }

    fn secret(&self, _config_dir_id: &str, pairing_id: &str) -> Result<Option<Vec<u8>>, ToolError> {
        // A pairing id is `p_` and hex; anything else is not a file name of ours.
        if pairing_id.contains(['/', '\\']) || pairing_id.contains("..") {
            return Ok(None);
        }
        Ok(std::fs::read_to_string(self.dir.join(pairing_id))
            .ok()
            .and_then(|h| hex_decode(h.trim())))
    }
}

/// In-memory credentials, for tests.
#[derive(Default)]
pub struct Memory {
    secrets: Mutex<BTreeMap<(String, String), Vec<u8>>>,
}

impl Memory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn put(&self, config_dir_id: &str, pairing_id: &str, secret: Vec<u8>) {
        self.secrets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert((config_dir_id.to_string(), pairing_id.to_string()), secret);
    }
}

impl CredentialSource for Memory {
    fn pairings(&self, config_dir_id: &str) -> Result<Vec<String>, ToolError> {
        Ok(self
            .secrets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keys()
            .filter(|(c, _)| c == config_dir_id)
            .map(|(_, p)| p.clone())
            .collect())
    }

    fn secret(&self, config_dir_id: &str, pairing_id: &str) -> Result<Option<Vec<u8>>, ToolError> {
        Ok(self
            .secrets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(config_dir_id.to_string(), pairing_id.to_string()))
            .cloned())
    }
}

/// The same rule as `musaic-api`'s `verification::scratch_dir_ok`: the config
/// dir is under the system temp dir or under a directory named `work`.
pub fn scratch_dir_ok(config_dir: &Path) -> bool {
    let mut existing = config_dir.to_path_buf();
    let mut rest = Vec::new();
    while !existing.exists() {
        let Some(name) = existing.file_name().map(ToOwned::to_owned) else {
            return false;
        };
        rest.push(name);
        if !existing.pop() {
            return false;
        }
    }
    let Ok(mut cfg) = std::fs::canonicalize(&existing) else {
        return false;
    };
    for n in rest.iter().rev() {
        cfg.push(n);
    }
    if let Ok(tmp) = std::fs::canonicalize(std::env::temp_dir())
        && cfg.starts_with(&tmp)
    {
        return true;
    }
    for t in ["/tmp", "/private/tmp", "/var/tmp", "/private/var/tmp"] {
        if cfg.starts_with(t) {
            return true;
        }
    }
    cfg.ancestors()
        .skip(1)
        .any(|a| a.file_name().is_some_and(|n| n == "work"))
}
