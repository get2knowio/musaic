//! Secrets store.
//!
//! The Python app keeps three API keys as plaintext rows in the `settings`
//! table, so they travel into every backup and snapshot. This build keeps
//! the one it still uses, [`MIGRATED_SECRET_KEYS`] (the AcoustID key), in the
//! OS keyring instead; the two it no longer uses, [`RETIRED_SECRET_KEYS`]
//! (the model provider's and FireCrawl's), are scrubbed wherever a row of
//! them is found and never written to the keyring:
//!
//! * [`SecretStore`] is the storage interface; [`OsKeyring`] is the real one
//!   (macOS Keychain, Windows Credential Manager, freedesktop Secret Service),
//!   [`MemoryStore`] the test double.
//! * [`migrate::migrate_settings_secrets`] moves any migrated secret rows out
//!   of a library database on open (a row only fills an empty keyring slot, is
//!   read back, and only then deleted; the keyring always wins), and scrubs
//!   them from the pre-migration backup and older copies once the keyring holds
//!   them; retired rows are scrubbed from all of them unconditionally, with no
//!   keyring call; idempotently.
//! * [`view::all_settings`] reproduces the Python `GET /api/settings` shape
//!   minus the retired settings, reading the `*_set` flag of each migrated key
//!   from the store; [`view::set_secret`] is the write path (`PATCH
//!   /api/settings`) and accepts only migrated keys. Nothing here ever writes a
//!   secret to SQLite.
//!
//! Dependency direction: this crate depends on `musaic-db` (it takes a
//! `&Db`); `musaic-db` does not
//! know about secrets. So the app must call the migration after every
//! `Db::open` and after every `backup::restore_from`.
//!
//! Keyring unavailable (e.g. a headless Linux session with no Secret Service):
//! every store call returns [`StoreError::Unavailable`]. The migration leaves
//! the migrated keys' rows where they are (it never deletes a value it could not store) and
//! retries on the next launch; reads report every secret as not set and
//! `keyring_available: false`, so the UI can explain why the
//! features that need one are off; writes fail with the error for the API to
//! surface. There is no SQLite or file fallback.

pub mod agent;
pub mod migrate;
pub mod store;
pub mod view;

pub use migrate::{MigrationReport, migrate_settings_secrets};
pub use store::{MemoryStore, OsKeyring, SecretStore, StoreError};
pub use view::{
    SecretStatus, all_settings, clear_all, secret, secret_status, set_secret,
    snapshot_min_interval_hours,
};

/// The settings keys that are secrets, as the Python app names them
/// (`settings.py` KEY_AI_API_KEY, KEY_ACOUSTID_API_KEY, KEY_FIRECRAWL_API_KEY):
/// the union of [`MIGRATED_SECRET_KEYS`] and [`RETIRED_SECRET_KEYS`]. Every
/// scrub list (copies, snapshots, restores) and the log redaction use this, so
/// a retired key's value is still never kept or logged.
pub const SECRET_KEYS: [&str; 3] = ["ai_api_key", "acoustid_api_key", "firecrawl_api_key"];

/// Secrets the app still uses: moved from a settings row into the keyring.
pub const MIGRATED_SECRET_KEYS: [&str; 1] = ["acoustid_api_key"];

/// Secrets the app no longer uses: a row is scrubbed, never moved to the
/// keyring, and no keyring call is made for it here.
pub const RETIRED_SECRET_KEYS: [&str; 2] = ["ai_api_key", "firecrawl_api_key"];

/// Keyring service name for the app's entries (one entry per key, the key
/// name as the account).
pub const SERVICE: &str = "musaic";

pub fn is_secret_key(key: &str) -> bool {
    SECRET_KEYS.contains(&key)
}

pub fn is_migrated_key(key: &str) -> bool {
    MIGRATED_SECRET_KEYS.contains(&key)
}

pub fn is_retired_key(key: &str) -> bool {
    RETIRED_SECRET_KEYS.contains(&key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_keys_are_the_union_of_migrated_and_retired() {
        for k in SECRET_KEYS {
            assert_ne!(is_migrated_key(k), is_retired_key(k), "{k}");
        }
        assert_eq!(
            SECRET_KEYS.len(),
            MIGRATED_SECRET_KEYS.len() + RETIRED_SECRET_KEYS.len()
        );
    }
}
