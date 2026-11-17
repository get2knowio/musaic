//! Where secrets live: the OS keyring, or memory for tests.

use std::collections::BTreeMap;
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    /// No usable keyring (no Secret Service on D-Bus, locked or denied
    /// keychain, ...). The app keeps running without the affected features.
    #[error("the OS keyring is unavailable: {0}")]
    Unavailable(String),
    /// The keyring answered but refused or mangled this entry.
    #[error("the OS keyring failed: {0}")]
    Backend(String),
    /// The value read back after a write was not the value written.
    #[error("the OS keyring returned a different value than was stored for {0}")]
    ReadBackMismatch(String),
}

/// A string-valued secret store keyed by settings key.
pub trait SecretStore {
    /// `Ok(None)` when there is no entry.
    fn get(&self, key: &str) -> Result<Option<String>, StoreError>;
    fn set(&self, key: &str, value: &str) -> Result<(), StoreError>;
    /// Removing an absent entry is not an error.
    fn delete(&self, key: &str) -> Result<(), StoreError>;
    /// Whether the entry holds a non-blank value. For a store that reports
    /// the values it hands out (the app's log masking), this is the question
    /// that does not hand one out.
    fn is_set(&self, key: &str) -> Result<bool, StoreError> {
        Ok(self
            .get(key)?
            .is_some_and(|v| !musaic_core::pycompat::strip(&v).is_empty()))
    }
}

/// The platform keyring through the `keyring` crate: one generic password per
/// key under service [`crate::SERVICE`] (or a test service name).
#[derive(Debug, Clone)]
pub struct OsKeyring {
    service: String,
}

impl OsKeyring {
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    /// The store the app uses.
    pub fn app() -> Self {
        Self::new(crate::SERVICE)
    }

    pub fn service(&self) -> &str {
        &self.service
    }

    fn entry(&self, key: &str) -> Result<keyring::Entry, StoreError> {
        keyring::Entry::new(&self.service, key).map_err(map_err)
    }
}

fn map_err(e: keyring::Error) -> StoreError {
    match e {
        keyring::Error::NoStorageAccess(inner) => StoreError::Unavailable(inner.to_string()),
        keyring::Error::PlatformFailure(inner) => StoreError::Unavailable(inner.to_string()),
        // keyring 4: the platform store could not be initialised at all (for
        // example no Secret Service on D-Bus). That is "unavailable".
        keyring::Error::NoDefaultStore => {
            let why = match keyring::Entry::store_status() {
                Err(e) => e.to_string(),
                Ok(()) => "no credential store".to_string(),
            };
            StoreError::Unavailable(why)
        }
        other => StoreError::Backend(other.to_string()),
    }
}

/// One keyring call at a time in this process. macOS Keychain answers
/// "An invalid record was encountered" when two threads delete or write the
/// same item at once (seen with keyring 4.2 on 2026-11-14), which would be
/// reported as an unavailable keyring.
static OS_KEYRING_CALLS: Mutex<()> = Mutex::new(());

fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    OS_KEYRING_CALLS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl SecretStore for OsKeyring {
    fn get(&self, key: &str) -> Result<Option<String>, StoreError> {
        let _serial = one_at_a_time();
        match self.entry(key)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(map_err(e)),
        }
    }

    fn set(&self, key: &str, value: &str) -> Result<(), StoreError> {
        let _serial = one_at_a_time();
        self.entry(key)?.set_password(value).map_err(map_err)
    }

    fn delete(&self, key: &str) -> Result<(), StoreError> {
        let _serial = one_at_a_time();
        match self.entry(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(map_err(e)),
        }
    }
}

/// In-memory store with fault injection, for tests (and for running without
/// any keyring in development).
#[derive(Debug, Default)]
pub struct MemoryStore {
    inner: Mutex<MemoryInner>,
}

#[derive(Debug, Default)]
struct MemoryInner {
    entries: BTreeMap<String, String>,
    unavailable: bool,
    fail_set: Vec<String>,
    corrupt_set: Vec<String>,
    calls: Vec<String>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// A store where every call fails with [`StoreError::Unavailable`].
    pub fn unavailable() -> Self {
        let s = Self::default();
        s.lock().unavailable = true;
        s
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MemoryInner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn set_unavailable(&self, on: bool) {
        self.lock().unavailable = on;
    }

    /// Make `set(key, ..)` fail with a backend error.
    pub fn fail_set_for(&self, key: &str) {
        self.lock().fail_set.push(key.to_owned());
    }

    /// Make `set(key, v)` store something other than `v` (silent corruption).
    pub fn corrupt_set_for(&self, key: &str) {
        self.lock().corrupt_set.push(key.to_owned());
    }

    /// Everything stored, for assertions.
    pub fn entries(&self) -> BTreeMap<String, String> {
        self.lock().entries.clone()
    }

    /// Calls in order, as `get:key` / `set:key` / `delete:key`.
    pub fn calls(&self) -> Vec<String> {
        self.lock().calls.clone()
    }
}

impl SecretStore for MemoryStore {
    fn get(&self, key: &str) -> Result<Option<String>, StoreError> {
        let mut s = self.lock();
        s.calls.push(format!("get:{key}"));
        if s.unavailable {
            return Err(StoreError::Unavailable("memory store switched off".into()));
        }
        Ok(s.entries.get(key).cloned())
    }

    fn set(&self, key: &str, value: &str) -> Result<(), StoreError> {
        let mut s = self.lock();
        s.calls.push(format!("set:{key}"));
        if s.unavailable {
            return Err(StoreError::Unavailable("memory store switched off".into()));
        }
        if s.fail_set.iter().any(|k| k == key) {
            return Err(StoreError::Backend(format!("injected failure for {key}")));
        }
        let stored = if s.corrupt_set.iter().any(|k| k == key) {
            format!("{value}-corrupted")
        } else {
            value.to_owned()
        };
        s.entries.insert(key.to_owned(), stored);
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<(), StoreError> {
        let mut s = self.lock();
        s.calls.push(format!("delete:{key}"));
        if s.unavailable {
            return Err(StoreError::Unavailable("memory store switched off".into()));
        }
        s.entries.remove(key);
        Ok(())
    }
}
