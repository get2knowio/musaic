//! The `database` group: what the database file holds, a backup copy of it,
//! a restore from a file or from a named snapshot, and the snapshots.
//!
//! Inputs are files on disk, never an upload body: a face that receives an
//! upload writes it to a file (a scratch directory from [`Database::scratch`])
//! and passes the path. A write takes its guard itself: snapshot create and
//! delete the work guard, a restore the restore guard (refused while work
//! runs). Each is `Busy` with nothing changed.

use std::path::{Path, PathBuf};

use musaic_db::Db;
use musaic_db::backup::{self, RestoreFailed};
pub use musaic_db::backup::{Counts, CreatedSnapshot, DbInfo, RestoreInfo, SnapshotEntry};
use musaic_services::db_admin::{self, BackgroundWork};
pub use musaic_services::db_admin::{SNAPSHOTS_DISABLED, TempDir as Scratch};

use super::error::OpError;
use crate::App;

/// A backup copy: where it is and the name to offer it under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupFile {
    pub path: PathBuf,
    pub suggested_name: String,
}

/// A backup copy made by [`Database::backup`]: the file and the name to offer
/// it under. The file and its scratch directory are removed when this drops.
pub struct Backup {
    pub file: BackupFile,
    _dir: Scratch,
}

/// What [`Database::delete_snapshot`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotDeleted {
    /// Whether a file was there to remove.
    pub deleted: bool,
    pub name: String,
}

/// The `database` operations.
pub struct Database<'a> {
    pub(super) app: &'a App,
}

impl Database<'_> {
    fn open(&self) -> Result<Db, OpError> {
        let (db, _) = Db::open(self.app.db_path())?;
        Ok(db)
    }

    /// Runs while a restore still holds the gate and the file lock, so no
    /// writing work, pool, backup or snapshot of ours holds a connection that
    /// could block the scrub's checkpoint (only a short read request can). An
    /// outcome that is not clean is logged, reported as `keys_pending_removal`
    /// and retried later.
    fn after_restore(&self, db: &Db) {
        self.app.scrub_keys(db, "after restore");
        self.app.0.resolver.invalidate(None);
    }

    fn restore_with(
        &self,
        swap: impl FnOnce(
            Db,
            &str,
            std::time::SystemTime,
        ) -> Result<(Db, RestoreInfo), Box<RestoreFailed>>,
    ) -> Result<RestoreInfo, OpError> {
        let app = self.app;
        // Neither the fingerprint pool nor the check sweep holds the gate; a
        // restore stops the one and suspends the other before the file is
        // swapped.
        db_admin::restore_with(
            BackgroundWork {
                pool: &app.0.pool,
                sweeper: &app.0.sweeper,
            },
            app.0.on_restore.as_deref(),
            &app.0.db_gate,
            || self.open(),
            &|| app.now(),
            swap,
            |db| self.after_restore(db),
        )
    }

    /// Where the database is and what it holds, under the file lock for
    /// reading.
    pub fn info(&self) -> Result<DbInfo, OpError> {
        let _g = self.app.0.db_gate.read().unwrap_or_else(|e| e.into_inner());
        let db = self.open()?;
        Ok(backup::db_info(&db))
    }

    /// A fresh scratch directory next to the database (so a copy never leaves
    /// its volume), removed with everything in it when dropped.
    pub fn scratch(&self) -> Result<Scratch, OpError> {
        Ok(db_admin::tempdir_in_config(self.app.config_dir())?)
    }

    /// Write a backup copy of the database into the directory `dir` (the
    /// caller removes it), under the file lock for reading.
    pub fn backup_to(&self, dir: &Path) -> Result<BackupFile, OpError> {
        let (path, tag) =
            db_admin::backup_into(&self.app.0.db_gate, || self.open(), dir, &|| self.app.now())?;
        Ok(BackupFile {
            path,
            suggested_name: backup::suggested_backup_filename(&tag),
        })
    }

    /// Write a backup copy of the database into a scratch directory of its
    /// own, under the file lock for reading; the copy is removed when the
    /// returned [`Backup`] drops.
    pub fn backup(&self) -> Result<Backup, OpError> {
        let dir = self.scratch()?;
        let file = self.backup_to(dir.path())?;
        Ok(Backup { file, _dir: dir })
    }

    /// An early answer to "would a restore be allowed now" (`Busy` while a
    /// restore or any work runs), for a face that wants to refuse before it
    /// reads an upload. The guard [`Self::restore_from`] takes is what counts.
    pub fn restore_allowed(&self) -> Result<(), OpError> {
        Ok(self.app.gate().restore_allowed()?)
    }

    /// Replace the database with the file at `path` (an upload, a backup).
    /// Takes the restore guard (`Busy` while work runs), stops the fingerprint
    /// pool, suspends the check sweep, and swaps under the file lock; the
    /// previous database is kept aside. A file that is not a Musaic backup is
    /// `Rejected` and the live database is put back. Afterwards the secret
    /// scrub runs on the restored database and the resolver's memo is dropped.
    pub fn restore_from(&self, path: &Path) -> Result<RestoreInfo, OpError> {
        let _guard = self.app.gate().begin_restore()?;
        self.restore_with(|db, tag, now| backup::restore_from(db, path, tag, now))
    }

    /// [`Self::restore_from`] for a file a face wrote into `upload` (a
    /// [`Self::scratch`] directory): the directory and the file in it are
    /// removed right after the swap, so a large upload does not sit beside the
    /// restored database through the scrub that follows; on an earlier
    /// refusal, once the guard is released.
    pub fn restore_upload(&self, upload: Scratch, file: &Path) -> Result<RestoreInfo, OpError> {
        let _guard = self.app.gate().begin_restore()?;
        let mut upload = Some(upload);
        self.restore_with(|db, tag, now| {
            let r = backup::restore_from(db, file, tag, now);
            drop(upload.take());
            r
        })
    }

    /// The snapshots, in the order the backup module lists them.
    pub fn snapshots(&self) -> Result<Vec<SnapshotEntry>, OpError> {
        backup::list_snapshots(&self.app.0.config.snapshot_dir()).map_err(OpError::internal)
    }

    /// Snapshot the database now and prune to the retention count. `Conflict`
    /// while snapshotting is disabled (retention 0).
    pub fn create_snapshot(&self) -> Result<CreatedSnapshot, OpError> {
        let _guard = self.app.gate().begin_work()?;
        db_admin::create_snapshot_with(
            &self.app.0.db_gate,
            || self.open(),
            &self.app.0.config.snapshot_dir(),
            &|| self.app.now(),
        )
    }

    /// Delete a snapshot by file name: the guard first, then the name is looked
    /// at (a name that is not a snapshot's is `Rejected`).
    pub fn delete_snapshot(&self, name: &str) -> Result<SnapshotDeleted, OpError> {
        let _guard = self.app.gate().begin_work()?;
        let deleted = backup::delete_snapshot(&self.app.0.config.snapshot_dir(), name)?;
        Ok(SnapshotDeleted {
            deleted,
            name: name.to_string(),
        })
    }

    /// Replace the database with a named snapshot: the restore guard first,
    /// then the name is looked at (a bad or unknown name is `Rejected`, as
    /// the route always answered). Otherwise as [`Self::restore_from`].
    pub fn restore_snapshot(&self, name: &str) -> Result<RestoreInfo, OpError> {
        let _guard = self.app.gate().begin_restore()?;
        self.restore_with(|db, tag, now| {
            backup::restore_from_snapshot(db, &self.app.0.config.snapshot_dir(), name, tag, now)
        })
    }
}
