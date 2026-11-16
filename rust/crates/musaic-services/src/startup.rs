//! Small startup and settings helpers a host needs when it assembles the
//! services (the Python app's `main._lifespan`, `db.py`, `settings.py`):
//! the "database opened / migrated" log line, the startup automatic
//! snapshot, the AcoustID key read, the resolver's parts factory, the
//! checks' clock, the fingerprint pool's dependencies and its INFO lines.
//! They live here rather than in musaic-api so a non-HTTP face starts the
//! same way. The host keeps its own state and passes the
//! pieces in.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use musaic_db::Db;
use musaic_events::Bus;
use musaic_resolve::Resolver;
use musaic_secrets::SecretStore;
use rusqlite::Connection;

use crate::fingerprint_pool::{
    FingerprintPool, PoolDeps, PoolSleeper, PoolStatus, fingerprint_job,
};
use crate::host_log::HostLog;
use crate::production::{Fingerprinting, PartsFactory, SharedTransport};
use crate::sources::ResolveParts;

/// A secrets store shared across threads.
pub type SharedSecrets = Arc<dyn SecretStore + Send + Sync>;
/// Wall clock (tests script it).
pub type WallClock = Arc<dyn Fn() -> SystemTime + Send + Sync>;

/// How the database was opened, for the log (`db.py`'s
/// "Migrating DB schema" lines).
pub fn log_opened(log: &dyn HostLog, db_path: &Path, opened: &musaic_db::Opened) {
    let path = db_path.display();
    match opened {
        musaic_db::Opened::Current => log.info(
            "musaic.db",
            &format!("database {path} at schema v{}", musaic_db::SCHEMA_VERSION),
        ),
        musaic_db::Opened::Created => log.info(
            "musaic.db",
            &format!(
                "created database {path} at schema v{}",
                musaic_db::SCHEMA_VERSION
            ),
        ),
        musaic_db::Opened::Migrated { from, backup } => log.info(
            "musaic.db",
            &format!(
                "Migrated DB schema {from} -> {} ({path}; backup: {})",
                musaic_db::SCHEMA_VERSION,
                backup
                    .as_ref()
                    .map_or_else(|| "none".to_string(), |b| b.display().to_string())
            ),
        ),
    }
}

/// The lifespan's automatic snapshot on startup (`snapshot_retention_count`
/// clamped to 0..=100, `snapshot_min_interval_hours`); a failure is logged
/// and startup continues.
pub fn auto_snapshot_on_startup(
    db: &Db,
    snapshot_dir: &Path,
    clock: &dyn Fn() -> SystemTime,
    log: &dyn HostLog,
) {
    let conn = db.connection();
    let retention = musaic_db::settings::get_int(conn, "snapshot_retention_count", 10)
        .unwrap_or(10)
        .clamp(0, 100);
    let interval = snapshot_interval(conn);
    let now = clock();
    if let Err(e) = musaic_db::backup::maybe_auto_snapshot(
        db,
        snapshot_dir,
        retention,
        interval,
        &musaic_db::backup::filename_tag(now),
        now,
    ) {
        log.warn(
            "musaic.main",
            &format!("auto-snapshot on startup failed (continuing): {e}"),
        );
    }
}

/// The fingerprint pool state last logged (status, started_at, total).
pub type PoolLogged = Mutex<Option<(PoolStatus, Option<String>, usize)>>;

/// One fingerprint pool change: log a pass starting (once its candidate
/// count is known), ending, cancelled, or the pool unavailable
/// (services/fingerprint_pool.py:113, 178, 210, 223).
pub fn log_pool(pool: &FingerprintPool, pool_logged: &PoolLogged, log: &dyn HostLog) {
    use crate::fingerprint_pool::PoolStatus as S;
    let s = pool.state();
    let key = (s.status, s.started_at.clone(), s.total);
    let mut last = pool_logged
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let prev = last.replace(key.clone());
    let same_pass = prev
        .as_ref()
        .is_some_and(|(_, started, _)| *started == key.1);
    const T: &str = "musaic.services.fingerprint_pool";
    match s.status {
        S::Running => {
            let was_total = prev
                .as_ref()
                .filter(|_| same_pass)
                .filter(|(st, ..)| *st == S::Running)
                .map(|p| p.2);
            if was_total != Some(s.total) && (s.total > 0 || s.current_album_id.is_some()) {
                log.info(
                    T,
                    &format!(
                        "Eager fingerprinting starting: {} album(s) need fingerprints",
                        s.total
                    ),
                );
            }
        }
        st if prev.as_ref().is_some_and(|p| p.0 == st) && same_pass => {}
        S::Done => log.info(
            T,
            &format!(
                "Eager fingerprinting done: completed={} errors={}",
                s.completed, s.errors
            ),
        ),
        S::Cancelled => log.info(
            T,
            &format!(
                "Eager fingerprinting cancelled: completed={} errors={} of {}",
                s.completed, s.errors, s.total
            ),
        ),
        S::Unavailable => log.info(
            T,
            &format!(
                "Eager fingerprinting unavailable: {}",
                s.skip_reason.as_deref().unwrap_or("")
            ),
        ),
        S::Idle => {}
    }
}

/// The resolver's parts as of now: the AcoustID key is read from the
/// secrets store on every call, as the Python app read the setting.
pub fn parts_factory(
    resolver: Arc<Resolver>,
    transport: SharedTransport,
    secrets: SharedSecrets,
    fingerprinter: Arc<dyn crate::fingerprint::Fingerprinter>,
    bus: Bus,
    clock: WallClock,
) -> PartsFactory {
    Arc::new(move || {
        let c1 = clock.clone();
        let c2 = clock.clone();
        ResolveParts {
            resolver: resolver.clone(),
            transport: transport.clone(),
            acoustid_key: acoustid_key(secrets.as_ref()),
            fingerprinter: fingerprinter.clone(),
            bus: bus.clone(),
            now_iso: Arc::new(move || musaic_db::backup::iso_seconds(c1())),
            now_epoch: Arc::new(move || epoch(c2())),
        }
    })
}

fn epoch(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `settings.acoustid_api_key()`: `None` when unset or unreadable.
pub fn acoustid_key(secrets: &(dyn SecretStore + Send + Sync)) -> Option<String> {
    musaic_secrets::secret(secrets, "acoustid_api_key")
        .ok()
        .filter(|k| !k.is_empty())
}

/// The same reading as the settings body shows (one accessor), so the
/// interval the user sees is the one the startup snapshot obeys.
fn snapshot_interval(conn: &Connection) -> f64 {
    musaic_secrets::snapshot_min_interval_hours(conn).unwrap_or(24.0)
}

/// The checks' clock over the host's wall clock.
pub struct ChecksClock(pub WallClock);

impl musaic_checks::Clock for ChecksClock {
    fn now_iso(&self) -> String {
        musaic_db::backup::iso_seconds((self.0)())
    }
    fn now(&self) -> SystemTime {
        (self.0)()
    }
}

/// What a pool pass needs (`fingerprint_pool._run_pool`).
pub fn pool_deps(
    db_path: &Path,
    clock: &WallClock,
    secrets: &SharedSecrets,
    fingerprinting: &Fingerprinting,
    transport: &SharedTransport,
    bus: &Bus,
    sleeper: &Arc<dyn PoolSleeper>,
) -> PoolDeps {
    let path = db_path.to_path_buf();
    let clock = clock.clone();
    let c2 = clock.clone();
    let secrets = secrets.clone();
    let fp = fingerprinting.clone();
    let fingerprinter = fp.fingerprinter();
    PoolDeps {
        open_db: Arc::new(move || musaic_db::open_connection(&path).map_err(|e| e.to_string())),
        availability: Arc::new(move || fp.availability()),
        job: fingerprint_job(
            transport.clone(),
            Arc::new(move || acoustid_key(secrets.as_ref())),
            fingerprinter.clone(),
            bus.clone(),
            Arc::new(move || musaic_db::backup::iso_seconds(c2())),
        ),
        clock: Arc::new(move || musaic_db::backup::iso_seconds(clock())),
        sleeper: sleeper.clone(),
        fingerprint_impl: fingerprinter.implementation().to_string(),
    }
}

/// Why there is no copy before a schema step, and so no upgrade.
#[derive(Debug, thiserror::Error)]
pub enum PreUpgradeCopyFailed {
    #[error(
        "could not copy the database to {dir} before upgrading it from schema v{found} to v{to}: {reason}. \
         Nothing was upgraded and the database was not changed. Free some space in {dir} or make that \
         folder writable, then start again."
    )]
    Copy {
        dir: String,
        found: i64,
        to: i64,
        reason: String,
    },
    #[error(
        "could not read the database {db} to check whether it needs upgrading: {reason}. \
         Nothing was upgraded and the database was not changed. Close anything else that has it \
         open and check that the file is readable, then start again."
    )]
    Read { db: String, reason: String },
}

/// What [`snapshot_before_schema_step`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreUpgradeCopy {
    /// Nothing to copy: no database yet, one already at the current schema,
    /// a Python-app one (musaic-db backs those up itself), or a file that is
    /// not a library (opening it says so).
    NotNeeded,
    /// A new snapshot of this name was taken.
    Taken(String),
    /// A snapshot of this name, taken for the same schema version after the
    /// database last changed, already exists (an earlier start that did not
    /// finish upgrading): no second copy, so repeated failed starts do not
    /// each take a full copy and prune older snapshots.
    Existing(String),
}

/// How long to wait for a lock on the database being read before giving up.
const READ_BUSY_TIMEOUT: std::time::Duration = musaic_db::BUSY_TIMEOUT;

/// When the database last changed, in whole seconds (a snapshot's own
/// modification time is read in whole seconds): the later of its file and
/// its WAL.
fn live_mtime(db_path: &Path) -> Option<i64> {
    let mut wal = db_path.as_os_str().to_owned();
    wal.push("-wal");
    [db_path.to_path_buf(), wal.into()]
        .iter()
        .filter_map(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
        .filter_map(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .max()
}

/// A snapshot in `dir` written for schema `found` (by its name), that opens
/// at that version and is not older than the database's last change. A
/// newer database with an older copy (changes made since) gets a new copy.
fn existing_copy(dir: &Path, db_path: &Path, found: i64) -> Option<String> {
    let changed = live_mtime(db_path)?;
    // Newest first.
    for s in musaic_db::backup::list_snapshots(dir).ok()? {
        let named = musaic_db::backup::parse_snapshot_name(&s.filename)
            .is_some_and(|n| i64::try_from(n.version) == Ok(found));
        if named && s.valid && s.schema_version == Some(found) && s.modified_unix >= changed {
            return Some(s.filename);
        }
    }
    None
}

/// Before a database this build wrote (v17 or later) is upgraded to the
/// current schema: one copy of it, as a snapshot in `snapshot_dir` (named
/// `library-<tag>-v<found>.db`, so it is listed, restorable and, as the
/// newest such copy, kept by the database's pruning until a later upgrade
/// supersedes it). A database at the current version, a missing, empty or
/// blank one and a Python-app one (v16 or older, which musaic-db backs up
/// itself) get none: [`PreUpgradeCopy::NotNeeded`].
///
/// An existing database that cannot be opened read-only or whose version
/// cannot be read is an error, not "nothing to copy": the migration that
/// follows must not run without a copy because the check failed. (A file
/// with no schema objects, no `schema_version` table or no version row is
/// not a library at all; `Db::open_or_create` creates over a blank one and
/// refuses the others without migrating them.)
///
/// The copy holds no secret: rows of [`musaic_secrets::SECRET_KEYS`] left in
/// `settings` (a key the OS keyring could not take yet) are deleted from the
/// copy, with `secure_delete`, and the copy is vacuumed. If the copy cannot
/// be written the caller must not migrate.
pub fn snapshot_before_schema_step(
    db_path: &Path,
    snapshot_dir: &Path,
    now: SystemTime,
) -> Result<PreUpgradeCopy, PreUpgradeCopyFailed> {
    match std::fs::metadata(db_path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PreUpgradeCopy::NotNeeded);
        }
        Err(e) => {
            return Err(PreUpgradeCopyFailed::Read {
                db: db_path.display().to_string(),
                reason: e.to_string(),
            });
        }
        Ok(m) if !m.is_file() || m.len() == 0 => return Ok(PreUpgradeCopy::NotNeeded),
        Ok(_) => {}
    }
    let read_failed = |e: &dyn std::fmt::Display| PreUpgradeCopyFailed::Read {
        db: db_path.display().to_string(),
        reason: e.to_string(),
    };
    // Read without migrating (and without creating anything).
    let src = Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| read_failed(&e))?;
    src.busy_timeout(READ_BUSY_TIMEOUT)
        .map_err(|e| read_failed(&e))?;
    let has_table: bool = src
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'schema_version')",
            [],
            |r| r.get(0),
        )
        .map_err(|e| read_failed(&e))?;
    if !has_table {
        return Ok(PreUpgradeCopy::NotNeeded);
    }
    let found = match src.query_row("SELECT version FROM schema_version LIMIT 1", [], |r| {
        r.get::<_, i64>(0)
    }) {
        Ok(v) => v,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(PreUpgradeCopy::NotNeeded),
        Err(e) => return Err(read_failed(&e)),
    };
    if found <= musaic_db::PYTHON_SCHEMA_VERSION || found >= musaic_db::SCHEMA_VERSION {
        return Ok(PreUpgradeCopy::NotNeeded);
    }
    let fail = |reason: String| PreUpgradeCopyFailed::Copy {
        dir: snapshot_dir.display().to_string(),
        found,
        to: musaic_db::SCHEMA_VERSION,
        reason,
    };
    if let Some(name) = existing_copy(snapshot_dir, db_path, found) {
        return Ok(PreUpgradeCopy::Existing(name));
    }
    let retention = musaic_db::settings::get_int(&src, "snapshot_retention_count", 10)
        .unwrap_or(10)
        .clamp(0, 100);
    std::fs::create_dir_all(snapshot_dir).map_err(|e| fail(e.to_string()))?;
    let tag = musaic_db::backup::filename_tag(now);
    let stem = format!("{}{tag}-v{found}", musaic_db::backup::SNAPSHOT_PREFIX);
    let mut target = snapshot_dir.join(format!("{stem}{}", musaic_db::backup::SNAPSHOT_SUFFIX));
    let mut n = 0;
    while target.exists() {
        n += 1;
        target = snapshot_dir.join(format!("{stem}.{n}{}", musaic_db::backup::SNAPSHOT_SUFFIX));
    }
    let scrub = |target: &Path| -> Result<(), String> {
        musaic_db::backup::vacuum_into(&src, target).map_err(|e| e.to_string())?;
        let copy = Connection::open(target).map_err(|e| e.to_string())?;
        copy.pragma_update(None, "secure_delete", "ON")
            .map_err(|e| e.to_string())?;
        for key in musaic_secrets::SECRET_KEYS {
            copy.execute("DELETE FROM settings WHERE key = ?1", [key])
                .map_err(|e| e.to_string())?;
        }
        copy.execute_batch("VACUUM").map_err(|e| e.to_string())?;
        Ok(())
    };
    if let Err(e) = scrub(&target) {
        // A partial copy is not a snapshot to keep.
        let _ = std::fs::remove_file(&target);
        return Err(fail(e));
    }
    let _ = musaic_db::backup::prune_snapshots(snapshot_dir, retention);
    Ok(PreUpgradeCopy::Taken(
        target
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default(),
    ))
}
