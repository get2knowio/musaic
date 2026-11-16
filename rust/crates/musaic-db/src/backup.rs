//! Database backup, restore and named snapshots.
//!
//! Ports the Python app's `db_backup.py`:
//!
//! * backups and snapshots are written with `VACUUM INTO`: one self-contained
//!   file, no `-wal`/`-shm`;
//! * snapshots live in `<config dir>/backups/` as
//!   `library-<YYYYMMDD-HHMMSSZ>-v<schema>.db`, a same-second clash adding
//!   `.1`, `.2`, ... before `.db`; listing is newest first by
//!   `(modified_at, tag, version, clash number)` descending, and retention
//!   prunes everything past the newest N snapshots (valid or not);
//! * a snapshot name is a bare file name: empty, containing `/` or `\`, or
//!   starting with `.` is rejected before touching the disk, and a name whose
//!   resolved path leaves the snapshot dir (a symlink) is rejected after;
//! * restore validates the upload first, moves `library.db` (+ `-wal`,
//!   `-shm`) aside as `<name>.bak-<tag>`, copies the upload into place and
//!   reopens.
//!
//! Differences from the Python app, all deliberate:
//!
//! * restore takes the live [`Db`] by value and closes it (checkpointing the
//!   WAL) before moving files, so the aside copy is complete on its own. The
//!   Python app moves the files under open connections.
//! * if the restored file cannot be opened, the previous database is put back
//!   (the Python app leaves the broken file live and reports an error).
//! * an upload older than v16 is accepted and migrated on reopen, as
//!   the Python app migrates it (only a version below `OLDEST_SCHEMA_VERSION` is
//!   refused); the newest accepted version is this build's, not
//!   the Python app's v16.
//! * the schema version in new file names is this build's ([`SCHEMA_VERSION`]).
//! * a name containing NUL is rejected as invalid (the Python app raises
//!   `ValueError`, an HTTP 500).
//! * only file names the app writes count as snapshots
//!   ([`parse_snapshot_name`]); the Python app took every `library-*.db`
//!   file for one (and restored from or deleted any named file), so a stray
//!   file in `backups/` could be listed, pruned, restored or deleted. Now it
//!   is never touched.
//! * same-second snapshots are ordered by their clash number, so `.2` is
//!   newer than `.1`, which is newer than the plain name (the Python app
//!   compared names as text, which put the plain name first and pruned `.1`
//!   before it, and `.10` before `.9`).
//!
//! The clock is a parameter everywhere (`tag`, `now`) so tests can script it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags};

use crate::{Db, OLDEST_SCHEMA_VERSION, Opened, SCHEMA_VERSION};

pub const SNAPSHOT_PREFIX: &str = "library-";
pub const SNAPSHOT_SUFFIX: &str = ".db";

/// Tables an upload must have to count as a Musaic database.
const REQUIRED_TABLES: [&str; 3] = ["albums", "libraries", "schema_version"];

const COUNT_QUERIES: [(&str, &str); 7] = [
    ("albums", "SELECT COUNT(*) FROM albums"),
    (
        "albums_with_mbid",
        "SELECT COUNT(*) FROM albums WHERE mb_release_group_mbid IS NOT NULL",
    ),
    (
        "track_fingerprints",
        "SELECT COUNT(*) FROM track_fingerprints",
    ),
    ("track_tags", "SELECT COUNT(*) FROM track_tags"),
    ("tag_vocabulary", "SELECT COUNT(*) FROM tag_vocabulary"),
    ("ai_jobs", "SELECT COUNT(*) FROM ai_jobs"),
    (
        "check_findings_open",
        "SELECT COUNT(*) FROM check_findings WHERE resolved_at IS NULL AND ignored_at IS NULL",
    ),
];

/// Row counts for the headline tables; a table that is missing counts 0.
pub type Counts = BTreeMap<String, i64>;

pub fn gather_counts(conn: &Connection) -> Counts {
    COUNT_QUERIES
        .iter()
        .map(|(label, sql)| {
            let n = conn.query_row(sql, [], |r| r.get(0)).unwrap_or(0);
            ((*label).to_owned(), n)
        })
        .collect()
}

// ------------------------------------------------------------------ clock

/// `YYYYMMDD-HHMMSSZ` in UTC, the tag in backup and aside file names.
pub fn filename_tag(t: SystemTime) -> String {
    let (y, mo, d, h, mi, s) = utc_parts(unix_secs(t));
    format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}Z")
}

/// `YYYY-MM-DDTHH:MM:SSZ` in UTC (Python's `isoformat(timespec="seconds")` with `Z`).
pub fn iso_seconds(t: SystemTime) -> String {
    iso_from_secs(unix_secs(t))
}

fn iso_from_secs(secs: i64) -> String {
    let (y, mo, d, h, mi, s) = utc_parts(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

fn unix_secs(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs_f64().ceil() as i64),
    }
}

fn utc_parts(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (
        y,
        m,
        d,
        (rem / 3600) as u32,
        (rem % 3600 / 60) as u32,
        (rem % 60) as u32,
    )
}

// ------------------------------------------------------------------ errors

/// Validation or IO failure. `Display` is the user-facing message, worded as
/// the Python app words it where the case exists there.
#[derive(Debug, thiserror::Error)]
pub enum RestoreError {
    #[error("Upload path doesn't exist: {}", .0.display())]
    UploadMissing(PathBuf),
    #[error("Upload is too small to be a SQLite database.")]
    TooSmall,
    #[error("Upload doesn't look like a SQLite database — wrong file format.")]
    NotSqlite,
    #[error("Could not open upload as SQLite: {0}")]
    CannotOpen(String),
    #[error("Upload is a SQLite DB but doesn't look like a Musaic backup — missing tables: {0}")]
    MissingTables(String),
    #[error("Could not read schema_version from upload: {0}")]
    SchemaUnreadable(String),
    #[error("Upload's schema_version table is empty — refusing to restore.")]
    SchemaEmpty,
    #[error(
        "Upload is from a newer build (schema v{found}); this build only knows up to v{supported}. Update Musaic before restoring."
    )]
    Newer { found: i64, supported: i64 },
    #[error(
        "Upload's schema_version v{found} is older than any Musaic database (oldest v{oldest}); refusing to restore."
    )]
    Older { found: i64, oldest: i64 },
    #[error("Invalid snapshot name: {}", py_repr(.0))]
    InvalidName(String),
    #[error("Snapshot name escapes backup dir: {}", py_repr(.0))]
    Escapes(String),
    #[error("Snapshot not found: {0}")]
    NotFound(String),
    #[error("Couldn't replace live DB: {0}")]
    Swap(std::io::Error),
    #[error("The restored database could not be opened ({0}); the previous database was put back.")]
    OpenFailed(crate::Error),
    #[error(transparent)]
    Db(#[from] crate::Error),
}

impl From<rusqlite::Error> for RestoreError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Db(e.into())
    }
}

impl From<std::io::Error> for RestoreError {
    fn from(e: std::io::Error) -> Self {
        Self::Db(e.into())
    }
}

/// Python's `repr()` of a str, enough for names in messages.
fn py_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::new();
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

// ------------------------------------------------------------------ info / backup

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbInfo {
    pub path: PathBuf,
    pub exists: bool,
    pub size_bytes: u64,
    pub schema_version: Option<i64>,
    pub current_schema: i64,
    pub counts: Counts,
    pub modified_at: Option<String>,
}

/// `GET /api/db/info`.
pub fn db_info(db: &Db) -> DbInfo {
    let meta = std::fs::metadata(db.path()).ok().filter(|m| m.is_file());
    DbInfo {
        path: db.path().to_path_buf(),
        exists: meta.is_some(),
        size_bytes: meta.as_ref().map_or(0, |m| m.len()),
        schema_version: db.schema_version().ok(),
        current_schema: SCHEMA_VERSION,
        counts: gather_counts(db.connection()),
        modified_at: meta.and_then(|m| m.modified().ok()).map(iso_seconds),
    }
}

/// `VACUUM INTO target`: a clean single-file copy of the live database.
/// Refuses (SQLite error) when `target` exists.
pub fn vacuum_into(conn: &Connection, target: &Path) -> crate::Result<()> {
    let Some(t) = target.to_str() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "backup path is not valid UTF-8",
        )
        .into());
    };
    conn.execute_batch(&format!("VACUUM INTO '{}'", t.replace('\'', "''")))?;
    Ok(())
}

/// The name a downloaded backup arrives as: `musaic-v<schema>-<tag>.db`.
pub fn suggested_backup_filename(tag: &str) -> String {
    format!("musaic-v{SCHEMA_VERSION}-{tag}.db")
}

/// `make_backup(target_dir)`: writes `<dir>/library-<tag>.db`, replacing a
/// file of that name. (The download route's temp-file variant is the
/// caller's choice of `dir`.)
pub fn make_backup_in(db: &Db, dir: &Path, tag: &str) -> crate::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let target = dir.join(format!("{SNAPSHOT_PREFIX}{tag}{SNAPSHOT_SUFFIX}"));
    if target.exists() {
        std::fs::remove_file(&target)?;
    }
    vacuum_into(db.connection(), &target)?;
    Ok(target)
}

// ------------------------------------------------------------------ restore

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadInfo {
    pub schema_version: i64,
    pub size_bytes: u64,
    pub counts: Counts,
}

/// Confirm `path` is a Musaic database this build can open, without changing it.
pub fn validate_upload(path: &Path) -> Result<UploadInfo, RestoreError> {
    let meta = match std::fs::metadata(path) {
        Ok(m) if m.is_file() => m,
        _ => return Err(RestoreError::UploadMissing(path.to_path_buf())),
    };
    if meta.len() < 16 {
        return Err(RestoreError::TooSmall);
    }
    let mut header = [0u8; 15];
    {
        use std::io::Read;
        std::fs::File::open(path)?.read_exact(&mut header)?;
    }
    if &header != b"SQLite format 3" {
        return Err(RestoreError::NotSqlite);
    }
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| RestoreError::CannotOpen(e.to_string()))?;
    let tables: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table'")
            .map_err(|e| RestoreError::CannotOpen(e.to_string()))?;
        stmt.query_map([], |r| r.get(0))
            .and_then(|rows| rows.collect())
            .map_err(|e| RestoreError::CannotOpen(e.to_string()))?
    };
    let missing: Vec<&str> = REQUIRED_TABLES
        .iter()
        .copied()
        .filter(|t| !tables.iter().any(|n| n == t))
        .collect();
    if !missing.is_empty() {
        return Err(RestoreError::MissingTables(missing.join(", ")));
    }
    let version: i64 = match conn.query_row("SELECT version FROM schema_version LIMIT 1", [], |r| {
        r.get(0)
    }) {
        Ok(v) => v,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Err(RestoreError::SchemaEmpty),
        Err(e) => return Err(RestoreError::SchemaUnreadable(sqlite_message(&e))),
    };
    if version > SCHEMA_VERSION {
        return Err(RestoreError::Newer {
            found: version,
            supported: SCHEMA_VERSION,
        });
    }
    if version < OLDEST_SCHEMA_VERSION {
        return Err(RestoreError::Older {
            found: version,
            oldest: OLDEST_SCHEMA_VERSION,
        });
    }
    Ok(UploadInfo {
        schema_version: version,
        size_bytes: meta.len(),
        counts: gather_counts(&conn),
    })
}

/// SQLite's own message (what Python's `str(sqlite3.Error)` shows).
fn sqlite_message(e: &rusqlite::Error) -> String {
    match e {
        rusqlite::Error::SqliteFailure(_, Some(msg)) => msg.clone(),
        rusqlite::Error::SqlInputError { msg, .. } => msg.clone(),
        other => other.to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreInfo {
    /// The upload's version before this build migrated it.
    pub schema_version: i64,
    pub size_bytes: u64,
    pub counts: Counts,
    pub restored_at: String,
    /// Where the previous `library.db` (and any `-wal` / `-shm`) went.
    pub aside_paths: Vec<PathBuf>,
    /// What reopening did (an older upload is migrated to [`SCHEMA_VERSION`]).
    pub opened: Opened,
}

/// A failed restore. `db` is the live database, open again, whenever it could
/// be reopened (always, unless the disk itself is failing).
pub struct RestoreFailed {
    pub error: RestoreError,
    pub db: Option<Db>,
}

impl std::fmt::Debug for RestoreFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RestoreFailed")
            .field("error", &self.error)
            .field("db_open", &self.db.is_some())
            .finish()
    }
}

/// Replace the live database with `upload`. `tag` names the aside
/// files (`library.db.bak-<tag>`), `now` stamps `restored_at`.
///
/// Order: validate (nothing touched on failure) -> checkpoint and close the
/// live connection -> move `library.db`, `-wal`, `-shm` aside -> copy the
/// upload in -> open it (migrating an older file to [`SCHEMA_VERSION`]). A failure after the move puts
/// the previous files back and reopens them.
///
/// Secrets: an upload written by the Python app may carry API keys in its
/// settings table. The caller must run the secrets migration (musaic-secrets)
/// on the returned `Db`, exactly as after any other open.
pub fn restore_from(
    db: Db,
    upload: &Path,
    tag: &str,
    now: SystemTime,
) -> Result<(Db, RestoreInfo), Box<RestoreFailed>> {
    let info = match validate_upload(upload) {
        Ok(i) => i,
        Err(error) => {
            return Err(Box::new(RestoreFailed {
                error,
                db: Some(db),
            }));
        }
    };
    let live = db.path().to_path_buf();
    if let Err(error) = close_checkpointed(db) {
        let db = Db::open(&live).ok().map(|(d, _)| d);
        return Err(Box::new(RestoreFailed { error, db }));
    }

    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    let swap = (|| -> std::io::Result<()> {
        if let Some(parent) = live.parent() {
            std::fs::create_dir_all(parent)?;
        }
        for ext in ["", "-wal", "-shm"] {
            let src = PathBuf::from(format!("{}{ext}", live.display()));
            if src.exists() {
                let dst = PathBuf::from(format!("{}.bak-{tag}", src.display()));
                std::fs::rename(&src, &dst)?;
                moved.push((src, dst));
            }
        }
        std::fs::copy(upload, &live)?;
        Ok(())
    })();
    if let Err(e) = swap {
        put_back(&live, &moved);
        return Err(Box::new(RestoreFailed {
            error: RestoreError::Swap(e),
            db: Db::open(&live).ok().map(|(d, _)| d),
        }));
    }

    match Db::open(&live) {
        Ok((db, opened)) => Ok((
            db,
            RestoreInfo {
                schema_version: info.schema_version,
                size_bytes: info.size_bytes,
                counts: info.counts,
                restored_at: iso_seconds(now),
                aside_paths: moved.into_iter().map(|(_, d)| d).collect(),
                opened,
            },
        )),
        Err(e) => {
            put_back(&live, &moved);
            Err(Box::new(RestoreFailed {
                error: RestoreError::OpenFailed(e),
                db: Db::open(&live).ok().map(|(d, _)| d),
            }))
        }
    }
}

/// Fold the WAL into the main file and close, so the main file alone is the
/// whole database.
fn close_checkpointed(db: Db) -> Result<(), RestoreError> {
    let Db { conn, .. } = db;
    let mode: String = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
    if mode.eq_ignore_ascii_case("wal") {
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
    }
    conn.close().map_err(|(_, e)| RestoreError::from(e))
}

/// Undo a partial swap: drop whatever is at `live` (and its WAL/SHM, which
/// would belong to the restored file) and move the asides back.
fn put_back(live: &Path, moved: &[(PathBuf, PathBuf)]) {
    for ext in ["", "-wal", "-shm"] {
        let p = PathBuf::from(format!("{}{ext}", live.display()));
        if !moved.iter().any(|(_, d)| d == &p) {
            let _ = std::fs::remove_file(&p);
        }
    }
    for (src, dst) in moved.iter().rev() {
        let _ = std::fs::rename(dst, src);
    }
}

// ------------------------------------------------------------------ snapshots

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotEntry {
    pub filename: String,
    pub size_bytes: u64,
    pub modified_at: String,
    pub schema_version: Option<i64>,
    pub valid: bool,
    pub counts: Counts,
    /// mtime in whole seconds (what `modified_at` shows).
    pub modified_unix: i64,
}

/// What a snapshot's file name says about it, in age order: the tag
/// (`YYYYMMDD-HHMMSSZ`, which sorts as text), the schema version, and the
/// clash number (0 for the plain name, then 1, 2, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SnapshotName<'a> {
    pub tag: &'a str,
    pub version: u64,
    pub clash: u64,
}

/// `name` as [`create_snapshot`] (or the Python app) writes it:
/// `library-<YYYYMMDD-HHMMSSZ>-v<schema>.db`, or with `.<n>` before `.db`
/// after a same-second clash. Anything else is not a snapshot: the app
/// never lists, prunes, restores from or deletes it.
pub fn parse_snapshot_name(name: &str) -> Option<SnapshotName<'_>> {
    fn number(s: &str) -> Option<u64> {
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        s.parse().ok()
    }
    let rest = name
        .strip_prefix(SNAPSHOT_PREFIX)?
        .strip_suffix(SNAPSHOT_SUFFIX)?;
    let tag = rest.get(..16)?;
    let b = tag.as_bytes();
    let tag_ok = b.iter().enumerate().all(|(i, c)| match i {
        8 => *c == b'-',
        15 => *c == b'Z',
        _ => c.is_ascii_digit(),
    });
    if !tag_ok {
        return None;
    }
    let rest = rest[16..].strip_prefix("-v")?;
    let (version, clash) = match rest.split_once('.') {
        Some((v, n)) => (number(v)?, number(n)?),
        None => (number(rest)?, 0),
    };
    Some(SnapshotName {
        tag,
        version,
        clash,
    })
}

/// Whether `name` is a snapshot file name the app writes ([`parse_snapshot_name`]).
pub fn is_snapshot_name(name: &str) -> bool {
    parse_snapshot_name(name).is_some()
}

/// Every regular file (or link to one) in `dir` whose name is a snapshot
/// name ([`parse_snapshot_name`]), newest first: by whole-second mtime, then
/// by what the name says (tag, version, clash number). Files that do not
/// open as a Musaic database are listed with `valid: false` so they can be
/// deleted. Creates `dir` if needed.
pub fn list_snapshots(dir: &Path) -> std::io::Result<Vec<SnapshotEntry>> {
    std::fs::create_dir_all(dir)?;
    let mut entries = Vec::new();
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let Ok(name) = e.file_name().into_string() else {
            continue;
        };
        if !is_snapshot_name(&name) {
            continue;
        }
        let path = e.path();
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let modified_unix = meta.modified().map(unix_secs).unwrap_or(0);
        let mut entry = SnapshotEntry {
            filename: name,
            size_bytes: meta.len(),
            modified_at: iso_from_secs(modified_unix),
            schema_version: None,
            valid: false,
            counts: Counts::new(),
            modified_unix,
        };
        if let Ok(conn) = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) && let Ok(v) = conn.query_row("SELECT version FROM schema_version LIMIT 1", [], |r| {
            r.get::<_, i64>(0)
        }) {
            entry.schema_version = Some(v);
            entry.valid = true;
            entry.counts = gather_counts(&conn);
        }
        entries.push(entry);
    }
    // Every listed name parses (checked above).
    fn key(e: &SnapshotEntry) -> (i64, Option<SnapshotName<'_>>) {
        (e.modified_unix, parse_snapshot_name(&e.filename))
    }
    entries.sort_by(|a, b| key(b).cmp(&key(a)));
    Ok(entries)
}

/// A snapshot name to its path inside `dir`, refusing anything that is not a
/// bare file name, that resolves outside `dir`, that is not there, or whose
/// name the app does not write ([`parse_snapshot_name`]; checked last, so a
/// missing file still answers "not found" as the Python app does).
pub fn resolve_snapshot(dir: &Path, name: &str) -> Result<PathBuf, RestoreError> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.starts_with('.')
        || name.contains('\0')
    {
        return Err(RestoreError::InvalidName(name.to_owned()));
    }
    let p = musaic_core::config::resolve_path(&dir.join(name))?;
    let snap_dir = musaic_core::config::resolve_path(dir)?;
    if p.strip_prefix(&snap_dir).is_err() {
        return Err(RestoreError::Escapes(name.to_owned()));
    }
    if !p.is_file() {
        return Err(RestoreError::NotFound(name.to_owned()));
    }
    if !is_snapshot_name(name) {
        return Err(RestoreError::InvalidName(name.to_owned()));
    }
    Ok(p)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedSnapshot {
    pub filename: String,
    pub size_bytes: u64,
    pub modified_at: String,
    pub schema_version: i64,
    pub pruned: Vec<String>,
}

/// `library-<tag>-v<schema>.db`, the name a new snapshot gets before clash handling.
pub fn snapshot_file_name(tag: &str) -> String {
    format!("{SNAPSHOT_PREFIX}{tag}-v{SCHEMA_VERSION}{SNAPSHOT_SUFFIX}")
}

/// Take a snapshot now and prune to the newest `retention` (0 or less: no pruning).
pub fn create_snapshot(
    db: &Db,
    dir: &Path,
    retention: i64,
    tag: &str,
    now: SystemTime,
) -> crate::Result<CreatedSnapshot> {
    std::fs::create_dir_all(dir)?;
    let base = snapshot_file_name(tag);
    let stem = &base[..base.len() - SNAPSHOT_SUFFIX.len()];
    let mut target = dir.join(&base);
    let mut n = 0;
    while target.exists() {
        n += 1;
        target = dir.join(format!("{stem}.{n}{SNAPSHOT_SUFFIX}"));
    }
    vacuum_into(db.connection(), &target)?;
    let size_bytes = std::fs::metadata(&target)?.len();
    let pruned = prune_snapshots(dir, retention)?;
    Ok(CreatedSnapshot {
        filename: target
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_owned(),
        size_bytes,
        modified_at: iso_seconds(now),
        schema_version: SCHEMA_VERSION,
        pruned,
    })
}

fn prune_snapshots(dir: &Path, retention: i64) -> std::io::Result<Vec<String>> {
    if retention <= 0 {
        return Ok(Vec::new());
    }
    let snaps = list_snapshots(dir)?;
    let keep = usize::try_from(retention).unwrap_or(usize::MAX);
    let mut deleted = Vec::new();
    for s in snaps.into_iter().skip(keep) {
        match std::fs::remove_file(dir.join(&s.filename)) {
            Ok(()) => deleted.push(s.filename),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => deleted.push(s.filename),
            Err(_) => {} // the Python app logs and carries on
        }
    }
    Ok(deleted)
}

/// Remove one snapshot. `Ok(false)` if it vanished between the check and the delete.
pub fn delete_snapshot(dir: &Path, name: &str) -> Result<bool, RestoreError> {
    let p = resolve_snapshot(dir, name)?;
    match std::fs::remove_file(p) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Restore the live database from a snapshot in `dir`.
pub fn restore_from_snapshot(
    db: Db,
    dir: &Path,
    name: &str,
    tag: &str,
    now: SystemTime,
) -> Result<(Db, RestoreInfo), Box<RestoreFailed>> {
    match resolve_snapshot(dir, name) {
        Ok(p) => restore_from(db, &p, tag, now),
        Err(error) => Err(Box::new(RestoreFailed {
            error,
            db: Some(db),
        })),
    }
}

/// Seconds since the newest snapshot's (whole-second) mtime; `None` without snapshots.
pub fn latest_snapshot_age_seconds(dir: &Path, now: SystemTime) -> std::io::Result<Option<f64>> {
    let snaps = list_snapshots(dir)?;
    Ok(snaps.first().map(|s| {
        let now = now
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs_f64();
        now - s.modified_unix as f64
    }))
}

/// The startup hook: snapshot unless retention is 0 or the newest snapshot is
/// younger than `min_interval_hours` (0 or less: always snapshot).
pub fn maybe_auto_snapshot(
    db: &Db,
    dir: &Path,
    retention: i64,
    min_interval_hours: f64,
    tag: &str,
    now: SystemTime,
) -> crate::Result<Option<CreatedSnapshot>> {
    if retention <= 0 {
        return Ok(None);
    }
    if min_interval_hours > 0.0
        && let Some(age) = latest_snapshot_age_seconds(dir, now)?
        && age < min_interval_hours * 3600.0
    {
        return Ok(None);
    }
    create_snapshot(db, dir, retention, tag, now).map(Some)
}

/// Suffixes of the files a SQLite database at `p` may have beside it.
const SQLITE_SIDECARS: [&str; 3] = ["-journal", "-wal", "-shm"];

/// Remove a temporary database copy and every SQLite file beside it
/// (`-journal`, `-wal`, `-shm`). A copy interrupted by a crash, e.g. the
/// pre-migration backup's `library.v16.before-rust.db.tmp`, holds
/// whatever the source held, settings rows with API keys included;
/// a hot `-journal` or a `-wal` beside it can hold more. The main file goes
/// first, so an interruption here never leaves a sidecar that could be
/// applied to a later file of the same name without the next call deleting
/// it too. A file that is not there is not an error.
pub fn remove_db_with_sidecars(p: &Path) -> std::io::Result<()> {
    let base = p.as_os_str().to_owned();
    let mut paths = vec![p.to_path_buf()];
    for s in SQLITE_SIDECARS {
        let mut name = base.clone();
        name.push(s);
        paths.push(PathBuf::from(name));
    }
    for path in paths {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
