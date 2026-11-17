//! `musaic/fingerprint.py::fingerprint_album`: Chromaprint every FLAC of an
//! album, look each fingerprint up on AcoustID, store the best recording in
//! `track_fingerprints`, publish `TrackFingerprinted`.
//!
//! Reference: `testdata/golden/fingerprint/album.json` and
//! `best_recording.json` (recorded from the Python app's own function
//! against recorded AcoustID answers). Kept from the Python app:
//!
//! * the cache rule: unless `force`, the stored rows are returned when their
//!   filename set equals the FLACs on disk (as amended for rejected files
//!   below);
//! * fingerprints are all computed first, then looked up and stored one file
//!   at a time; a file the fingerprinter rejects is skipped; none at all is
//!   `no-fingerprints`; a directory without FLACs is an empty result;
//! * a completed lookup with no result at or above the floor stores the
//!   fingerprint with no recording and no score;
//! * [`best_recording`]: the first recording with an id of the
//!   highest-scoring result at or above 0.85 (`_best_recording`);
//! * `TrackFingerprinted(album_id, filenames)` once, after the rows, when at
//!   least one file was stored.
//!
//! Decided changes:
//!
//! * No AcoustID key is not an error. Fingerprints are computed and stored
//!   with no recording; only the lookup is skipped. The fingerprinter is
//!   in-process, so there is no "fpcalc missing".
//! * Every row records the implementation that computed
//!   it; rows from another implementation (the Python app's fpcalc) are not
//!   a cache hit, so the album is recomputed. A recording already stored for
//!   a file is kept when its new lookup does not complete (failed, or no
//!   key); a completed lookup replaces it, even with "no match".
//! * Every row records its lookup outcome
//!   (`acoustid_lookup_state`): NULL = no outcome yet (computed without a
//!   key, a fingerprint that changed since its last lookup, or a Python-app
//!   row with no recording), `done` = completed (match or no match: never
//!   asked again unless forced or the fingerprint changes), `failed` = the
//!   lookup did not complete. Every failure counts as `failed`: a network
//!   error or timeout, an HTTP error status (5xx, 429, and also 4xx such as
//!   AcoustID's "invalid API key", which a corrected key fixes), an
//!   unreadable answer. When the rows are otherwise a cache hit and a key is
//!   set, the rows whose lookup is due ([`FingerprintRecord::lookup_due`])
//!   are looked up from the stored fingerprint, without fingerprinting
//!   anything again, and announced: this is how a key added later reaches
//!   fingerprints stored without one.
//!   Retry rule ([`lookup_retry_delay_s`]): after the first failure of a row
//!   it is due again at once (the next call or pool pass, never twice in one
//!   call: each file is asked at most once per call, and a pool pass visits
//!   an album at most once); after the n-th consecutive failure (n >= 2) it
//!   waits 1 h * 4^(n-2), capped at 7 days (1 h, 4 h, 16 h, 64 h, 7 d, 7 d,
//!   ...). It is never given up for good; a completed lookup resets the
//!   count; a forced call asks regardless.
//! * On recompute, rows (and remembered rejects) of files that no
//!   longer exist are deleted (only when the directory could be listed, so
//!   an unreadable directory deletes nothing). A file the fingerprinter
//!   rejects is remembered in `fingerprint_rejects` with its signature
//!   (size, modification and change times, [`file_signature`]) and the
//!   implementation; the cache comparison leaves out a remembered file
//!   whose signature and implementation are unchanged, and a non-forced
//!   recompute does not try it again. A file that changes (rewritten,
//!   replaced, its permissions changed) or a new fingerprinter is tried
//!   again, and a file that fingerprints loses its reject row. A file that
//!   cannot even be opened is not remembered (tried again next time), nor
//!   is a refusal by a fingerprinter whose errors are not about the file
//!   ([`Fingerprinter::rejection_is_about_the_file`]). A
//!   file that fingerprinted before and is rejected now loses its old row
//!   (the rows are the files that fingerprint). A forced call tries every
//!   file.
//! * The album's FLACs come from the shared listing
//!   (`musaic_scan::list_flacs`: `._` skipped, sorted, same-inode names
//!   collapsed); the Python app's own listing did not collapse inodes.
//! * The lookup is `musaic_net::AcoustIdClient`'s (HTTPS, release-group
//!   metadata, 15 s timeout, shared pacing, our User-Agent). Over all 93
//!   recorded fingerprints the Python app's own `_best_recording` picks the
//!   same recording from that answer as from its `meta=recordings` one.
//! * Cancellation (the Python app's pool abandoned a thread that kept
//!   going): a [`CancelToken`] is checked before each fingerprint and each
//!   lookup; rows already stored stay, are announced, and the call returns
//!   [`FingerprintError::Cancelled`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::UNIX_EPOCH;

use musaic_db::fingerprints_ext::{self, FingerprintRecord, RejectRecord};
use musaic_events::{Bus, Event};
use musaic_net::{AcoustIdClient, ClientError, Transport};
use musaic_resolve::LookupResult;
use musaic_resolve::source::NetError;
use rusqlite::Connection;

/// `fingerprint.HIGH_CONFIDENCE_SCORE`.
pub const HIGH_CONFIDENCE_SCORE: f64 = 0.85;

/// The Python app's `is_available()` reason without a key; this build uses it
/// only where AcoustID is needed (Tier 0c).
pub const NO_KEY_REASON: &str = "AcoustID API key not set (configure in Settings)";

/// Computes one file's fingerprint.
pub trait Fingerprinter: Send + Sync {
    /// `(compressed fingerprint, duration in whole seconds)`, or why not.
    fn fingerprint(&self, path: &Path) -> Result<(String, u64), String>;
    /// Stored in `track_fingerprints.fingerprint_impl`.
    fn implementation(&self) -> &str;
    /// Whether an `Err` from [`Fingerprinter::fingerprint`] is a
    /// verdict on the file's content, to be remembered until the file
    /// changes (and its old row dropped). False by default, so a
    /// fingerprinter that refuses everything (one that cannot run at all)
    /// leaves rows and files alone.
    fn rejection_is_about_the_file(&self) -> bool {
        false
    }
}

/// The in-process Chromaprint port (`musaic-dsp`).
#[derive(Debug, Clone, Copy, Default)]
pub struct Chromaprint;

impl Fingerprinter for Chromaprint {
    fn fingerprint(&self, path: &Path) -> Result<(String, u64), String> {
        musaic_dsp::fingerprint_file(path)
            .map(|f| (f.compressed, f.duration_s))
            .map_err(|e| e.to_string())
    }

    fn implementation(&self) -> &str {
        musaic_db::FINGERPRINT_IMPL_RUST
    }

    /// Decode or analysis failures of the file itself.
    fn rejection_is_about_the_file(&self) -> bool {
        true
    }
}

/// One AcoustID lookup: every result for the fingerprint, or why not.
pub trait AcoustIdLookup {
    fn lookup(&self, fingerprint: &str, duration_s: u64) -> Result<Vec<LookupResult>, NetError>;
}

/// The resolver's three failure classes for a client error (an AcoustID
/// `status: error` answer counts as unreadable).
pub fn net_error(e: ClientError) -> NetError {
    match e {
        // The caller stopped the request (shutdown): not a network failure.
        ClientError::Transport(t) if t.is_cancelled() => NetError::Cancelled,
        ClientError::Transport(t) => NetError::Network(t.to_string()),
        ClientError::Status { status, .. } => NetError::Http(status),
        other => NetError::Parse(other.to_string()),
    }
}

impl<T: Transport> AcoustIdLookup for AcoustIdClient<T> {
    fn lookup(&self, fingerprint: &str, duration_s: u64) -> Result<Vec<LookupResult>, NetError> {
        AcoustIdClient::lookup(self, fingerprint, duration_s)
            .map(|r| r.results)
            .map_err(net_error)
    }
}

/// `_best_recording(results)`: `(recording_mbid, score)` of the first
/// recording with a non-empty id on the highest-scoring result whose score
/// is at least [`HIGH_CONFIDENCE_SCORE`] (a missing score reads 0; on equal
/// scores the earlier result wins).
pub fn best_recording(results: &[LookupResult]) -> Option<(String, f64)> {
    let mut best: Option<(String, f64)> = None;
    let mut best_score = 0.0;
    for r in results {
        let score = r.score_or_zero();
        if score < HIGH_CONFIDENCE_SCORE {
            continue;
        }
        let first = r
            .recordings
            .iter()
            .find_map(|rec| rec.id.as_deref().filter(|id| !id.is_empty()));
        if let Some(id) = first
            && score > best_score
        {
            best_score = score;
            best = Some((id.to_owned(), score));
        }
    }
    best
}

/// Cooperative cancellation shared between a caller and a running job.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// What `fingerprint_album` needs.
pub struct FingerprintEnv<'a> {
    pub conn: &'a Connection,
    pub fingerprinter: &'a dyn Fingerprinter,
    /// `None` when no AcoustID key is set (fingerprints are still
    /// computed and stored).
    pub acoustid: Option<&'a dyn AcoustIdLookup>,
    pub bus: &'a Bus,
    /// `cache._now_iso()`, stamped on every row this call writes.
    pub now_iso: &'a str,
    pub cancel: Option<&'a CancelToken>,
}

/// One file's stored result (`TrackFingerprint`).
#[derive(Debug, Clone, PartialEq)]
pub struct TrackFingerprint {
    pub filename: String,
    pub fingerprint: String,
    pub duration_s: f64,
    pub acoustid_recording_mbid: Option<String>,
    pub acoustid_score: Option<f64>,
}

impl From<&FingerprintRecord> for TrackFingerprint {
    fn from(r: &FingerprintRecord) -> Self {
        Self {
            filename: r.filename.clone(),
            fingerprint: r.fingerprint.clone(),
            duration_s: r.duration_s,
            acoustid_recording_mbid: r.acoustid_recording_mbid.clone(),
            acoustid_score: r.acoustid_score,
        }
    }
}

/// The result of [`fingerprint_album`].
#[derive(Debug, Clone, Default)]
pub struct AlbumFingerprints {
    /// In file order.
    pub tracks: Vec<TrackFingerprint>,
    /// Served from the stored rows; nothing was computed or asked.
    pub from_cache: bool,
    /// The AcoustID answers this call received, by filename (only files it
    /// looked up). Tier 0c reuses them instead of asking twice.
    pub lookups: BTreeMap<String, Result<Vec<LookupResult>, NetError>>,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum FingerprintError {
    /// `FingerprintError("album-missing", "Album directory not found: …")`.
    #[error("Album directory not found: {0}")]
    AlbumMissing(String),
    /// `FingerprintError("no-fingerprints", …)`.
    #[error("Couldn't fingerprint any files in this album")]
    NoFingerprints,
    /// Stopped by the caller's [`CancelToken`]; `stored` rows were written.
    #[error("cancelled after {stored} file(s)")]
    Cancelled { stored: usize },
    /// The database refused a read or write (the Python app let the sqlite
    /// exception propagate).
    #[error("store: {0}")]
    Store(String),
}

impl FingerprintError {
    /// The Python app's `FingerprintError.code`, where it has one.
    pub fn code(&self) -> &'static str {
        match self {
            FingerprintError::AlbumMissing(_) => "album-missing",
            FingerprintError::NoFingerprints => "no-fingerprints",
            FingerprintError::Cancelled { .. } => "cancelled",
            FingerprintError::Store(_) => "store",
        }
    }
}

/// The Python app's logger for this module (`musaic.fingerprint`).
pub const FINGERPRINT_LOG: &str = "musaic.fingerprint";

/// fingerprint.py:213 `log.warning("AcoustID lookup failed for %s: %s", ...)`.
/// The Python app logged the `WebServiceError`s pyacoustid raises (no answer,
/// a body that is not JSON) and stored an HTTP error status or an AcoustID
/// `status: error` answer silently as "no match"; here every one of them is
/// a failed lookup, so every one is logged. `e` is this build's
/// [`NetError`] text, which carries no key (`ClientError` texts redact it;
/// see musaic-net's tests/secrets.rs).
///
/// Deliberate change: the per-track line is DEBUG, and each album gets one
/// WARNING summary ([`LookupFailures::log_summary`]). With no network every
/// track fails the same way, and the Python app's one WARNING per track
/// filled the log with thousands of identical lines.
fn log_lookup_failure(filename: &str, e: &NetError) {
    musaic_log::debug(
        FINGERPRINT_LOG,
        &format!("AcoustID lookup failed for {filename}: {e}"),
    );
}

/// The failed AcoustID lookups of one album, for its summary line.
#[derive(Default)]
struct LookupFailures {
    asked: usize,
    failed: usize,
    first: Option<String>,
}

impl LookupFailures {
    fn record(&mut self, filename: &str, answer: &Result<Vec<LookupResult>, NetError>) {
        // A cancelled request is neither asked nor failed: it is not logged.
        if matches!(answer, Err(NetError::Cancelled)) {
            return;
        }
        self.asked += 1;
        if let Err(e) = answer {
            log_lookup_failure(filename, e);
            self.failed += 1;
            if self.first.is_none() {
                self.first = Some(e.to_string());
            }
        }
    }

    /// One WARNING for the album when any lookup failed:
    /// `AcoustID lookup failed for N of M track(s) in <folder>: <first error>`.
    fn log_summary(&self, album_path: &str) {
        if self.failed == 0 {
            return;
        }
        let folder = Path::new(album_path).file_name().map_or_else(
            || album_path.to_owned(),
            |n| n.to_string_lossy().into_owned(),
        );
        musaic_log::warning(
            FINGERPRINT_LOG,
            &format!(
                "AcoustID lookup failed for {} of {} track(s) in {folder}: {}",
                self.failed,
                self.asked,
                self.first.as_deref().unwrap_or_default()
            ),
        );
    }
}

fn store_err(e: musaic_db::Error) -> FingerprintError {
    FingerprintError::Store(e.to_string())
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// How long a row waits after its `failures`-th consecutive failed
/// lookup: 0 after the first, then 1 h * 4^(failures-2), capped at
/// [`LOOKUP_RETRY_MAX_S`].
pub fn lookup_retry_delay_s(failures: i64) -> i64 {
    if failures <= 1 {
        return 0;
    }
    let exp = u32::try_from(failures - 2).unwrap_or(u32::MAX).min(16);
    LOOKUP_RETRY_FIRST_S
        .saturating_mul(4_i64.saturating_pow(exp))
        .min(LOOKUP_RETRY_MAX_S)
}

/// The wait after the second consecutive failure.
pub const LOOKUP_RETRY_FIRST_S: i64 = 3600;
/// The longest wait between two retries.
pub const LOOKUP_RETRY_MAX_S: i64 = 7 * 86_400;

/// `YYYY-MM-DDTHH:MM:SSZ` (the services' timestamp format) to Unix seconds.
fn parse_iso_utc(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
    {
        return None;
    }
    let num = |a: usize, z: usize| s.get(a..z)?.parse::<i64>().ok();
    let (y, m, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hh, mm, ss) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    // Howard Hinnant's days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hh * 3600 + mm * 60 + ss)
}

/// When a row that has now failed `failures` times in a row is due again.
/// An unreadable `now` makes it due at `now` (compared as text).
fn retry_after(now: &str, failures: i64) -> String {
    match parse_iso_utc(now) {
        Some(t) => crate::clock::format_utc(t.saturating_add(lookup_retry_delay_s(failures))),
        None => now.to_owned(),
    }
}

/// What a file looks like, to tell whether a rejected file has
/// changed since: `size:mtime_ns:ctime_ns` (ctime on Unix only, so a
/// permissions change counts too; 0 elsewhere). `None` when it cannot be
/// read.
pub fn file_signature(path: &Path) -> Option<String> {
    let m = std::fs::metadata(path).ok()?;
    let mtime = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    #[cfg(unix)]
    let ctime = {
        use std::os::unix::fs::MetadataExt;
        i128::from(m.ctime()) * 1_000_000_000 + i128::from(m.ctime_nsec())
    };
    #[cfg(not(unix))]
    let ctime = 0_i128;
    Some(format!("{}:{mtime}:{ctime}", m.len()))
}

/// The row stored for one file after this call: `prev` is what was stored,
/// `answer` the lookup this call made (`None`: none, no key).
#[allow(clippy::too_many_arguments)]
fn next_row(
    prev: Option<&FingerprintRecord>,
    filename: String,
    fingerprint: String,
    duration_s: f64,
    computed_at: String,
    implementation: &str,
    answer: Option<&Result<Vec<LookupResult>, NetError>>,
    now: &str,
) -> FingerprintRecord {
    let same_print =
        prev.is_some_and(|p| p.fingerprint == fingerprint && p.fingerprint_impl == implementation);
    // An unfinished lookup keeps what was stored for the file.
    let kept = prev
        .filter(|p| p.acoustid_recording_mbid.is_some())
        .map_or((None, None), |p| {
            (p.acoustid_recording_mbid.clone(), p.acoustid_score)
        });
    let (mbid, score, state, failures, retry) = match answer {
        Some(Ok(results)) => {
            let (m, s) = best_recording(results).map_or((None, None), |(m, s)| (Some(m), Some(s)));
            (m, s, Some(musaic_db::LOOKUP_DONE.to_owned()), 0, None)
        }
        Some(Err(_)) => {
            let failures = prev
                .filter(|p| p.acoustid_lookup_state.as_deref() == Some(musaic_db::LOOKUP_FAILED))
                .map_or(0, |p| p.acoustid_lookup_failures)
                + 1;
            (
                kept.0,
                kept.1,
                Some(musaic_db::LOOKUP_FAILED.to_owned()),
                failures,
                Some(retry_after(now, failures)),
            )
        }
        // No lookup: the outcome stands only for the same fingerprint.
        None => match prev.filter(|_| same_print) {
            Some(p) => (
                kept.0,
                kept.1,
                p.acoustid_lookup_state.clone(),
                p.acoustid_lookup_failures,
                p.acoustid_retry_after.clone(),
            ),
            None => (kept.0, kept.1, None, 0, None),
        },
    };
    FingerprintRecord {
        filename,
        fingerprint,
        duration_s,
        acoustid_recording_mbid: mbid,
        acoustid_score: score,
        last_computed_at: computed_at,
        fingerprint_impl: implementation.to_owned(),
        acoustid_lookup_state: state,
        acoustid_lookup_failures: failures,
        acoustid_retry_after: retry,
    }
}

/// `fingerprint_album(album_id, album_path, force=force)`. See the module
/// documentation for the rules and the deliberate changes.
pub fn fingerprint_album(
    env: &FingerprintEnv<'_>,
    album_id: &str,
    album_path: &str,
    force: bool,
) -> Result<AlbumFingerprints, FingerprintError> {
    let dir = Path::new(album_path);
    if !dir.is_dir() {
        return Err(FingerprintError::AlbumMissing(album_path.to_owned()));
    }
    let cancelled = || env.cancel.is_some_and(CancelToken::is_cancelled);
    let implementation = env.fingerprinter.implementation();
    let stored = fingerprints_ext::get_album_fingerprints(env.conn, album_id).map_err(store_err)?;
    let listed = std::fs::read_dir(dir).is_ok();
    let flacs: Vec<(String, PathBuf)> = musaic_scan::list_flacs(dir)
        .into_iter()
        .map(|p| (file_name(&p), p))
        .collect();
    let rejects: BTreeMap<String, RejectRecord> = fingerprints_ext::get_rejects(env.conn, album_id)
        .map_err(store_err)?
        .into_iter()
        .map(|r| (r.filename.clone(), r))
        .collect();
    // A remembered reject, unchanged since, by the same fingerprinter.
    let still_rejected = |name: &str, path: &Path| {
        rejects.get(name).is_some_and(|r| {
            r.fingerprint_impl == implementation
                && file_signature(path).as_deref() == Some(r.file_signature.as_str())
        })
    };

    if !force && !stored.is_empty() {
        let cached: BTreeSet<&str> = stored.iter().map(|r| r.filename.as_str()).collect();
        let readable: BTreeSet<&str> = flacs
            .iter()
            .filter(|(n, p)| !still_rejected(n, p))
            .map(|(n, _)| n.as_str())
            .collect();
        let same_impl = stored.iter().all(|r| r.fingerprint_impl == implementation);
        if cached == readable && same_impl {
            let due: Vec<usize> = match env.acoustid {
                Some(_) => (0..stored.len())
                    .filter(|&i| stored[i].lookup_due(env.now_iso))
                    .collect(),
                None => Vec::new(),
            };
            if due.is_empty() {
                return Ok(AlbumFingerprints {
                    tracks: stored.iter().map(TrackFingerprint::from).collect(),
                    from_cache: true,
                    lookups: BTreeMap::new(),
                });
            }
            return lookup_stored(env, album_id, album_path, stored, &due);
        }
    }

    // Rows of files that are gone go first (only from a listing
    // that worked: an unreadable directory says nothing about its files).
    if listed {
        let names: Vec<String> = flacs.iter().map(|(n, _)| n.clone()).collect();
        fingerprints_ext::delete_vanished(env.conn, album_id, &names).map_err(store_err)?;
    }
    if flacs.is_empty() {
        return Ok(AlbumFingerprints::default());
    }

    let mut computed: Vec<(String, String, u64)> = Vec::new();
    for (name, path) in &flacs {
        if cancelled() {
            return Err(FingerprintError::Cancelled { stored: 0 });
        }
        if !force && still_rejected(name, path) {
            continue;
        }
        match env.fingerprinter.fingerprint(path) {
            Ok((fp, dur)) => {
                if rejects.contains_key(name) {
                    fingerprints_ext::delete_reject(env.conn, album_id, name).map_err(store_err)?;
                }
                computed.push((name.clone(), fp, dur));
            }
            // A file the fingerprinter rejects is skipped and remembered,
            // unless it cannot even be opened.
            Err(e) => {
                // fingerprint.py:191; `e` is this build's decoder / io text,
                // not pyacoustid's.
                musaic_log::warning(
                    FINGERPRINT_LOG,
                    &format!("Chromaprint failed for {name}: {e}"),
                );
                if env.fingerprinter.rejection_is_about_the_file()
                    && std::fs::File::open(path).is_ok()
                    && let Some(sig) = file_signature(path)
                {
                    let rec = RejectRecord {
                        filename: name.clone(),
                        file_signature: sig,
                        fingerprint_impl: implementation.to_owned(),
                        rejected_at: env.now_iso.to_owned(),
                    };
                    fingerprints_ext::record_reject(env.conn, album_id, &rec).map_err(store_err)?;
                }
            }
        }
    }
    if computed.is_empty() {
        return Err(FingerprintError::NoFingerprints);
    }

    let previous: BTreeMap<&str, &FingerprintRecord> =
        stored.iter().map(|r| (r.filename.as_str(), r)).collect();
    let mut out = AlbumFingerprints::default();
    let mut result = Ok(());
    let mut failures = LookupFailures::default();
    for (filename, fp, dur) in computed {
        if cancelled() {
            result = Err(FingerprintError::Cancelled {
                stored: out.tracks.len(),
            });
            break;
        }
        let mut answer = env.acoustid.map(|a| a.lookup(&fp, dur));
        // A lookup the caller cancelled says nothing about the file: keep
        // the fingerprint, store no lookup outcome, and stop after this row.
        let lookup_cancelled = matches!(answer, Some(Err(NetError::Cancelled)));
        if lookup_cancelled {
            answer = None;
        }
        if let Some(a) = &answer {
            failures.record(&filename, a);
        }
        let rec = next_row(
            previous.get(filename.as_str()).copied(),
            filename.clone(),
            fp,
            dur as f64,
            env.now_iso.to_owned(),
            implementation,
            answer.as_ref(),
            env.now_iso,
        );
        if let Err(e) = fingerprints_ext::upsert_track_fingerprint(env.conn, album_id, &rec) {
            result = Err(store_err(e));
            break;
        }
        if let Some(a) = answer {
            out.lookups.insert(filename, a);
        }
        out.tracks.push(TrackFingerprint::from(&rec));
        if lookup_cancelled {
            result = Err(FingerprintError::Cancelled {
                stored: out.tracks.len(),
            });
            break;
        }
    }

    failures.log_summary(album_path);
    if !out.tracks.is_empty() {
        env.bus.publish(&Event::TrackFingerprinted {
            album_id: album_id.to_owned(),
            filenames: out.tracks.iter().map(|t| t.filename.clone()).collect(),
        });
    }
    result.map(|()| out)
}

/// The stored rows are current; look up the `due` ones
/// from their stored fingerprints (nothing is fingerprinted again), store
/// each outcome, and announce the files asked. The result holds every row.
fn lookup_stored(
    env: &FingerprintEnv<'_>,
    album_id: &str,
    album_path: &str,
    mut rows: Vec<FingerprintRecord>,
    due: &[usize],
) -> Result<AlbumFingerprints, FingerprintError> {
    let Some(acoustid) = env.acoustid else {
        unreachable!("lookup_stored is only called with a key");
    };
    let mut out = AlbumFingerprints::default();
    let mut asked = Vec::new();
    let mut result = Ok(());
    let mut failures = LookupFailures::default();
    for &i in due {
        if env.cancel.is_some_and(CancelToken::is_cancelled) {
            result = Err(FingerprintError::Cancelled {
                stored: asked.len(),
            });
            break;
        }
        let prev = rows[i].clone();
        let answer = acoustid.lookup(&prev.fingerprint, prev.duration_s as u64);
        // Cancelled: the stored row stays as it was (no failure counted, no
        // retry delay) and nothing more is asked.
        if matches!(answer, Err(NetError::Cancelled)) {
            result = Err(FingerprintError::Cancelled {
                stored: asked.len(),
            });
            break;
        }
        failures.record(&prev.filename, &answer);
        let rec = next_row(
            Some(&prev),
            prev.filename.clone(),
            prev.fingerprint.clone(),
            prev.duration_s,
            prev.last_computed_at.clone(),
            &prev.fingerprint_impl,
            Some(&answer),
            env.now_iso,
        );
        if let Err(e) = fingerprints_ext::upsert_track_fingerprint(env.conn, album_id, &rec) {
            result = Err(store_err(e));
            break;
        }
        out.lookups.insert(prev.filename.clone(), answer);
        asked.push(prev.filename);
        rows[i] = rec;
    }
    failures.log_summary(album_path);
    out.tracks = rows.iter().map(TrackFingerprint::from).collect();
    if !asked.is_empty() {
        env.bus.publish(&Event::TrackFingerprinted {
            album_id: album_id.to_owned(),
            filenames: asked,
        });
    }
    result.map(|()| out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_round_trips_through_the_clock_format() {
        for t in [
            0_i64,
            978_307_200,
            1_794_268_800,
            1_794_268_800 + 86_399,
            4_102_444_800,
        ] {
            assert_eq!(parse_iso_utc(&crate::clock::format_utc(t)), Some(t));
        }
        assert_eq!(parse_iso_utc("2001-01-01T00:00:00+00:00"), None);
    }

    // The retry schedule.
    #[test]
    fn retry_delays() {
        let d: Vec<i64> = (1..=8).map(lookup_retry_delay_s).collect();
        assert_eq!(
            d,
            [0, 3600, 14_400, 57_600, 230_400, 604_800, 604_800, 604_800]
        );
        assert_eq!(lookup_retry_delay_s(i64::MAX), LOOKUP_RETRY_MAX_S);
        assert_eq!(
            retry_after("2001-01-01T00:00:00Z", 2),
            "2001-01-01T01:00:00Z"
        );
        assert_eq!(retry_after("garbage", 3), "garbage");
    }
}
