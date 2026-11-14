//! Verification-only switches of the server binary (and the desktop app's
//! dev switch), kept in the library so the rules are unit-tested.
//!
//! `--verification-memory-secrets[=MODE]` replaces the OS keyring with an
//! in-memory store. `MODE` reproduces the keyring's failures without the OS
//! keychain, so the 503 answer for a failing keyring and the settings'
//! `keyring_available: false` can be observed by end-to-end test runs:
//!
//! * (no value): a working in-memory store;
//! * `unavailable`: every call fails as a missing keyring backend
//!   (`MemoryStore::unavailable()`): `keyring_available: false`, and a
//!   secret write answers 503;
//! * `fail-set:<key>`: writing `<key>` (one of
//!   [`musaic_secrets::SECRET_KEYS`]) fails with a backend error (503);
//!   everything else works.
//!
//! How it cannot be enabled by accident in a normal launch:
//!
//! * a mode is only a value of the verification switch itself (written
//!   `--verification-memory-secrets=MODE`, one argument): there is no
//!   environment variable, no settings key and no separate flag that could
//!   be left set;
//! * the switch, with or without a mode, is refused (exit 2, before
//!   anything is created) unless the config dir is under the system temp
//!   dir or under a directory named `work` ([`scratch_dir_ok`]), so it can
//!   never run against a real `~/.musaic`;
//! * an unknown mode or a key that is not a secret key is a usage error,
//!   never a silent fallback;
//! * the start-up log line names the mode at WARNING.

use std::path::Path;

use musaic_secrets::MemoryStore;

/// The switch.
pub const MEMORY_SECRETS_FLAG: &str = "--verification-memory-secrets";

/// What `--verification-memory-secrets[=MODE]` selects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemorySecrets {
    Working,
    Unavailable,
    FailSet(String),
}

impl MemorySecrets {
    /// The mode from the switch's value (`None`: the bare switch).
    pub fn parse(value: Option<&str>) -> Result<MemorySecrets, String> {
        let Some(v) = value else {
            return Ok(MemorySecrets::Working);
        };
        if v == "unavailable" {
            return Ok(MemorySecrets::Unavailable);
        }
        if let Some(key) = v.strip_prefix("fail-set:") {
            if musaic_secrets::SECRET_KEYS.contains(&key) {
                return Ok(MemorySecrets::FailSet(key.to_string()));
            }
            return Err(format!(
                "{MEMORY_SECRETS_FLAG}=fail-set:{key:?}: not a secret key ({})",
                musaic_secrets::SECRET_KEYS.join(", ")
            ));
        }
        Err(format!(
            "{MEMORY_SECRETS_FLAG}={v:?}: unknown mode (unavailable | fail-set:<key>)"
        ))
    }

    /// The store this mode builds.
    pub fn store(&self) -> MemoryStore {
        match self {
            MemorySecrets::Working => MemoryStore::new(),
            MemorySecrets::Unavailable => MemoryStore::unavailable(),
            MemorySecrets::FailSet(k) => {
                let s = MemoryStore::new();
                s.fail_set_for(k);
                s
            }
        }
    }

    /// For the start-up log line.
    pub fn describe(&self) -> String {
        match self {
            MemorySecrets::Working => "in memory (verification)".into(),
            MemorySecrets::Unavailable => "in memory, keyring unavailable (verification)".into(),
            MemorySecrets::FailSet(k) => format!("in memory, writes of {k} fail (verification)"),
        }
    }
}

/// The verification / dev secret switches are allowed only for a config
/// dir under the system temp dir or under a directory named `work`.
/// Nothing is created: the nearest existing ancestor is canonicalised.
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

#[cfg(test)]
mod tests {
    use super::*;
    use musaic_secrets::{SecretStore, StoreError};

    // The modes and their refusals.
    #[test]
    fn modes_parse_and_refuse() {
        assert_eq!(MemorySecrets::parse(None), Ok(MemorySecrets::Working));
        assert_eq!(
            MemorySecrets::parse(Some("unavailable")),
            Ok(MemorySecrets::Unavailable)
        );
        assert_eq!(
            MemorySecrets::parse(Some("fail-set:acoustid_api_key")),
            Ok(MemorySecrets::FailSet("acoustid_api_key".into()))
        );
        for bad in ["", "probe", "fail-set:", "fail-set:nope", "Unavailable"] {
            assert!(MemorySecrets::parse(Some(bad)).is_err(), "{bad}");
        }
    }

    // What each mode's store does.
    #[test]
    fn mode_stores_fail_as_named() {
        let s = MemorySecrets::Unavailable.store();
        assert!(matches!(
            s.get("ai_api_key"),
            Err(StoreError::Unavailable(_))
        ));
        let s = MemorySecrets::FailSet("acoustid_api_key".into()).store();
        assert!(s.set("acoustid_api_key", "x").is_err());
        assert!(s.set("ai_api_key", "x").is_ok());
        assert!(
            MemorySecrets::Working
                .store()
                .set("ai_api_key", "x")
                .is_ok()
        );
    }

    #[test]
    fn scratch_dirs() {
        let td = tempfile::tempdir().unwrap();
        assert!(scratch_dir_ok(&td.path().join("a/b")));
        assert!(!scratch_dir_ok(Path::new("/")));
    }
}
