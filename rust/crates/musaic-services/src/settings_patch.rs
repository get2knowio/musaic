//! `PATCH /api/settings` (`musaic/api/settings.py`): range checks, the provider allowlist, the one transaction with
//! the secret writes just before it commits (undone when a later write or
//! the commit fails), the "settings saved" log line, and the fingerprint
//! pool restart on a new AcoustID key. This lives here rather than in
//! musaic-api's `routes/settings.rs` so a non-HTTP face changes
//! settings the same way. The host parses the request into a
//! [`SettingsPatch`], takes the restore gate's work guard, passes the
//! providers it has installed (data, so this crate needs no AI types), and
//! maps a [`PatchError`] to its answer.

use musaic_core::pycompat::strip;
use musaic_db::settings::set_raw;
use rusqlite::Connection;

use crate::fingerprint_pool::{FingerprintPool, PoolDeps};
use crate::host_log::HostLog;

/// The fields of one PATCH, as parsed (each `None` when absent).
#[derive(Debug, Clone, Default)]
pub struct SettingsPatch {
    pub max_parallel: Option<i64>,
    pub paused: Option<bool>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub ai_key: Option<String>,
    pub acoustid: Option<String>,
    pub firecrawl: Option<String>,
    pub retention: Option<i64>,
    pub interval: Option<f64>,
    /// The four filename / dir templates, in the Python app's order.
    pub tpl: [(&'static str, Option<String>); 4],
}

/// The template keys of [`SettingsPatch::tpl`], in the Python app's order.
pub const TEMPLATE_KEYS: [&str; 4] = [
    "filename_template_single_artist",
    "filename_template_compilation",
    "dir_template_single_artist",
    "dir_template_compilation",
];

impl SettingsPatch {
    /// A new, non-empty AcoustID key: the stored fingerprints can now be
    /// looked up (see [`kick_fingerprint_pool`]).
    pub fn sets_acoustid_key(&self) -> bool {
        self.acoustid
            .as_deref()
            .is_some_and(|k| !strip(k).is_empty())
    }
}

/// Why a PATCH was refused.
#[derive(Debug)]
pub enum PatchError {
    /// A field out of range or an unknown provider (400 with this detail).
    /// Fields validated before it are still persisted, secrets included.
    Invalid(String),
    /// A secret-store write failed (503 with this
    /// detail, which names the keyring and never the value). Nothing
    /// persisted.
    Keyring(String),
    /// A settings write failed (500). Nothing persisted.
    Db(musaic_db::Error),
    /// Starting or committing the transaction failed (500). Nothing
    /// persisted.
    Sqlite(rusqlite::Error),
}

impl PatchError {
    /// A database error (500): it persists nothing, as before the secrets.
    fn is_internal(&self) -> bool {
        matches!(self, PatchError::Db(_) | PatchError::Sqlite(_))
    }
}

impl From<musaic_db::Error> for PatchError {
    fn from(e: musaic_db::Error) -> Self {
        PatchError::Db(e)
    }
}

impl From<rusqlite::Error> for PatchError {
    fn from(e: rusqlite::Error) -> Self {
        PatchError::Sqlite(e)
    }
}

/// A failed keyring write answers 503 with a detail
/// that names the keyring. Never contains the value.
fn secret_error(key: &str, e: &musaic_secrets::StoreError) -> PatchError {
    PatchError::Keyring(format!(
        "Could not save {key} to the OS keyring ({e}). No settings were changed."
    ))
}

/// The secret writes of one PATCH, undoable. Each key's previous value is
/// read before it is written; [`undo`](Self::undo) puts them back
/// (best effort) when a later write or the database commit fails.
struct SecretWrites<'a> {
    store: &'a dyn musaic_secrets::SecretStore,
    previous: Vec<(&'static str, Option<String>)>,
}

impl<'a> SecretWrites<'a> {
    fn write(&mut self, key: &'static str, value: &str) -> Result<(), PatchError> {
        let prev = self.store.get(key).map_err(|e| secret_error(key, &e))?;
        self.previous.push((key, prev));
        musaic_secrets::set_secret(self.store, key, value).map_err(|e| secret_error(key, &e))
    }

    fn undo(self) {
        for (key, prev) in self.previous.into_iter().rev() {
            let _ = match prev {
                Some(v) => self.store.set(key, &v),
                None => self.store.delete(key),
            };
        }
    }
}

/// Apply one PATCH on `conn` (closed before the log line) and `store`,
/// accepting `installed_providers` (and "") as `ai_provider_id`. Logs which
/// settings changed (never a value). The caller holds the restore
/// gate's work guard and, on success, wakes the AI queue and calls
/// [`kick_fingerprint_pool`] when [`SettingsPatch::sets_acoustid_key`].
pub fn apply_patch(
    conn: Connection,
    store: &dyn musaic_secrets::SecretStore,
    patch: SettingsPatch,
    installed_providers: &[&str],
    log: &dyn HostLog,
) -> Result<(), PatchError> {
    let SettingsPatch {
        max_parallel,
        paused,
        provider,
        model,
        ai_key,
        acoustid,
        firecrawl,
        retention,
        interval,
        tpl,
    } = patch;
    // The SQLite writes of this PATCH go in one transaction and
    // the secrets are written just before it commits, so a keyring
    // failure rolls everything back (503, nothing persisted). The
    // Python app's order is kept otherwise: fields are validated and
    // written in its order, and when a later field is refused (400) the
    // earlier ones, secrets included, are still persisted.
    // IMMEDIATE: this transaction reads (current values) before it
    // writes; a deferred one fails at once with "database is locked"
    // under WAL when another connection writes (musaic_db::immediate).
    let tx = musaic_db::immediate(&conn)?;
    let mut changed: Vec<String> = Vec::new();
    let mut secrets: Vec<(&'static str, String)> = Vec::new();
    let outcome = (|| -> Result<(), PatchError> {
        if let Some(n) = max_parallel {
            if !(1..=8).contains(&n) {
                return Err(PatchError::Invalid(
                    "ai_max_parallel must be between 1 and 8".into(),
                ));
            }
            set_raw(&tx, "ai_max_parallel", &n.to_string())?;
            changed.push(format!("ai_max_parallel={n}"));
        }
        if let Some(b) = paused {
            set_raw(&tx, "ai_queue_paused", if b { "1" } else { "0" })?;
            changed.push(format!("ai_queue_paused={b}"));
        }
        if let Some(p) = provider {
            let pid = strip(&p).to_string();
            if !pid.is_empty() && !installed_providers.contains(&pid.as_str()) {
                return Err(PatchError::Invalid(format!(
                    "Unknown or not-installed provider {}. Installed: {}. Install its airframe extra to add more.",
                    musaic_fsops::py_repr(&pid),
                    installed_providers.join(", ")
                )));
            }
            set_raw(&tx, "ai_provider_id", &pid)?;
            changed.push(format!("ai_provider_id={pid}"));
        }
        if let Some(m) = model {
            set_raw(&tx, "ai_model_id", strip(&m))?;
            changed.push("ai_model_id".into());
        }
        for (key, v) in [
            ("ai_api_key", ai_key),
            ("acoustid_api_key", acoustid),
            ("firecrawl_api_key", firecrawl),
        ] {
            if let Some(v) = v {
                changed.push(format!(
                    "{key} ({})",
                    if strip(&v).is_empty() {
                        "cleared"
                    } else {
                        "set"
                    }
                ));
                secrets.push((key, v));
            }
        }
        if let Some(n) = retention {
            if !(0..=100).contains(&n) {
                return Err(PatchError::Invalid(
                    "snapshot_retention_count must be between 0 and 100".into(),
                ));
            }
            set_raw(&tx, "snapshot_retention_count", &n.to_string())?;
            changed.push(format!("snapshot_retention_count={n}"));
        }
        if let Some(h) = interval {
            if h < 0.0 {
                return Err(PatchError::Invalid(
                    "snapshot_min_interval_hours must be >= 0".into(),
                ));
            }
            set_raw(
                &tx,
                "snapshot_min_interval_hours",
                &crate::json::py_float_repr(h.max(0.0)),
            )?;
            changed.push("snapshot_min_interval_hours".into());
        }
        for (key, v) in tpl {
            if let Some(v) = v {
                set_raw(&tx, key, strip(&v))?;
                changed.push(key.to_string());
            }
        }
        Ok(())
    })();
    // A database error (500) persists nothing, as before the secrets.
    let outcome = match outcome {
        Err(e) if e.is_internal() => return Err(e),
        other => other,
    };
    let mut writes = SecretWrites {
        store,
        previous: Vec::new(),
    };
    for (key, v) in &secrets {
        if let Err(e) = writes.write(key, v) {
            writes.undo();
            // `tx` is dropped here: rolled back.
            return Err(e);
        }
    }
    if let Err(e) = tx.commit() {
        writes.undo();
        return Err(PatchError::Sqlite(e));
    }
    outcome?;
    drop(conn);
    // Which settings changed, never a value (a secret is
    // named with "set" / "cleared" only).
    log.info(
        "musaic.api.settings",
        &format!("settings saved: {}", changed.join(", ")),
    );
    Ok(())
}

/// Deliberate addition (the Python app had none; its pool started only
/// after a scan): once a PATCH that sets a non-empty AcoustID key has
/// succeeded, restart the fingerprint pool on the configured library (stop
/// the pass in flight, then `warm_library`, as after a scan) so fingerprints
/// stored without a key get their lookups now instead of after the next
/// scan. Nothing happens without a configured library. The PATCH holds the
/// restore gate meanwhile, so no restore can be stopping the pool. `deps`
/// is called after the pass in flight has stopped.
pub fn kick_fingerprint_pool(
    pool: &FingerprintPool,
    library_dir: Option<String>,
    deps: impl FnOnce() -> PoolDeps,
) {
    let Some(lib) = library_dir.filter(|l| !l.is_empty()) else {
        return;
    };
    pool.stop();
    pool.warm_library(&lib, deps());
}
