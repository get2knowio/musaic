//! The secret-settings migration as the host runs it (keys move from the
//! settings table, and from the pre-upgrade backup, into the OS keyring;
//! nothing moves without a usable keyring):
//! `musaic_secrets::migrate_settings_secrets` at startup,
//! after every restore, on a timed retry while a scrub is unfinished, and
//! once more on shutdown.
//!
//! Every outcome that is not clean is logged: keys the keyring could not
//! confirm (their rows stay), a backup scrub that failed, older copies of
//! the database (snapshots, restore asides) that could not be scrubbed or
//! keep a key, and an error (most
//! often a WAL checkpoint blocked by another connection's read transaction,
//! which leaves rows or old pages in the file). Only key names and error
//! texts are logged, never a value. The settings body carries
//! `keys_pending_removal` (a sibling of `keyring_available`, a field the
//! Python app did not have; see musaic-api's `tests/key_scrub.rs`) while the
//! last outcome was not clean, so the UI can say the keys are not yet removed from the database
//! file.
//!
//! When the run can be blocked by this process's own connections: at startup
//! it runs on the first connection, before any other is opened; after a
//! restore it runs while the restore still holds the gate and the file lock
//! (no writing work, no fingerprint pool, no backup or snapshot can hold a
//! connection; only a short read request can). A retry waits until no
//! writing work holds the gate and takes the database file lock for reading
//! (a restore cannot swap the file under it). Shutdown retries once after the
//! server has stopped and the pool has exited.
//!
//! This lives here rather than in musaic-api; the host keeps only what is
//! bound to its runtime: scheduling the retry thread against its state and
//! calling [`shutdown_retry`] from its shutdown.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use musaic_db::Db;

use crate::gate::{DbGate, WorkGate};
use crate::host_log::HostLog;
use musaic_secrets::{MigrationReport, SecretStore, StoreError};

/// A log line sink (default: stderr). Tests capture it.
pub type Logger = Arc<dyn Fn(&str) + Send + Sync>;

/// The logger name of these lines.
pub const TARGET: &str = "musaic.secrets";

/// Default pauses before the timed retries; the last one repeats.
pub const RETRY_DELAYS: &[Duration] = &[
    Duration::from_secs(5),
    Duration::from_secs(30),
    Duration::from_secs(120),
    Duration::from_secs(600),
];

/// The clean outcome's wording when this run removed something.
pub const NONE_LEFT: &str = "no API key left in the database file, its backup or older snapshots";
/// The clean outcome's wording when there was nothing to do: true both when
/// an earlier run moved the keys and when there never were any.
pub const NONE_STORED: &str = "no API key stored in the database file, its backup or older \
     snapshots (keys are kept in the system keyring)";

/// What a clean run removed, for the log (key names and counts only); `None`
/// when it removed nothing.
fn done_summary(rep: &MigrationReport) -> Option<String> {
    let mut parts = Vec::new();
    if !rep.moved.is_empty() {
        parts.push(format!(
            "moved {} to the system keyring",
            rep.moved.join(", ")
        ));
    }
    let mut dropped: Vec<&str> = rep.kept_keyring.clone();
    dropped.extend(&rep.dropped_blank);
    if !dropped.is_empty() {
        parts.push(format!(
            "removed {} from the database file (already in the keyring or blank)",
            dropped.join(", ")
        ));
    }
    if !rep.dropped_retired.is_empty() {
        parts.push(format!(
            "removed {} from the database file (retired, never moved to the keyring)",
            rep.dropped_retired.join(", ")
        ));
    }
    if !rep.backup_scrubbed.is_empty() {
        parts.push(format!(
            "removed {} from the pre-migration backup",
            rep.backup_scrubbed.join(", ")
        ));
    }
    if !rep.copies_scrubbed.is_empty() {
        parts.push(format!(
            "removed keys from {} older snapshot(s) or restore backup(s)",
            rep.copies_scrubbed.len()
        ));
    }
    (!parts.is_empty()).then(|| parts.join("; "))
}

/// How one run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScrubOutcome {
    /// Nothing left in the database file, the pre-migration backup or any
    /// older copy (snapshots, restore asides).
    Clean,
    /// Something is left. `retry` is false when only the keyring's being
    /// unavailable stands in the way: waiting does not help, the
    /// next launch or restore tries again.
    Pending { retry: bool },
}

/// Shared state of the migration runs.
pub struct KeyScrub {
    pending: AtomicBool,
    retrying: AtomicBool,
    log: Arc<dyn HostLog>,
    delays: Vec<Duration>,
    last_logged: Mutex<Option<String>>,
}

impl KeyScrub {
    pub fn new(log: impl HostLog + 'static, delays: Vec<Duration>) -> KeyScrub {
        KeyScrub {
            pending: AtomicBool::new(false),
            retrying: AtomicBool::new(false),
            log: Arc::new(log),
            delays,
            last_logged: Mutex::new(None),
        }
    }

    /// True while the last run left keys (or old copies of them) in the
    /// database file, its pre-migration backup or an older copy.
    pub fn pending(&self) -> bool {
        self.pending.load(Ordering::SeqCst)
    }

    /// A problem line (`musaic.secrets`, WARNING).
    pub fn log(&self, line: &str) {
        self.log.warn(TARGET, line);
    }

    /// Run the migration on `db` and record / log the outcome. `when` names
    /// the occasion in the log ("startup", "after restore", ...). A retry
    /// that ends exactly as the previous run did is not logged again.
    pub fn run(&self, db: &Db, store: &dyn SecretStore, when: &str) -> ScrubOutcome {
        let result = musaic_secrets::migrate_settings_secrets(db, store);
        let (outcome, summary) = classify(&result);
        self.pending
            .store(outcome != ScrubOutcome::Clean, Ordering::SeqCst);
        let mut last = self.last_logged.lock().unwrap_or_else(|p| p.into_inner());
        // Key names, file paths and error texts only, never a value. INFO
        // for a clean outcome (once per occasion, after an unclean one, and
        // whenever this run removed something), WARNING while something is
        // left.
        match &summary {
            Some(s) => {
                if last.as_deref() != Some(s.as_str()) || !when.starts_with("retry") {
                    self.log(&format!("settings secret migration ({when}): {s}"));
                }
            }
            None => {
                let done = result.as_ref().ok().and_then(done_summary);
                if let Some(d) = done {
                    self.log.info(
                        TARGET,
                        &format!("settings secret migration ({when}): {d}; {NONE_LEFT}"),
                    );
                } else if last.is_some() {
                    self.log.info(
                        TARGET,
                        &format!("settings secret migration ({when}): completed, {NONE_LEFT}"),
                    );
                } else if !when.starts_with("retry") {
                    self.log.info(
                        TARGET,
                        &format!("settings secret migration ({when}): {NONE_STORED}"),
                    );
                }
            }
        }
        *last = summary;
        outcome
    }

    /// Start the timed retry thread unless one runs. After each pause,
    /// `attempt` runs one retry ([`Retry::Ran`]), or answers [`Retry::Busy`]
    /// (not now: work holds the gate) or [`Retry::Gone`] (the app was
    /// dropped). The thread ends on a clean outcome, on one that waiting
    /// cannot fix (keyring unavailable), or when the app is gone.
    pub fn spawn_retry(self: &Arc<Self>, attempt: impl Fn() -> Retry + Send + 'static) {
        if self.retrying.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = self.clone();
        let delays = if self.delays.is_empty() {
            RETRY_DELAYS.to_vec()
        } else {
            self.delays.clone()
        };
        let spawned = std::thread::Builder::new()
            .name("key-scrub-retry".into())
            .spawn(move || {
                let mut i = 0;
                loop {
                    std::thread::sleep(delays[i.min(delays.len() - 1)]);
                    i += 1;
                    match attempt() {
                        Retry::Busy => continue,
                        Retry::Gone => break,
                        Retry::Ran(ScrubOutcome::Pending { retry: true }) => continue,
                        Retry::Ran(_) => break,
                    }
                }
                me.retrying.store(false, Ordering::SeqCst);
            });
        if spawned.is_err() {
            self.retrying.store(false, Ordering::SeqCst);
        }
    }
}

/// One retry slot.
pub enum Retry {
    /// Not now (work holds the gate); try at the next slot.
    Busy,
    /// The app is gone.
    Gone,
    Ran(ScrubOutcome),
}

fn store_error_kind(e: &StoreError) -> String {
    // The texts carry the keyring's own message and, for a read-back
    // mismatch, the key name; never a value.
    e.to_string()
}

/// The outcome and, unless clean, a log summary (key names and error texts
/// only).
pub fn classify(r: &musaic_db::Result<MigrationReport>) -> (ScrubOutcome, Option<String>) {
    let mut parts = Vec::new();
    let mut retry = false;
    let report = match r {
        Ok(rep) => Some(rep),
        Err(e) => {
            retry = true;
            parts.push(format!(
                "could not finish removing keys from the database file ({e}); will retry"
            ));
            None
        }
    };
    if let Some(rep) = report {
        if !rep.failed.is_empty() {
            let keys: Vec<String> = rep
                .failed
                .iter()
                .map(|(k, e)| format!("{k} ({})", store_error_kind(e)))
                .collect();
            if rep
                .failed
                .iter()
                .any(|(_, e)| !matches!(e, StoreError::Unavailable(_)))
            {
                retry = true;
            }
            parts.push(format!(
                "keys left in the database file because the keyring could not confirm them: {}",
                keys.join(", ")
            ));
        }
        if let Some(b) = &rep.backup_error {
            retry = true;
            parts.push(format!(
                "the pre-migration backup could not be scrubbed ({b}); will retry"
            ));
        }
        // Waiting does not change these (a damaged or unreadable file, a
        // key the keyring cannot confirm): reported, tried again on the
        // next launch or restore, and the settings page says keys remain.
        if !rep.copies_left.is_empty() {
            let files: Vec<String> = rep
                .copies_left
                .iter()
                .map(|(p, why)| format!("{} {why}", p.display()))
                .collect();
            parts.push(format!(
                "older copies of the database may still hold API keys: {}",
                files.join("; ")
            ));
        }
    }
    if parts.is_empty() {
        (ScrubOutcome::Clean, None)
    } else {
        (ScrubOutcome::Pending { retry }, Some(parts.join("; ")))
    }
}

/// One retry of an unfinished secret scrub, now: `None` (nothing done)
/// while writing work or a restore holds the gate. The database file lock
/// is held for reading so a restore cannot swap the file under it.
pub fn retry_once(
    scrub: &KeyScrub,
    gate: &WorkGate,
    db_gate: &DbGate,
    db_path: &Path,
    store: &dyn SecretStore,
) -> Option<ScrubOutcome> {
    if gate.snapshot() != (0, false) {
        return None;
    }
    let _g = db_gate.read().unwrap_or_else(|e| e.into_inner());
    match Db::open(db_path) {
        Ok((db, _)) => Some(scrub.run(&db, store, "retry")),
        Err(e) => {
            scrub.log(&format!(
                "settings secret migration (retry): cannot open the database: {e}"
            ));
            Some(ScrubOutcome::Pending { retry: true })
        }
    }
}

/// The shutdown retry: an unfinished secret scrub gets one more try (the
/// host calls this after the server has stopped and the pool has exited),
/// under the database file lock held for reading.
pub fn shutdown_retry(scrub: &KeyScrub, db_gate: &DbGate, db_path: &Path, store: &dyn SecretStore) {
    if scrub.pending() {
        let _g = db_gate.read().unwrap_or_else(|e| e.into_inner());
        match Db::open(db_path) {
            Ok((db, _)) => {
                scrub.run(&db, store, "shutdown");
            }
            Err(e) => scrub.log(&format!(
                "settings secret migration (shutdown): cannot open the database: {e}"
            )),
        }
    }
}

/// The settings row that records that the retired keyring entries are gone
/// (not in the settings body).
pub const RETIRED_KEYS_MARKER: &str = "retired_keys_removed";

/// What [`remove_retired_keys`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetiredKeys {
    /// The marker was present: no keyring call was made.
    AlreadyRemoved,
    /// Every retired entry is gone (a missing one counts) and the marker was
    /// written.
    Removed,
    /// These entries could not be deleted (or the marker not written); the
    /// next start tries again.
    Failed(Vec<&'static str>),
}

/// Delete the keyring entries of the retired keys
/// ([`musaic_secrets::RETIRED_SECRET_KEYS`]: the removed model provider's
/// and web-scraping service's keys, service [`musaic_secrets::SERVICE`])
/// once. The settings row [`RETIRED_KEYS_MARKER`] is written only after
/// every delete succeeded; while it is present this makes no keyring call. A
/// failure (a locked secret service) is logged by key name only and retried
/// at the next start. A restored older database lacks the marker, so the
/// next start deletes again, which is harmless. Never reads a value.
pub fn remove_retired_keys(db: &Db, store: &dyn SecretStore, log: &dyn HostLog) -> RetiredKeys {
    let conn = db.connection();
    match musaic_db::settings::get_raw(conn, RETIRED_KEYS_MARKER) {
        Ok(Some(_)) => return RetiredKeys::AlreadyRemoved,
        Ok(None) => {}
        Err(_) => {
            log.warn(
                TARGET,
                "retired API keys: could not read the settings; trying again at the next start",
            );
            return RetiredKeys::Failed(Vec::new());
        }
    }
    let failed: Vec<&'static str> = musaic_secrets::RETIRED_SECRET_KEYS
        .into_iter()
        .filter(|key| store.delete(key).is_err())
        .collect();
    if !failed.is_empty() {
        log.warn(
            TARGET,
            &format!(
                "retired API keys: could not remove {} from the system keyring; trying again at the next start",
                failed.join(", ")
            ),
        );
        return RetiredKeys::Failed(failed);
    }
    if musaic_db::settings::set_raw(conn, RETIRED_KEYS_MARKER, "1").is_err() {
        log.warn(
            TARGET,
            "retired API keys: removed from the system keyring, but could not record it; trying again at the next start",
        );
        return RetiredKeys::Failed(Vec::new());
    }
    RetiredKeys::Removed
}
