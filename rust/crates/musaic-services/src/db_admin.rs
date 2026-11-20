//! Database backup, restore and named snapshots (`musaic/api/settings.py`,
//! including the upload size limits): the scratch directory next to the database,
//! the backup copy, the restore sequence under the restore gate (stop the
//! fingerprint pool, the test hook, the file lock, swap, the after-restore
//! work) and snapshot creation with its retention. It lives here rather than
//! in musaic-api so a non-HTTP face backs up and restores the same way. The
//! caller takes the gate's guards (a restore's [`crate::gate::RestoreGuard`], a
//! snapshot's work guard) and supplies the upload as a file; a refusal is an
//! [`OpError`] (a refused restore or a bad snapshot name `Rejected`, snapshots
//! disabled `Conflict`, anything unexpected `Internal`).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use musaic_db::Db;
use musaic_db::backup::{self, CreatedSnapshot, RestoreError, RestoreFailed, RestoreInfo};
use musaic_db::settings::get_int;

use crate::check_sweep::CheckSweeper;
use crate::error::OpError;
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

impl From<ScratchError> for OpError {
    fn from(e: ScratchError) -> Self {
        OpError::internal(e)
    }
}

/// A refused restore, or a snapshot name that is not acceptable: understood and
/// refused, with the restore's own sentence.
impl From<RestoreError> for OpError {
    fn from(e: RestoreError) -> Self {
        OpError::Rejected {
            message: e.to_string(),
            details: None,
        }
    }
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

/// Make a backup copy of the database in `dir` (a directory of the caller's,
/// which owns its removal) under the file lock for reading, and return the
/// copy's path and the filename tag it was named with.
pub fn backup_into(
    db_gate: &DbGate,
    open: impl FnOnce() -> Result<Db, OpError>,
    dir: &Path,
    clock: &dyn Fn() -> SystemTime,
) -> Result<(PathBuf, String), OpError> {
    let _g = db_gate.read().unwrap_or_else(|e| e.into_inner());
    let db = open()?;
    let tag = backup::filename_tag(clock());
    let path = backup::make_backup_in(&db, dir, &tag)?;
    Ok((path, tag))
}

/// The background workers that hold no work guard, which a restore quiets
/// before it touches the database ([`restore_with`]).
pub struct BackgroundWork<'a> {
    pub pool: &'a FingerprintPool,
    pub sweeper: &'a CheckSweeper,
}

/// The restore sequence, run while the caller holds the
/// [`crate::gate::RestoreGuard`]: the fingerprint pool never
/// holds the gate, so it is stopped (between two files) before the file is
/// swapped; nor does the background check sweep, so it is suspended (its
/// batch in progress ends and its connection closes) until the restore
/// returns, then plans again on the restored database; then the test hook; then the file lock for writing, the
/// database, the time, `swap` (restore from an upload or a snapshot), and
/// on success `after` with the restored database while the lock is still
/// held (the secret scrub, the resolver memo). A refused restore converts
/// into the host's error type ([`RestoreError`], a 400 for HTTP).
pub fn restore_with(
    background: BackgroundWork<'_>,
    on_restore: Option<&(dyn Fn() + Send + Sync)>,
    db_gate: &DbGate,
    open: impl FnOnce() -> Result<Db, OpError>,
    clock: &dyn Fn() -> SystemTime,
    swap: impl FnOnce(Db, &str, SystemTime) -> Result<(Db, RestoreInfo), Box<RestoreFailed>>,
    after: impl FnOnce(&Db),
) -> Result<RestoreInfo, OpError> {
    background.pool.stop();
    let _sweep = background.sweeper.suspend();
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
        Err(f) => Err(OpError::from(f.error)),
    }
}

/// 409 detail when a snapshot is asked for with retention 0.
pub const SNAPSHOTS_DISABLED: &str = "Snapshotting is disabled (retention count is 0). Set snapshot_retention_count > 0 in Settings first.";

impl OpError {
    /// Snapshotting is disabled (retention count 0): `Conflict`.
    fn snapshots_disabled() -> OpError {
        OpError::Conflict {
            message: SNAPSHOTS_DISABLED.to_string(),
            details: None,
        }
    }
}

/// Create a named snapshot in `snapshot_dir` under the file lock for
/// reading, pruning to the retention count (`snapshot_retention_count`,
/// clamped to 0..=100; 0 refuses). The caller holds a work guard.
pub fn create_snapshot_with(
    db_gate: &DbGate,
    open: impl FnOnce() -> Result<Db, OpError>,
    snapshot_dir: &Path,
    clock: &dyn Fn() -> SystemTime,
) -> Result<CreatedSnapshot, OpError> {
    let _g = db_gate.read().unwrap_or_else(|e| e.into_inner());
    let db = open()?;
    let retention = get_int(db.connection(), "snapshot_retention_count", 10)?.clamp(0, 100);
    if retention <= 0 {
        return Err(OpError::snapshots_disabled());
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
