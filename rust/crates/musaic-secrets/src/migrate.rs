//! First-launch move of secret settings rows into the store.
//!
//! The keyring is the single source of truth. For each of
//! [`MIGRATED_SECRET_KEYS`] that has a row in the live `settings` table:
//!
//! 1. the value is stripped, as the Python accessors strip it on read
//!    (`(get_raw(key) or "").strip()`); a blank row holds nothing and is dropped;
//! 2. if the keyring already holds a non-empty value for the key, the keyring
//!    wins: nothing is written and the row is dropped (so restoring an old
//!    backup never brings an old key back);
//! 3. otherwise the row's value is written to the keyring and read back;
//! 4. rows are deleted only for keys the keyring demonstrably holds (or blank
//!    rows). In WAL mode (every real database) the WAL is first checkpointed
//!    and truncated, so the rows' newest copies, which the Python app may have
//!    left only there, are folded into the main file; if another connection
//!    blocks that, nothing is deleted and the call fails (retried next time).
//!    Then `VACUUM` with `secure_delete` on rebuilds the file while the rows
//!    still exist (older values the Python app overwrote stay in free space
//!    otherwise), the rows are deleted in one transaction with `secure_delete`
//!    on (their cells are zeroed), and the WAL is checkpointed and truncated
//!    again, which overwrites every old page of the main file and empties the
//!    WAL. Only a checkpoint that reports no blocking reader *and* leaves a
//!    zero-length WAL counts; anything else is an error, never a success.
//!    With no rows to remove the checkpoint still runs, which finishes a scrub
//!    an earlier call could not (blocked, or the process stopped after the
//!    delete).
//!
//! A row of one of [`RETIRED_SECRET_KEYS`] (keys the app no longer uses) is
//! removed in the same scrub, whatever its value and whatever the keyring
//! holds: it is never written to the keyring (that would create the entry the
//! app is retiring), and no keyring call is made for it, so it goes even when
//! the keyring is unavailable. It is reported in
//! [`MigrationReport::dropped_retired`]. The same holds in the backup and the
//! older copies below.
//!
//! Then the pre-migration backup (`library.v16.before-rust.db` next to
//! the live file) gets the same treatment: every migrated secret row in it is
//! removed if the keyring holds a value for that key (or it is blank), and
//! every retired one unconditionally. The scrub works on a
//! temporary copy that is integrity-checked and renamed over the backup, so a
//! crash leaves either the old backup or the scrubbed one, never a broken file;
//! a leftover temporary copy is deleted on the next run.
//!
//! Then every older copy of the database in the config directory gets the
//! same treatment: the snapshots under `backups/` (`library-*.db`, written by
//! this app or the Python app) and the files a restore moved aside
//! (`library.db.bak-<tag>`, with the `-wal` / `-shm` the Python app moved
//! beside them). Each is scrubbed in a temporary copy (any WAL folded in,
//! rows deleted with `secure_delete`, `VACUUM`), which is verified (integrity
//! check, same schema version, rows gone, and no byte sequence of a removed
//! value or of the keyring's value left anywhere in the file) before it
//! replaces the original, keeping its modification time so the snapshot list
//! keeps its order. A copy that cannot be opened, scrubbed or verified is
//! left unchanged and reported ([`MigrationReport::copies_left`]), never
//! counted as clean, and never stops the migration. Values shorter than
//! [`MIN_VALUE_BYTES`] are not searched for as raw bytes (too likely to occur
//! by chance); the row checks still apply to them.
//!
//! A migrated key the keyring cannot confirm keeps its row everywhere; the
//! next launch retries. With the keyring unavailable no migrated row is moved
//! or deleted; only retired rows are scrubbed (from the live file, the backup
//! and the copies), besides the content-preserving steps (the WAL
//! checkpoint, and folding a backup that is not a self-contained
//! rollback-journal file into one).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use musaic_db::{BACKUP_FILE_NAME, Db};
use rusqlite::{Connection, OpenFlags};

#[cfg(doc)]
use crate::RETIRED_SECRET_KEYS;
use crate::store::{SecretStore, StoreError};
use crate::{MIGRATED_SECRET_KEYS, SECRET_KEYS, is_retired_key};

/// Suffix of the temporary copy the backup is scrubbed in.
pub const SCRUB_TMP_SUFFIX: &str = ".scrub-tmp";

/// How long the scrub's WAL checkpoint waits for other connections' read
/// transactions before giving up. The connection's own busy timeout (30 s,
/// is restored afterwards.
pub const CHECKPOINT_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    /// Written to the empty keyring slot, confirmed, row deleted.
    pub moved: Vec<&'static str>,
    /// The keyring already held a value, which wins; row deleted.
    pub kept_keyring: Vec<&'static str>,
    /// Blank in the database; row deleted, keyring untouched.
    pub dropped_blank: Vec<&'static str>,
    /// A retired key ([`RETIRED_SECRET_KEYS`]); row deleted whatever its
    /// value, never written to the keyring, no keyring call made.
    pub dropped_retired: Vec<&'static str>,
    /// Could not be confirmed in the keyring; row left in place.
    pub failed: Vec<(&'static str, StoreError)>,
    /// Secret rows removed from the pre-migration backup.
    pub backup_scrubbed: Vec<&'static str>,
    /// The backup scrub failed (the backup is unchanged; retried next launch).
    pub backup_error: Option<String>,
    /// Older copies of the database (snapshots, restore asides) that held
    /// secrets and were scrubbed.
    pub copies_scrubbed: Vec<PathBuf>,
    /// Older copies that may still hold a secret, with the reason: they could
    /// not be opened, scrubbed or verified (the file is unchanged), or they
    /// hold a key the keyring could not confirm (that row is kept).
    pub copies_left: Vec<(PathBuf, String)>,
}

impl MigrationReport {
    pub fn is_noop(&self) -> bool {
        *self == Self::default()
    }

    /// Rows deleted from the live database.
    pub fn removed_rows(&self) -> Vec<&'static str> {
        let mut v: Vec<&'static str> = self
            .moved
            .iter()
            .chain(&self.kept_keyring)
            .chain(&self.dropped_blank)
            .chain(&self.dropped_retired)
            .copied()
            .collect();
        v.sort_unstable();
        v
    }
}

/// Write `value` under `key` and confirm it reads back unchanged. A value that
/// reads back different is removed again, so the slot is empty rather than
/// holding something that would later "win" over the real value in a row.
pub(crate) fn write_verified(
    store: &dyn SecretStore,
    key: &str,
    value: &str,
) -> Result<(), StoreError> {
    store.set(key, value)?;
    match store.get(key)? {
        Some(v) if v == value => Ok(()),
        _ => {
            let _ = store.delete(key);
            Err(StoreError::ReadBackMismatch(key.to_owned()))
        }
    }
}

/// Whether the keyring holds a non-empty value for `key`.
fn keyring_holds(store: &dyn SecretStore, key: &str) -> Result<bool, StoreError> {
    Ok(store.get(key)?.is_some_and(|v| !v.trim().is_empty()))
}

/// Whether a row of `key` in a backup or an older copy may be removed: a
/// retired key always (with no keyring call); a migrated one when it is blank
/// or the keyring demonstrably holds a value for it. A keyring error
/// (unavailable) keeps a migrated row, blank or not.
fn may_remove(store: &dyn SecretStore, key: &str, blank: bool) -> bool {
    is_retired_key(key) || matches!(keyring_holds(store, key), Ok(holds) if holds || blank)
}

/// The pre-migration backup that sits next to `db`: whichever
/// `library.v<N>.before-rust.db` exists (the lowest N), or the v16 name when
/// there is none yet.
pub fn pre_migration_backup_path(db: &Db) -> PathBuf {
    let dir = db.path().parent().unwrap_or_else(|| Path::new("."));
    musaic_db::existing_backup(dir).unwrap_or_else(|| dir.join(BACKUP_FILE_NAME))
}

/// Every pre-migration backup next to `db`, whatever schema version its name
/// carries. Normally one; all of them are scrubbed so that none keeps a key.
fn pre_migration_backups(db: &Db) -> Vec<PathBuf> {
    let dir = db.path().parent().unwrap_or_else(|| Path::new("."));
    let mut found: Vec<PathBuf> = (musaic_db::OLDEST_SCHEMA_VERSION..=16)
        .map(|v| dir.join(musaic_db::backup_file_name(v)))
        .filter(|p| p.is_file())
        .collect();
    if found.is_empty() {
        found.push(dir.join(BACKUP_FILE_NAME));
    }
    found
}

/// Move secret rows out of `db` (and its pre-migration backup) into `store`.
/// Safe to call on every open: with nothing to move it only reads, plus a WAL
/// checkpoint.
///
/// Returns an error when the live database could not be left without a copy
/// of a removed value: most often another connection holds a read transaction
/// that blocks the WAL checkpoint for longer than [`CHECKPOINT_BUSY_TIMEOUT`].
/// Then either nothing was deleted (the blocked checkpoint came first) or the
/// rows are gone but old pages remain until a checkpoint completes; the next
/// call finishes either case. The backup scrub still runs. The error cannot
/// tell a pending scrub from a checkpoint that merely had nothing to do while
/// a reader was active, so it is reported in both cases.
pub fn migrate_settings_secrets(
    db: &Db,
    store: &dyn SecretStore,
) -> musaic_db::Result<MigrationReport> {
    let conn = db.connection();
    let mut report = MigrationReport::default();
    for key in SECRET_KEYS {
        let Some(raw) = musaic_db::settings::get_raw(conn, key)? else {
            continue;
        };
        if is_retired_key(key) {
            report.dropped_retired.push(key);
            continue;
        }
        let value = musaic_core::pycompat::strip(&raw);
        // The keyring is consulted even for a blank row, so that with no
        // keyring nothing at all changes.
        match keyring_holds(store, key) {
            Ok(_) if value.is_empty() => report.dropped_blank.push(key),
            Ok(true) => report.kept_keyring.push(key),
            Ok(false) => match write_verified(store, key, value) {
                Ok(()) => report.moved.push(key),
                Err(e) => report.failed.push((key, e)),
            },
            Err(e) => report.failed.push((key, e)),
        }
    }
    let remove = report.removed_rows();
    let live = if remove.is_empty() {
        // Finish a scrub an earlier call could not complete.
        checkpoint_truncate(conn)
    } else {
        scrub_rows(conn, &remove)
    };

    for backup in pre_migration_backups(db) {
        match scrub_backup(&backup, store) {
            Ok(keys) => {
                for k in keys {
                    if !report.backup_scrubbed.contains(&k) {
                        report.backup_scrubbed.push(k);
                    }
                }
            }
            Err(e) => report.backup_error = Some(e.to_string()),
        }
    }
    scrub_copies(db, store, &mut report);
    live?;
    Ok(report)
}

/// In WAL mode, `wal_checkpoint(TRUNCATE)`, failing unless it completed: no
/// blocking reader and a zero-length (or absent) WAL afterwards. SQLite
/// reports a blocked checkpoint as a result row, not an error, so the row is
/// checked. A no-op for other journal modes.
fn checkpoint_truncate(conn: &Connection) -> musaic_db::Result<()> {
    let mode: String = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Ok(());
    }
    let previous: i64 = conn.query_row("PRAGMA busy_timeout", [], |r| r.get(0))?;
    conn.busy_timeout(CHECKPOINT_BUSY_TIMEOUT)?;
    let row = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
        ))
    });
    conn.busy_timeout(Duration::from_millis(
        u64::try_from(previous).unwrap_or_default(),
    ))?;
    let (busy, frames, copied) = row?;
    let wal_left = match conn.path().filter(|p| !p.is_empty()) {
        Some(p) => match std::fs::metadata(format!("{p}-wal")) {
            Ok(m) => m.len(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
            Err(e) => return Err(e.into()),
        },
        None => 0,
    };
    if busy != 0 || wal_left != 0 {
        return Err(std::io::Error::other(format!(
            "WAL checkpoint did not complete (blocked by another connection: \
             busy {busy}, {copied} of {frames} frames copied, {wal_left} bytes \
             left in the WAL); old copies of removed secrets may remain on disk \
             until a later call completes it"
        ))
        .into());
    }
    Ok(())
}

/// Delete the rows and leave no trace of their values in the file (or its WAL).
fn scrub_rows(conn: &Connection, keys: &[&str]) -> musaic_db::Result<()> {
    // Nothing is deleted unless the WAL can be emptied first: a reader that
    // blocks this would block the final checkpoint too, and failing here
    // leaves the rows (whose values the keyring now holds) for the next call.
    checkpoint_truncate(conn)?;
    let previous: i64 = conn.query_row("PRAGMA secure_delete", [], |r| r.get(0))?;
    conn.execute_batch("PRAGMA secure_delete = ON")?;
    let result = (|| -> musaic_db::Result<()> {
        // Earlier overwrites of the same rows by the Python app (which did not
        // use secure_delete) can leave old values in free space: rebuild
        // first, while the rows still exist, so that an interruption after
        // this leaves them for the next call to redo everything; the
        // secure_delete below then zeroes the rows' own cells.
        conn.execute_batch("VACUUM")?;
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let deleted = (|| -> rusqlite::Result<()> {
            let mut stmt = conn.prepare("DELETE FROM settings WHERE key = ?1")?;
            for k in keys {
                stmt.execute([k])?;
            }
            Ok(())
        })();
        match deleted {
            Ok(()) => conn.execute_batch("COMMIT")?,
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(e.into());
            }
        }
        // Fold the new pages into the main file (overwriting every old one)
        // and empty the WAL, which still holds the pre-delete pages.
        checkpoint_truncate(conn)
    })();
    conn.pragma_update(None, "secure_delete", previous)?;
    result
}

fn sidecar(p: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}{suffix}", p.display()))
}

fn remove_with_sidecars(p: &Path) -> std::io::Result<()> {
    remove_files(p, &["", "-wal", "-shm", "-journal"])
}

fn remove_files(p: &Path, suffixes: &[&str]) -> std::io::Result<()> {
    for s in suffixes {
        match std::fs::remove_file(sidecar(p, s)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Whether the file's header marks it as a WAL database (bytes 18-19 = 2).
fn header_is_wal(p: &Path) -> std::io::Result<bool> {
    let mut h = [0u8; 20];
    let mut f = std::fs::File::open(p)?;
    Ok(f.read_exact(&mut h).is_ok() && h[18] == 2 && h[19] == 2)
}

/// Make the backup a self-contained rollback-journal file with nothing beside
/// it, before it is read: fold a `-wal` beside it into it, let SQLite roll back
/// a hot `-journal`, then delete what is left beside it (after a read-write
/// open has dealt with them, a remaining `-journal` is not hot and a `-shm`
/// holds no rows; either may hold old bytes). musaic-db writes the backup in
/// rollback-journal mode with nothing beside it, so this only acts on a backup
/// another build or tool left otherwise. Content-preserving.
fn settle_backup(backup: &Path) -> musaic_db::Result<()> {
    let has_sidecar = ["-wal", "-journal", "-shm"]
        .iter()
        .any(|s| sidecar(backup, s).exists());
    if !has_sidecar && !header_is_wal(backup)? {
        return Ok(());
    }
    let conn = Connection::open(backup)?;
    let mode: String = conn.query_row("PRAGMA journal_mode = DELETE", [], |r| r.get(0))?;
    // A read transaction, so a hot journal is rolled back now.
    let _: i64 = conn.query_row("SELECT COUNT(*) FROM sqlite_master", [], |r| r.get(0))?;
    conn.close().map_err(|(_, e)| e)?;
    if !mode.eq_ignore_ascii_case("delete") {
        return Err(std::io::Error::other(format!(
            "could not switch the backup to rollback-journal mode (still {mode})"
        ))
        .into());
    }
    remove_files(backup, &["-wal", "-journal", "-shm"])?;
    Ok(())
}

/// Remove, from the backup at `backup`, every retired secret row and every
/// migrated one whose key the keyring demonstrably holds (blank rows too).
/// Returns the keys removed.
fn scrub_backup(backup: &Path, store: &dyn SecretStore) -> musaic_db::Result<Vec<&'static str>> {
    let tmp = sidecar(backup, SCRUB_TMP_SUFFIX);
    // A crash during an earlier scrub may have left a copy (possibly still
    // holding secrets): drop it; the backup itself was never touched.
    remove_with_sidecars(&tmp)?;
    if !backup.is_file() {
        return Ok(Vec::new());
    }
    settle_backup(backup)?;

    // Decide from a read-only look which rows may go.
    let rows: Vec<(&'static str, String)> = {
        let ro = Connection::open_with_flags(
            backup,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let mut v = Vec::new();
        for key in SECRET_KEYS {
            if let Some(val) = musaic_db::settings::get_raw(&ro, key)? {
                v.push((key, val));
            }
        }
        v
    };
    let mut remove = Vec::new();
    let mut rows_removed = Vec::new();
    for (key, val) in rows {
        let blank = musaic_core::pycompat::strip(&val).is_empty();
        if may_remove(store, key, blank) {
            remove.push(key);
            rows_removed.push((key, val));
        }
    }
    if remove.is_empty() {
        return Ok(remove);
    }

    let result = (|| -> musaic_db::Result<()> {
        std::fs::copy(backup, &tmp)?;
        let version_before;
        {
            let conn = Connection::open(&tmp)?;
            version_before = schema_version(&conn)?;
            scrub_rows(&conn, &remove)?;
            conn.close().map_err(|(_, e)| e)?;
        }
        // Verify before replacing: intact, same version, rows gone, and no
        // copy of a removed value (or the keyring's) left in the file.
        let needles = removed_value_needles(&rows_removed, store);
        if let Some(k) = first_needle_in(&tmp, &needles)? {
            return Err(std::io::Error::other(format!(
                "scrubbed backup still contains the bytes of {k}"
            ))
            .into());
        }
        {
            let conn = Connection::open_with_flags(
                &tmp,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            let check: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
            let mut left = 0;
            for k in &remove {
                left += usize::from(musaic_db::settings::get_raw(&conn, k)?.is_some());
            }
            let version = schema_version(&conn)?;
            if check != "ok" || version != version_before || left != 0 {
                return Err(std::io::Error::other(format!(
                    "scrubbed backup failed verification (integrity: {check}, version {version})"
                ))
                .into());
            }
            conn.close().map_err(|(_, e)| e)?;
        }
        // Opened for writing: on Windows, flushing a handle without write
        // access fails ("Access is denied"), which left every backup
        // unscrubbed there.
        std::fs::OpenOptions::new()
            .write(true)
            .open(&tmp)?
            .sync_all()?;
        // Anything that appeared beside the backup since settle_backup
        // belongs to the file about to be replaced: removed before the
        // rename, so a stale journal is never applied to the scrubbed file.
        remove_files(backup, &["-wal", "-journal", "-shm"])?;
        std::fs::rename(&tmp, backup)?;
        remove_with_sidecars(&tmp)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = remove_with_sidecars(&tmp);
    }
    result.map(|()| remove)
}

fn schema_version(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("SELECT version FROM schema_version LIMIT 1", [], |r| {
        r.get(0)
    })
}

/// Values shorter than this are not searched for as raw bytes in a scrubbed
/// file: a short value could occur in unrelated data by chance and would make
/// the file look unscrubbable forever. Real API keys are far longer.
pub const MIN_VALUE_BYTES: usize = 8;

/// The byte sequences that must not survive a scrub of `rows`: each removed
/// row's value (as stored and stripped) and, for a migrated key, the
/// keyring's value for it (a retired key is never looked up).
fn removed_value_needles(
    rows: &[(&'static str, String)],
    store: &dyn SecretStore,
) -> Vec<(&'static str, Vec<u8>)> {
    let mut v: Vec<(&'static str, Vec<u8>)> = Vec::new();
    let mut add = |k: &'static str, s: &str| {
        if s.len() >= MIN_VALUE_BYTES && !v.iter().any(|(_, n)| n == s.as_bytes()) {
            v.push((k, s.as_bytes().to_vec()));
        }
    };
    for (k, raw) in rows {
        add(k, raw);
        add(k, musaic_core::pycompat::strip(raw));
        if is_retired_key(k) {
            continue;
        }
        if let Ok(Some(kv)) = store.get(k) {
            add(k, &kv);
            add(k, musaic_core::pycompat::strip(&kv));
        }
    }
    v
}

/// The keyring's values (for every migrated key it holds), as needles.
fn keyring_needles(store: &dyn SecretStore) -> Vec<(&'static str, Vec<u8>)> {
    let mut v: Vec<(&'static str, Vec<u8>)> = Vec::new();
    for k in MIGRATED_SECRET_KEYS {
        if let Ok(Some(kv)) = store.get(k) {
            let kv = musaic_core::pycompat::strip(&kv);
            if kv.len() >= MIN_VALUE_BYTES {
                v.push((k, kv.as_bytes().to_vec()));
            }
        }
    }
    v
}

/// The key of the first needle whose bytes occur in the file at `p` (a
/// missing file holds none).
fn first_needle_in(
    p: &Path,
    needles: &[(&'static str, Vec<u8>)],
) -> std::io::Result<Option<&'static str>> {
    if needles.is_empty() {
        return Ok(None);
    }
    let bytes = match std::fs::read(p) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    Ok(needles
        .iter()
        .find(|(_, n)| !n.is_empty() && bytes.windows(n.len()).any(|w| w == n.as_slice()))
        .map(|(k, _)| *k))
}

/// An older copy of the database in the config directory.
struct DetachedCopy {
    /// The database file.
    main: PathBuf,
    /// A WAL that belongs to it: `<main>-wal`, or for a file a restore moved
    /// aside, the WAL moved aside with it (`library.db-wal.bak-<tag>`).
    wal: Option<PathBuf>,
    /// Further files that belong to it and go once it is scrubbed
    /// (`-shm` files; they hold no row data).
    extra: Vec<PathBuf>,
}

/// Every older copy of the live database in its directory: snapshots in
/// `backups/` and restore asides next to the live file. Leftover temporary
/// copies of an interrupted scrub are deleted on the way.
fn detached_copies(db: &Db, report: &mut MigrationReport) -> Vec<DetachedCopy> {
    let live = db.path();
    let dir = live.parent().unwrap_or_else(|| Path::new("."));
    let live_name = live
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut found = Vec::new();

    let snap_dir = dir.join(musaic_core::config::SNAPSHOT_DIR_NAME);
    for name in dir_names(&snap_dir, report) {
        let p = snap_dir.join(&name);
        if name.contains(SCRUB_TMP_SUFFIX) {
            let _ = std::fs::remove_file(&p);
            continue;
        }
        let is_snapshot = name.len()
            >= musaic_db::backup::SNAPSHOT_PREFIX.len() + musaic_db::backup::SNAPSHOT_SUFFIX.len()
            && name.starts_with(musaic_db::backup::SNAPSHOT_PREFIX)
            && name.ends_with(musaic_db::backup::SNAPSHOT_SUFFIX);
        if !is_snapshot {
            continue;
        }
        if let Some(c) = regular_copy(&p, None, Vec::new(), report) {
            found.push(c);
        }
    }

    if !live_name.is_empty() {
        let aside_prefix = format!("{live_name}.bak-");
        for name in dir_names(dir, report) {
            let p = dir.join(&name);
            if name.contains(SCRUB_TMP_SUFFIX) {
                if name.starts_with(&aside_prefix) {
                    let _ = std::fs::remove_file(&p);
                }
                continue;
            }
            let Some(tag) = name.strip_prefix(&aside_prefix) else {
                continue;
            };
            if ["-wal", "-shm", "-journal"]
                .iter()
                .any(|s| tag.ends_with(s))
            {
                // A sidecar of an aside (handled with it).
                continue;
            }
            let wal_aside = dir.join(format!("{live_name}-wal.bak-{tag}"));
            let shm_aside = dir.join(format!("{live_name}-shm.bak-{tag}"));
            let wal = wal_aside.is_file().then_some(wal_aside);
            let extra = if shm_aside.is_file() {
                vec![shm_aside]
            } else {
                Vec::new()
            };
            if let Some(c) = regular_copy(&p, wal, extra, report) {
                found.push(c);
            }
        }
    }
    found
}

/// The names in `dir` (none when it does not exist). A directory that exists
/// but cannot be listed is reported.
fn dir_names(dir: &Path, report: &mut MigrationReport) -> Vec<String> {
    match std::fs::read_dir(dir) {
        Ok(rd) => {
            let mut v: Vec<String> = rd
                .flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .collect();
            v.sort();
            v
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            report
                .copies_left
                .push((dir.to_path_buf(), format!("cannot list the folder: {e}")));
            Vec::new()
        }
    }
}

/// `p` as a copy to scrub when it is a regular file. A link is never written
/// through (it may point anywhere): it is reported only when its target
/// holds a secret.
fn regular_copy(
    p: &Path,
    wal: Option<PathBuf>,
    extra: Vec<PathBuf>,
    report: &mut MigrationReport,
) -> Option<DetachedCopy> {
    let meta = std::fs::symlink_metadata(p).ok()?;
    if meta.file_type().is_symlink() {
        if quick_rows(p).is_some_and(|rows| !rows.is_empty()) {
            report.copies_left.push((
                p.to_path_buf(),
                "is a link to another file, which holds API keys; not changed".into(),
            ));
        }
        return None;
    }
    if !meta.is_file() {
        return None;
    }
    let wal = wal.or_else(|| {
        let w = sidecar(p, "-wal");
        w.is_file().then_some(w)
    });
    Some(DetachedCopy {
        main: p.to_path_buf(),
        wal,
        extra,
    })
}

/// A read-only look at the secret rows of a self-contained database file;
/// `None` when it cannot be read that way.
fn quick_rows(p: &Path) -> Option<Vec<&'static str>> {
    let ro = Connection::open_with_flags(
        p,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    let mut v = Vec::new();
    for key in SECRET_KEYS {
        if musaic_db::settings::get_raw(&ro, key).ok()?.is_some() {
            v.push(key);
        }
    }
    Some(v)
}

/// Scrub every older copy (see the module docs) and record the outcome.
fn scrub_copies(db: &Db, store: &dyn SecretStore, report: &mut MigrationReport) {
    let keyring = keyring_needles(store);
    for copy in detached_copies(db, report) {
        match scrub_copy(&copy, store, &keyring) {
            Ok(done) => {
                if done.scrubbed {
                    report.copies_scrubbed.push(copy.main.clone());
                }
                if !done.kept.is_empty() {
                    report.copies_left.push((
                        copy.main.clone(),
                        format!(
                            "keeps {} because the keyring could not confirm a value for it",
                            done.kept.join(", ")
                        ),
                    ));
                }
            }
            Err(e) => report.copies_left.push((
                copy.main.clone(),
                format!("could not be scrubbed, left unchanged: {e}"),
            )),
        }
    }
}

#[derive(Default)]
struct CopyScrub {
    scrubbed: bool,
    /// Keys whose rows stay (the keyring could not confirm them).
    kept: Vec<&'static str>,
}

fn scrub_copy(
    copy: &DetachedCopy,
    store: &dyn SecretStore,
    keyring: &[(&'static str, Vec<u8>)],
) -> musaic_db::Result<CopyScrub> {
    // Cheap look first: nothing to do for a self-contained file with no
    // secret row and none of the keyring's values in its bytes (every
    // snapshot this build takes, and every copy already scrubbed).
    let in_files = |needles: &[(&'static str, Vec<u8>)]| -> std::io::Result<bool> {
        Ok(first_needle_in(&copy.main, needles)?.is_some()
            || match &copy.wal {
                Some(w) => first_needle_in(w, needles)?.is_some(),
                None => false,
            })
    };
    let names: Vec<(&'static str, Vec<u8>)> = SECRET_KEYS
        .iter()
        .map(|k| (*k, k.as_bytes().to_vec()))
        .collect();
    let values_present = in_files(keyring)?;
    // A settings row stores its key's name as text: with neither a key name
    // nor a keyring value anywhere in the bytes there is nothing to remove
    // (this also covers a file that is not a database at all).
    if !values_present && !in_files(&names)? {
        return Ok(CopyScrub::default());
    }
    if copy.wal.is_none() && !values_present && quick_rows(&copy.main).is_some_and(|r| r.is_empty())
    {
        return Ok(CopyScrub::default());
    }

    let tmp = sidecar(&copy.main, SCRUB_TMP_SUFFIX);
    remove_with_sidecars(&tmp)?;
    let result = (|| -> musaic_db::Result<CopyScrub> {
        std::fs::copy(&copy.main, &tmp)?;
        if let Some(w) = &copy.wal {
            std::fs::copy(w, sidecar(&tmp, "-wal"))?;
        }
        let mut out = CopyScrub::default();
        let rows_removed;
        let version_before;
        {
            let conn = Connection::open(&tmp)?;
            // Fold a WAL in and leave a self-contained rollback-journal file.
            let mode: String = conn.query_row("PRAGMA journal_mode = DELETE", [], |r| r.get(0))?;
            if !mode.eq_ignore_ascii_case("delete") {
                return Err(std::io::Error::other(format!(
                    "could not switch the copy to rollback-journal mode (still {mode})"
                ))
                .into());
            }
            version_before = schema_version(&conn)?;
            let mut rows = Vec::new();
            for key in SECRET_KEYS {
                if let Some(val) = musaic_db::settings::get_raw(&conn, key)? {
                    rows.push((key, val));
                }
            }
            let mut removed = Vec::new();
            for (key, val) in rows {
                let blank = musaic_core::pycompat::strip(&val).is_empty();
                if may_remove(store, key, blank) {
                    removed.push((key, val));
                } else {
                    out.kept.push(key);
                }
            }
            if removed.is_empty() && !values_present && copy.wal.is_none() {
                // Only rows the keyring cannot confirm: nothing to change.
                conn.close().map_err(|(_, e)| e)?;
                remove_with_sidecars(&tmp)?;
                return Ok(out);
            }
            let keys: Vec<&str> = removed.iter().map(|(k, _)| *k).collect();
            scrub_rows(&conn, &keys)?;
            conn.close().map_err(|(_, e)| e)?;
            rows_removed = removed;
        }
        // Verify before replacing.
        {
            let conn = Connection::open_with_flags(
                &tmp,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            let check: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
            let mut left = 0;
            for (k, _) in &rows_removed {
                left += usize::from(musaic_db::settings::get_raw(&conn, k)?.is_some());
            }
            let version = schema_version(&conn)?;
            conn.close().map_err(|(_, e)| e)?;
            if check != "ok" || version != version_before || left != 0 {
                return Err(std::io::Error::other(format!(
                    "the scrubbed copy failed verification (integrity: {check}, version {version}, \
                     {left} row(s) left)"
                ))
                .into());
            }
        }
        for s in ["-wal", "-journal"] {
            if sidecar(&tmp, s).exists() {
                return Err(std::io::Error::other(format!(
                    "the scrubbed copy still has a {s} file"
                ))
                .into());
            }
        }
        let mut needles = removed_value_needles(&rows_removed, store);
        for n in keyring {
            if !out.kept.contains(&n.0) && !needles.iter().any(|m| m.1 == n.1) {
                needles.push(n.clone());
            }
        }
        if let Some(k) = first_needle_in(&tmp, &needles)? {
            return Err(std::io::Error::other(format!(
                "the bytes of {k} are still in the file after scrubbing (stored outside the settings table?)"
            ))
            .into());
        }
        // Keep the original's modification time: the snapshot list is
        // ordered, and retention and the startup snapshot are decided, by it.
        let mtime = std::fs::metadata(&copy.main)?.modified()?;
        let f = std::fs::OpenOptions::new().write(true).open(&tmp)?;
        f.set_modified(mtime)?;
        f.sync_all()?;
        drop(f);
        remove_files(&copy.main, &["-wal", "-journal", "-shm"])?;
        std::fs::rename(&tmp, &copy.main)?;
        remove_with_sidecars(&tmp)?;
        if let Some(w) = &copy.wal {
            match std::fs::remove_file(w) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        for x in &copy.extra {
            let _ = std::fs::remove_file(x);
        }
        out.scrubbed = true;
        Ok(out)
    })();
    if result.is_err() {
        let _ = remove_with_sidecars(&tmp);
    }
    result
}
