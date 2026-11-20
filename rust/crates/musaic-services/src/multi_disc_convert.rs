//! Converting a multi-disc set kept in one folder to the layout Musaic aims
//! for (`POST /api/checks/multi-disc-layout/findings/{id}/convert`): tracks
//! numbered straight through the discs, file names by the filename
//! template with no disc part, no DISCNUMBER / DISCTOTAL / TOTALDISCS tags,
//! the folder named by the directory template with no disc marker.
//!
//! This module makes every check and holds what keeps other work away for
//! both shapes of set; the steps below are those of a set kept in one
//! folder. A set kept in one folder per disc is gathered by
//! [`crate::multi_disc_gather`].
//!
//! # What it carries out
//!
//! Exactly the plan the preview shows, re-derived from the disk and the
//! database at this moment (rule 2; nothing the client sends is used but
//! the preview's digest, which must match the fresh plan's). An incomplete
//! set and any plan with a blocker are refused before anything is
//! written.
//!
//! # Order
//!
//! 1. The record ([`musaic_fsops::conversion_log`]) is written and flushed
//!    to disk with the whole plan before any file changes.
//! 2. File renames, all in one batch of the rename code
//!    ([`musaic_fsops::rename::execute_renames`]): every target is checked
//!    first (a name taken by a file outside the batch refuses the batch,
//!    case- and normalisation-insensitively), every file moves through a
//!    temporary name created without replacing anything, so a disc 1 file
//!    and a disc 2 file swapping names never meet, and nothing is ever
//!    overwritten. The rows (fingerprints, rejects, track tags) and the
//!    track-tag sidecar follow under the name the folder lists.
//! 3. The tag writes, through the apply pipeline
//!    ([`crate::apply::apply_disc_conversion`]): per file, in one save,
//!    TRACKNUMBER = the new number, TRACKTOTAL and TOTALTRACKS = the track
//!    count, each only where the file already has that key (a file with no
//!    track total gets none; a `5/12` TRACKNUMBER keeps that form),
//!    DISCNUMBER, DISCTOTAL and TOTALDISCS removed. The values written are
//!    the record's (`conversion_log::converted_numbering`). Each file is
//!    read back afterwards and must carry exactly the recorded new values.
//! 4. The folder rename, if its name changes, through the same rename code,
//!    which re-keys the album (`AlbumRenamed`).
//!
//! Renames come before the tags so that an interruption after them leaves
//! a set whose tags still say everything: the disk alone re-derives the
//! same plan. A step that fails stops the conversion; the steps before it
//! stay done.
//!
//! # Interruption and resume
//!
//! Every step leaves a valid library. Running Convert again re-derives the
//! plan: a file already written has no disc tag left, so the preview takes
//! its old disc and number from the record (`multi_disc::derive_resumed`),
//! checks that every file is in its old or its new state, and continues
//! with what is left. A folder that no longer matches the record is
//! refused. The record lists each attempt and each step's outcome.
//!
//! # Events and the gate
//!
//! The host holds the work gate (no restore meanwhile). Scans are kept
//! away for the whole conversion ([`ScanService::hold_layout`]): refused
//! while one runs, and one that starts meanwhile waits. The steps publish
//! their own events as they happen (`AlbumRenamed` for the file renames and
//! for the folder, `AlbumTagsChanged` from the pipeline) under
//! `in_bulk_run`, then one `BulkBatchCompleted` for the one rerun that
//! resolves the finding.

use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

use musaic_core::album_id as album_id_of;
use musaic_core::rename_reason::{Reason, couldnt_rename, quoted};
use musaic_db::findings::get_finding;
use musaic_events::{Bus, Event as BusEvent};
use musaic_fsops::conversion_log::{
    Attempt, LoggedFile, NumberingTags, Record, converted_numbering, numbering_of,
};
use musaic_fsops::multi_disc::{self, ConversionPreview, PreviewState};
use musaic_fsops::rename::{PlanEntry, RenameEnv, RenameOutcome, execute_renames};
use musaic_scan::{TrackOrder, py_resolve};
use rusqlite::Connection;
use serde_json::{Value, json};
use unicode_normalization::UnicodeNormalization;

use crate::apply::{ApplyEnv, NoCovers};
use crate::error::{Blocker, Details, OpError, Refusal as ErrRefusal, Subject};
use crate::scan::ScanService;

/// `<config dir>/conversions`, where the records are kept.
pub use musaic_fsops::conversion_log::log_dir;
/// `<config dir>/trash`, where a conversion puts what it no longer needs.
pub use musaic_fsops::trash::trash_dir;

/// The albums and folders of every unfinished conversion recorded in
/// `logs`: what a scan leaves as it is ([`crate::scan::HeldAlbums`]).
pub fn held_albums(logs: &Path) -> crate::scan::HeldAlbums {
    let mut held = crate::scan::HeldAlbums::default();
    for r in musaic_fsops::conversion_log::unfinished(logs) {
        held.ids.extend(r.album_ids());
        held.paths.extend(r.folder_paths());
    }
    held
}

/// The check whose findings this converts.
pub const CHECK_ID: &str = "multi-disc-layout";

/// Where a test stops the conversion as a crash would: nothing after that
/// point runs, and the record keeps what it had (no outcome for the step).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopAt {
    /// After renaming only the first `n` files that need it.
    RenamedFiles(usize),
    /// After writing the tags of only the first `n` files that need it.
    TaggedFiles(usize),
    /// After the tags, before the folder rename.
    BeforeFolderRename,
    /// A set in several folders: right after the record is written.
    AfterRecord,
    /// A set in several folders: after moving `n` files into the
    /// destination; `rows` says whether the last one's database rows moved
    /// too (a crash between a file's move and its rows' commit is `false`).
    MovedFiles { n: usize, rows: bool },
    /// A set in several folders: after moving `n` of the other files (a
    /// rip log, a cue sheet) of the emptied folders into the destination.
    MovedStrays(usize),
    /// A set in several folders: after the covers and the clutter.
    AfterCovers,
    /// A set in several folders: after the destination's sidecar is
    /// written, before the old ones go.
    AfterSidecar,
    /// A set in several folders: after the fold of the rows.
    AfterFold,
    /// A set in several folders: after removing `n` emptied folders.
    RemovedFolders(usize),
    /// A set in several folders: after the folder rename, before the last
    /// rescan and the record's close.
    AfterFolderRename,
}

/// What the conversion needs from its host.
pub struct ConvertEnv<'a> {
    pub conn: &'a mut Connection,
    pub bus: &'a Bus,
    /// `config.library_dir` as stored.
    pub library_dir: Option<&'a str>,
    /// ISO time for the record and the rows.
    pub now: &'a dyn Fn() -> String,
    pub track_order: TrackOrder,
    /// `<config dir>/conversions`.
    pub logs: &'a Path,
    pub scan: &'a ScanService,
    /// Test seam ([`StopAt`]); `None` in the app.
    pub stop_at: Option<StopAt>,
    /// `<config dir>/trash`: where the covers and the clutter a set in
    /// several folders no longer needs go.
    pub trash: &'a Path,
    /// Paused for the whole conversion: a pass would write fingerprint rows
    /// under albums that the conversion re-keys or folds away.
    pub pool: Option<&'a crate::fingerprint_pool::FingerprintPool>,
    /// Suspended for the whole conversion, for the same reason (findings
    /// and inputs under a member id).
    pub sweeper: Option<&'a crate::check_sweep::CheckSweeper>,
    /// Told what a long conversion is doing: what an operation's sink is fed
    /// from.
    pub on_progress: Option<&'a dyn Fn(ConvertProgress)>,
}

/// What a long conversion is doing: the step it is in and how far along.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvertProgress {
    /// `rename_files` | `write_tags` | `move_files`.
    pub step: String,
    pub done: usize,
    pub total: usize,
}

/// The numbers to write to the file the record calls `new_name`: exactly
/// the record's new values, so a file is given a track total only where
/// the record says it had one. (A file the record does not know gets its
/// number and nothing else; the read-back then refuses it.)
pub(crate) fn recorded_numbers(
    record: &Record,
    new_name: &str,
    new_track: i64,
) -> musaic_tags::ConvertedNumbers {
    use musaic_fsops::conversion_log::values_of;
    let tags = record
        .files
        .iter()
        .find(|e| nfc(&e.new_name) == nfc(new_name))
        .map(|e| &e.new_tags);
    let first = |key: &str| tags.and_then(|t| values_of(t, key).first().cloned());
    musaic_tags::ConvertedNumbers {
        tracknumber: first("TRACKNUMBER").unwrap_or_else(|| new_track.to_string()),
        tracktotal: first("TRACKTOTAL"),
        totaltracks: first("TOTALTRACKS"),
    }
}

/// Tell the host what the conversion is doing.
pub(crate) fn report(env: &ConvertEnv<'_>, step: &str, done: usize, total: usize) {
    if let Some(p) = env.on_progress {
        p(ConvertProgress {
            step: step.to_string(),
            done,
            total,
        });
    }
}

/// A conversion refused before anything was written (or a request that is
/// wrong): the status, the `detail`, and more body keys (`reason_code`,
/// `user_message`, `blockers`, `digest`).
#[derive(Debug, Clone, PartialEq)]
pub struct Refusal {
    pub status: u16,
    pub detail: String,
    pub extra: serde_json::Map<String, Value>,
}

impl Refusal {
    pub(crate) fn new(status: u16, code: &str, detail: impl Into<String>) -> Refusal {
        let detail = detail.into();
        let mut extra = serde_json::Map::new();
        extra.insert("reason_code".into(), json!(code));
        extra.insert("user_message".into(), json!(detail));
        Refusal {
            status,
            detail,
            extra,
        }
    }

    pub(crate) fn with(mut self, k: &str, v: Value) -> Refusal {
        self.extra.insert(k.to_string(), v);
        self
    }

    /// The body: `detail` and the extra keys.
    pub fn body(&self) -> Value {
        let mut m = self.extra.clone();
        m.insert("detail".into(), json!(self.detail));
        Value::Object(m)
    }

    /// The refusal as the operation's error: the status picks the variant, the
    /// code, sentence, blockers and fresh digest travel as typed details.
    /// `finding_id` is the finding the request named (a 404's subject).
    pub fn into_op_error(self, finding_id: i64) -> OpError {
        let text = |k: &str| {
            self.extra
                .get(k)
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        let reason_code = text("reason_code").unwrap_or_default();
        let blockers = self
            .extra
            .get("blockers")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|b| Blocker {
                        code: b["code"].as_str().unwrap_or_default().to_string(),
                        message: b["message"].as_str().unwrap_or_default().to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let details = Some(Details::Refusal(Box::new(ErrRefusal {
            user_message: text("user_message").unwrap_or_else(|| self.detail.clone()),
            digest: text("digest"),
            reason_code: reason_code.clone(),
            blockers,
        })));
        let message = self.detail;
        match self.status {
            400 => OpError::Rejected { message, details },
            404 => OpError::NotFound {
                what: if reason_code == Reason::AlbumNotFound.code() {
                    Subject::Album
                } else {
                    Subject::Finding
                },
                id: finding_id.to_string(),
                message,
            },
            409 => OpError::Conflict { message, details },
            _ => OpError::Failed { message, details },
        }
    }
}

pub(crate) fn db(e: impl std::fmt::Display) -> Refusal {
    Refusal::new(500, "other", format!("Database error: {e}"))
}

/// The plan's blockers as the preview lists them.
pub fn blockers_json(p: &ConversionPreview) -> Value {
    Value::Array(
        p.plan
            .blockers
            .iter()
            .map(|b| json!({"code": b.code, "message": b.message}))
            .collect(),
    )
}

/// Convert the set of multi-disc-layout finding `finding_id`, if the plan
/// re-derived now has the digest `digest` and nothing stops it. `Ok` is the
/// result (also when a step failed: `ok: false` with per-file reasons);
/// `Err` a refusal that changed nothing.
pub fn convert(env: &mut ConvertEnv<'_>, finding_id: i64, digest: &str) -> Result<Value, Refusal> {
    let finding = match get_finding(env.conn, finding_id).map_err(db)? {
        Some(f) if f.check_id == CHECK_ID => f,
        _ => return Err(Refusal::new(404, "not_found", "Finding not found")),
    };
    let Some(album_id) = finding.album_id.clone().filter(|a| !a.is_empty()) else {
        return Err(Refusal::new(400, "no_album", "Finding has no album"));
    };
    if env.library_dir.is_none_or(str::is_empty) {
        return Err(Refusal::new(
            409,
            Reason::NoLibrary.code(),
            "No music folder is set up, so nothing was converted. Choose a music folder in Settings, then try again.",
        ));
    }
    if let Some(since) = musaic_db::albums::missing_since(env.conn, &album_id)
        .map_err(db)?
        .filter(|s| !s.is_empty())
    {
        return Err(Refusal::new(
            409,
            Reason::AlbumMissing.code(),
            format!(
                "This album's folder is missing from disk (missing since {since}), so nothing was converted. Rescan the library once the folder is back."
            ),
        ));
    }
    // Held to the end: no scan reads the folder while it changes.
    let _layout = env.scan.hold_layout().ok_or_else(|| {
        Refusal::new(
            409,
            "scan_running",
            "A library scan is running, so nothing was converted. Try again when it finishes.",
        )
    })?;
    // No fingerprint pass and no background check batch writes rows under
    // an album id the conversion re-keys or folds away; both start again
    // when it ends.
    let _pool = env.pool.map(|p| p.pause());
    let _sweep = env.sweeper.map(|s| s.suspend());
    let Some(preview) =
        multi_disc::derive_conversion(env.conn, &album_id, Some(env.logs)).map_err(db)?
    else {
        return Err(Refusal::new(
            404,
            Reason::AlbumNotFound.code(),
            "This album is no longer in the library, so nothing was converted.",
        ));
    };
    match preview.state {
        PreviewState::OneFolder | PreviewState::Folders => {}
        PreviewState::Incomplete | PreviewState::NotASet => {
            let reason = preview
                .reason
                .clone()
                .unwrap_or_else(|| "This set cannot be converted.".into());
            return Err(Refusal::new(409, "not_convertible", reason));
        }
    }
    if !preview.plan.blockers.is_empty() {
        let n = preview.plan.blockers.len();
        return Err(Refusal::new(
            409,
            "blocked",
            format!(
                "{} the conversion, so nothing was changed: {}",
                if n == 1 {
                    "One thing stops".to_string()
                } else {
                    format!("{n} things stop")
                },
                preview.plan.blockers[0].message
            ),
        )
        .with("blockers", blockers_json(&preview))
        .with("digest", json!(preview.digest())));
    }
    // Without a blocker every file has a number and a name; a plan that
    // somehow lacks one is not carried out half.
    if preview.plan.tracks.is_empty()
        || preview.plan.tracks.iter().any(|t| {
            t.new_track.is_none()
                || t.disc.is_none()
                || t.track.is_none()
                || t.new_filename.as_deref().is_none_or(str::is_empty)
        })
    {
        return Err(Refusal::new(
            409,
            "blocked",
            "The plan does not give every file a new number and name, so nothing was changed.",
        ));
    }
    let fresh = preview.digest();
    if fresh != digest {
        return Err(Refusal::new(
            409,
            "plan_changed",
            "The folder or the settings changed since the preview was made, so nothing was changed. Look at the new preview, then convert again.",
        )
        .with("digest", json!(fresh)));
    }
    let Some(dir) = preview.target_path.clone().map(PathBuf::from) else {
        return Err(Refusal::new(
            404,
            Reason::AlbumNotFound.code(),
            "This album is no longer in the library, so nothing was converted.",
        ));
    };
    if preview.state == PreviewState::Folders {
        return crate::multi_disc_gather::convert_folders(env, &preview, &dir);
    }

    // ---- the record, before anything changes ----
    let now = (env.now)();
    let mut record = match preview.resume.clone() {
        Some(r) => r,
        None => new_record(env, &preview, &dir, &now),
    };
    record.attempts.push(Attempt {
        started_at: now.clone(),
        finished_at: None,
        outcome: None,
        steps: Vec::new(),
    });
    if let Err(e) = record.save() {
        return Err(Refusal::new(
            500,
            Reason::from_io(&e).code(),
            format!(
                "Musaic couldn't write the record of the conversion ({e}), so nothing was changed."
            ),
        ));
    }

    let bus = env.bus;
    let out = musaic_events::context::with_bulk_run(|| run_steps(env, &preview, &dir, &mut record));
    if out.interrupted {
        // As a crash would: nothing more is written.
        return Ok(out.json(&record));
    }
    let end = (env.now)();
    {
        let a = record.attempts.last_mut().expect("pushed above");
        a.finished_at = Some(end.clone());
        a.outcome = Some(if out.ok { "completed" } else { "failed" }.into());
    }
    if out.ok {
        record.finished_at = Some(end);
        record.album_id_after = Some(out.album_id.clone());
    }
    if let Err(e) = record.save() {
        musaic_log::error(
            TARGET,
            &format!(
                "could not update the conversion record {}: {e}",
                record.path.display()
            ),
        );
    }
    bus.publish(&BusEvent::BulkBatchCompleted);
    Ok(out.json(&record))
}

/// The logger name of conversions.
pub const TARGET: &str = "musaic.services.multi_disc_convert";

fn new_record(env: &ConvertEnv<'_>, p: &ConversionPreview, dir: &Path, now: &str) -> Record {
    let total = p.plan.track_total;
    let folder_name_before = p.target_current.clone().unwrap_or_default();
    let folder_name_after = p
        .plan
        .target_name
        .clone()
        .unwrap_or_else(|| folder_name_before.clone());
    let expected = if folder_name_after == folder_name_before {
        p.album_id.clone()
    } else {
        let new_path = dir.with_file_name(&folder_name_after);
        album_id_of(&py_resolve(&new_path).to_string_lossy())
    };
    Record {
        path: env.logs.join(Record::file_name(&p.album_id, now)),
        kind: musaic_fsops::conversion_log::KIND_ONE_FOLDER.into(),
        started_at: now.to_string(),
        finished_at: None,
        library_path: p.library_path.clone(),
        album_id_before: p.album_id.clone(),
        album_id_expected: expected,
        album_id_after: None,
        folder_before: dir.to_string_lossy().into_owned(),
        folder_name_before,
        folder_name_after,
        template: p.template.clone(),
        dir_template: p.dir_template.clone(),
        track_total: total,
        files: p
            .plan
            .tracks
            .iter()
            .map(|t| LoggedFile {
                album_id: String::new(),
                disc: t.disc.unwrap_or_default(),
                track: t.track.unwrap_or_default(),
                new_track: t.new_track.unwrap_or_default(),
                old_name: t.filename.clone(),
                new_name: t.new_filename.clone().unwrap_or_default(),
                new_tags: converted_numbering(&t.numbering, t.new_track.unwrap_or_default(), total),
                old_tags: t.numbering.clone(),
            })
            .collect(),
        attempts: Vec::new(),
        members: Vec::new(),
        folders: Vec::new(),
        orphan_rows: Vec::new(),
        abandoned_at: None,
    }
}

/// One file's outcome.
#[derive(Debug, Clone, Default)]
struct FileOutcome {
    filename: String,
    new_filename: String,
    /// `renamed` | `already` | `failed` | `not_reached`.
    rename: &'static str,
    /// `written` | `already` | `failed` | `not_reached`.
    tags: &'static str,
    reason: Option<Reason>,
    user_message: Option<String>,
}

struct Outcome {
    ok: bool,
    interrupted: bool,
    album_id: String,
    files: Vec<FileOutcome>,
    folder_old: String,
    folder_new: String,
    /// `renamed` | `unchanged` | `failed` | `not_reached`.
    folder: &'static str,
    folder_reason: Option<Reason>,
    folder_message: Option<String>,
    user_message: String,
}

impl Outcome {
    fn json(&self, record: &Record) -> Value {
        let files: Vec<Value> = self
            .files
            .iter()
            .map(|f| {
                let mut m = json!({
                    "filename": f.filename,
                    "new_filename": f.new_filename,
                    "rename": f.rename,
                    "tags": f.tags,
                });
                if let (Some(r), Some(u)) = (f.reason, &f.user_message) {
                    m["reason_code"] = json!(r.code());
                    m["user_message"] = json!(u);
                }
                m
            })
            .collect();
        let mut folder = json!({
            "old": self.folder_old,
            "new": self.folder_new,
            "outcome": self.folder,
        });
        if let (Some(r), Some(u)) = (self.folder_reason, &self.folder_message) {
            folder["reason_code"] = json!(r.code());
            folder["user_message"] = json!(u);
        }
        json!({
            "ok": self.ok,
            "interrupted": self.interrupted,
            "album_id": self.album_id,
            "user_message": self.user_message,
            "files": files,
            "folder": folder,
            "record": record.path.to_string_lossy(),
            "attempts": record.attempts.len(),
        })
    }
}

pub(crate) fn nfc(s: &str) -> String {
    s.nfc().collect()
}

/// The name `dir` lists for `name` (equal after NFC normalisation: a
/// volume that stores names decomposed lists the NFD form).
pub(crate) fn listed_as(dir: &Path, name: &str) -> Option<String> {
    let want = nfc(name);
    musaic_scan::dir_names(dir).ok()?.into_iter().find_map(|n| {
        let n = n.to_string_lossy().into_owned();
        (n == name || nfc(&n) == want).then_some(n)
    })
}

/// Why `path` cannot be written, as far as opening it for writing says
/// (nothing is written).
pub(crate) fn write_probe(path: &Path) -> Reason {
    match std::fs::OpenOptions::new().write(true).open(path) {
        Ok(_) => Reason::Other,
        Err(e) => Reason::from_io(&e),
    }
}

pub(crate) fn step(
    record: &mut Record,
    name: &str,
    ok: bool,
    message: &str,
    results: Vec<Value>,
    at: String,
) {
    if let Some(a) = record.attempts.last_mut() {
        a.steps.push(json!({
            "step": name,
            "at": at,
            "ok": ok,
            "message": message,
            "results": results,
        }));
    }
    if let Err(e) = record.save() {
        musaic_log::error(
            TARGET,
            &format!(
                "could not update the conversion record {}: {e}",
                record.path.display()
            ),
        );
    }
}

fn run_steps(
    env: &mut ConvertEnv<'_>,
    p: &ConversionPreview,
    dir: &Path,
    record: &mut Record,
) -> Outcome {
    let mut album_id = p.album_id.clone();
    let folder_old = p.target_current.clone().unwrap_or_default();
    let folder_new = p
        .plan
        .target_name
        .clone()
        .unwrap_or_else(|| folder_old.clone());
    let mut out = Outcome {
        ok: false,
        interrupted: false,
        album_id: album_id.clone(),
        files: p
            .plan
            .tracks
            .iter()
            .map(|t| FileOutcome {
                filename: t.filename.clone(),
                new_filename: t.new_filename.clone().unwrap_or_default(),
                rename: "not_reached",
                tags: "not_reached",
                ..Default::default()
            })
            .collect(),
        folder_old: folder_old.clone(),
        folder_new: folder_new.clone(),
        folder: "not_reached",
        folder_reason: None,
        folder_message: None,
        user_message: String::new(),
    };
    let now = (env.now)();
    let renv = RenameEnv {
        library_dir: env.library_dir,
        now: &now,
        track_order: env.track_order,
    };

    // ---- 1. file renames ----
    let mut entries: Vec<(usize, PlanEntry)> = Vec::new();
    for (i, t) in p.plan.tracks.iter().enumerate() {
        let new = t.new_filename.clone().unwrap_or_default();
        if nfc(&t.filename) == nfc(&new) {
            out.files[i].rename = "already";
        } else {
            entries.push((
                i,
                PlanEntry {
                    old: t.filename.clone(),
                    new,
                },
            ));
        }
    }
    let mut stop_after_renames = false;
    if let Some(StopAt::RenamedFiles(n)) = env.stop_at {
        entries.truncate(n);
        stop_after_renames = true;
    }
    if !entries.is_empty() {
        let plan: Vec<PlanEntry> = entries.iter().map(|(_, e)| e.clone()).collect();
        // Which file is which, by identity: in a chain (disc 2's `01` to
        // `03` while disc 1's `1-01` becomes `01`) a name says nothing.
        let before: Vec<Option<same_file::Handle>> = entries
            .iter()
            .map(|(_, e)| same_file::Handle::from_path(dir.join(&e.old)).ok())
            .collect();
        let r = execute_renames(env.conn, &renv, &album_id, &plan, None);
        if let Some(ev) = &r.album_renamed {
            env.bus.publish(&BusEvent::AlbumRenamed {
                old_album_id: ev.old_album_id.clone(),
                new_album_id: ev.new_album_id.clone(),
            });
        }
        let mut results = Vec::new();
        let mut failed = 0;
        for ((i, e), was) in entries.iter().zip(before) {
            let now = same_file::Handle::from_path(dir.join(&e.new)).ok();
            let done = was.is_some() && now == was && listed_as(dir, &e.new).is_some();
            let f = &mut out.files[*i];
            if done {
                f.rename = "renamed";
            } else {
                failed += 1;
                f.rename = "failed";
                let reason = r.reason.unwrap_or(Reason::Other);
                f.reason = Some(reason);
                // One sentence per file; the batch's own sentence goes in
                // the answer's `user_message` once.
                f.user_message = Some(format!(
                    "{} could not be renamed to {} because {}. {}",
                    quoted(&e.old),
                    quoted(&e.new),
                    reason.cause(),
                    reason.advice()
                ));
            }
            results.push(json!({"old": e.old, "new": e.new, "outcome": f.rename}));
        }
        if stop_after_renames {
            out.interrupted = true;
            return out;
        }
        step(
            record,
            "rename_files",
            failed == 0,
            &r.message,
            results,
            (env.now)(),
        );
        if failed > 0 {
            out.user_message = rename_failure_message(&r, failed);
            return out;
        }
    } else if stop_after_renames {
        out.interrupted = true;
        return out;
    }

    // ---- 2. tags ----
    let mut conversion = musaic_tags::DiscConversion::default();
    let mut tagged: Vec<(usize, String)> = Vec::new();
    for (i, t) in p.plan.tracks.iter().enumerate() {
        if t.tags_done {
            out.files[i].tags = "already";
            continue;
        }
        let want = t.new_filename.clone().unwrap_or_default();
        let Some(on_disk) = listed_as(dir, &want) else {
            out.files[i].tags = "failed";
            out.files[i].reason = Some(Reason::NotFound);
            out.files[i].user_message = Some(format!(
                "{} is not in the folder, so its tags were not written. {}",
                quoted(&want),
                Reason::NotFound.advice()
            ));
            continue;
        };
        tagged.push((i, on_disk.clone()));
        conversion.files.push((
            on_disk,
            recorded_numbers(record, &want, t.new_track.unwrap_or_default()),
        ));
    }
    let mut stop_after_tags = false;
    if let Some(StopAt::TaggedFiles(n)) = env.stop_at {
        conversion.files.truncate(n);
        tagged.truncate(n);
        stop_after_tags = true;
    }
    let lost = out.files.iter().filter(|f| f.tags == "failed").count();
    if !conversion.files.is_empty() {
        let mut apply_error: Option<String> = None;
        {
            let mut aenv = ApplyEnv {
                conn: env.conn,
                bus: env.bus,
                covers: &NoCovers,
                now: env.now,
                track_order: env.track_order,
            };
            crate::apply::apply_disc_conversion(
                &mut aenv,
                &album_id,
                &dir.to_string_lossy(),
                env.library_dir,
                &conversion,
                &mut |event| {
                    if let crate::apply::ApplyEvent::Error { detail } = event {
                        apply_error = Some(detail);
                    }
                    ControlFlow::Continue(())
                },
            );
        }
        let mut results = Vec::new();
        let mut failed = 0;
        for (i, name) in &tagged {
            let want = p.plan.tracks[*i].new_filename.as_deref().unwrap_or("");
            let expected = record
                .files
                .iter()
                .find(|e| nfc(&e.new_name) == nfc(want))
                .map(|e| e.new_tags.clone());
            let now_tags = read_numbering(&dir.join(name));
            let f = &mut out.files[*i];
            if now_tags.is_some() && now_tags == expected {
                f.tags = "written";
            } else {
                failed += 1;
                f.tags = "failed";
                let reason = write_probe(&dir.join(name));
                f.reason = Some(reason);
                f.user_message = Some(format!(
                    "The track number and disc tags of {} could not be written because {}. {}",
                    quoted(name),
                    reason.cause(),
                    reason.advice()
                ));
            }
            results.push(json!({"file": name, "outcome": f.tags}));
        }
        if stop_after_tags {
            out.interrupted = true;
            return out;
        }
        step(
            record,
            "write_tags",
            failed == 0 && apply_error.is_none() && lost == 0,
            apply_error.as_deref().unwrap_or(""),
            results,
            (env.now)(),
        );
        if failed > 0 || lost > 0 || apply_error.is_some() {
            let n = failed + lost;
            out.user_message = match apply_error {
                Some(e) if n == 0 => format!(
                    "The tags could not be written ({e}). Nothing else was changed after the file renames; convert again to finish."
                ),
                _ => format!(
                    "The tags of {} could not be written; the folder was not renamed. Fix what each file says, then convert again to finish: what is done stays done.",
                    if n == 1 {
                        "1 file".to_string()
                    } else {
                        format!("{n} files")
                    }
                ),
            };
            return out;
        }
    } else {
        if stop_after_tags {
            out.interrupted = true;
            return out;
        }
        if lost > 0 {
            out.user_message = "Some files are not in the folder, so their tags were not written. Rescan the library, then check again.".into();
            return out;
        }
    }
    if env.stop_at == Some(StopAt::BeforeFolderRename) {
        out.interrupted = true;
        return out;
    }

    // ---- 3. the folder ----
    if nfc(&folder_old) == nfc(&folder_new) {
        out.folder = "unchanged";
    } else {
        let r = execute_renames(
            env.conn,
            &renv,
            &album_id,
            &[],
            Some(&PlanEntry {
                old: folder_old.clone(),
                new: folder_new.clone(),
            }),
        );
        if let Some(ev) = &r.album_renamed {
            env.bus.publish(&BusEvent::AlbumRenamed {
                old_album_id: ev.old_album_id.clone(),
                new_album_id: ev.new_album_id.clone(),
            });
        }
        let renamed = r.detail.as_ref().is_some_and(|d| d.dir_renamed);
        if renamed {
            out.folder = "renamed";
            if let Some(d) = &r.detail {
                album_id = d.new_album_id.clone();
            }
        } else {
            out.folder = "failed";
            let reason = r.reason.unwrap_or(Reason::Other);
            out.folder_reason = Some(reason);
            out.folder_message = Some(
                r.user_message
                    .clone()
                    .unwrap_or_else(|| couldnt_rename("The album folder", reason)),
            );
        }
        step(
            record,
            "rename_folder",
            renamed,
            &r.message,
            vec![
                json!({"old": folder_old, "new": folder_new, "outcome": out.folder, "album_id": album_id}),
            ],
            (env.now)(),
        );
        if !renamed {
            out.user_message = format!(
                "The files were renumbered and renamed, but the folder was not renamed: {}",
                out.folder_message.clone().unwrap_or_default()
            );
            return out;
        }
    }
    out.album_id = album_id;
    out.ok = true;
    let renamed = out.files.iter().filter(|f| f.rename == "renamed").count();
    let written = out.files.iter().filter(|f| f.tags == "written").count();
    out.user_message = format!(
        "Converted: {} renumbered{}{}.",
        if written == 1 {
            "1 file".to_string()
        } else {
            format!("{written} files")
        },
        if renamed > 0 {
            format!(", {renamed} renamed")
        } else {
            String::new()
        },
        if out.folder == "renamed" {
            ", the folder renamed"
        } else {
            ""
        }
    );
    out
}

fn rename_failure_message(r: &RenameOutcome, failed: usize) -> String {
    let base = r.user_message.clone().unwrap_or_default();
    format!(
        "{} not renamed, so no tag was written and the folder kept its name. {base}",
        if failed == 1 {
            "1 file was".to_string()
        } else {
            format!("{failed} files were")
        },
    )
}

/// The numbering tags of the FLAC at `path`; `None` when it cannot be read.
pub(crate) fn read_numbering(path: &Path) -> Option<NumberingTags> {
    let meta = musaic_scan::flacmeta::read(path).ok()?;
    Some(numbering_of(|k| meta.values(k).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listed_as_matches_normalisation_only() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("Cafe\u{301}.flac"), b"x").unwrap();
        assert!(listed_as(tmp.path(), "Caf\u{e9}.flac").is_some());
        assert!(listed_as(tmp.path(), "cafe.flac").is_none());
    }

    #[test]
    fn refusal_body_keeps_detail_beside_the_code() {
        let r = Refusal::new(409, "plan_changed", "x").with("digest", json!("d"));
        assert_eq!(
            r.body(),
            json!({"detail": "x", "reason_code": "plan_changed", "user_message": "x", "digest": "d"})
        );
    }

    #[test]
    fn conversion_log_dir_is_under_the_config_dir() {
        assert_eq!(
            musaic_fsops::conversion_log::log_dir(Path::new("/cfg")),
            Path::new("/cfg/conversions")
        );
    }
}
