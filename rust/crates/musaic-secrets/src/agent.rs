//! Where an agent pairing's secret lives: the OS keyring under service
//! `musaic-agent`, one entry per pairing, account `<configDirId>:<pairingId>`
//! (so two config dirs never share a credential). The value is the hex of the
//! 32 random bytes. Nothing here writes a secret to SQLite or to a file; the
//! scratch-dir credential file of a development run is the app's, not this
//! crate's.

pub use musaic_commands::auth::{AGENT_SERVICE, agent_account, config_dir_id, index_account};

use crate::{OsKeyring, SecretStore, StoreError};

/// The keyring the app uses for agent secrets.
pub fn agent_keyring() -> OsKeyring {
    OsKeyring::new(AGENT_SERVICE)
}

/// Store a pairing's secret (hex) in `store`.
pub fn put_secret(
    store: &dyn SecretStore,
    config_dir_id: &str,
    pairing_id: &str,
    secret_hex: &str,
) -> Result<(), StoreError> {
    store.set(&agent_account(config_dir_id, pairing_id), secret_hex)
}

/// A pairing's secret (hex), if its entry exists.
pub fn get_secret(
    store: &dyn SecretStore,
    config_dir_id: &str,
    pairing_id: &str,
) -> Result<Option<String>, StoreError> {
    store.get(&agent_account(config_dir_id, pairing_id))
}

/// Remove a pairing's entry (absent is fine).
pub fn delete_secret(
    store: &dyn SecretStore,
    config_dir_id: &str,
    pairing_id: &str,
) -> Result<(), StoreError> {
    store.delete(&agent_account(config_dir_id, pairing_id))
}

/// Record which pairings are active for this config dir, for the CLI to
/// find its candidates (ids only). An empty list removes the entry.
pub fn put_index(
    store: &dyn SecretStore,
    config_dir_id: &str,
    pairing_ids: &[String],
) -> Result<(), StoreError> {
    let account = index_account(config_dir_id);
    if pairing_ids.is_empty() {
        store.delete(&account)
    } else {
        store.set(&account, &pairing_ids.join(","))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryStore;

    #[test]
    fn secrets_are_per_config_dir_and_pairing() {
        let s = MemoryStore::new();
        put_secret(&s, "aaaaaaaaaaaaaaaa", "p_1", "abcd").unwrap();
        assert_eq!(
            get_secret(&s, "aaaaaaaaaaaaaaaa", "p_1")
                .unwrap()
                .as_deref(),
            Some("abcd")
        );
        assert_eq!(get_secret(&s, "bbbbbbbbbbbbbbbb", "p_1").unwrap(), None);
        assert_eq!(get_secret(&s, "aaaaaaaaaaaaaaaa", "p_2").unwrap(), None);
        delete_secret(&s, "aaaaaaaaaaaaaaaa", "p_1").unwrap();
        delete_secret(&s, "aaaaaaaaaaaaaaaa", "p_1").unwrap();
        assert_eq!(get_secret(&s, "aaaaaaaaaaaaaaaa", "p_1").unwrap(), None);
        assert_eq!(agent_keyring().service(), "musaic-agent");
    }
}
