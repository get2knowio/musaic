//! `PATCH /api/settings` (`musaic/api/settings.py`): range checks, the provider allowlist, the one transaction with
//! the secret writes just before it commits (undone when a later write or
//! the commit fails), the "settings saved" log line, and the fingerprint
//! pool restart on a new AcoustID key. This lives here rather than in
//! musaic-api's `routes/settings.rs` so a non-HTTP face changes
//! settings the same way. The host parses the request into a
//! [`SettingsPatch`], takes the restore gate's work guard, passes the
//! providers it has installed (data, so this crate needs no AI types); a
//! refusal is an [`OpError`] (`Rejected` for a value out of range or an
//! unknown provider, `Unavailable` for the keyring, `Internal` for the
//! database).

use musaic_core::pycompat::strip;
use musaic_db::settings::set_raw;
use rusqlite::Connection;

use crate::error::OpError;
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

/// A field out of range or an unknown provider. Fields validated before it are
/// still persisted, secrets included.
fn invalid(message: impl Into<String>) -> OpError {
    OpError::Rejected {
        message: message.into(),
        details: None,
    }
}

/// A failed keyring write: the detail names the keyring and never the value.
/// Nothing persisted.
fn secret_error(key: &str, e: &musaic_secrets::StoreError) -> OpError {
    OpError::Unavailable {
        message: format!("Could not save {key} to the OS keyring ({e}). No settings were changed."),
    }
}

/// The secret writes of one PATCH, undoable. Each key's previous value is
/// read before it is written; [`undo`](Self::undo) puts them back
/// (best effort) when a later write or the database commit fails.
struct SecretWrites<'a> {
    store: &'a dyn musaic_secrets::SecretStore,
    previous: Vec<(&'static str, Option<String>)>,
}

impl<'a> SecretWrites<'a> {
    fn write(&mut self, key: &'static str, value: &str) -> Result<(), OpError> {
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

/// The `ai_provider_id` the Python app stored by default. Still accepted
/// (with "") although no provider of that id is listed, so a database the
/// Python app wrote, and a client that saves the value it read back, keep
/// working.
pub const STORED_DEFAULT_PROVIDER_ID: &str = "claude";

/// Apply one PATCH on `conn` (closed before the log line) and `store`,
/// accepting the `listed_providers` (the ids `GET /api/ai/providers` lists),
/// [`STORED_DEFAULT_PROVIDER_ID`] and "" as `ai_provider_id`. Logs which
/// settings changed (never a value). The caller holds the restore
/// gate's work guard and, on success, wakes the AI queue and calls
/// [`kick_fingerprint_pool`] when [`SettingsPatch::sets_acoustid_key`].
///
/// Once the transaction has committed, publishes
/// [`Event::SettingsChanged`](musaic_events::Event::SettingsChanged) on
/// `bus` with the names of the settings written (never a value), also when a
/// later field was refused (the earlier ones are persisted): the background
/// check sweep marks the checks that depend on one of them stale.
pub fn apply_patch(
    conn: Connection,
    store: &dyn musaic_secrets::SecretStore,
    patch: SettingsPatch,
    listed_providers: &[&str],
    log: &dyn HostLog,
    bus: &musaic_events::Bus,
) -> Result<(), OpError> {
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
    // The setting names written, for `SettingsChanged`.
    let mut keys: Vec<&'static str> = Vec::new();
    let mut secrets: Vec<(&'static str, String)> = Vec::new();
    let outcome = (|| -> Result<(), OpError> {
        if let Some(n) = max_parallel {
            if !(1..=8).contains(&n) {
                return Err(invalid("ai_max_parallel must be between 1 and 8"));
            }
            set_raw(&tx, "ai_max_parallel", &n.to_string())?;
            changed.push(format!("ai_max_parallel={n}"));
            keys.push("ai_max_parallel");
        }
        if let Some(b) = paused {
            set_raw(&tx, "ai_queue_paused", if b { "1" } else { "0" })?;
            changed.push(format!("ai_queue_paused={b}"));
            keys.push("ai_queue_paused");
        }
        if let Some(p) = provider {
            let pid = strip(&p).to_string();
            let accepted = pid.is_empty()
                || pid == STORED_DEFAULT_PROVIDER_ID
                || listed_providers.contains(&pid.as_str());
            if !accepted {
                return Err(invalid(format!(
                    "Unknown or not-installed provider {}. Installed: {}.",
                    musaic_fsops::py_repr(&pid),
                    listed_providers.join(", ")
                )));
            }
            set_raw(&tx, "ai_provider_id", &pid)?;
            changed.push(format!("ai_provider_id={pid}"));
            keys.push("ai_provider_id");
        }
        if let Some(m) = model {
            set_raw(&tx, "ai_model_id", strip(&m))?;
            changed.push("ai_model_id".into());
            keys.push("ai_model_id");
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
                keys.push(key);
            }
        }
        if let Some(n) = retention {
            if !(0..=100).contains(&n) {
                return Err(invalid(
                    "snapshot_retention_count must be between 0 and 100",
                ));
            }
            set_raw(&tx, "snapshot_retention_count", &n.to_string())?;
            changed.push(format!("snapshot_retention_count={n}"));
            keys.push("snapshot_retention_count");
        }
        if let Some(h) = interval {
            // Infinity and NaN parse as floats but are not intervals (and
            // infinity cannot be sent back as JSON).
            if !h.is_finite() {
                return Err(invalid(
                    "snapshot_min_interval_hours must be a finite number",
                ));
            }
            if h < 0.0 {
                return Err(invalid("snapshot_min_interval_hours must be >= 0"));
            }
            set_raw(
                &tx,
                "snapshot_min_interval_hours",
                &crate::json::py_float_repr(h.max(0.0)),
            )?;
            changed.push("snapshot_min_interval_hours".into());
            keys.push("snapshot_min_interval_hours");
        }
        for (key, v) in tpl {
            if let Some(v) = v {
                set_raw(&tx, key, strip(&v))?;
                changed.push(key.to_string());
                keys.push(key);
            }
        }
        Ok(())
    })();
    // A database error (500) persists nothing, as before the secrets.
    let outcome = match outcome {
        Err(e @ OpError::Internal { .. }) => return Err(e),
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
        return Err(e.into());
    }
    // Committed: what was written is in effect, also when a later field
    // was refused.
    if !keys.is_empty() {
        bus.publish(&musaic_events::Event::SettingsChanged {
            keys: keys.iter().map(|k| k.to_string()).collect(),
        });
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
