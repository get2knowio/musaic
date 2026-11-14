//! Database backup, restore and named snapshots (`musaic/api/settings.py`,
//! including the upload size limits): the scratch directory next to the database,
//! the backup copy, the restore sequence under the restore gate (stop the
//! fingerprint pool, the test hook, the file lock, swap, the after-restore
//! work) and snapshot creation with its retention. It lives here rather than
//! in musaic-api so a non-HTTP face backs up and restores the same way. The host takes the gate's guards (a restore's
//! [`crate::gate::RestoreGuard`], a snapshot's work guard), receives the
//! upload, and renders the results.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use musaic_db::Db;
use musaic_db::backup::{self, CreatedSnapshot, RestoreError, RestoreFailed, RestoreInfo};
use musaic_db::settings::get_int;

use crate::fingerprint_pool::FingerprintPool;
use crate::gate::DbGate;

/// A scratch directory next to the database (not the system temp dir, so a
/// backup never leaves the config dir's volume and permissions). Removed
/// with everything in it when dropped.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Why no scratch directory could be made (both a 500 for the host).
#[derive(Debug, thiserror::Error)]
pub enum ScratchError {
    #[error("{0}")]
    Io(std::io::Error),
    #[error("no scratch directory")]
    Exhausted,
}

/// A fresh scratch directory under `<config>/tmp`.
pub fn tempdir_in_config(config_dir: &Path) -> Result<TempDir, ScratchError> {
    let base = config_dir.join("tmp");
    std::fs::create_dir_all(&base).map_err(ScratchError::Io)?;
    for n in 0..1000u32 {
        let p = base.join(format!("api-{}-{n}", std::process::id()));
        if std::fs::create_dir(&p).is_ok() {
            return Ok(TempDir(p));
        }
    }
    Err(ScratchError::Exhausted)
}

/// Make a backup copy of the database in a fresh scratch directory and
/// hand its path and filename tag to `respond` (the host reads it into its
/// answer), all under the file lock for reading. The scratch directory is
/// removed once `respond` returns. Errors convert into the host's error
/// type where they happen.
pub fn backup_with<R, E>(
    db_gate: &DbGate,
    open: impl FnOnce() -> Result<Db, E>,
    config_dir: &Path,
    clock: &dyn Fn() -> SystemTime,
    respond: impl FnOnce(&Path, &str) -> Result<R, E>,
) -> Result<R, E>
where
    E: From<musaic_db::Error> + From<ScratchError>,
{
    let _g = db_gate.read().unwrap_or_else(|e| e.into_inner());
    let db = open()?;
    let tmp = tempdir_in_config(config_dir)?;
    let tag = backup::filename_tag(clock());
    let path = backup::make_backup_in(&db, tmp.path(), &tag)?;
    let r = respond(&path, &tag)?;
    drop(tmp);
    Ok(r)
}

/// The restore sequence, run while the caller holds the
/// [`crate::gate::RestoreGuard`]: the fingerprint pool never
/// holds the gate, so it is stopped (between two files) before the file is
/// swapped; then the test hook; then the file lock for writing, the
/// database, the time, `swap` (restore from an upload or a snapshot), and
/// on success `after` with the restored database while the lock is still
/// held (the secret scrub, the resolver memo). A refused restore converts
/// into the host's error type ([`RestoreError`], a 400 for HTTP).
pub fn restore_with<E>(
    pool: &FingerprintPool,
    on_restore: Option<&(dyn Fn() + Send + Sync)>,
    db_gate: &DbGate,
    open: impl FnOnce() -> Result<Db, E>,
    clock: &dyn Fn() -> SystemTime,
    swap: impl FnOnce(Db, &str, SystemTime) -> Result<(Db, RestoreInfo), Box<RestoreFailed>>,
    after: impl FnOnce(&Db),
) -> Result<RestoreInfo, E>
where
    E: From<RestoreError>,
{
    pool.stop();
    if let Some(h) = on_restore {
        h();
    }
    let _g = db_gate.write().unwrap_or_else(|e| e.into_inner());
    let db = open()?;
    let now = clock();
    match swap(db, &backup::filename_tag(now), now) {
        Ok((db, info)) => {
            after(&db);
            Ok(info)
        }
        Err(f) => Err(E::from(f.error)),
    }
}

/// 409 detail when a snapshot is asked for with retention 0.
pub const SNAPSHOTS_DISABLED: &str = "Snapshotting is disabled (retention count is 0). Set snapshot_retention_count > 0 in Settings first.";

/// Snapshotting is disabled (retention count 0; a 409 for HTTP).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotsDisabled;

/// Create a named snapshot in `snapshot_dir` under the file lock for
/// reading, pruning to the retention count (`snapshot_retention_count`,
/// clamped to 0..=100; 0 refuses). The caller holds a work guard.
pub fn create_snapshot_with<E>(
    db_gate: &DbGate,
    open: impl FnOnce() -> Result<Db, E>,
    snapshot_dir: &Path,
    clock: &dyn Fn() -> SystemTime,
) -> Result<CreatedSnapshot, E>
where
    E: From<musaic_db::Error> + From<SnapshotsDisabled>,
{
    let _g = db_gate.read().unwrap_or_else(|e| e.into_inner());
    let db = open()?;
    let retention = get_int(db.connection(), "snapshot_retention_count", 10)?.clamp(0, 100);
    if retention <= 0 {
        return Err(E::from(SnapshotsDisabled));
    }
    let now = clock();
    Ok(backup::create_snapshot(
        &db,
        snapshot_dir,
        retention,
        &backup::filename_tag(now),
        now,
    )?)
}
