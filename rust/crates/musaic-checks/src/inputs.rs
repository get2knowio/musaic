//! What a check last ran against (`check_inputs`, schema v19), and the
//! staleness rule the background check sweep plans from.
//!
//! **The digest.** Every Present album gets a *base digest* of everything
//! about it a check can read from the database: its whole `albums` row
//! (every column, so a column added later is covered without a change
//! here; the v20 disc columns only once they hold a value,
//! `SKIPPED_WHEN_NULL`), its fingerprint rows (implementation, time computed, AcoustID
//! recording and score; read from `idx_track_fingerprints_inputs`, not the
//! fingerprint text), its blocked MusicBrainz release groups and its AI jobs
//! (type, status and times: no-cover-art reads recent cover jobs). Files are
//! represented by what the scan stores about them (the row's track lists,
//! cover columns and directory mtime), so a change on disk counts once a
//! scan has seen it: scans observe. A check's digest for an album is the
//! base digest with the check's id, [`INPUTS_VERSION`] and the check's
//! [`Check::input_extras`] (the filename templates) folded in; a [`Granularity::Library`] check has one digest over every
//! Present album's base digest.
//!
//! The digest is computed *before* a run and recorded only when the run
//! completes ([`crate::runner::run_check`]), so a change made while the run
//! was reading leaves the album stale, and a run that changes its own inputs
//! (a resolver storing an anchor) is followed by one more run, after which
//! the inputs are stable.
//!
//! **Stale** means: no row, a row with another digest, or a row older than
//! the check's [`Check::refresh_after`].

use std::collections::{BTreeMap, HashMap};
use std::time::SystemTime;

use musaic_db::check_inputs::{self, LIBRARY_ROW};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use crate::base::{Check, CheckCtx, CheckEnv, CheckError, CheckScope, Granularity, ScopeKind};

/// Folded into every digest: bump it when what a digest covers changes, so
/// every check is rerun once.
pub const INPUTS_VERSION: &str = "check-inputs/2";

/// A SHA-256 over typed, length-prefixed fields (two different field
/// sequences never feed the same bytes).
struct Hasher(Sha256);

impl Hasher {
    fn new() -> Self {
        Hasher(Sha256::new())
    }

    fn bytes(&mut self, tag: u8, b: &[u8]) {
        self.0.update([tag]);
        self.0.update((b.len() as u64).to_le_bytes());
        self.0.update(b);
    }

    fn str(&mut self, s: &str) {
        self.bytes(b's', s.as_bytes());
    }

    fn value(&mut self, v: ValueRef<'_>) {
        match v {
            ValueRef::Null => self.0.update(b"n"),
            ValueRef::Integer(i) => self.bytes(b'i', &i.to_le_bytes()),
            ValueRef::Real(f) => self.bytes(b'r', &f.to_bits().to_le_bytes()),
            ValueRef::Text(t) => self.bytes(b't', t),
            ValueRef::Blob(b) => self.bytes(b'b', b),
        }
    }

    fn hex(self) -> String {
        self.0
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}

/// Columns added after rows already had recorded inputs, hashed only once
/// they hold a value: a NULL there digests as if the column did not exist,
/// so the upgrade that adds them leaves every album's digest as it was
/// (otherwise every check, the network ones included, would rerun over the
/// whole library once). Filling one in changes that album's digest.
const SKIPPED_WHEN_NULL: [&str; 2] = ["track_discs_json", "tag_disc_total"];

/// Fold every column of `row` (from `stmt`'s column `start` on) into `h`.
fn fold_row(h: &mut Hasher, names: &[String], row: &rusqlite::Row<'_>, start: usize) {
    for (i, name) in names.iter().enumerate().skip(start) {
        let value = row.get_ref(i).unwrap_or(ValueRef::Null);
        if matches!(value, ValueRef::Null) && SKIPPED_WHEN_NULL.contains(&name.as_str()) {
            continue;
        }
        h.str(name);
        h.value(value);
    }
}

/// Fold the rows of `sql` (first column the album id, the rest its data)
/// into the hashers of the albums they belong to, each row under `section`.
fn fold_child_rows(
    conn: &Connection,
    sql: &str,
    section: &str,
    hashers: &mut HashMap<String, Hasher>,
) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(sql)?;
    let names: Vec<String> = stmt.column_names().map(|s| s.to_string()).collect();
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        if let Some(h) = hashers.get_mut(&id) {
            h.str(section);
            fold_row(h, &names, row, 1);
        }
    }
    Ok(())
}

/// The base digest of every Present album of `library_path` (NULL or ""
/// `missing_since`, as the runner's scope), by album id; `only` limits it
/// to those ids.
pub fn album_digests(
    conn: &Connection,
    library_path: &str,
    only: Option<&[String]>,
) -> rusqlite::Result<BTreeMap<String, String>> {
    let only_json = only.map(|ids| serde_json::Value::from(ids.to_vec()).to_string());
    let mut hashers: HashMap<String, Hasher> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT * FROM albums WHERE library_path = ?1 \
               AND (missing_since IS NULL OR missing_since = '') \
               AND (?2 IS NULL OR id IN (SELECT value FROM json_each(?2)))",
        )?;
        let names: Vec<String> = stmt.column_names().map(|s| s.to_string()).collect();
        let id_col = names.iter().position(|n| n == "id").unwrap_or(0);
        let mut rows = stmt.query(rusqlite::params![library_path, only_json])?;
        while let Some(row) = rows.next()? {
            let id: String = row.get(id_col)?;
            let mut h = Hasher::new();
            h.str("album");
            fold_row(&mut h, &names, row, 0);
            hashers.insert(id, h);
        }
    }
    if hashers.is_empty() {
        return Ok(BTreeMap::new());
    }
    // Children in a fixed order, each table read once (its rows for albums
    // outside the map are skipped).
    fold_child_rows(
        conn,
        "SELECT album_id, filename, fingerprint_impl, last_computed_at, \
                acoustid_recording_mbid, acoustid_score \
         FROM track_fingerprints ORDER BY album_id, filename",
        "fingerprint",
        &mut hashers,
    )?;
    fold_child_rows(
        conn,
        "SELECT album_id, release_group_mbid FROM album_blocked_mb_release_groups \
         ORDER BY album_id, release_group_mbid",
        "blocked",
        &mut hashers,
    )?;
    fold_child_rows(
        conn,
        "SELECT album_id, id, job_type, status, created_at, completed_at, acted_on_at, \
                acted_on_action, length(result_json) \
         FROM ai_jobs ORDER BY album_id, id",
        "job",
        &mut hashers,
    )?;
    Ok(hashers.into_iter().map(|(id, h)| (id, h.hex())).collect())
}

/// A [`Granularity::Album`] check's digest for one album.
pub fn album_check_digest(check_id: &str, extras: &str, base: &str) -> String {
    let mut h = Hasher::new();
    h.str(INPUTS_VERSION);
    h.str(check_id);
    h.str(extras);
    h.str(base);
    h.hex()
}

/// A [`Granularity::Library`] check's digest over every Present album.
pub fn library_check_digest(
    check_id: &str,
    extras: &str,
    bases: &BTreeMap<String, String>,
) -> String {
    let mut h = Hasher::new();
    h.str(INPUTS_VERSION);
    h.str(check_id);
    h.str(extras);
    h.str("library");
    for (id, base) in bases {
        h.str(id);
        h.str(base);
    }
    h.hex()
}

/// The `(album_id, digest)` rows a completed run of `check` over `scope`
/// records, computed before the run: an album-scoped or library-wide run of
/// a [`Granularity::Album`] check records each album in scope; a
/// [`Granularity::Library`] check records its one library row when the run
/// covered the whole library (scope `all`, or a check that always sees the
/// whole library). `None`: the run records nothing (an incremental run, or
/// an album-scoped run of a library check).
pub fn run_digests(
    ctx: &CheckCtx<'_>,
    check: &dyn Check,
    scope: &CheckScope,
) -> Result<Option<Vec<(String, String)>>, CheckError> {
    let id = check.meta().id;
    match check.granularity() {
        Granularity::Album => {
            if !scope.can_resolve() {
                return Ok(None);
            }
            if scope.albums.is_empty() {
                return Ok(Some(Vec::new()));
            }
            let ids: Vec<String> = scope.albums.iter().map(|a| a.id.clone()).collect();
            let bases = if scope.kind == ScopeKind::All {
                album_digests(ctx.conn, &scope.library_path, None)?
            } else {
                album_digests(ctx.conn, &scope.library_path, Some(&ids))?
            };
            let extras = check.input_extras(ctx)?;
            Ok(Some(
                ids.into_iter()
                    .filter_map(|a| {
                        let d = album_check_digest(id, &extras, bases.get(&a)?);
                        Some((a, d))
                    })
                    .collect(),
            ))
        }
        Granularity::Library => {
            if scope.kind != ScopeKind::All && !check.sees_whole_library() {
                return Ok(None);
            }
            let bases = album_digests(ctx.conn, &scope.library_path, None)?;
            let extras = check.input_extras(ctx)?;
            Ok(Some(vec![(
                LIBRARY_ROW.to_string(),
                library_check_digest(id, &extras, &bases),
            )]))
        }
    }
}

/// One check's stale work in a library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleWork {
    pub check_id: &'static str,
    pub granularity: Granularity,
    pub uses_network: bool,
    /// The albums to rerun ([`Granularity::Album`]); empty for a library
    /// check (which reruns library-wide).
    pub albums: Vec<String>,
}

impl StaleWork {
    /// Units of work, for progress: one per album, one for a library run.
    pub fn units(&self) -> usize {
        match self.granularity {
            Granularity::Album => self.albums.len(),
            Granularity::Library => 1,
        }
    }
}

/// Every registered check's stale work in `library_path` at `now`, in
/// registry order (checks with nothing stale left out). Empty when the
/// library has never been scanned. Missing albums are never stale: they
/// are not checked (their findings and rows stay as they are).
pub fn plan_stale(
    conn: &Connection,
    env: &CheckEnv,
    library_path: &str,
    now: SystemTime,
) -> Result<Vec<StaleWork>, CheckError> {
    let known: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM libraries WHERE path = ?1",
            [library_path],
            |r| r.get(0),
        )
        .optional()?;
    if known.is_none() {
        return Ok(Vec::new());
    }
    let bases = album_digests(conn, library_path, None)?;
    let mut recorded: HashMap<(String, String), (String, String)> = HashMap::new();
    for r in check_inputs::rows_for_library(conn, library_path)? {
        recorded.insert((r.check_id, r.album_id), (r.digest, r.checked_at));
    }
    let ctx = CheckCtx::new(conn, env);
    let mut out = Vec::new();
    for check in env.registry.iter() {
        let id = check.meta().id;
        let extras = check.input_extras(&ctx)?;
        let cutoff = check
            .refresh_after()
            .and_then(|d| now.checked_sub(d))
            .map(musaic_db::backup::iso_seconds);
        let fresh = |album: &str, want: &str| {
            recorded
                .get(&(id.to_string(), album.to_string()))
                .is_some_and(|(digest, at)| {
                    digest == want && cutoff.as_deref().is_none_or(|c| at.as_str() >= c)
                })
        };
        let granularity = check.granularity();
        let albums: Vec<String> = match granularity {
            Granularity::Album => bases
                .iter()
                .filter(|(a, base)| !fresh(a, &album_check_digest(id, &extras, base)))
                .map(|(a, _)| a.clone())
                .collect(),
            Granularity::Library => {
                if fresh(LIBRARY_ROW, &library_check_digest(id, &extras, &bases)) {
                    continue;
                }
                Vec::new()
            }
        };
        if granularity == Granularity::Album && albums.is_empty() {
            continue;
        }
        out.push(StaleWork {
            check_id: id,
            granularity,
            uses_network: check.uses_network(),
            albums,
        });
    }
    Ok(out)
}

/// Whether `check_id`'s recorded row for `album_id` ([`LIBRARY_ROW`] for
/// the library row) is still fresh for `want`.
fn row_fresh(
    conn: &Connection,
    check: &dyn Check,
    library_path: &str,
    album_id: &str,
    want: &str,
    now: SystemTime,
) -> Result<bool, CheckError> {
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT digest, checked_at FROM check_inputs \
             WHERE check_id = ?1 AND library_path = ?2 AND album_id = ?3",
            rusqlite::params![check.meta().id, library_path, album_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let cutoff = check
        .refresh_after()
        .and_then(|d| now.checked_sub(d))
        .map(musaic_db::backup::iso_seconds);
    Ok(row.is_some_and(|(digest, at)| {
        digest == want && cutoff.as_deref().is_none_or(|c| at.as_str() >= c)
    }))
}

/// Right before a planned batch runs: which of `albums` (a
/// [`Granularity::Album`] check's batch) are still stale, or (`albums`
/// `None`, a library check) whether the library row is: `Some` with the
/// work left, `None` when nothing is. Another run (a post-scan rerun) may
/// have refreshed them while the sweep waited; an album no longer Present
/// is dropped.
pub fn still_stale(
    conn: &Connection,
    env: &CheckEnv,
    library_path: &str,
    check_id: &str,
    albums: Option<&[String]>,
    now: SystemTime,
) -> Result<Option<Vec<String>>, CheckError> {
    let Some(check) = env.registry.get(check_id) else {
        return Ok(None);
    };
    let ctx = CheckCtx::new(conn, env);
    let extras = check.input_extras(&ctx)?;
    match albums {
        Some(ids) => {
            let bases = album_digests(conn, library_path, Some(ids))?;
            let mut left = Vec::new();
            for id in ids {
                let Some(base) = bases.get(id) else { continue };
                let want = album_check_digest(check_id, &extras, base);
                if !row_fresh(conn, check.as_ref(), library_path, id, &want, now)? {
                    left.push(id.clone());
                }
            }
            Ok((!left.is_empty()).then_some(left))
        }
        None => {
            let bases = album_digests(conn, library_path, None)?;
            let want = library_check_digest(check_id, &extras, &bases);
            let fresh = row_fresh(conn, check.as_ref(), library_path, LIBRARY_ROW, &want, now)?;
            Ok((!fresh).then(Vec::new))
        }
    }
}
