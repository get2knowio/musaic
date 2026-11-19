//! Converting a multi-disc set kept in several folders (one per disc) to
//! the layout Musaic aims for: one folder, tracks numbered straight
//! through the discs, file names by the filename template with no disc
//! part, no disc tags, the folder named by the directory template with no
//! disc marker. Disc 1's folder and album row survive; the other discs'
//! files move into it and their rows are folded into its row.
//!
//! [`crate::multi_disc_convert::convert`] makes every check before this
//! runs (the plan re-derived from disk and database now, no blocker, the
//! preview's digest) and holds the scan hold, the paused fingerprint pool
//! and the suspended check sweep for the whole run. Everything below runs
//! under `in_bulk_run` and publishes its events as it commits.
//!
//! # Steps, and what holds after each
//!
//! 1. **The record** (`kind: "folders"`), flushed to disk before anything
//!    changes: every member, every file (member, old name and tags, new
//!    number, name and tags), each folder's covers, clutter, sidecar and
//!    other files with what happens to them (where each goes and under
//!    which name), the rows of files no longer on disk, the folder names
//!    and the ids before and after. *Disk and database unchanged.*
//! 2. **Sidecar tags into the database**: each member folder's track-tag
//!    sidecar is added to its rows (`INSERT OR IGNORE`, for the files the
//!    folder holds), so nothing the sidecar has and the database lacks is
//!    lost when the old sidecars go. *Disk unchanged; the rows hold the
//!    union.*
//! 3. **Renames in place**: in each member folder, each file gets its final
//!    name through the rename code (one batch per folder, temporary names,
//!    no-clobber, rows and sidecar follow, the folder rescanned). Disc 2's
//!    `05 - X.flac` is `17 - X.flac` in its own folder, so the names of all
//!    discs are disjoint by construction. *N valid albums.*
//! 4. **Tags in place**: per member, through the apply pipeline, one save
//!    per file: TRACKNUMBER the new number, TRACKTOTAL and TOTALTRACKS the
//!    set's count where the file already has that key (a file with no track
//!    total gets none), the disc tags removed; read back. *N valid albums, each numbered
//!    straight on from the discs before it, none with disc tags.* Tags come
//!    before the move so that the destination never holds a file that
//!    still says it is on another disc (a scan would read a half-moved
//!    folder as a set in one folder), and so that a moved file has one
//!    state only: new name, new tags.
//! 5. **Moves**: file by file, a no-clobber rename into the destination
//!    (same volume: a rename, never a copy), the name read back as the
//!    destination lists it, then that file's rows (`move_track_rows`) in
//!    their own transaction. A name that appears in the destination
//!    meanwhile stops the run. *Every file is in exactly one folder under
//!    one name and its rows are under that folder's album, except for at
//!    most the one file between its move and its rows' commit, which the
//!    next run repairs.* Then the destination is rescanned.
//! 6. **Covers, clutter and other files** of the other folders: the covers
//!    the destination takes (it had none) move in; copies of a kept cover
//!    and other pictures go to the app trash, as does the clutter
//!    (`.DS_Store`, AppleDouble files). Every other file (a rip log, a cue
//!    sheet, a booklet) moves into the destination with a no-clobber
//!    rename, under its own name or, where a different file has that name
//!    there, under the name the plan gave it (`rip (disc 2).log`); a copy
//!    of a file the destination already has goes to the app trash. Each is
//!    in exactly one folder at any moment, so the next run sees which are
//!    done. *Nothing is deleted.*
//! 7. **Sidecars**: the destination's is written from the database (every
//!    member's rows are under it now); the old ones then go to the trash.
//! 8. **Rows of files no longer on disk** move to the destination under
//!    their own name (the preview blocked a name a converted file gets).
//! 9. **The fold** (`merge_album_rows`, one transaction) with every other
//!    member as emptied (proven: their folders list no FLAC and every file
//!    of theirs is in the destination). `AlbumRenamed(member → survivor)`
//!    for each. *One album row with everything; the others gone.*
//! 10. **Emptied folders** (nothing left at all) are removed with a
//!     non-recursive remove. Only a folder that still holds something
//!     stays: a subfolder (never moved), or a file that appeared since the
//!     plan was made.
//! 11. **The folder rename** through the rename code, which re-keys the
//!     survivor (`AlbumRenamed(old → new)`).
//! 12. **The last rescan** of the survivor (`AlbumTagsChanged`), the record
//!     closed, one `BulkBatchCompleted`.
//!
//! # Interruption
//!
//! A crash anywhere leaves a library every file of which is a readable
//! FLAC under one name. While the record is unfinished, scans leave every
//! album and folder it names as they are ([`crate::scan::HeldAlbums`]), so
//! no scan marks a half-emptied disc Missing or adds the renamed folder as
//! a new album. Running Convert again re-derives where each file is from
//! the record and the disk (`multi_disc::derive_conversion`) and carries on
//! from there; every step above only does what is still left.

use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

use musaic_core::album_id as album_id_of;
use musaic_core::rename_reason::{Reason, couldnt_rename, quoted};
use musaic_db::merge::{MergeRequest, merge_album_rows, move_track_rows};
use musaic_events::Event as BusEvent;
use musaic_fsops::conversion_log::{
    Attempt, KIND_FOLDERS, LoggedFile, LoggedFolder, LoggedMember, LoggedStray, Record,
    converted_numbering,
};
use musaic_fsops::multi_disc::{ConversionPreview, CoverAction, Gather, StrayAction, TrackState};
use musaic_fsops::rename::{PlanEntry, RenameEnv, execute_renames};
use musaic_fsops::sidecar::SIDECAR_FILENAME;
use musaic_scan::py_resolve;
use serde_json::{Value, json};

use crate::apply::{ApplyEnv, DISC_CONVERSION_FIELDS, NoCovers};
use crate::multi_disc_convert::{
    ConvertEnv, Refusal, StopAt, TARGET, listed_as, nfc, read_numbering, recorded_numbers, report,
    step, write_probe,
};

fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

fn name_of(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// One file's outcome.
#[derive(Debug, Clone, Default)]
struct FileOut {
    filename: String,
    new_filename: String,
    /// The folder it was in before the conversion.
    folder: String,
    /// `renamed` | `already` | `failed` | `not_reached`.
    rename: &'static str,
    /// `written` | `already` | `failed` | `not_reached`.
    tags: &'static str,
    /// `moved` | `already` | `stays` (disc 1's own) | `failed` | `not_reached`.
    moved: &'static str,
    reason: Option<Reason>,
    user_message: Option<String>,
}

/// One member folder's outcome.
#[derive(Debug, Clone, Default)]
struct FolderOut {
    dir_name: String,
    destination: bool,
    /// `{name, action, outcome}`.
    covers: Vec<Value>,
    junk: Vec<Value>,
    /// `{name, action, new_name, outcome}`: the other files and where each
    /// went.
    strays: Vec<Value>,
    /// `merged` | `none` | `not_reached` | `failed`.
    sidecar: &'static str,
    /// What is left in the folder when it stays.
    left: Vec<String>,
    /// `destination` | `removed` | `kept` | `not_reached`.
    outcome: &'static str,
}

struct Out {
    ok: bool,
    interrupted: bool,
    album_id: String,
    files: Vec<FileOut>,
    folders: Vec<FolderOut>,
    merged: Vec<String>,
    folder_old: String,
    folder_new: String,
    /// `renamed` | `unchanged` | `failed` | `not_reached`.
    folder: &'static str,
    folder_reason: Option<Reason>,
    folder_message: Option<String>,
    user_message: String,
}

impl Out {
    fn json(&self, record: &Record) -> Value {
        let files: Vec<Value> = self
            .files
            .iter()
            .map(|f| {
                let mut m = json!({
                    "filename": f.filename,
                    "new_filename": f.new_filename,
                    "folder": f.folder,
                    "rename": f.rename,
                    "tags": f.tags,
                    "move": f.moved,
                });
                if let (Some(r), Some(u)) = (f.reason, &f.user_message) {
                    m["reason_code"] = json!(r.code());
                    m["user_message"] = json!(u);
                }
                m
            })
            .collect();
        let folders: Vec<Value> = self
            .folders
            .iter()
            .map(|f| {
                json!({
                    "dir_name": f.dir_name,
                    "destination": f.destination,
                    "covers": f.covers,
                    "junk": f.junk,
                    "strays": f.strays,
                    "sidecar": f.sidecar,
                    "left": f.left,
                    "outcome": f.outcome,
                })
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
            "folders": folders,
            "merged": self.merged,
            "folder": folder,
            "record": record.path.to_string_lossy(),
            "attempts": record.attempts.len(),
        })
    }
}

/// `env.now()` as the trash's collision stamp (`YYYY-MM-DDTHH-MM-SS`).
fn trash_stamp(now: &str) -> String {
    now.chars()
        .take(19)
        .map(|c| if c == ':' { '-' } else { c })
        .collect()
}

/// The record of a new conversion of a set kept in several folders.
fn new_record(
    env: &ConvertEnv<'_>,
    p: &ConversionPreview,
    g: &Gather,
    dir: &Path,
    now: &str,
) -> Record {
    let dest = &g.members[0];
    let before = p.target_current.clone().unwrap_or_default();
    let after = p.plan.target_name.clone().unwrap_or_else(|| before.clone());
    let expected = if after == before {
        dest.album_id.clone()
    } else {
        album_id_of(&py_resolve(&dir.with_file_name(&after)).to_string_lossy())
    };
    let total = p.plan.track_total;
    Record {
        path: env.logs.join(Record::file_name(&dest.album_id, now)),
        kind: KIND_FOLDERS.into(),
        started_at: now.to_string(),
        finished_at: None,
        library_path: p.library_path.clone(),
        album_id_before: dest.album_id.clone(),
        album_id_expected: expected,
        album_id_after: None,
        folder_before: dir.to_string_lossy().into_owned(),
        folder_name_before: before,
        folder_name_after: after,
        template: p.template.clone(),
        dir_template: p.dir_template.clone(),
        track_total: total,
        files: p
            .plan
            .tracks
            .iter()
            .map(|t| LoggedFile {
                album_id: t.album_id.clone(),
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
        members: g
            .members
            .iter()
            .map(|m| LoggedMember {
                album_id: m.album_id.clone(),
                folder: m.dir.to_string_lossy().into_owned(),
                folder_name: m.dir_name.clone(),
                disc: m.disc,
                destination: m.destination,
            })
            .collect(),
        folders: p
            .plan
            .folders
            .iter()
            .map(|f| LoggedFolder {
                album_id: f.album_id.clone(),
                folder_name: f.dir_name.clone(),
                covers: f
                    .covers
                    .iter()
                    .map(|c| (c.name.clone(), c.action.as_str().to_string()))
                    .collect(),
                junk: f.junk.iter().map(|(n, _)| n.clone()).collect(),
                sidecar: f.sidecar,
                other_files: f.other_files.clone(),
                strays: f
                    .strays
                    .iter()
                    .map(|s| LoggedStray {
                        name: s.name.clone(),
                        action: s.action.as_str().to_string(),
                        new_name: s.new_name.clone(),
                    })
                    .collect(),
            })
            .collect(),
        orphan_rows: g.orphan_rows.clone(),
        abandoned_at: None,
    }
}

/// Carry out the conversion of a set kept in several folders, as `p`
/// plans it (checked by the caller: no blocker, the digest the user saw).
/// `dir` is the destination folder's path as its row stores it.
pub(crate) fn convert_folders(
    env: &mut ConvertEnv<'_>,
    p: &ConversionPreview,
    dir: &Path,
) -> Result<Value, Refusal> {
    let Some(g) = p.gather.clone() else {
        return Err(Refusal::new(
            409,
            "blocked",
            "The plan does not say which folders the set is in, so nothing was changed.",
        ));
    };
    let now = (env.now)();
    let mut record = match p.resume.clone() {
        Some(r) => r,
        None => new_record(env, p, &g, dir, &now),
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
    if env.stop_at == Some(StopAt::AfterRecord) {
        return Ok(json!({
            "ok": false,
            "interrupted": true,
            "album_id": g.members[0].album_id,
            "record": record.path.to_string_lossy(),
            "attempts": record.attempts.len(),
        }));
    }
    let bus = env.bus;
    let out = musaic_events::context::with_bulk_run(|| run(env, p, &g, &mut record));
    if out.interrupted {
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

/// Rescan an album folder and store what it holds now.
fn rescan(env: &mut ConvertEnv<'_>, library: &str, dir: &Path) {
    if let Some(fresh) = musaic_scan::scan_album(dir, env.track_order) {
        let now = (env.now)();
        if let Err(e) = musaic_db::albums::upsert_album(env.conn, library, &now, &fresh) {
            musaic_log::error(TARGET, &format!("rescan of {} failed: {e}", dir.display()));
        }
    }
}

fn failed(f: &mut FileOut, reason: Reason, message: String) {
    f.reason = Some(reason);
    f.user_message = Some(message);
}

fn run(env: &mut ConvertEnv<'_>, p: &ConversionPreview, g: &Gather, record: &mut Record) -> Out {
    let tracks = &p.plan.tracks;
    let library = record.library_path.clone();
    let mut ids: Vec<String> = g.members.iter().map(|m| m.album_id.clone()).collect();
    let mut dirs: Vec<PathBuf> = g.members.iter().map(|m| m.dir.clone()).collect();
    let member_of = |id: &str| g.members.iter().position(|m| m.album_id == id).unwrap_or(0);
    let members: Vec<usize> = tracks.iter().map(|t| member_of(&t.album_id)).collect();
    let mut state: Vec<TrackState> = tracks.iter().map(|t| t.state).collect();
    let folder_old = p.target_current.clone().unwrap_or_default();
    let folder_new = p
        .plan
        .target_name
        .clone()
        .unwrap_or_else(|| folder_old.clone());
    let mut out = Out {
        ok: false,
        interrupted: false,
        album_id: ids[0].clone(),
        files: tracks
            .iter()
            .zip(&members)
            .map(|(t, k)| FileOut {
                filename: t.filename.clone(),
                new_filename: t.new_filename.clone().unwrap_or_default(),
                folder: g.members[*k].dir_name.clone(),
                rename: "not_reached",
                tags: "not_reached",
                moved: if *k == 0 { "stays" } else { "not_reached" },
                ..Default::default()
            })
            .collect(),
        folders: p
            .plan
            .folders
            .iter()
            .map(|f| FolderOut {
                dir_name: f.dir_name.clone(),
                destination: f.destination,
                sidecar: if f.sidecar { "not_reached" } else { "none" },
                outcome: if f.destination {
                    "destination"
                } else if f.exists {
                    "not_reached"
                } else {
                    "removed"
                },
                ..Default::default()
            })
            .collect(),
        merged: Vec::new(),
        folder_old: folder_old.clone(),
        folder_new: folder_new.clone(),
        folder: "not_reached",
        folder_reason: None,
        folder_message: None,
        user_message: String::new(),
    };
    macro_rules! stop_here {
        () => {{
            out.interrupted = true;
            return out;
        }};
    }
    let now = (env.now)();

    // ---- a folder rename that reached the disk but not the database ----
    if g.rekey_pending {
        let listed = listed_as(
            dirs[0].parent().unwrap_or(Path::new("")),
            &name_of(&dirs[0]),
        )
        .map(|n| dirs[0].with_file_name(n))
        .unwrap_or_else(|| dirs[0].clone());
        let new_id = album_id_of(&py_resolve(&listed).to_string_lossy());
        match musaic_db::renames::rename_album_directory(
            env.conn,
            &ids[0],
            &new_id,
            &listed.to_string_lossy(),
            &name_of(&listed),
        ) {
            Ok(_) => {
                env.bus.publish(&BusEvent::AlbumRenamed {
                    old_album_id: ids[0].clone(),
                    new_album_id: new_id.clone(),
                });
                step(
                    record,
                    "repair_folder_rekey",
                    true,
                    "",
                    vec![json!({"old_id": ids[0], "new_id": new_id})],
                    (env.now)(),
                );
                ids[0] = new_id;
                dirs[0] = listed;
            }
            Err(e) => {
                step(
                    record,
                    "repair_folder_rekey",
                    false,
                    &e.to_string(),
                    vec![],
                    (env.now)(),
                );
                out.user_message = format!(
                    "The set's folder already has its new name, but Musaic couldn't move its \
library records to it ({e}). Nothing else was changed; convert again to finish."
                );
                return out;
            }
        }
    }
    out.album_id = ids[0].clone();

    // ---- 2. the sidecars' tags into the database ----
    let mut results = Vec::new();
    for (k, m) in g.members.iter().enumerate() {
        if !m.has_row || !dirs[k].is_dir() {
            continue;
        }
        let map = match crate::sidecar_import::read_sidecar(&dirs[k]) {
            Ok(map) => map,
            Err(e) => {
                // Kept in the trash with the rest, so nothing is lost.
                musaic_log::warning(
                    TARGET,
                    &format!("could not read the sidecar of {}: {e}", dirs[k].display()),
                );
                continue;
            }
        };
        if map.is_empty() {
            continue;
        }
        let flacs: Vec<String> = musaic_scan::list_flacs(&dirs[k])
            .iter()
            .map(|p| name_of(p))
            .collect();
        let added = (|| -> rusqlite::Result<usize> {
            let tx = musaic_db::immediate(env.conn)?;
            let mut n = 0;
            for (file, tags) in &map {
                let Some(listed) = flacs.iter().find(|f| nfc(f) == nfc(file)) else {
                    continue;
                };
                for tag in tags {
                    n += tx.execute(
                        "INSERT OR IGNORE INTO track_tags (album_id, filename, tag, created_at) \
                         VALUES (?1, ?2, ?3, ?4)",
                        [ids[k].as_str(), listed, tag, &now],
                    )?;
                }
            }
            tx.commit()?;
            Ok(n)
        })();
        match added {
            Ok(n) => results.push(json!({"folder": m.dir_name, "tags_added": n})),
            Err(e) => {
                step(
                    record,
                    "merge_sidecar_tags",
                    false,
                    &e.to_string(),
                    results,
                    (env.now)(),
                );
                out.user_message = format!(
                    "Musaic couldn't add the tags kept in {}'s tag file to its library records \
({e}). Nothing was changed on disk; convert again to finish.",
                    m.dir_name
                );
                return out;
            }
        }
    }
    step(record, "merge_sidecar_tags", true, "", results, (env.now)());

    let renv_now = (env.now)();
    let renv = RenameEnv {
        library_dir: env.library_dir,
        now: &renv_now,
        track_order: env.track_order,
    };

    // ---- 3. renames, each member in its own folder ----
    let mut entries: Vec<usize> = Vec::new();
    for (i, t) in tracks.iter().enumerate() {
        let new = t.new_filename.clone().unwrap_or_default();
        if state[i] != TrackState::Untouched || nfc(&t.filename) == nfc(&new) {
            out.files[i].rename = "already";
        } else {
            entries.push(i);
        }
    }
    let stop_renames = matches!(env.stop_at, Some(StopAt::RenamedFiles(_)));
    if let Some(StopAt::RenamedFiles(n)) = env.stop_at {
        entries.truncate(n);
    }
    let total = entries.len();
    let mut results = Vec::new();
    let mut rename_failed = 0;
    let mut batch_message = String::new();
    let mut batch_user: Option<String> = None;
    for k in 0..g.members.len() {
        let mine: Vec<usize> = entries
            .iter()
            .copied()
            .filter(|i| members[*i] == k)
            .collect();
        if mine.is_empty() {
            continue;
        }
        let plan: Vec<PlanEntry> = mine
            .iter()
            .map(|i| PlanEntry {
                old: tracks[*i].filename.clone(),
                new: tracks[*i].new_filename.clone().unwrap_or_default(),
            })
            .collect();
        let before: Vec<Option<same_file::Handle>> = plan
            .iter()
            .map(|e| same_file::Handle::from_path(dirs[k].join(&e.old)).ok())
            .collect();
        let r = execute_renames(env.conn, &renv, &ids[k], &plan, None);
        if let Some(ev) = &r.album_renamed {
            env.bus.publish(&BusEvent::AlbumRenamed {
                old_album_id: ev.old_album_id.clone(),
                new_album_id: ev.new_album_id.clone(),
            });
        }
        for ((i, e), was) in mine.iter().zip(&plan).zip(before) {
            let now_h = same_file::Handle::from_path(dirs[k].join(&e.new)).ok();
            let done = was.is_some() && now_h == was && listed_as(&dirs[k], &e.new).is_some();
            let f = &mut out.files[*i];
            if done {
                f.rename = "renamed";
                state[*i] = TrackState::Renamed;
            } else {
                rename_failed += 1;
                f.rename = "failed";
                let reason = r.reason.unwrap_or(Reason::Other);
                failed(
                    f,
                    reason,
                    format!(
                        "{} in {} could not be renamed to {} because {}. {}",
                        quoted(&e.old),
                        g.members[k].dir_name,
                        quoted(&e.new),
                        reason.cause(),
                        reason.advice()
                    ),
                );
            }
            results.push(json!({"folder": g.members[k].dir_name, "old": e.old, "new": e.new, "outcome": f.rename}));
        }
        report(env, "rename_files", results.len(), total);
        if rename_failed > 0 {
            batch_message = r.message.clone();
            batch_user = r.user_message.clone();
            break;
        }
    }
    if stop_renames {
        stop_here!();
    }
    if !entries.is_empty() {
        step(
            record,
            "rename_files",
            rename_failed == 0,
            &batch_message,
            results,
            (env.now)(),
        );
    }
    if rename_failed > 0 {
        out.user_message = format!(
            "{} not renamed, so nothing was moved and the folders were not merged. {}",
            if rename_failed == 1 {
                "1 file was".to_string()
            } else {
                format!("{rename_failed} files were")
            },
            batch_user.unwrap_or_default()
        );
        return out;
    }

    // ---- 4. tags, each member in its own folder ----
    let mut todo: Vec<(usize, String)> = Vec::new();
    let mut lost = 0;
    for (i, t) in tracks.iter().enumerate() {
        if matches!(state[i], TrackState::Tagged | TrackState::Moved) {
            out.files[i].tags = "already";
            continue;
        }
        let want = t.new_filename.clone().unwrap_or_default();
        match listed_as(&dirs[members[i]], &want) {
            Some(on_disk) => todo.push((i, on_disk)),
            None => {
                lost += 1;
                out.files[i].tags = "failed";
                failed(
                    &mut out.files[i],
                    Reason::NotFound,
                    format!(
                        "{} is not in {}, so its tags were not written. {}",
                        quoted(&want),
                        g.members[members[i]].dir_name,
                        Reason::NotFound.advice()
                    ),
                );
            }
        }
    }
    let stop_tags = matches!(env.stop_at, Some(StopAt::TaggedFiles(_)));
    if let Some(StopAt::TaggedFiles(n)) = env.stop_at {
        todo.truncate(n);
    }
    let mut results = Vec::new();
    let mut tag_failed = 0;
    let mut apply_error: Option<String> = None;
    let total = todo.len();
    for k in 0..g.members.len() {
        let mine: Vec<&(usize, String)> = todo.iter().filter(|(i, _)| members[*i] == k).collect();
        if mine.is_empty() {
            continue;
        }
        let mut conversion = musaic_tags::DiscConversion::default();
        for (i, name) in &mine {
            conversion.files.push((
                name.clone(),
                recorded_numbers(
                    record,
                    tracks[*i].new_filename.as_deref().unwrap_or(""),
                    tracks[*i].new_track.unwrap_or_default(),
                ),
            ));
        }
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
                &ids[k],
                &dirs[k].to_string_lossy(),
                env.library_dir,
                &conversion,
                &mut |item| {
                    if item.event == "error" {
                        apply_error = item
                            .data
                            .get("detail")
                            .and_then(crate::Json::as_str)
                            .map(str::to_string);
                    }
                    ControlFlow::Continue(())
                },
            );
        }
        for (i, name) in mine {
            let want = tracks[*i].new_filename.as_deref().unwrap_or("");
            let expected = record
                .files
                .iter()
                .find(|e| nfc(&e.new_name) == nfc(want))
                .map(|e| e.new_tags.clone());
            let path = dirs[k].join(name);
            let now_tags = read_numbering(&path);
            let f = &mut out.files[*i];
            if now_tags.is_some() && now_tags == expected {
                f.tags = "written";
                state[*i] = TrackState::Tagged;
            } else {
                tag_failed += 1;
                f.tags = "failed";
                let reason = write_probe(&path);
                failed(
                    f,
                    reason,
                    format!(
                        "The track number and disc tags of {} in {} could not be written because \
{}. {}",
                        quoted(name),
                        g.members[k].dir_name,
                        reason.cause(),
                        reason.advice()
                    ),
                );
            }
            results.push(json!({"folder": g.members[k].dir_name, "file": name, "outcome": f.tags}));
        }
        report(env, "write_tags", results.len(), total);
    }
    if stop_tags {
        stop_here!();
    }
    if !todo.is_empty() || lost > 0 {
        step(
            record,
            "write_tags",
            tag_failed == 0 && lost == 0 && apply_error.is_none(),
            apply_error.as_deref().unwrap_or(""),
            results,
            (env.now)(),
        );
    }
    if tag_failed > 0 || lost > 0 || apply_error.is_some() {
        let n = tag_failed + lost;
        out.user_message = match apply_error {
            Some(e) if n == 0 => format!(
                "The tags could not be written ({e}). Nothing was moved; convert again to finish."
            ),
            _ => format!(
                "The tags of {} could not be written, so nothing was moved and the folders were \
not merged. Fix what each file says, then convert again to finish: what is done stays done.",
                plural(n, "file")
            ),
        };
        return out;
    }

    // ---- 5. moves into the destination ----
    let to_move: Vec<usize> = (0..tracks.len())
        .filter(|i| members[*i] > 0 && state[*i] != TrackState::Moved)
        .collect();
    let total = to_move.len();
    let mut moved_now = 0;
    let mut results = Vec::new();
    let mut move_error: Option<String> = None;
    for i in 0..tracks.len() {
        let k = members[i];
        if k == 0 {
            continue;
        }
        let new = tracks[i].new_filename.clone().unwrap_or_default();
        if state[i] == TrackState::Moved {
            // A crash may have fallen between the move and its rows'
            // commit: moving them again finds nothing left when it did not.
            out.files[i].moved = "already";
            if let Some(listed) = listed_as(&dirs[0], &new)
                && let Err(e) = move_track_rows(env.conn, &ids[k], &listed, &ids[0], &listed)
            {
                out.files[i].moved = "failed";
                failed(
                    &mut out.files[i],
                    Reason::LibraryUpdateFailed,
                    format!(
                        "{} is in {}, but Musaic couldn't move its library records with it ({e}).",
                        quoted(&listed),
                        name_of(&dirs[0])
                    ),
                );
                move_error = Some(e.to_string());
                break;
            }
            continue;
        }
        let Some(src_name) = listed_as(&dirs[k], &new) else {
            out.files[i].moved = "failed";
            failed(
                &mut out.files[i],
                Reason::NotFound,
                format!(
                    "{} is not in {}, so it was not moved. {}",
                    quoted(&new),
                    g.members[k].dir_name,
                    Reason::NotFound.advice()
                ),
            );
            move_error = Some("source not found".into());
            break;
        };
        let src = dirs[k].join(&src_name);
        let dst = dirs[0].join(&src_name);
        if listed_as(&dirs[0], &new).is_some() || std::fs::symlink_metadata(&dst).is_ok() {
            out.files[i].moved = "failed";
            failed(
                &mut out.files[i],
                Reason::AlreadyExists,
                format!(
                    "A file named {} appeared in {} since the plan was made, so {} was not moved \
and nothing was overwritten. Look at both files, then convert again.",
                    quoted(&new),
                    name_of(&dirs[0]),
                    quoted(&src_name)
                ),
            );
            move_error = Some("target exists".into());
            break;
        }
        let handle = same_file::Handle::from_path(&src).ok();
        if let Err(e) = musaic_fsops::move_file_noclobber(&src, &dst) {
            let reason = Reason::from_io(&e);
            out.files[i].moved = "failed";
            failed(
                &mut out.files[i],
                reason,
                format!(
                    "{} could not be moved from {} into {} because {}. {}",
                    quoted(&src_name),
                    g.members[k].dir_name,
                    name_of(&dirs[0]),
                    reason.cause(),
                    reason.advice()
                ),
            );
            move_error = Some(e.to_string());
            break;
        }
        let names = musaic_scan::dir_names(&dirs[0]).unwrap_or_default();
        let listed = musaic_scan::listed_spelling(&dirs[0], &src_name, &names, handle.as_ref())
            .map(|n| n.to_string_lossy().into_owned());
        let Some(listed) = listed.filter(|l| {
            handle.is_some() && same_file::Handle::from_path(dirs[0].join(l)).ok() == handle
        }) else {
            out.files[i].moved = "failed";
            failed(
                &mut out.files[i],
                Reason::Unverified,
                format!(
                    "{} was moved into {}, but Musaic couldn't read the folder back to confirm \
it. Rescan the library, then convert again.",
                    quoted(&src_name),
                    name_of(&dirs[0])
                ),
            );
            move_error = Some("not verified".into());
            break;
        };
        state[i] = TrackState::Moved;
        moved_now += 1;
        if env.stop_at
            == Some(StopAt::MovedFiles {
                n: moved_now,
                rows: false,
            })
        {
            stop_here!();
        }
        if let Err(e) = move_track_rows(env.conn, &ids[k], &src_name, &ids[0], &listed) {
            out.files[i].moved = "failed";
            failed(
                &mut out.files[i],
                Reason::LibraryUpdateFailed,
                format!(
                    "{} was moved into {}, but Musaic couldn't move its library records with it \
({e}). Convert again to finish.",
                    quoted(&listed),
                    name_of(&dirs[0])
                ),
            );
            move_error = Some(e.to_string());
            break;
        }
        out.files[i].moved = "moved";
        results.push(json!({"from": g.members[k].dir_name, "file": listed}));
        if env.stop_at
            == Some(StopAt::MovedFiles {
                n: moved_now,
                rows: true,
            })
        {
            stop_here!();
        }
        report(env, "move_files", moved_now, total);
    }
    if total > 0 || move_error.is_some() {
        step(
            record,
            "move_files",
            move_error.is_none(),
            move_error.as_deref().unwrap_or(""),
            results,
            (env.now)(),
        );
    }
    if move_error.is_some() {
        out.user_message = format!(
            "{} moved into {}; the rest were not, and the folders were not merged. Fix what the \
file says, then convert again to finish: what is done stays done.",
            if moved_now == 1 {
                "1 file was".to_string()
            } else {
                format!("{moved_now} files were")
            },
            name_of(&dirs[0])
        );
        return out;
    }
    if moved_now > 0 {
        rescan(env, &library, &dirs[0].clone());
    }

    // ---- 6. covers, clutter and other files of the other folders ----
    let stamp = trash_stamp(&now);
    let mut results = Vec::new();
    let mut cover_error: Option<String> = None;
    let mut strays_moved = 0;
    let mut stopped = false;
    'folders: for (fi, f) in p.plan.folders.iter().enumerate() {
        if f.destination || !f.exists {
            continue;
        }
        let k = member_of(&f.album_id);
        for c in &f.covers {
            let outcome = if c.done {
                "already"
            } else {
                let Some(name) = listed_as(&dirs[k], &c.name) else {
                    cover_error = Some(format!("{} is no longer in {}", c.name, f.dir_name));
                    break 'folders;
                };
                let src = dirs[k].join(&name);
                let r = match c.action {
                    CoverAction::Kept => Ok("kept"),
                    CoverAction::Moved => {
                        musaic_fsops::move_file_noclobber(&src, &dirs[0].join(&name))
                            .map(|()| "moved")
                            .map_err(|e| e.to_string())
                    }
                    CoverAction::Identical | CoverAction::Different => {
                        musaic_fsops::trash::send_to_trash(
                            env.trash,
                            &src,
                            Some(&f.dir_name),
                            || stamp.clone(),
                        )
                        .map(|_| "trashed")
                        .map_err(|e| e.to_string())
                    }
                };
                match r {
                    Ok(o) => o,
                    Err(e) => {
                        cover_error = Some(format!("{} in {}: {e}", c.name, f.dir_name));
                        out.folders[fi]
                            .covers
                            .push(json!({"name": c.name, "action": c.action.as_str(), "outcome": "failed"}));
                        break 'folders;
                    }
                }
            };
            out.folders[fi]
                .covers
                .push(json!({"name": c.name, "action": c.action.as_str(), "outcome": outcome}));
            results.push(json!({"folder": f.dir_name, "name": c.name, "action": c.action.as_str(), "outcome": outcome}));
        }
        for (name, done) in &f.junk {
            let outcome = if *done {
                "already"
            } else {
                let Some(listed) = listed_as(&dirs[k], name) else {
                    continue;
                };
                match musaic_fsops::trash::send_to_trash(
                    env.trash,
                    &dirs[k].join(&listed),
                    Some(&f.dir_name),
                    || stamp.clone(),
                ) {
                    Ok(_) => "trashed",
                    Err(e) => {
                        cover_error = Some(format!("{name} in {}: {e}", f.dir_name));
                        break 'folders;
                    }
                }
            };
            out.folders[fi]
                .junk
                .push(json!({"name": name, "outcome": outcome}));
            results.push(json!({"folder": f.dir_name, "name": name, "action": "clutter", "outcome": outcome}));
        }
        // The other files, after the covers: a cover the destination takes
        // has its name there before a file is given one.
        for s in &f.strays {
            let mut to = s.target().to_string();
            let outcome = if s.done {
                "already"
            } else {
                let Some(name) = listed_as(&dirs[k], &s.name) else {
                    cover_error = Some(format!("{} is no longer in {}", s.name, f.dir_name));
                    break 'folders;
                };
                let src = dirs[k].join(&name);
                let r = match s.action {
                    StrayAction::Identical => musaic_fsops::trash::send_to_trash(
                        env.trash,
                        &src,
                        Some(&f.dir_name),
                        || stamp.clone(),
                    )
                    .map(|_| "trashed")
                    .map_err(|e| e.to_string()),
                    StrayAction::Moved => {
                        // Its own name as its folder lists it, or the name
                        // the plan gave it.
                        let want = s.new_name.clone().unwrap_or_else(|| name.clone());
                        move_stray(&src, &dirs[0], &want).map(|listed| {
                            to = listed;
                            "moved"
                        })
                    }
                };
                match r {
                    Ok(o) => o,
                    Err(e) => {
                        cover_error = Some(format!("{} in {}: {e}", s.name, f.dir_name));
                        out.folders[fi].strays.push(json!({"name": s.name, "action": s.action.as_str(), "new_name": s.new_name, "outcome": "failed"}));
                        break 'folders;
                    }
                }
            };
            out.folders[fi].strays.push(json!({"name": s.name, "action": s.action.as_str(), "new_name": s.new_name, "outcome": outcome}));
            results.push(json!({
                "folder": f.dir_name,
                "name": s.name,
                "action": if s.action == StrayAction::Moved { "file_moved" } else { "file_identical" },
                "to": (s.action == StrayAction::Moved).then_some(&to),
                "outcome": outcome,
            }));
            if outcome == "moved" {
                strays_moved += 1;
                if env.stop_at == Some(StopAt::MovedStrays(strays_moved)) {
                    stopped = true;
                    break 'folders;
                }
            }
        }
    }
    if stopped {
        stop_here!();
    }
    step(
        record,
        "covers",
        cover_error.is_none(),
        cover_error.as_deref().unwrap_or(""),
        results,
        (env.now)(),
    );
    if let Some(e) = cover_error {
        out.user_message = format!(
            "Every file was moved, but a cover or another small file could not be handled ({e}). \
The folders were not merged; fix it, then convert again to finish."
        );
        return out;
    }
    if env.stop_at == Some(StopAt::AfterCovers) {
        stop_here!();
    }

    // ---- 7. sidecars: the destination's from the database, then the old
    // ones to the trash ----
    match musaic_fsops::sidecar::resync_after_renames(env.conn, &ids[0], &dirs[0]) {
        Ok(Some(false)) | Err(_) => {
            step(
                record,
                "sidecar",
                false,
                "the tag file could not be written",
                vec![],
                (env.now)(),
            );
            out.user_message = format!(
                "Every file was moved, but the tag file of {} could not be written. The folders \
were not merged; convert again to finish.",
                name_of(&dirs[0])
            );
            return out;
        }
        Ok(_) => {}
    }
    if env.stop_at == Some(StopAt::AfterSidecar) {
        stop_here!();
    }
    let mut results = Vec::new();
    for (fi, f) in p.plan.folders.iter().enumerate() {
        if f.destination || !f.sidecar {
            continue;
        }
        let k = member_of(&f.album_id);
        let path = dirs[k].join(SIDECAR_FILENAME);
        let outcome = if !f.exists || f.sidecar_done || !path.is_file() {
            "merged"
        } else {
            match musaic_fsops::trash::send_to_trash(env.trash, &path, Some(&f.dir_name), || {
                stamp.clone()
            }) {
                Ok(_) => "merged",
                Err(e) => {
                    out.folders[fi].sidecar = "failed";
                    step(
                        record,
                        "sidecar",
                        false,
                        &e.to_string(),
                        results,
                        (env.now)(),
                    );
                    out.user_message = format!(
                        "The old tag file of {} could not be moved to the trash ({e}). The folders \
were not merged; convert again to finish.",
                        f.dir_name
                    );
                    return out;
                }
            }
        };
        out.folders[fi].sidecar = outcome;
        results.push(json!({"folder": f.dir_name, "outcome": outcome}));
    }
    step(record, "sidecar", true, "", results, (env.now)());

    // ---- 8. rows of files no longer on disk ----
    for (aid, name) in &g.orphan_rows {
        if let Err(e) = move_track_rows(env.conn, aid, name, &ids[0], name) {
            step(
                record,
                "orphan_rows",
                false,
                &e.to_string(),
                vec![],
                (env.now)(),
            );
            out.user_message = format!(
                "Musaic couldn't carry its records of {} (a file no longer on disk) to the set \
({e}). The folders were not merged; convert again to finish.",
                quoted(name)
            );
            return out;
        }
    }

    // ---- 9. the fold ----
    if !g.folded {
        // Proof that the other members are emptied by this conversion:
        // every file of theirs is in the destination and their folders
        // hold no FLAC.
        let mut left: Vec<String> = Vec::new();
        for (dir, m) in dirs.iter().zip(&g.members).skip(1) {
            if dir.is_dir() {
                for f in musaic_scan::list_flacs(dir) {
                    left.push(format!("{} in {}", name_of(&f), m.dir_name));
                }
            }
        }
        let pending = (0..tracks.len()).any(|i| members[i] > 0 && state[i] != TrackState::Moved);
        if !left.is_empty() || pending {
            step(record, "fold", false, &left.join(", "), vec![], (env.now)());
            out.user_message = format!(
                "The albums were not merged: {}. Nothing else was changed; convert again once \
it is sorted out.",
                if left.is_empty() {
                    "not every file was moved".to_string()
                } else {
                    format!(
                        "these FLACs are still in a disc's folder: {}",
                        left.join(", ")
                    )
                }
            );
            return out;
        }
        let from: Vec<String> = ids[1..]
            .iter()
            .filter(|id| {
                env.conn
                    .query_row("SELECT 1 FROM albums WHERE id = ?1", [id.as_str()], |_| {
                        Ok(())
                    })
                    .is_ok()
            })
            .cloned()
            .collect();
        if !from.is_empty() {
            let fold_now = (env.now)();
            let req = MergeRequest {
                from_ids: &from,
                into_id: &ids[0],
                emptied: &from,
                now: &fold_now,
            };
            match merge_album_rows(env.conn, &req) {
                Ok(report) => {
                    for id in &from {
                        env.bus.publish(&BusEvent::AlbumRenamed {
                            old_album_id: id.clone(),
                            new_album_id: ids[0].clone(),
                        });
                    }
                    out.merged = from.clone();
                    step(
                        record,
                        "fold",
                        true,
                        "",
                        vec![json!({
                            "into": ids[0],
                            "from": from,
                            "albums_deleted": report.albums_deleted,
                            "findings_resolved": report.check_findings_resolved,
                            "blocklist_rows": report.album_blocked_mb_release_groups,
                            "jobs_repointed": report.ai_jobs,
                            "release_group": report.release_group,
                            "release": report.release,
                            "release_cleared": report.release_cleared,
                        })],
                        (env.now)(),
                    );
                }
                Err(e) => {
                    step(record, "fold", false, &e.to_string(), vec![], (env.now)());
                    out.user_message = format!(
                        "Every file was moved into {}, but the albums could not be merged in the \
library ({e}). Convert again to finish.",
                        name_of(&dirs[0])
                    );
                    return out;
                }
            }
        }
    }
    if env.stop_at == Some(StopAt::AfterFold) {
        stop_here!();
    }

    // ---- 10. emptied folders ----
    let mut removed_now = 0;
    let mut results = Vec::new();
    for (fi, f) in p.plan.folders.iter().enumerate() {
        if f.destination {
            continue;
        }
        let k = member_of(&f.album_id);
        if !dirs[k].is_dir() {
            out.folders[fi].outcome = "removed";
            continue;
        }
        let left: Vec<String> = musaic_scan::dir_names(&dirs[k])
            .unwrap_or_default()
            .iter()
            .map(|n| n.to_string_lossy().into_owned())
            .collect();
        if left.is_empty() {
            // Non-recursive: it removes nothing but an empty folder.
            match std::fs::remove_dir(&dirs[k]) {
                Ok(()) => {
                    out.folders[fi].outcome = "removed";
                    removed_now += 1;
                }
                Err(e) => {
                    out.folders[fi].outcome = "kept";
                    out.folders[fi].left =
                        vec![format!("(the empty folder could not be removed: {e})")];
                }
            }
        } else {
            let mut left = left;
            left.sort();
            out.folders[fi].outcome = "kept";
            out.folders[fi].left = left;
        }
        results.push(json!({"folder": f.dir_name, "outcome": out.folders[fi].outcome, "left": out.folders[fi].left}));
        if env.stop_at == Some(StopAt::RemovedFolders(removed_now)) && removed_now > 0 {
            stop_here!();
        }
    }
    step(record, "remove_folders", true, "", results, (env.now)());

    // ---- 11. the destination folder's name ----
    if env.stop_at == Some(StopAt::BeforeFolderRename) {
        stop_here!();
    }
    let current = name_of(&dirs[0]);
    if g.folder_renamed || nfc(&current) == nfc(&folder_new) {
        out.folder = if g.folder_renamed {
            "renamed"
        } else {
            "unchanged"
        };
    } else {
        let r = execute_renames(
            env.conn,
            &renv,
            &ids[0],
            &[],
            Some(&PlanEntry {
                old: current.clone(),
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
                ids[0] = d.new_album_id.clone();
                if let Ok(Some(path)) = musaic_db::renames::album_path(env.conn, &ids[0]) {
                    dirs[0] = PathBuf::from(path);
                }
            }
        } else {
            out.folder = "failed";
            let reason = r.reason.unwrap_or(Reason::Other);
            out.folder_reason = Some(reason);
            out.folder_message = Some(
                r.user_message
                    .clone()
                    .unwrap_or_else(|| couldnt_rename("The set's folder", reason)),
            );
        }
        step(
            record,
            "rename_folder",
            renamed,
            &r.message,
            vec![
                json!({"old": current, "new": folder_new, "outcome": out.folder, "album_id": ids[0]}),
            ],
            (env.now)(),
        );
        if !renamed {
            out.album_id = ids[0].clone();
            out.user_message = format!(
                "The set is in one folder and renumbered, but the folder was not renamed: {}",
                out.folder_message.clone().unwrap_or_default()
            );
            return out;
        }
    }
    if env.stop_at == Some(StopAt::AfterFolderRename) {
        stop_here!();
    }

    // ---- 12. the survivor as it is now ----
    rescan(env, &library, &dirs[0].clone());
    env.bus.publish(&BusEvent::AlbumTagsChanged {
        album_id: ids[0].clone(),
        fields: DISC_CONVERSION_FIELDS
            .iter()
            .map(|s| s.to_string())
            .collect(),
    });
    out.album_id = ids[0].clone();
    out.ok = true;
    let moved_from = g.members.len() - 1;
    let kept: Vec<String> = out
        .folders
        .iter()
        .filter(|f| f.outcome == "kept")
        .map(|f| format!("{} ({})", f.dir_name, f.left.join(", ")))
        .collect();
    out.user_message = format!(
        "Converted: the {} of {} discs are in {}, numbered 1 to {}{}.",
        plural(tracks.len(), "file"),
        moved_from + 1,
        name_of(&dirs[0]),
        tracks.len(),
        if kept.is_empty() {
            String::new()
        } else {
            format!(
                ". {} with files left in {}: {}",
                if kept.len() == 1 {
                    "One old folder stays".to_string()
                } else {
                    format!("{} old folders stay", kept.len())
                },
                if kept.len() == 1 { "it" } else { "them" },
                kept.join("; ")
            )
        }
    );
    out
}

/// Move one other file of an emptied folder into the destination as
/// `want`, never replacing anything, and return the name the destination
/// lists it under (the same file, read back: a volume may store another
/// spelling than the one asked for).
fn move_stray(src: &Path, dest: &Path, want: &str) -> Result<String, String> {
    if listed_as(dest, want).is_some() || std::fs::symlink_metadata(dest.join(want)).is_ok() {
        return Err(format!(
            "a file named {} appeared in {} since the plan was made, so nothing was overwritten",
            quoted(want),
            name_of(dest)
        ));
    }
    let handle = same_file::Handle::from_path(src).map_err(|e| e.to_string())?;
    musaic_fsops::move_file_noclobber(src, &dest.join(want)).map_err(|e| e.to_string())?;
    let names = musaic_scan::dir_names(dest).unwrap_or_default();
    musaic_scan::listed_spelling(dest, want, &names, Some(&handle))
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|l| same_file::Handle::from_path(dest.join(l)).ok().as_ref() == Some(&handle))
        .ok_or_else(|| {
            format!(
                "it was moved into {}, but the folder could not be read back to confirm it",
                name_of(dest)
            )
        })
}

/// Stop treating the unfinished conversion of finding `finding_id`'s
/// album as one: its record is closed as abandoned (kept, with every
/// attempt), so scans read its folders again and the check judges them
/// as they are. Nothing on disk moves. Rows of files that already moved
/// into the destination but whose rows had not followed are moved first,
/// so the database matches the disk.
pub fn abandon(env: &mut ConvertEnv<'_>, finding_id: i64) -> Result<Value, Refusal> {
    let finding = match musaic_db::findings::get_finding(env.conn, finding_id)
        .map_err(crate::multi_disc_convert::db)?
    {
        Some(f) if f.check_id == crate::multi_disc_convert::CHECK_ID => f,
        _ => return Err(Refusal::new(404, "not_found", "Finding not found")),
    };
    let Some(album_id) = finding.album_id.clone().filter(|a| !a.is_empty()) else {
        return Err(Refusal::new(400, "no_album", "Finding has no album"));
    };
    let _layout = env.scan.hold_layout().ok_or_else(|| {
        Refusal::new(
            409,
            "scan_running",
            "A library scan is running, so nothing was changed. Try again when it finishes.",
        )
    })?;
    let mut records = musaic_fsops::conversion_log::unfinished_of(env.logs, &album_id);
    if records.is_empty() {
        return Err(Refusal::new(
            409,
            "no_conversion",
            "No unfinished conversion of this album was found, so nothing was changed.",
        ));
    }
    let preview = musaic_fsops::multi_disc::derive_conversion(env.conn, &album_id, Some(env.logs))
        .map_err(crate::multi_disc_convert::db)?;
    let mut where_now: Vec<Value> = Vec::new();
    if let Some(p) = &preview {
        if let Some(g) = &p.gather {
            for t in &p.plan.tracks {
                if t.state == TrackState::Moved
                    && let Some(new) = &t.new_filename
                    && let Some(listed) = listed_as(&g.members[0].dir, new)
                {
                    let _ = move_track_rows(
                        env.conn,
                        &t.album_id,
                        &listed,
                        &g.members[0].album_id,
                        &listed,
                    );
                }
            }
        }
        for t in &p.plan.tracks {
            where_now
                .push(json!({"file": t.filename, "folder": t.folder, "state": t.state.as_str()}));
        }
        // The other files too: one that already moved stays in the set's
        // folder under the name recorded here, and the record says so.
        let dest = p.plan.folders.iter().find(|f| f.destination);
        for f in p.plan.folders.iter().filter(|f| !f.destination) {
            for s in &f.strays {
                let (folder, name, state) = match (s.done, s.action) {
                    (false, _) => (f.dir_name.as_str(), s.name.as_str(), "untouched"),
                    (true, StrayAction::Moved) => (
                        dest.map_or("", |d| d.dir_name.as_str()),
                        s.target(),
                        "moved",
                    ),
                    (true, StrayAction::Identical) => ("", s.name.as_str(), "trashed"),
                };
                where_now.push(json!({
                    "file": s.name,
                    "from": f.dir_name,
                    "folder": folder,
                    "name": name,
                    "state": state,
                }));
            }
        }
    }
    let now = (env.now)();
    let mut paths = Vec::new();
    for r in records.iter_mut() {
        r.abandoned_at = Some(now.clone());
        r.finished_at = Some(now.clone());
        r.attempts.push(Attempt {
            started_at: now.clone(),
            finished_at: Some(now.clone()),
            outcome: Some("abandoned".into()),
            steps: vec![json!({"step": "abandon", "at": now, "ok": true, "message": "", "results": where_now})],
        });
        if let Err(e) = r.save() {
            return Err(Refusal::new(
                500,
                Reason::from_io(&e).code(),
                format!("Musaic couldn't update the record of the conversion ({e})."),
            ));
        }
        paths.push(r.path.to_string_lossy().into_owned());
    }
    env.bus.publish(&BusEvent::BulkBatchCompleted);
    Ok(json!({
        "ok": true,
        "records": paths,
        "files": where_now,
        "user_message": "Musaic no longer treats this as a conversion in progress. Every file \
    stays where it is now; the record keeps what was done.",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stray_moves_without_replacing_anything() {
        let tmp = tempfile::tempdir().unwrap();
        let (disc, dest) = (tmp.path().join("disc 2"), tmp.path().join("set"));
        std::fs::create_dir(&disc).unwrap();
        std::fs::create_dir(&dest).unwrap();
        std::fs::write(disc.join("rip.log"), b"two").unwrap();
        std::fs::write(dest.join("rip.log"), b"one").unwrap();

        // The name is taken: nothing moves and nothing is overwritten.
        let e = move_stray(&disc.join("rip.log"), &dest, "rip.log").unwrap_err();
        assert!(e.contains("nothing was overwritten"), "{e}");
        assert_eq!(std::fs::read(disc.join("rip.log")).unwrap(), b"two");
        assert_eq!(std::fs::read(dest.join("rip.log")).unwrap(), b"one");
        // Taken under another spelling of the same name too (as a volume
        // that ignores normalisation would see it).
        std::fs::write(dest.join("caf\u{e9}.txt"), b"x").unwrap();
        std::fs::write(disc.join("cafe\u{301}.txt"), b"y").unwrap();
        assert!(move_stray(&disc.join("cafe\u{301}.txt"), &dest, "cafe\u{301}.txt").is_err());
        assert_eq!(std::fs::read(dest.join("caf\u{e9}.txt")).unwrap(), b"x");

        // A free name: it moves, and the name returned is the one the
        // folder lists for that very file.
        let listed = move_stray(&disc.join("rip.log"), &dest, "rip (disc 2).log").unwrap();
        assert_eq!(listed, "rip (disc 2).log");
        assert!(!disc.join("rip.log").exists());
        assert_eq!(std::fs::read(dest.join(&listed)).unwrap(), b"two");
        assert_eq!(std::fs::read(dest.join("rip.log")).unwrap(), b"one");
    }
}
