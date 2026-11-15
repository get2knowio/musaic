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
