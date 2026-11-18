//! Persistence layer.
//!
//! Opens the SQLite database the Python app wrote (schema v16), takes a
//! backup, and migrates it one way to v19 (v17: `fingerprint_impl`; v18:
//! the AcoustID lookup outcome and rejected files; v19: `check_inputs`,
//! what each health check last ran against); or creates a fresh
//! database ([`Db::open_or_create`]) with the Python app's v16 statements
//! followed by the same migrations, so a fresh file and a migrated one have
//! identical schemas.

pub mod ai_jobs;
pub mod albums;
pub mod backup;
pub mod check_inputs;
pub mod findings;
pub mod fingerprints_ext;
pub mod mb_anchor;
mod migrate;
mod python_chain;
pub mod renames;
mod schema_migrations;
mod schema_v16;
pub mod settings;
pub mod track_tags;

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::Connection;

pub use migrate::{
    BACKUP_FILE_NAME, FINGERPRINT_IMPL_FPCALC, FINGERPRINT_IMPL_RUST, LOOKUP_DONE, LOOKUP_FAILED,
    backup_file_name, existing_backup,
};
pub use musaic_core::album_id;
pub use python_chain::OLDEST_SCHEMA_VERSION;

/// Schema version this build reads and writes.
pub const SCHEMA_VERSION: i64 = 19;
/// Last schema version written by the Python app.
pub const PYTHON_SCHEMA_VERSION: i64 = 16;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "database schema v{found} is newer than this build understands (v{supported}); refusing to open it"
    )]
    NewerSchema { found: i64, supported: i64 },
    #[error(
        "database schema v{found} is older than any Musaic database (oldest v{oldest}); refusing to open it"
    )]
    TooOld { found: i64, oldest: i64 },
    #[error("{0} has no schema_version row; not a Musaic library database")]
    NotALibrary(PathBuf),
    /// A row a scan planned to move to a new id changed (or the new id was
    /// taken) between the plan and the save; nothing was written.
    #[error(
        "album {old_id} changed while the scan ran, so it cannot move to {new_id}; nothing was saved"
    )]
    AdoptionStale { old_id: String, new_id: String },
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// What `Db::open` did to bring the database to [`SCHEMA_VERSION`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Opened {
    /// Already at the current version; nothing changed.
    Current,
    /// Was older: migrated. `backup` is the copy taken first, for a database
    /// the Python app wrote (v16 or older; the existing one if an earlier
    /// attempt took it); a v17 or v18 one (this build's) gets none.
    Migrated { from: i64, backup: Option<PathBuf> },
    /// There was no database (no file, an empty file, or a file with no
    /// schema objects at all): created at [`SCHEMA_VERSION`], no backup.
    Created,
}

/// `sqlite3.connect(..., timeout=30)` in the Python app's `db._connect_raw`
/// (testdata/golden/persistence/fresh-db.json `connection_pragmas`).
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(30);

/// The per-connection state of every connection the Python app's `db.connect()`
/// hands out: `busy_timeout` 30000 ms, `foreign_keys=ON`, and
/// `synchronous=FULL`. The Python app sets `synchronous=NORMAL` only on
/// `bootstrap()`'s own short-lived connection, so its working connections run
/// at SQLite's default, FULL (recorded, not assumed: the golden's
/// `connection_pragmas` against `bootstrap_connection_pragmas`). FULL is set
/// explicitly so a build of SQLite with another default cannot change it.
/// Autocommit (`isolation_level=None`) is rusqlite's default already.
pub fn configure_connection(conn: &Connection) -> Result<()> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    Ok(())
}

/// A further connection to an already opened library database, configured as
/// [`configure_connection`] does (the Python app's `db.connect()` after
/// bootstrap). It neither migrates nor changes the journal mode: open the
/// database once with [`Db::open`] / [`Db::open_or_create`] first, which
/// leaves it in WAL mode. Other crates that need their own connection (worker
/// threads, background pools) should use this rather than `Connection::open`.
pub fn open_connection(path: impl AsRef<Path>) -> Result<Connection> {
    let conn = Connection::open(path.as_ref())?;
    configure_connection(&conn)?;
    Ok(conn)
}

/// What [`checkpoint_on_close`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseCheckpoint {
    /// No database file: nothing to do (none is created).
    NoDatabase,
    /// The database is not in WAL mode: nothing to fold.
    NotWal,
    /// The WAL was folded into the main file and truncated.
    Done,
    /// Another connection still held the database after `busy_timeout`: the
    /// WAL was not (fully) folded. SQLite finishes it on the next open.
    Blocked,
}

/// Fold the WAL into the main database file and truncate it
/// (`wal_checkpoint(TRUNCATE)`), on a connection of its own that waits at
/// most `busy_timeout` for other connections, then close that connection.
/// The app calls this last thing on shutdown, after its own workers have
/// stopped, so the main file alone is the whole database (and, being the
/// last connection, closing it removes the WAL file too). Never creates a
/// database.
pub fn checkpoint_on_close(
    path: impl AsRef<Path>,
    busy_timeout: Duration,
) -> Result<CloseCheckpoint> {
    let path = path.as_ref();
    if !path.is_file() {
        return Ok(CloseCheckpoint::NoDatabase);
    }
    let conn = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(busy_timeout)?;
    let mode: String = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
    let outcome = if mode.eq_ignore_ascii_case("wal") {
        // (busy, frames in the WAL, frames checkpointed); `busy` is 1 when
        // a reader or writer kept the checkpoint from completing.
        let (busy, log, done): (i64, i64, i64) =
            conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
        if busy == 0 && log <= done {
            CloseCheckpoint::Done
        } else {
            CloseCheckpoint::Blocked
        }
    } else {
        CloseCheckpoint::NotWal
    };
    conn.close().map_err(|(_, e)| Error::from(e))?;
    Ok(outcome)
}

/// Begin a write transaction with `BEGIN IMMEDIATE`.
///
/// Every helper here that reads and then writes inside one transaction uses
/// this instead of rusqlite's default DEFERRED `BEGIN`. A deferred transaction
/// that has already read holds a WAL read snapshot; when it then asks for the
/// write lock SQLite does not call the busy handler (it only retries a lock
/// taken from no transaction at all), so a concurrent writer makes it fail at
/// once with "database is locked", and a commit by another connection since
/// its snapshot makes it fail with SQLITE_BUSY_SNAPSHOT, `busy_timeout`
/// notwithstanding. IMMEDIATE takes the write lock as the transaction begins,
/// where the 30 s busy handler applies, and its snapshot is always current.
/// The Python app's `db.transaction` issues a plain (deferred) `BEGIN`; the
/// atomicity is the same, only the lock is taken earlier
/// (tests/write_locking.rs). Works on `&Connection` and `&mut Connection`.
pub fn immediate(conn: &Connection) -> rusqlite::Result<rusqlite::Transaction<'_>> {
    rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)
}

pub struct Db {
    conn: Connection,
    path: PathBuf,
}

impl Db {
    /// Open `path`, migrating it to [`SCHEMA_VERSION`]; a database the
    /// Python app wrote (v16, or older: brought to v16 by the Python app's own
    /// chain first) is backed up first.
    ///
    /// Refuses a database newer than this build knows, or with a version
    /// below [`OLDEST_SCHEMA_VERSION`].
    ///
    /// Like the Python app's `bootstrap()`, which runs on every process start, an
    /// accepted database is switched to `journal_mode=WAL` (a file left in
    /// another mode is converted, as the Python app converts it) with
    /// `synchronous=NORMAL` while any migration runs; the connection handed
    /// out then has [`configure_connection`]'s state. A refused
    /// file is not touched.
    pub fn open(path: impl AsRef<Path>) -> Result<(Self, Opened)> {
        let path = path.as_ref().to_path_buf();
        let mut conn = Connection::open(&path)?;
        configure_connection(&conn)?;
        let found = schema_version(&conn, &path)?;
        let opened = if found > SCHEMA_VERSION {
            return Err(Error::NewerSchema {
                found,
                supported: SCHEMA_VERSION,
            });
        } else if found < OLDEST_SCHEMA_VERSION {
            return Err(Error::TooOld {
                found,
                oldest: OLDEST_SCHEMA_VERSION,
            });
        } else {
            conn.pragma_update(None, "journal_mode", "WAL")?;
            conn.pragma_update(None, "synchronous", "NORMAL")?;
            if found == SCHEMA_VERSION {
                Opened::Current
            } else {
                let backup = if found <= PYTHON_SCHEMA_VERSION {
                    Some(migrate::backup_before_migration(&conn, &path, found)?)
                } else {
                    None
                };
                migrate::upgrade(&mut conn, found)?;
                Opened::Migrated {
                    from: found,
                    backup,
                }
            }
        };
        configure_connection(&conn)?;
        Ok((Self { conn, path }, opened))
    }

    /// [`Db::open`], except that a missing database is created first: when
    /// `path` does not exist, is empty, or holds no schema objects at all
    /// (an earlier creation that was interrupted before its transaction
    /// committed), the Python app's v16 schema is created (`db.bootstrap()`:
    /// `journal_mode=WAL`, then every `SCHEMA_STATEMENTS` entry and the
    /// version row in one transaction), then migrated to [`SCHEMA_VERSION`] in place. No
    /// backup is written: there is nothing to go back to. Any other file is
    /// opened exactly as [`Db::open`] does (a non-library file is still
    /// refused with [`Error::NotALibrary`]).
    pub fn open_or_create(path: impl AsRef<Path>) -> Result<(Self, Opened)> {
        let path = path.as_ref().to_path_buf();
        let fresh = match std::fs::metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
            Err(e) => return Err(e.into()),
            Ok(m) => m.len() == 0 || is_blank(&path)?,
        };
        if !fresh {
            return Self::open(&path);
        }
        let mut conn = Connection::open(&path)?;
        configure_connection(&conn)?;
        migrate::create_fresh(&mut conn)?;
        configure_connection(&conn)?;
        Ok((Self { conn, path }, Opened::Created))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    pub fn connection_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    pub fn schema_version(&self) -> Result<i64> {
        schema_version(&self.conn, &self.path)
    }

    /// `(id, path)` for every album row, in id order.
    pub fn album_paths(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, path FROM albums ORDER BY id")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
}

/// A SQLite file with no schema objects (what an interrupted creation leaves).
fn is_blank(path: &Path) -> Result<bool> {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM sqlite_master", [], |r| r.get(0))?;
    Ok(n == 0)
}

fn schema_version(conn: &Connection, path: &Path) -> Result<i64> {
    let has_table: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'schema_version')",
        [],
        |r| r.get(0),
    )?;
    if !has_table {
        return Err(Error::NotALibrary(path.to_path_buf()));
    }
    conn.query_row("SELECT version FROM schema_version LIMIT 1", [], |r| {
        r.get(0)
    })
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Error::NotALibrary(path.to_path_buf()),
        other => Error::Sqlite(other),
    })
}
