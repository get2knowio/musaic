//! The service functions behind the six check fix-action routes of
//! `musaic/api/health.py`, plus `checks/runner.py::fix_finding` /
//! `try_quick_fix_finding` and every check's `fix()` / `try_quick_fix()`:
//!
//! | route | function |
//! |---|---|
//! | `POST /api/checks/{check_id}/findings/{finding_id}/fix` | [`fix_finding`] |
//! | `POST /api/checks/{check_id}/quick-fix-all` (SSE) | [`quick_fix_all`] |
//! | `POST .../filename-mismatch/findings/{id}/apply` | [`apply_filename_rename`] |
//! | `POST .../duplicate-tracks/findings/{id}/delete-file` | [`delete_duplicate_file`] |
//! | `POST .../inconsistent-artist-sort/findings/{id}/apply-sort` | [`apply_cluster_sort`] |
//! | `GET .../duplicate-tracks/findings/{id}/canonical` | not here: a MusicBrainz recording lookup (musaic-net) |
//!
//! Each returns the route's body, or the [`HttpError`] the route raises with
//! the Python app's status and detail text. Bus events are published on
//! `env.bus` at the Python app's moments. Request-body validation (422) is
//! the HTTP layer's.
//!
//! # What the host supplies
//!
//! [`FixSources`]: fingerprinting, the resolver's anchor, and the two
//! MusicBrainz lookups the cover and track-gaps cascades make. With these
//! the four resolver / MusicBrainz checks run the Python app's deterministic
//! cascade here. Where the Python app would enqueue an AI job, the result is
//! [`FixOutcome::NeedsAiJob`] carrying the job it would enqueue and the
//! message it would answer on success; the AI queue is the host's, so the
//! host enqueues it (and owns the "Already has an active job ({status})"
//! conflict answer).
//!
//! Deliberate change: when the host says no model backend exists
//! ([`FixEnv::model_unavailable`]), such a fix answers
//! `{ok: false, message: <not-available message>, job_id: null}` instead of
//! [`FixOutcome::NeedsAiJob`]; a tags-out-of-sync fix that wrote resolver
//! values first answers `"<what it wrote>; <not-available message>"`.
//! Quick fixes never queue a job, so quick-fix-all is unchanged.
//!
//! # Deliberate changes and reproduced quirks
//!
//! - Delete-duplicate refuses (409) when the album directory cannot be
//!   reached; nothing is deleted. The Python app took the file for "already
//!   gone", and its rescan deleted the whole album row.
//! - The artist-sort bulk apply runs no check sweep of its own; the
//!   `BulkBatchCompleted` it publishes triggers the one sweep.
//! - Rename details (case- and normalisation-only collisions, trash naming,
//!   fingerprint and tag rows following a rename, the sidecar rewrite) live
//!   in `musaic_fsops::rename::execute_renames`.
//! - `apply_direct` refuses a Missing album, so a fix on a Missing album
//!   reports that refusal (the Python app would have applied).
//! - When delete-duplicate's rescan finds no FLAC left in a reachable
//!   directory, the album row is kept and marked Missing
//!   (`AlbumMarkedMissing`, as `mark_album_missing` publishes), so its
//!   fingerprints, track tags, anchor and blocklist survive; the Python app
//!   deleted the row with all of them (`AlbumDeleted`). The trashed file's
//!   own fingerprint row is still deleted first, as in the Python app: it
//!   describes a file that is no longer in the album (restoring it from the
//!   trash makes it unfingerprinted, and the pool fingerprints it again).
//! - Tags-out-of-sync's fix and quick fix read the album with its path, as
//!   the Python app's `_load_album` does (it selects `path`), so a matched
//!   track whose ARTIST differs from the resolver's canonical artist is
//!   written (check_fixes.json, section `per-track-artist`).
//! - A finding whose album is Missing is not attempted: `fix_finding`
//!   answers `{ok: false, message: "Not applicable: ..."}`
//!   ([`missing_album_not_applicable`]) and quick-fix-all reports it
//!   `not-applicable`; the Python app attempted both (section
//!   `missing-album`).
//! - The host's MusicBrainz name search for these fixes
//!   ([`FixSources::find_cover`], the uncached half of
//!   [`FixSources::find_tracklist`]) goes through
//!   [`crate::sources::find_release_for_fix`], which saves a confident hit
//!   as the anchor only after the track-count and duration check.
//! - A panicking fix in quick-fix-all is an `error` outcome and the loop
//!   goes on; `BulkBatchCompleted` is published whatever happens.
//! - quick-fix-all reruns its check over the albums it fixed and reports
//!   only the findings that are gone as `fixed` (`still_open` for the rest).
//!
//! Golden: testdata/golden/services/check_fixes.json (recorded from the
//! Python app); test: tests/golden_check_fixes.rs.

use std::ops::ControlFlow;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use musaic_checks::checks::duplicate_tracks;
use musaic_checks::checks::inconsistent_artist_sort::{self as artist_sort, SortWrite};
use musaic_checks::checks::tags_out_of_sync::{self as tags_sync, AlbumDiffs, TagsRow};
use musaic_checks::checks::track_gaps::{self, MbTrack};
use musaic_checks::{CheckEnv, FixKind};
use musaic_db::findings::{self, FindingFilter, FindingRow};
use musaic_events::{Bus, Event as BusEvent};
use musaic_fsops::py_repr;
use musaic_fsops::rename::{PlanEntry, RenameEnv, RenameOutcome};
use musaic_scan::{TrackOrder, py_resolve};
use musaic_tags::{Suggestions, TrackSuggestions};
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

use crate::apply::{ApplyEnv, CoverSource, HttpError, apply_direct, missing_album_detail};
use crate::json::Json;

fn err(status: u16, detail: impl Into<String>) -> HttpError {
    HttpError {
        status,
        detail: detail.into(),
    }
}

fn db_err(e: impl std::fmt::Display) -> HttpError {
    err(500, e.to_string())
}

/// The message of a fix or quick fix that was
/// not attempted because the finding's album is Missing.
pub fn missing_album_not_applicable(missing_since: &str) -> String {
    format!("Not applicable: {}", missing_album_detail(missing_since))
}

/// `missing_since` of the finding's album when that album is Missing
/// (neither NULL nor empty, the runner's test); `None` otherwise, also for a
/// finding without an album or whose album row is gone.
fn missing_album_of(conn: &Connection, album_id: Option<&str>) -> Option<String> {
    let id = album_id?;
    musaic_db::albums::missing_since(conn, id)
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
}

/// The text of a panic payload (`panic!` with a literal or a format).
fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic".to_string()
    }
}

// ---------- what the host supplies ----------

/// `fingerprint.FingerprintError`: `code` drives the user-facing text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FingerprintFailure {
    /// `fpcalc-missing` | `no-api-key` | `pyacoustid-missing` |
    /// `album-missing` | `no-fingerprints` | anything else.
    pub code: String,
    pub detail: String,
}

/// One fingerprinted file (`fingerprint_album`'s result entries).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprinted {
    pub filename: String,
    pub acoustid_recording_mbid: Option<String>,
}

/// The resolver's anchor (`AlbumResolution.anchor` + `anchor_strategy`).
#[derive(Debug, Clone, PartialEq)]
pub struct Anchor {
    pub value: String,
    pub strategy: String,
    /// `anchor.confidence` (0..1); `None` counts as 0.
    pub confidence: Option<f64>,
}

/// `mb_lookup.find_release(..., fetch_cover=True)`, reduced.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CoverMatch {
    pub confident: bool,
    pub cover_url: Option<String>,
    pub cover_url_verified: bool,
}

/// `mb_lookup.find_release_cached(..., fetch_tracklist=True)`, reduced.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TracklistMatch {
    pub confident: bool,
    pub tracklist: Vec<MbTrack>,
}

/// The capabilities the fix cascades need from outside this crate.
pub trait FixSources {
    /// `fingerprint.is_available()` (the track-gaps tier-2 precheck).
    fn fingerprint_available(&self) -> bool;
    /// `fingerprint.fingerprint_album(album_id, album_path)`; it stores the
    /// rows and publishes `TrackFingerprinted` itself.
    fn fingerprint_album(
        &self,
        conn: &mut Connection,
        album_id: &str,
        album_path: &str,
    ) -> Result<Vec<Fingerprinted>, FingerprintFailure>;
    /// `resolver.resolve_album(album_id)`'s anchor; `Err` is the exception text.
    fn resolve_anchor(
        &self,
        conn: &mut Connection,
        album_id: &str,
    ) -> Result<Option<Anchor>, String>;
    /// `mb_lookup.find_release(album, fetch_cover=True, fetch_tracklist=False)`.
    fn find_cover(&self, conn: &mut Connection, album_id: &str) -> CoverMatch;
    /// `mb_lookup.find_release_cached(album, fetch_cover=False, fetch_tracklist=True)`.
    fn find_tracklist(&self, conn: &mut Connection, album_id: &str) -> TracklistMatch;
}

/// No network, no fingerprinter: every lookup comes back empty and
/// fingerprinting fails with `no-api-key`.
#[derive(Debug, Default, Clone, Copy)]
pub struct OfflineSources;

impl FixSources for OfflineSources {
    fn fingerprint_available(&self) -> bool {
        false
    }
    fn fingerprint_album(
        &self,
        _conn: &mut Connection,
        _album_id: &str,
        _album_path: &str,
    ) -> Result<Vec<Fingerprinted>, FingerprintFailure> {
        Err(FingerprintFailure {
            code: "no-api-key".into(),
            detail: String::new(),
        })
    }
    fn resolve_anchor(
        &self,
        _conn: &mut Connection,
        _album_id: &str,
    ) -> Result<Option<Anchor>, String> {
        Ok(None)
    }
    fn find_cover(&self, _conn: &mut Connection, _album_id: &str) -> CoverMatch {
        CoverMatch::default()
    }
    fn find_tracklist(&self, _conn: &mut Connection, _album_id: &str) -> TracklistMatch {
        TracklistMatch::default()
    }
}

/// Everything the fix routes need.
pub struct FixEnv<'a> {
    pub conn: &'a mut Connection,
    pub bus: &'a Bus,
    /// Registry, resolver (`ResolutionSource`, read by tags-out-of-sync),
    /// clock and the checks-stream notifier.
    pub checks: &'a CheckEnv,
    pub sources: &'a dyn FixSources,
    /// Cover downloads for `apply_direct(cover_url=...)`.
    pub covers: &'a dyn CoverSource,
    /// `config.library_dir` as stored.
    pub library_dir: Option<&'a str>,
    /// `<config dir>/trash`.
    pub trash_root: &'a Path,
    /// `_now_iso()`.
    pub now: &'a dyn Fn() -> String,
    /// Local-time `YYYY-MM-DDTHH-MM-SS` for a trash name collision.
    pub trash_stamp: &'a dyn Fn() -> String,
    pub track_order: TrackOrder,
    /// `Some(message)` when no model backend
    /// exists (the host reads it from its AI provider). Where a fix would
    /// enqueue an AI job it answers `{ok: false, message}` instead.
    pub model_unavailable: Option<&'a str>,
}

impl FixEnv<'_> {
    fn resolved_library(&self) -> Option<String> {
        self.library_dir
            .filter(|l| !l.is_empty())
            .map(|l| py_resolve(Path::new(l)).to_string_lossy().into_owned())
    }

    fn apply_direct(
        &mut self,
        album_id: &str,
        suggestions: &Suggestions,
        track_suggestions: &TrackSuggestions,
        cover_url: Option<&str>,
    ) -> (bool, String) {
        let now = self.now;
        let mut env = ApplyEnv {
            conn: self.conn,
            bus: self.bus,
            covers: self.covers,
            now,
            track_order: self.track_order,
        };
        apply_direct(
            &mut env,
            self.library_dir,
            album_id,
            suggestions,
            track_suggestions,
            cover_url,
        )
    }
}

// ---------- results ----------

/// `FixResult` (ok, message, job_id).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixResult {
    pub ok: bool,
    pub message: String,
    pub job_id: Option<String>,
}

impl FixResult {
    fn new(ok: bool, message: impl Into<String>) -> Self {
        FixResult {
            ok,
            message: message.into(),
            job_id: None,
        }
    }

    /// The route body `{ok, message, job_id}`.
    pub fn to_json(&self) -> Json {
        Json::obj([
            ("ok", Json::Bool(self.ok)),
            ("message", Json::from(self.message.as_str())),
            ("job_id", self.job_id.clone().map_or(Json::Null, Json::Str)),
        ])
    }
}

/// The AI job the Python app would enqueue (`jobs.enqueue(...)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiJobRequest {
    pub album_id: String,
    pub album_path: String,
    pub album_title: String,
    /// `metadata` | `cover`.
    pub job_type: &'static str,
    pub hint: String,
    /// What the Python app answers (`ok: true`, with the new job id) when the
    /// enqueue succeeds. On a conflict it answers `ok: false`, "Already has
    /// an active job ({status})" with the existing job's id instead.
    pub message: String,
}

/// What a fix attempt came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixOutcome {
    /// Done here; no AI involved.
    Done(FixResult),
    /// The Python app would enqueue this AI job now (the queue is the host's).
    NeedsAiJob(AiJobRequest),
}

/// One quick-fix-all stream item: `(event, payload)` as the Python app's
/// `sse_event` would frame it.
#[derive(Debug, Clone, PartialEq)]
pub struct QuickFixItem {
    pub event: &'static str,
    pub data: Json,
}

impl QuickFixItem {
    /// `services.sse.sse_event(name, data)`.
    pub fn sse_frame(&self) -> String {
        format!("event: {}\ndata: {}\n\n", self.event, self.data.dumps())
    }
}

// ---------- shared lookups ----------

fn finding_for(
    conn: &Connection,
    finding_id: i64,
    check_id: &str,
) -> Result<FindingRow, HttpError> {
    findings::get_finding(conn, finding_id)
        .map_err(db_err)?
        .filter(|f| f.check_id == check_id)
        .ok_or_else(|| err(404, "Finding not found"))
}

fn detail(f: &FindingRow) -> Option<Value> {
    f.detail_json
        .as_deref()
        .and_then(|d| serde_json::from_str::<Value>(d).ok())
        .filter(|v| !v.is_null())
}

/// The album columns the checks' `_load_album` helpers read (no library or
/// Missing filter, as in the Python app).
#[derive(Debug, Clone)]
struct AlbumBrief {
    path: Option<String>,
    title: Option<String>,
    dir_name: Option<String>,
}

impl AlbumBrief {
    /// `album["title"] or album["dir_name"] or ""`.
    fn job_title(&self) -> String {
        [&self.title, &self.dir_name]
            .into_iter()
            .flatten()
            .find(|s| !s.is_empty())
            .cloned()
            .unwrap_or_default()
    }
}

fn load_album(conn: &Connection, album_id: &str) -> Result<Option<AlbumBrief>, HttpError> {
    conn.query_row(
        "SELECT path, title, dir_name FROM albums WHERE id = ?1",
        [album_id],
        |r| {
            Ok(AlbumBrief {
                path: r.get(0)?,
                title: r.get(1)?,
                dir_name: r.get(2)?,
            })
        },
    )
    .optional()
    .map_err(db_err)
}

/// The AI job the Python app would enqueue here, or, with no model backend
/// `{ok: false, message: <not-available message>}`.
fn ai_job(
    env: &FixEnv<'_>,
    album_id: &str,
    album: &AlbumBrief,
    job_type: &'static str,
    hint: &str,
    message: impl Into<String>,
) -> FixOutcome {
    if let Some(m) = env.model_unavailable {
        return done(false, m);
    }
    FixOutcome::NeedsAiJob(AiJobRequest {
        album_id: album_id.to_string(),
        album_path: album.path.clone().unwrap_or_default(),
        album_title: album.job_title(),
        job_type,
        hint: hint.to_string(),
        message: message.into(),
    })
}

fn done(ok: bool, message: impl Into<String>) -> FixOutcome {
    FixOutcome::Done(FixResult::new(ok, message))
}

// ---------- fix_finding ----------

const DUPLICATES_STUB: &str = "Duplicate-tracks findings are resolved per-file via the Resolve\u{2026} dialog on the album page; this check has no single-click fix because the user has to choose which copy to keep.";
const FILENAME_STUB: &str = "Filename-mismatch findings are resolved via the Resolve\u{2026} dialog; this check has no single-click fix because the user has to review the rename plan first.";
const ARTIST_SORT_STUB: &str = "Inconsistent-artist-sort findings are resolved per-cluster via the Resolve\u{2026} dialog; this check has no single-click fix because the user has to confirm which canonical sort value to bulk-apply.";

/// `POST /api/checks/{check_id}/findings/{finding_id}/fix`: 404 for an
/// unknown finding or one of another check; 400 when the check is not
/// registered or has no fix path; else the check's `fix()`. The checks
/// stream is notified after every attempt (the runner's `finally`).
pub fn fix_finding(
    env: &mut FixEnv<'_>,
    check_id: &str,
    finding_id: i64,
    user_hint: &str,
) -> Result<FixOutcome, HttpError> {
    let f = finding_for(env.conn, finding_id, check_id)?;
    let Some(check) = env.checks.registry.get(&f.check_id) else {
        return Err(err(
            400,
            format!(
                "finding's check_id {} is not registered",
                py_repr(&f.check_id)
            ),
        ));
    };
    if check.meta().fix_kind == FixKind::None {
        return Err(err(
            400,
            format!("check {} has no fix path", check.meta().id),
        ));
    }
    // A finding of a Missing album is not attempted.
    if let Some(since) = missing_album_of(env.conn, f.album_id.as_deref()) {
        return Ok(done(false, missing_album_not_applicable(&since)));
    }
    let out = fix_one(env, &f, user_hint);
    (env.checks.notify)();
    out
}

fn fix_one(env: &mut FixEnv<'_>, f: &FindingRow, hint: &str) -> Result<FixOutcome, HttpError> {
    let album_id = f.album_id.as_deref();
    match f.check_id.as_str() {
        "duplicate-tracks" => Ok(done(false, DUPLICATES_STUB)),
        "filename-mismatch" => Ok(done(false, FILENAME_STUB)),
        "inconsistent-artist-sort" => Ok(done(false, ARTIST_SORT_STUB)),
        "tags-out-of-sync" => tags_fix(env, album_id, hint),
        "unidentified-album" => unidentified_fix(env, album_id, hint),
        "no-cover-art" => cover_fix(env, album_id, hint),
        "track-gaps" => gaps_fix(env, album_id, hint),
        other => Err(err(
            500,
            format!("Check {other} declared a fix_kind but has no fix()"),
        )),
    }
}

/// `try_quick_fix_finding`: `None` = the check has no quick-fix path (or
/// the album vanished, for the checks that say so); `Err` = what the Python app
/// raised (the dispatcher reports it as an `error` outcome).
pub fn try_quick_fix_finding(
    env: &mut FixEnv<'_>,
    finding_id: i64,
) -> Result<Option<FixResult>, String> {
    let f = findings::get_finding(env.conn, finding_id)
        .map_err(|e| format!("OperationalError: {e}"))?
        .ok_or_else(|| format!("ValueError: finding {finding_id} not found"))?;
    if env.checks.registry.get(&f.check_id).is_none() {
        return Err(format!(
            "ValueError: finding's check_id {} is not registered",
            py_repr(&f.check_id)
        ));
    }
    let album_id = f.album_id.as_deref();
    let r = match f.check_id.as_str() {
        "inconsistent-artist-sort" => Some(artist_sort_quick(env, &f)),
        "tags-out-of-sync" => tags_quick(env, album_id)?,
        "unidentified-album" => unidentified_quick(env, album_id)?,
        "no-cover-art" => cover_quick(env, album_id)?,
        "track-gaps" => gaps_quick(env, album_id)?,
        _ => None,
    };
    Ok(r)
}

fn http_to_string(e: HttpError) -> String {
    e.detail
}

// ---------- tags-out-of-sync ----------

/// `tags_out_of_sync._load_album` (musaic/checks/tags_out_of_sync.py:601),
/// path included, so per-track artist drift is part of the fix.
fn tags_row(conn: &Connection, album_id: &str) -> Result<Option<(TagsRow, AlbumBrief)>, HttpError> {
    conn.query_row(
        "SELECT id, dir_name, title, artist, album_artist, year, genre, album_artist_sort, \
                artist_sort, album_sort, is_compilation, mb_release_group_mbid, path \
         FROM albums WHERE id = ?1",
        [album_id],
        |r| {
            let row = TagsRow {
                id: r.get(0)?,
                dir_name: r.get(1)?,
                path: r.get(12)?,
                title: r.get(2)?,
                artist: r.get(3)?,
                album_artist: r.get(4)?,
                year: r.get(5)?,
                genre: r.get(6)?,
                album_artist_sort: r.get(7)?,
                artist_sort: r.get(8)?,
                album_sort: r.get(9)?,
                is_compilation: r.get::<_, Option<i64>>(10)?.unwrap_or(0) != 0,
                mb_release_group_mbid: r.get(11)?,
            };
            let brief = AlbumBrief {
                path: r.get(12)?,
                title: row.title.clone(),
                dir_name: row.dir_name.clone(),
            };
            Ok((row, brief))
        },
    )
    .optional()
    .map_err(db_err)
}

/// `_apply_resolver_diffs`: `(ok, message)`.
fn apply_resolver_diffs(env: &mut FixEnv<'_>, album_id: &str, d: &AlbumDiffs) -> (bool, String) {
    let mut suggestions: Suggestions = Vec::new();
    for (k, v) in d.applicable_album.iter().chain(d.sort_proposals.iter()) {
        match suggestions.iter_mut().find(|(sk, _)| sk == k) {
            Some(slot) => slot.1 = Some(v.clone()),
            None => suggestions.push((k.to_string(), Some(v.clone()))),
        }
    }
    let tracks: TrackSuggestions = d
        .applicable_tracks
        .iter()
        .map(|(f, fields)| {
            (
                f.clone(),
                fields
                    .iter()
                    .map(|(k, v)| (k.to_string(), Some(v.clone())))
                    .collect(),
            )
        })
        .collect();
    if suggestions.is_empty() && tracks.is_empty() {
        return (true, "No deterministic fixes to apply".into());
    }
    let (ok, msg) = env.apply_direct(album_id, &suggestions, &tracks, None);
    if !ok {
        return (false, format!("Apply failed: {msg}"));
    }
    let mut bits = Vec::new();
    if !suggestions.is_empty() {
        bits.push(format!("{} album field(s)", suggestions.len()));
    }
    if !tracks.is_empty() {
        let n: usize = tracks.iter().map(|(_, v)| v.len()).sum();
        bits.push(format!("{n} track field(s) on {} track(s)", tracks.len()));
    }
    (true, format!("Wrote {}", bits.join(" + ")))
}

fn applicable(d: &AlbumDiffs) -> bool {
    !(d.applicable_album.is_empty()
        && d.applicable_tracks.is_empty()
        && d.sort_proposals.is_empty())
}

const TAGS_AI: &str = "Asking Claude to fill in the missing tags";

fn tags_fix(
    env: &mut FixEnv<'_>,
    album_id: Option<&str>,
    hint: &str,
) -> Result<FixOutcome, HttpError> {
    let Some(album_id) = album_id else {
        return Ok(done(false, "No album to fix"));
    };
    let Some((row, brief)) = tags_row(env.conn, album_id)? else {
        return Ok(done(false, "Album not found in cache"));
    };
    if !hint.is_empty() {
        return Ok(ai_job(env, album_id, &brief, "metadata", hint, TAGS_AI));
    }
    let d = tags_sync::compute_diffs(env.conn, env.checks.resolver.as_ref(), &row);
    if !d.has_any() {
        return Ok(done(true, "Nothing to fix \u{2014} tags already in sync"));
    }
    let mut applied = String::new();
    if applicable(&d) {
        let (ok, msg) = apply_resolver_diffs(env, album_id, &d);
        if !ok {
            return Ok(done(false, msg));
        }
        applied = msg;
    }
    if !d.ai_fallback_fields.is_empty()
        || (!d.missing_core.is_empty() && d.applicable_album.is_empty())
    {
        // With no model backend, what was written is kept and
        // named before the not-available message.
        if let (Some(m), false) = (env.model_unavailable, applied.is_empty()) {
            return Ok(done(false, format!("{applied}; {m}")));
        }
        let msg = if applied.is_empty() {
            TAGS_AI.to_string()
        } else {
            format!("{applied}; also asking Claude to fill the rest")
        };
        return Ok(ai_job(env, album_id, &brief, "metadata", "", msg));
    }
    Ok(done(
        true,
        if applied.is_empty() {
            "All canonical diffs applied".to_string()
        } else {
            applied
        },
    ))
}

fn tags_quick(env: &mut FixEnv<'_>, album_id: Option<&str>) -> Result<Option<FixResult>, String> {
    let Some(album_id) = album_id else {
        return Ok(None);
    };
    let Some((row, _)) = tags_row(env.conn, album_id).map_err(http_to_string)? else {
        return Ok(None);
    };
    let d = tags_sync::compute_diffs(env.conn, env.checks.resolver.as_ref(), &row);
    if !applicable(&d) {
        return Ok(Some(FixResult::new(
            false,
            "No deterministic fixes available (anchor or canonical missing)",
        )));
    }
    let (ok, msg) = apply_resolver_diffs(env, album_id, &d);
    Ok(Some(FixResult::new(ok, msg)))
}

// ---------- unidentified-album ----------

fn has_fingerprints(conn: &Connection, album_id: &str) -> Result<bool, HttpError> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM track_fingerprints WHERE album_id = ?1)",
        [album_id],
        |r| r.get(0),
    )
    .map_err(db_err)
}

/// `_try_fingerprint`.
fn try_fingerprint(env: &mut FixEnv<'_>, album_id: &str, path: &str) -> FixResult {
    match env.sources.fingerprint_album(env.conn, album_id, path) {
        Err(e) => {
            let human = match e.code.as_str() {
                "fpcalc-missing" => "fpcalc binary not installed. Install with `brew install chromaprint` to enable AcoustID matching.".to_string(),
                "no-api-key" => "AcoustID API key not set. Configure it under Settings \u{2192} Track fingerprinting.".to_string(),
                "pyacoustid-missing" => "pyacoustid Python library not installed. Reinstall Musaic's dependencies.".to_string(),
                "album-missing" => "Album directory missing from disk.".to_string(),
                "no-fingerprints" => "Couldn't fingerprint any files in this album.".to_string(),
                _ if !e.detail.is_empty() => e.detail,
                _ => e.code,
            };
            FixResult::new(false, human)
        }
        Ok(results) if results.is_empty() => {
            FixResult::new(false, "Album had no FLACs to fingerprint")
        }
        Ok(results) => {
            let matched = results
                .iter()
                .filter(|r| {
                    r.acoustid_recording_mbid
                        .as_deref()
                        .is_some_and(|m| !m.is_empty())
                })
                .count();
            FixResult::new(
                true,
                format!(
                    "fingerprinted {} files ({matched} AcoustID matches)",
                    results.len()
                ),
            )
        }
    }
}

/// The skip message of an album neither the directory-name search nor the
/// fingerprints anchor (`_try_resolver`), less the Python app's AI-escalation
/// advice (no model backend exists to escalate to).
pub const UNRESOLVED_SKIP_MESSAGE: &str = "couldn't identify the album from the directory name or fingerprints \u{2014} try editing the directory name to the canonical \"Artist - Album\" form";

/// `_try_resolver`.
fn try_resolver(env: &mut FixEnv<'_>, album_id: &str) -> FixResult {
    match env.sources.resolve_anchor(env.conn, album_id) {
        Err(e) => FixResult::new(false, format!("resolver error: {e}")),
        // Deliberate change: the Python app's text ends ", or use the
        // per-finding Fix to escalate to AI"; without a model backend that
        // Fix answers ok:false (not available), so the advice is dropped.
        Ok(None) => FixResult::new(false, UNRESOLVED_SKIP_MESSAGE),
        Ok(Some(a)) => {
            // Python int() truncates toward zero.
            let score = (a.confidence.unwrap_or(0.0) * 100.0).trunc() as i64;
            FixResult::new(
                true,
                format!(
                    "anchored to release-group {} via {} (score {score}/100)",
                    a.value, a.strategy
                ),
            )
        }
    }
}

fn join_fp(fp: &str, msg: String) -> String {
    if fp.is_empty() {
        msg
    } else {
        format!("{fp}; {msg}")
    }
}

fn unidentified_fix(
    env: &mut FixEnv<'_>,
    album_id: Option<&str>,
    hint: &str,
) -> Result<FixOutcome, HttpError> {
    let Some(album_id) = album_id else {
        return Ok(done(false, "No album to identify"));
    };
    let Some(album) = load_album(env.conn, album_id)? else {
        return Ok(done(false, "Album not found in cache"));
    };
    let mut fp_message = String::new();
    if !has_fingerprints(env.conn, album_id)? {
        let path = album.path.clone().unwrap_or_default();
        fp_message = try_fingerprint(env, album_id, &path).message;
    }
    if hint.is_empty() {
        let r = try_resolver(env, album_id);
        if r.ok {
            return Ok(done(true, join_fp(&fp_message, r.message)));
        }
    }
    Ok(ai_job(
        env,
        album_id,
        &album,
        "metadata",
        hint,
        "Asking Claude to identify this album on MusicBrainz",
    ))
}

fn unidentified_quick(
    env: &mut FixEnv<'_>,
    album_id: Option<&str>,
) -> Result<Option<FixResult>, String> {
    let Some(album_id) = album_id else {
        return Ok(None);
    };
    let Some(album) = load_album(env.conn, album_id).map_err(http_to_string)? else {
        return Ok(None);
    };
    let mut fp_message = String::new();
    if !has_fingerprints(env.conn, album_id).map_err(http_to_string)? {
        let path = album.path.clone().unwrap_or_default();
        fp_message = try_fingerprint(env, album_id, &path).message;
    }
    let r = try_resolver(env, album_id);
    Ok(Some(FixResult::new(r.ok, join_fp(&fp_message, r.message))))
}

// ---------- no-cover-art ----------

/// `_try_mb`: `None` when MusicBrainz has no verified confident cover or
/// the apply failed.
fn cover_try_mb(env: &mut FixEnv<'_>, album_id: &str) -> Option<FixResult> {
    let m = env.sources.find_cover(env.conn, album_id);
    let url = m.cover_url.filter(|u| !u.is_empty())?;
    if !(m.confident && m.cover_url_verified) {
        return None;
    }
    let (ok, msg) = env.apply_direct(album_id, &Vec::new(), &Vec::new(), Some(&url));
    ok.then(|| {
        FixResult::new(
            true,
            format!("Embedded cover from MusicBrainz / Cover Art Archive ({msg})"),
        )
    })
}

fn cover_fix(
    env: &mut FixEnv<'_>,
    album_id: Option<&str>,
    hint: &str,
) -> Result<FixOutcome, HttpError> {
    let Some(album_id) = album_id else {
        return Ok(done(false, "No album to fix"));
    };
    let Some(album) = load_album(env.conn, album_id)? else {
        return Ok(done(false, "Album not found in cache"));
    };
    if hint.is_empty()
        && let Some(r) = cover_try_mb(env, album_id)
    {
        return Ok(FixOutcome::Done(r));
    }
    Ok(ai_job(
        env,
        album_id,
        &album,
        "cover",
        hint,
        "Asking Claude to find cover art for this album",
    ))
}

fn cover_quick(env: &mut FixEnv<'_>, album_id: Option<&str>) -> Result<Option<FixResult>, String> {
    let Some(album_id) = album_id else {
        return Ok(None);
    };
    if load_album(env.conn, album_id)
        .map_err(http_to_string)?
        .is_none()
    {
        return Ok(None);
    }
    Ok(Some(cover_try_mb(env, album_id).unwrap_or_else(|| {
        FixResult::new(
            false,
            "MusicBrainz / Cover Art Archive doesn't have a cover for this album",
        )
    })))
}

// ---------- track-gaps ----------

fn apply_renumbering(
    env: &mut FixEnv<'_>,
    album_id: &str,
    renumbering: &std::collections::BTreeMap<String, i64>,
    source: &str,
) -> Option<FixResult> {
    let tracks: TrackSuggestions = renumbering
        .iter()
        .map(|(f, p)| {
            (
                f.clone(),
                vec![("tracknumber".to_string(), Some(p.to_string()))],
            )
        })
        .collect();
    let (ok, _) = env.apply_direct(album_id, &Vec::new(), &tracks, None);
    ok.then(|| {
        FixResult::new(
            true,
            format!("Renumbered {} tracks via {source}", renumbering.len()),
        )
    })
}

/// `_try_deterministic`: MB title match, then AcoustID fingerprints.
fn gaps_try_deterministic(env: &mut FixEnv<'_>, album_id: &str, path: &str) -> Option<FixResult> {
    // The MusicBrainz tracklist is numbered straight through all discs, and
    // both tiers write only TRACKNUMBER: on an album kept as several discs
    // numbered from 1 that would replace the per-disc numbers with running
    // ones. Leave such an album to a person (or the model fix).
    if track_gaps::spans_multiple_discs(Path::new(path)) {
        return None;
    }
    let m = env.sources.find_tracklist(env.conn, album_id);
    if !(m.confident && !m.tracklist.is_empty()) {
        return None;
    }
    if let Some(r) = track_gaps::match_local_to_mb(Path::new(path), &m.tracklist) {
        return apply_renumbering(env, album_id, &r, "MB title match");
    }
    // Tier 2. The Python app's prechecks come before any fingerprinting:
    // availability, then a usable (non-repeating, non-empty) set of MB
    // recordings.
    if !env.sources.fingerprint_available() {
        return None;
    }
    let mut seen = std::collections::HashSet::new();
    for t in &m.tracklist {
        if let Some(r) = t.recording_mbid.as_deref().filter(|r| !r.is_empty())
            && !seen.insert(r)
        {
            return None;
        }
    }
    if seen.is_empty() {
        return None;
    }
    let fps = env
        .sources
        .fingerprint_album(env.conn, album_id, path)
        .ok()?;
    let local: Vec<(String, Option<String>)> = fps
        .into_iter()
        .map(|f| {
            (
                f.filename,
                f.acoustid_recording_mbid.filter(|m| !m.is_empty()),
            )
        })
        .collect();
    let r = track_gaps::fingerprint_renumbering(&m.tracklist, &local)?;
    apply_renumbering(env, album_id, &r, "AcoustID fingerprint match")
}

fn gaps_fix(
    env: &mut FixEnv<'_>,
    album_id: Option<&str>,
    hint: &str,
) -> Result<FixOutcome, HttpError> {
    let Some(album_id) = album_id else {
        return Ok(done(false, "No album to fix"));
    };
    let Some(album) = load_album(env.conn, album_id)? else {
        return Ok(done(false, "Album not found in cache"));
    };
    if hint.is_empty() {
        let path = album.path.clone().unwrap_or_default();
        if let Some(r) = gaps_try_deterministic(env, album_id, &path) {
            return Ok(FixOutcome::Done(r));
        }
    }
    Ok(ai_job(
        env,
        album_id,
        &album,
        "metadata",
        hint,
        "Asking Claude to verify the track sequence and propose a fix",
    ))
}

fn gaps_quick(env: &mut FixEnv<'_>, album_id: Option<&str>) -> Result<Option<FixResult>, String> {
    let Some(album_id) = album_id else {
        return Ok(None);
    };
    let Some(album) = load_album(env.conn, album_id).map_err(http_to_string)? else {
        // Unlike the other checks, track-gaps reports this as a skip.
        return Ok(Some(FixResult::new(false, "Album not found in cache")));
    };
    let path = album.path.clone().unwrap_or_default();
    Ok(Some(
        gaps_try_deterministic(env, album_id, &path).unwrap_or_else(|| {
            FixResult::new(
                false,
                "No deterministic fix found (MB title + fingerprint both inconclusive)",
            )
        }),
    ))
}

// ---------- inconsistent-artist-sort ----------

fn run_sort_writes(env: &mut FixEnv<'_>, writes: &[SortWrite]) -> usize {
    let bus = env.bus;
    artist_sort::apply_cluster_sort(bus, writes, |w| {
        let (album, tracks) = w.suggestions();
        let suggestions: Suggestions = album
            .into_iter()
            .map(|(k, v)| (k.to_string(), Some(v)))
            .collect();
        let mut track_suggestions: TrackSuggestions = Vec::new();
        for (f, k, v) in tracks {
            track_suggestions.push((f, vec![(k.to_string(), Some(v))]));
        }
        env.apply_direct(&w.album_id, &suggestions, &track_suggestions, None)
            .0
    })
}

fn artist_sort_quick(env: &mut FixEnv<'_>, f: &FindingRow) -> FixResult {
    match artist_sort::propagate_plan(detail(f).as_ref()) {
        Err(msg) => FixResult::new(false, msg),
        Ok((canonical, plan)) => {
            let applied = run_sort_writes(env, &plan);
            FixResult::new(true, artist_sort::propagate_message(&canonical, applied))
        }
    }
}

/// `POST /api/checks/inconsistent-artist-sort/findings/{finding_id}/apply-sort`.
pub fn apply_cluster_sort(
    env: &mut FixEnv<'_>,
    finding_id: i64,
    sort_value: &str,
) -> Result<Json, HttpError> {
    let f = finding_for(env.conn, finding_id, "inconsistent-artist-sort")?;
    let sort_value = musaic_core::pycompat::strip(sort_value);
    if sort_value.is_empty() {
        return Err(err(400, "sort_value is required"));
    }
    if sort_value.chars().count() > 200 {
        return Err(err(400, "sort_value too long (max 200 chars)"));
    }
    let members: Vec<Value> = detail(&f)
        .and_then(|d| d.get("members").cloned())
        .and_then(|m| m.as_array().cloned())
        .unwrap_or_default();
    if members.is_empty() {
        return Err(err(400, "Finding has no cluster members"));
    }
    let normalized: Vec<Value> = members
        .iter()
        .filter_map(artist_sort::normalize_cluster_member)
        .filter(|m| {
            m.get("album_id").is_some_and(|a| match a {
                Value::Null => false,
                Value::String(s) => !s.is_empty(),
                Value::Bool(b) => *b,
                Value::Number(n) => n.as_f64() != Some(0.0),
                Value::Array(a) => !a.is_empty(),
                Value::Object(o) => !o.is_empty(),
            })
        })
        .collect();
    if normalized.is_empty() {
        return Err(err(400, "Cluster members missing album_id"));
    }
    let plan = artist_sort::plan_cluster_sort(&normalized, sort_value);
    let applied = run_sort_writes(env, &plan);
    let distinct: std::collections::HashSet<String> = normalized
        .iter()
        .map(|m| match &m["album_id"] {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .collect();
    Ok(Json::obj([
        ("ok", Json::Bool(applied > 0)),
        ("applied", Json::from(applied)),
        ("cluster_size", Json::from(normalized.len())),
        ("distinct_albums", Json::from(distinct.len())),
        ("sort_value", Json::from(sort_value)),
        (
            "message",
            Json::Str(format!(
                "Wrote sort={} to {applied} of {} album(s) in the cluster.",
                py_repr(sort_value),
                distinct.len()
            )),
        ),
    ]))
}

// ---------- quick-fix-all ----------

/// `POST /api/checks/{check_id}/quick-fix-all`: 404 for an unknown check
/// (before anything streams); else `start {total}`, one `progress
/// {finding_id, outcome, message}` per open finding (outcome `fixed` |
/// `skipped` | `error` | `not-applicable`), then `done {total, fixed,
/// skipped, errors, not_applicable, still_open}`. The loop runs with `in_bulk_run` set;
/// afterwards, if there was at least one finding, `BulkBatchCompleted` is
/// published, also when the sink stopped reading (`Break`) early.
///
/// Deliberate change: before `done`, the check is rerun over the albums
/// whose fix reported success, and `done.fixed` counts only the findings
/// that are gone; the others are `still_open` (a partial fix). A `progress`
/// outcome `fixed` still means "the fix wrote what it could". The Python app
/// counted every successful fix as fixed and left the findings as they were
/// until the check's next run (not part of the post-scan sweep for
/// tags-out-of-sync).
///
/// A finding whose album is Missing is
/// reported `not-applicable` ([`missing_album_not_applicable`]) without
/// being attempted; the Python app attempted it.
///
/// A fix that panics is reported as an `error`
/// outcome (`panic: <message>`; the Python app's per-finding `except Exception`
/// reported `<Type>: <message>`) and the loop goes on. A panic anywhere else
/// in the loop (the sink itself) ends it with the Python app's fatal frame,
/// `error {detail}`. Either way the bulk-run flag is reset and
/// `BulkBatchCompleted` is still published (the Python app's `finally`).
///
/// `between` is called before every finding but the first: the Python app
/// sleeps one second there (MusicBrainz pacing). The host decides; the
/// network layer already paces MusicBrainz itself.
pub fn quick_fix_all(
    env: &mut FixEnv<'_>,
    check_id: &str,
    between: &mut dyn FnMut(),
    sink: &mut dyn FnMut(QuickFixItem) -> ControlFlow<()>,
) -> Result<(), HttpError> {
    if env.checks.registry.get(check_id).is_none() {
        return Err(err(404, format!("Unknown check_id: {}", py_repr(check_id))));
    }
    let ids: Vec<i64> = findings::list_findings(
        env.conn,
        &FindingFilter {
            check_id: Some(check_id),
            album_id: None,
            include_ignored: false,
            include_resolved: false,
            limit: None,
        },
    )
    .map_err(db_err)?
    .into_iter()
    .map(|f| f.id)
    .collect();
    let total = ids.len();
    let run = catch_unwind(AssertUnwindSafe(|| {
        musaic_events::context::with_bulk_run(|| quick_fix_loop(env, check_id, &ids, between, sink))
    }));
    if let Err(p) = run {
        let detail = format!("panic: {}", panic_text(p.as_ref()));
        let _ = catch_unwind(AssertUnwindSafe(|| {
            sink(QuickFixItem {
                event: "error",
                data: Json::obj([("detail", Json::from(detail))]),
            })
        }));
    }
    if total > 0 {
        env.bus.publish(&BusEvent::BulkBatchCompleted);
    }
    Ok(())
}

/// Which of the `applied` findings (id, album) are still open after their
/// fix reported success. A fix can succeed while writing only part of
/// what the finding asks for (tags-out-of-sync fills only the fields
/// MusicBrainz has), and not every check runs in the sweep that
/// `BulkBatchCompleted` triggers, so the check itself is rerun here, scoped to
/// the touched albums, and the findings are read back. A finding counts as
/// still open when its row is open or the rerun opened a new row for the same
/// album (its summary changed). Empty when there is nothing to verify, no
/// library is configured or the rerun did not complete; those are counted as
/// the fix reported them.
fn still_open_after_fix(
    env: &mut FixEnv<'_>,
    check_id: &str,
    applied: &[(i64, Option<String>)],
) -> Vec<i64> {
    let mut albums: Vec<String> = applied.iter().filter_map(|(_, a)| a.clone()).collect();
    albums.sort();
    albums.dedup();
    let Some(lib) = env.resolved_library() else {
        return Vec::new();
    };
    if albums.is_empty() {
        return Vec::new();
    }
    let conn: &Connection = env.conn;
    let Ok(max_before) =
        conn.query_row("SELECT COALESCE(MAX(id), 0) FROM check_findings", [], |r| {
            r.get::<_, i64>(0)
        })
    else {
        return Vec::new();
    };
    match musaic_checks::run_check(
        conn,
        env.checks,
        check_id,
        &lib,
        musaic_checks::ScopeKind::Albums,
        Some(&albums),
    ) {
        Ok(run) if run.status == "completed" => {}
        _ => return Vec::new(),
    }
    applied
        .iter()
        .filter(|(fid, album)| {
            let row_open = findings::get_finding(conn, *fid)
                .ok()
                .flatten()
                .is_some_and(|f| f.resolved_at.is_none());
            let reopened = album.as_deref().is_some_and(|a| {
                conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM check_findings WHERE check_id = ?1 \
                     AND album_id = ?2 AND id > ?3 AND resolved_at IS NULL)",
                    rusqlite::params![check_id, a, max_before],
                    |r| r.get::<_, bool>(0),
                )
                .unwrap_or(false)
            });
            row_open || reopened
        })
        .map(|(fid, _)| *fid)
        .collect()
}

/// The body of [`quick_fix_all`] between the bulk-run flag's set and reset.
fn quick_fix_loop(
    env: &mut FixEnv<'_>,
    check_id: &str,
    ids: &[i64],
    between: &mut dyn FnMut(),
    sink: &mut dyn FnMut(QuickFixItem) -> ControlFlow<()>,
) {
    let total = ids.len();
    {
        if sink(QuickFixItem {
            event: "start",
            data: Json::obj([("total", Json::from(total))]),
        })
        .is_break()
        {
            return;
        }
        let (mut fixed, mut skipped, mut errors, mut na) = (0usize, 0usize, 0usize, 0usize);
        let mut applied: Vec<(i64, Option<String>)> = Vec::new();
        for (i, fid) in ids.iter().enumerate() {
            if i > 0 {
                between();
            }
            let album = findings::get_finding(env.conn, *fid)
                .ok()
                .flatten()
                .and_then(|f| f.album_id);
            let attempt = match missing_album_of(env.conn, album.as_deref()) {
                Some(since) => Err(since),
                None => Ok(catch_unwind(AssertUnwindSafe(|| {
                    try_quick_fix_finding(env, *fid)
                }))),
            };
            let (outcome, message) = match attempt {
                Err(since) => {
                    na += 1;
                    ("not-applicable", missing_album_not_applicable(&since))
                }
                Ok(Err(p)) => {
                    errors += 1;
                    ("error", format!("panic: {}", panic_text(p.as_ref())))
                }
                Ok(Ok(r)) => match r {
                    Err(e) => {
                        errors += 1;
                        ("error", e)
                    }
                    Ok(None) => {
                        na += 1;
                        ("not-applicable", "Check has no quick-fix path".to_string())
                    }
                    Ok(Some(r)) if r.ok => {
                        fixed += 1;
                        applied.push((*fid, album.clone()));
                        ("fixed", r.message)
                    }
                    Ok(Some(r)) => {
                        skipped += 1;
                        ("skipped", r.message)
                    }
                },
            };
            let item = QuickFixItem {
                event: "progress",
                data: Json::obj([
                    ("finding_id", Json::Int(*fid)),
                    ("outcome", Json::from(outcome)),
                    ("message", Json::Str(message)),
                ]),
            };
            if sink(item).is_break() {
                return;
            }
        }
        // `fixed` counts only findings that are gone; a fix that reported
        // success but left its finding open is `still_open`.
        let still_open = still_open_after_fix(env, check_id, &applied).len();
        fixed -= still_open;
        let _ = sink(QuickFixItem {
            event: "done",
            data: Json::obj([
                ("total", Json::from(total)),
                ("fixed", Json::from(fixed)),
                ("skipped", Json::from(skipped)),
                ("errors", Json::from(errors)),
                ("not_applicable", Json::from(na)),
                ("still_open", Json::from(still_open)),
            ]),
        });
    }
}

// ---------- filename-mismatch apply ----------

/// `execute_renames`' dict, as the route returns it.
pub fn rename_outcome_json(o: &RenameOutcome) -> Json {
    let mut pairs: Vec<(String, Json)> = vec![
        ("ok".into(), Json::Bool(o.ok)),
        ("message".into(), Json::from(o.message.as_str())),
        ("renamed".into(), Json::from(o.renamed)),
    ];
    if o.already_renamed {
        pairs.push(("already_renamed".into(), Json::Bool(true)));
    }
    // Only when there is something to say, so the body stays the Python
    // app's otherwise.
    if !o.skipped.is_empty() {
        pairs.push((
            "skipped".into(),
            Json::Arr(
                o.skipped
                    .iter()
                    .map(|e| {
                        Json::obj([
                            ("old", Json::from(e.old.as_str())),
                            ("new", Json::from(e.new.as_str())),
                            ("reason", Json::from(e.reason.as_str())),
                        ])
                    })
                    .collect(),
            ),
        ));
    }
    // Only when the rename needs a note, so a clean result keeps its shape:
    // a stable code and the sentence to show the user (`message` is the
    // technical text).
    if let (Some(r), Some(u)) = (o.reason, &o.user_message) {
        pairs.push(("reason_code".into(), Json::from(r.code())));
        pairs.push(("user_message".into(), Json::from(u.as_str())));
    }
    if let Some(d) = &o.detail {
        pairs.push((
            "errors".into(),
            Json::Arr(d.errors.iter().map(|e| Json::from(e.as_str())).collect()),
        ));
        pairs.push(("dir_renamed".into(), Json::Bool(d.dir_renamed)));
        pairs.push(("new_album_id".into(), Json::from(d.new_album_id.as_str())));
        pairs.push((
            "dir_migration".into(),
            match d.dir_migration {
                Some(m) => Json::obj([
                    ("albums", Json::from(m.albums)),
                    ("track_fingerprints", Json::from(m.track_fingerprints)),
                    ("track_tags", Json::from(m.track_tags)),
                    ("check_findings", Json::from(m.check_findings)),
                    ("ai_jobs", Json::from(m.ai_jobs)),
                    (
                        "album_blocked_mb_release_groups",
                        Json::from(m.album_blocked_mb_release_groups),
                    ),
                ]),
                None => Json::Obj(Vec::new()),
            },
        ));
    }
    Json::Obj(pairs)
}

/// `POST /api/checks/filename-mismatch/findings/{finding_id}/apply`: 404 for
/// an unknown finding, 400 without an album or with neither a plan nor a
/// dir rename, 409 when nothing happened and the call failed (its message);
/// else the result. Publishes the `AlbumRenamed` the rename produced.
/// Deliberate change: a Missing album is refused (409, the apply
/// pipeline's text) before anything is touched.
pub fn apply_filename_rename(
    env: &mut FixEnv<'_>,
    finding_id: i64,
    plan: &[PlanEntry],
    dir_rename: Option<&PlanEntry>,
) -> Result<Json, RenameApplyError> {
    let f = finding_for(env.conn, finding_id, "filename-mismatch")?;
    let album_id = f
        .album_id
        .clone()
        .filter(|a| !a.is_empty())
        .ok_or_else(|| err(400, "Finding has no album"))?;
    if plan.is_empty() && dir_rename.is_none() {
        return Err(err(400, "plan or dir_rename is required").into());
    }
    crate::apply::refuse_missing(env.conn, &album_id)?;
    let now = (env.now)();
    let renv = RenameEnv {
        library_dir: env.library_dir,
        now: &now,
        track_order: env.track_order,
    };
    let out = musaic_fsops::rename::execute_renames(env.conn, &renv, &album_id, plan, dir_rename);
    if let Some(ev) = &out.album_renamed {
        env.bus.publish(&BusEvent::AlbumRenamed {
            old_album_id: ev.old_album_id.clone(),
            new_album_id: ev.new_album_id.clone(),
        });
    }
    let dir_renamed = out.detail.as_ref().is_some_and(|d| d.dir_renamed);
    if !out.ok && out.renamed == 0 && !dir_renamed {
        let msg = if out.message.is_empty() {
            "Rename failed".to_string()
        } else {
            out.message.clone()
        };
        let mut e = RenameApplyError::from(err(409, msg));
        if let (Some(r), Some(u)) = (out.reason, &out.user_message) {
            e.extra = vec![
                ("reason_code".into(), r.code().into()),
                ("user_message".into(), u.clone()),
            ];
        }
        return Err(e);
    }
    Ok(rename_outcome_json(&out))
}

/// A refusal of the rename apply: the usual `detail` plus, for a refusal
/// the rename code explains, extra top-level body keys (`reason_code`,
/// `user_message`) that sit beside `detail` and leave it untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameApplyError {
    pub error: HttpError,
    pub extra: Vec<(String, String)>,
}

impl From<HttpError> for RenameApplyError {
    fn from(error: HttpError) -> Self {
        RenameApplyError {
            error,
            extra: Vec::new(),
        }
    }
}

// ---------- delete-duplicate ----------

/// `POST /api/checks/duplicate-tracks/findings/{finding_id}/delete-file`.
///
/// Validation and the trash move are
/// `musaic_checks::checks::duplicate_tracks::trash_duplicate_file` (404 /
/// 400 / 409 for an unreachable album directory / 500), which also deletes the trashed file's
/// fingerprint row. Then rescan the album directory; no FLAC left -> the
/// album row is kept and marked Missing, `AlbumMarkedMissing` published
/// (nothing is published when it already was Missing); otherwise
/// the row is upserted under the configured library and
/// `AlbumTagsChanged(fields=["track_count"])` published. When the rescan
/// cannot run (no library configured, a database error) the fallback
/// `AlbumTagsChanged` is published instead.
pub fn delete_duplicate_file(
    env: &mut FixEnv<'_>,
    finding_id: i64,
    filename: &str,
) -> Result<Json, HttpError> {
    let finding = findings::get_finding(env.conn, finding_id).map_err(db_err)?;
    let library = env.resolved_library().unwrap_or_default();
    let stamp = env.trash_stamp;
    let trashed = duplicate_tracks::trash_duplicate_file(
        env.conn,
        &library,
        env.trash_root,
        finding.as_ref(),
        filename,
        stamp,
    )
    .map_err(|r| err(r.status, r.detail))?;
    let album_id = finding
        .and_then(|f| f.album_id)
        .expect("trash_duplicate_file checked the album");
    let album_path: Option<String> = env
        .conn
        .query_row("SELECT path FROM albums WHERE id = ?1", [&album_id], |r| {
            r.get(0)
        })
        .optional()
        .map_err(db_err)?;
    let tags_changed = || BusEvent::AlbumTagsChanged {
        album_id: album_id.clone(),
        fields: vec!["track_count".to_string()],
    };
    let rescanned: Result<(), String> = (|| {
        let path = PathBuf::from(album_path.clone().unwrap_or_default());
        match musaic_scan::scan_album(&path, env.track_order) {
            None => {
                // Keep the row (and everything hanging off it).
                let since = (env.now)();
                if musaic_db::albums::mark_album_missing(env.conn, &album_id, &since)
                    .map_err(|e| e.to_string())?
                {
                    env.bus.publish(&BusEvent::AlbumMarkedMissing {
                        album_id: album_id.clone(),
                        missing_since: since,
                    });
                }
            }
            Some(fresh) => {
                let lib = env
                    .resolved_library()
                    .ok_or("No library directory configured for post-delete rescan")?;
                musaic_db::albums::upsert_album(env.conn, &lib, &(env.now)(), &fresh)
                    .map_err(|e| e.to_string())?;
                env.bus.publish(&tags_changed());
            }
        }
        Ok(())
    })();
    if rescanned.is_err() {
        env.bus.publish(&tags_changed());
    }
    Ok(Json::obj([
        ("ok", Json::Bool(true)),
        ("message", Json::Str(trashed.message)),
        (
            "trashed_to",
            trashed
                .trashed_to
                .map_or(Json::Null, |p| Json::Str(p.to_string_lossy().into_owned())),
        ),
    ]))
}
