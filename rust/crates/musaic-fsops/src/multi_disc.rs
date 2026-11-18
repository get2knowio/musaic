//! Multi-disc sets that are not in the target layout (one folder per set,
//! tracks numbered continuously across the discs, no disc designation in
//! the names, no disc tags): the `multi-disc-layout` check's detection over
//! album rows, and the read-only conversion preview its Resolve dialog shows.
//!
//! Detection reads only album rows (the per-file disc numbers the scan
//! stores in `track_discs_json`), never the files, so it costs one query on
//! a network share. It never guesses: a set is found only from explicit disc
//! numbers, in the files' tags or in folder names.
//!
//! The preview re-derives everything from the current database and disk
//! (rule 2); a finding's stored detail is used for labels only. The plan it
//! computes ([`plan_conversion`], pure) is the one a later conversion step
//! will carry out, so everything that would stop it is reported as a
//! [`Blocker`] here rather than discovered half-way.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use musaic_core::pycompat::strip;
use musaic_db::renames::{RenamePlanRow, rename_plan_row};
use musaic_scan::{COVER_FILENAMES, flacmeta, list_flacs, parse_tracknum};
use rusqlite::{Connection, params};

use crate::conversion_log::{self, NumberingTags, Record, numbering_of, values_of};
use crate::fsutil::fold;
use crate::plan::{Templates, TrackTags, render_dir_name, render_track_stem, same_nfc};
use crate::sidecar::SIDECAR_FILENAME;

/// What detection reads of one album row.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiscRow {
    pub id: String,
    pub path: String,
    pub dir_name: Option<String>,
    pub dir_artist: Option<String>,
    pub dir_album: Option<String>,
    pub is_compilation: bool,
    /// From the folder name.
    pub disc_number: Option<i64>,
    pub disc_total: Option<i64>,
    pub track_numbers: Vec<Option<i64>>,
    /// Parallel to `track_numbers`; `None` = the files have not been read
    /// for it yet (a row from before schema v20), which says nothing about
    /// their tags.
    pub track_discs: Option<Vec<Option<i64>>>,
    /// ALBUMARTIST of the sample track.
    pub album_artist: Option<String>,
    pub year: Option<String>,
    pub release_group: Option<String>,
    pub missing: bool,
}

/// A JSON list of integers or nulls, as the scan stores track lists.
/// Anything malformed is an empty list.
fn int_list(raw: Option<&str>) -> Vec<Option<i64>> {
    let Some(raw) = raw.filter(|r| !r.is_empty()) else {
        return Vec::new();
    };
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(serde_json::Value::Array(items)) => items.iter().map(|v| v.as_i64()).collect(),
        _ => Vec::new(),
    }
}

/// Every album row of `library_path`, Missing ones included (a set with a
/// Missing member is reported as one that cannot be judged, never as an
/// incomplete one), in id order.
pub fn load_rows(conn: &Connection, library_path: &str) -> musaic_db::Result<Vec<DiscRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, path, dir_name, dir_artist, dir_album, is_compilation, disc_number, \
                disc_total, track_numbers_json, track_discs_json, album_artist, year, \
                mb_release_group_mbid, missing_since \
         FROM albums WHERE library_path = ?1 ORDER BY id",
    )?;
    let rows = stmt
        .query_map(params![library_path], |r| {
            let discs: Option<String> = r.get(9)?;
            Ok(DiscRow {
                id: r.get(0)?,
                path: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                dir_name: r.get(2)?,
                dir_artist: r.get(3)?,
                dir_album: r.get(4)?,
                is_compilation: r.get::<_, Option<i64>>(5)?.unwrap_or(0) != 0,
                disc_number: r.get(6)?,
                disc_total: r.get(7)?,
                track_numbers: int_list(r.get::<_, Option<String>>(8)?.as_deref()),
                track_discs: discs.map(|d| int_list(Some(&d))),
                album_artist: r.get(10)?,
                year: r.get(11)?,
                release_group: r.get(12)?,
                missing: r
                    .get::<_, Option<String>>(13)?
                    .is_some_and(|m| !m.is_empty()),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// How the tracks of a folder holding several discs are numbered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Numbering {
    /// Each disc starts again at 1.
    PerDisc,
    /// One run across the discs, as in the target layout: only the disc
    /// tags are left to remove.
    Continuous,
    /// Neither (gaps, repeats, files without a number or a disc).
    Irregular,
}

impl Numbering {
    pub fn as_str(self) -> &'static str {
        match self {
            Numbering::PerDisc => "per_disc",
            Numbering::Continuous => "continuous",
            Numbering::Irregular => "irregular",
        }
    }
}

/// The numbering of `(disc, track)` pairs.
pub fn classify_numbering(pairs: &[(Option<i64>, Option<i64>)]) -> Numbering {
    let mut by_disc: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
    for (disc, track) in pairs {
        let (Some(d), Some(t)) = (disc, track) else {
            return Numbering::Irregular;
        };
        by_disc.entry(*d).or_default().push(*t);
    }
    for tracks in by_disc.values_mut() {
        tracks.sort_unstable();
    }
    let runs_from = |start: &dyn Fn(i64) -> i64| {
        let mut offset = 0;
        by_disc.values().all(|tracks| {
            let first = start(offset);
            let ok = tracks
                .iter()
                .enumerate()
                .all(|(i, t)| *t == first + i as i64);
            offset += tracks.len() as i64;
            ok
        })
    };
    if runs_from(&|_| 1) {
        Numbering::PerDisc
    } else if runs_from(&|offset| offset + 1) {
        Numbering::Continuous
    } else {
        Numbering::Irregular
    }
}

/// One disc of a set: the album (folder) that holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetMember {
    pub album_id: String,
    pub dir_name: Option<String>,
    pub disc: i64,
    pub missing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetKind {
    /// One folder holds two or more discs.
    OneFolder {
        discs: Vec<i64>,
        numbering: Numbering,
    },
    /// Discs 1..total, each in a folder of its own, all present.
    Folders { total: i64 },
    /// Some discs of a set are here and others are not.
    Incomplete {
        /// `None` when no folder name says how many discs there are.
        total: Option<i64>,
        present: Vec<i64>,
        absent: Vec<i64>,
    },
}

/// A set found by [`detect`]. `anchor` is the album its finding is stored
/// on: the one folder, disc 1's folder, or (an incomplete set) the folder
/// of the lowest disc present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscSet {
    pub anchor: String,
    pub kind: SetKind,
    /// In disc order. A one-folder set lists its album once per disc.
    pub members: Vec<SetMember>,
}

impl DiscSet {
    /// A member is Missing: the set cannot be judged until it returns.
    pub fn has_missing(&self) -> bool {
        self.members.iter().any(|m| m.missing)
    }

    /// The distinct albums, in disc order.
    pub fn album_ids(&self) -> Vec<&str> {
        let mut seen = BTreeSet::new();
        self.members
            .iter()
            .filter(|m| seen.insert(m.album_id.as_str()))
            .map(|m| m.album_id.as_str())
            .collect()
    }
}

/// What [`detect`] found in one library.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Detection {
    pub sets: Vec<DiscSet>,
    /// Present albums detection could not judge: their per-file disc
    /// numbers have not been read yet although their track numbers repeat
    /// (a scan reads them).
    pub unknown: Vec<String>,
}

fn has_repeats(numbers: &[Option<i64>]) -> bool {
    let mut seen = BTreeSet::new();
    numbers.iter().flatten().any(|n| !seen.insert(*n))
}

fn nonempty(s: &Option<String>) -> Option<&str> {
    s.as_deref().map(strip).filter(|s| !s.is_empty())
}

/// At most one distinct value among the present ones, compared folded.
fn agree<'a>(values: impl Iterator<Item = Option<&'a str>>) -> bool {
    values.flatten().map(fold).collect::<BTreeSet<_>>().len() <= 1
}

/// The one-folder finding of a Present row, if any. `Err(())` = unknown.
fn one_folder(row: &DiscRow) -> Result<Option<SetKind>, ()> {
    let Some(discs) = &row.track_discs else {
        // A folder named as one disc is judged with the other folders; a
        // repeat in its numbers needs the disc tags to be told apart from
        // a track-gaps duplicate.
        if row.disc_number.is_none() && has_repeats(&row.track_numbers) {
            return Err(());
        }
        return Ok(None);
    };
    if discs.len() != row.track_numbers.len() {
        return Ok(None);
    }
    let distinct: BTreeSet<i64> = discs.iter().flatten().copied().collect();
    if distinct.len() < 2 {
        return Ok(None);
    }
    let pairs: Vec<(Option<i64>, Option<i64>)> = discs
        .iter()
        .copied()
        .zip(row.track_numbers.iter().copied())
        .collect();
    Ok(Some(SetKind::OneFolder {
        discs: distinct.into_iter().collect(),
        numbering: classify_numbering(&pairs),
    }))
}

/// Folder rows grouped as candidate discs of one set: same parent, same
/// folded folder artist (or both compilations), same folded folder album.
type GroupKey = (String, bool, String, String);

fn group_key(row: &DiscRow) -> Option<GroupKey> {
    let disc = row.disc_number?;
    if disc < 1 {
        return None;
    }
    let album = nonempty(&row.dir_album)?;
    let artist = if row.is_compilation {
        String::new()
    } else {
        fold(nonempty(&row.dir_artist)?)
    };
    // The scan stores siblings' paths from one listing of their parent, so
    // the parent's spelling is the same for each; the preview checks the
    // folders on disk (same-file) before anything would move.
    let parent = Path::new(&row.path)
        .parent()?
        .to_string_lossy()
        .into_owned();
    Some((parent, row.is_compilation, artist, fold(album)))
}

/// The set a group of disc folders forms, if every rule holds.
fn folder_set(rows: &[&DiscRow]) -> Option<DiscSet> {
    let mut rows: Vec<&DiscRow> = rows.to_vec();
    rows.sort_by(|a, b| (a.disc_number, &a.id).cmp(&(b.disc_number, &b.id)));
    let discs: Vec<i64> = rows.iter().filter_map(|r| r.disc_number).collect();
    // Two folders claiming one disc (two editions side by side): which
    // belongs to the set is not something to guess.
    if discs.iter().collect::<BTreeSet<_>>().len() != discs.len() {
        return None;
    }
    let total = rows[0].disc_total;
    if rows.iter().any(|r| r.disc_total != total) {
        return None;
    }
    // A file whose own disc tag names another disc than its folder.
    let tags_agree = rows.iter().all(|r| {
        r.track_discs
            .iter()
            .flatten()
            .flatten()
            .all(|d| Some(*d) == r.disc_number)
    });
    if !tags_agree
        || !agree(rows.iter().map(|r| nonempty(&r.album_artist)))
        || !agree(rows.iter().map(|r| nonempty(&r.year)))
        || !agree(rows.iter().map(|r| nonempty(&r.release_group)))
    {
        return None;
    }
    let present: BTreeSet<i64> = discs.iter().copied().collect();
    let kind = match total {
        Some(t) => {
            if t < 2 || present.iter().any(|d| *d > t) {
                return None;
            }
            let absent: Vec<i64> = (1..=t).filter(|d| !present.contains(d)).collect();
            if absent.is_empty() {
                SetKind::Folders { total: t }
            } else {
                SetKind::Incomplete {
                    total: Some(t),
                    present: present.iter().copied().collect(),
                    absent,
                }
            }
        }
        None => {
            let max = *present.iter().next_back()?;
            let absent: Vec<i64> = (1..=max).filter(|d| !present.contains(d)).collect();
            if absent.is_empty() {
                // "(disc 1)", "(disc 2)": the names do not say how many
                // there are, so the discs present from 1 on are the set. A
                // lone "(disc 1)" is just an album.
                if max < 2 {
                    return None;
                }
                SetKind::Folders { total: max }
            } else {
                SetKind::Incomplete {
                    total: None,
                    present: present.iter().copied().collect(),
                    absent,
                }
            }
        }
    };
    let members: Vec<SetMember> = rows
        .iter()
        .map(|r| SetMember {
            album_id: r.id.clone(),
            dir_name: r.dir_name.clone(),
            disc: r.disc_number.unwrap_or(0),
            missing: r.missing,
        })
        .collect();
    Some(DiscSet {
        anchor: members[0].album_id.clone(),
        kind,
        members,
    })
}

/// Every set in one library's rows ([`load_rows`]). Sets with a Missing
/// member are included (see [`DiscSet::has_missing`]); a one-folder set is
/// looked for only on Present rows.
pub fn detect(rows: &[DiscRow]) -> Detection {
    let mut out = Detection::default();
    let mut groups: BTreeMap<GroupKey, Vec<&DiscRow>> = BTreeMap::new();
    for row in rows {
        if let Some(key) = group_key(row) {
            groups.entry(key).or_default().push(row);
        }
        if row.missing {
            continue;
        }
        match one_folder(row) {
            Err(()) => out.unknown.push(row.id.clone()),
            Ok(None) => {}
            Ok(Some(kind)) => {
                let SetKind::OneFolder { discs, .. } = &kind else {
                    unreachable!("one_folder returns a one-folder kind")
                };
                let members = discs
                    .iter()
                    .map(|d| SetMember {
                        album_id: row.id.clone(),
                        dir_name: row.dir_name.clone(),
                        disc: *d,
                        missing: false,
                    })
                    .collect();
                out.sets.push(DiscSet {
                    anchor: row.id.clone(),
                    kind,
                    members,
                });
            }
        }
    }
    // How many parents hold discs of each title: discs of one title under
    // two parents are neither one set nor provably incomplete.
    let mut parents: BTreeMap<(bool, &str, &str), usize> = BTreeMap::new();
    for (_, comp, artist, album) in groups.keys() {
        *parents.entry((*comp, artist, album)).or_default() += 1;
    }
    for ((_, comp, artist, album), members) in &groups {
        let Some(set) = folder_set(members) else {
            continue;
        };
        if matches!(set.kind, SetKind::Incomplete { .. })
            && parents[&(*comp, artist.as_str(), album.as_str())] > 1
        {
            continue;
        }
        out.sets.push(set);
    }
    out
}

// ---------- the conversion preview ----------

/// Why the plan could not run as it stands. `code` is stable (the UI and
/// tests key on it); `message` is a plain sentence for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocker {
    pub code: &'static str,
    pub message: String,
}

pub mod blocker {
    pub const MEMBER_MISSING: &str = "member_missing";
    pub const MEMBER_UNREADABLE: &str = "member_unreadable";
    pub const DIFFERENT_PARENT: &str = "different_parent";
    pub const DIFFERENT_VOLUME: &str = "different_volume";
    pub const TRACK_NUMBERS: &str = "track_numbers";
    pub const DISC_UNKNOWN: &str = "disc_unknown";
    pub const DISC_MISMATCH: &str = "disc_mismatch";
    pub const DISC_GAP: &str = "disc_gap";
    pub const MISSING_TITLE: &str = "missing_title";
    pub const NAME_COLLISION: &str = "name_collision";
    pub const FOLDER_NAME: &str = "folder_name";
    pub const TARGET_TAKEN: &str = "target_taken";
    pub const JOB_ACTIVE: &str = "job_active";
    pub const RENDER_ERROR: &str = "render_error";
    /// A file left under a rename's temporary name.
    pub const STRANDED_TEMP: &str = "stranded_temp";
    /// The folder no longer matches the record of an interrupted
    /// conversion (a file added, gone or changed).
    pub const RESUME_MISMATCH: &str = "resume_mismatch";
    /// The plan made now differs from the one the interrupted conversion
    /// recorded (a template or a title changed since).
    pub const RESUME_CHANGED: &str = "resume_changed";
    /// More than one unfinished conversion names this album.
    pub const RESUME_AMBIGUOUS: &str = "resume_ambiguous";
}

/// One FLAC of a member as read now.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileFacts {
    pub name: String,
    /// `false` when the file's tags could not be read.
    pub readable: bool,
    /// The raw TRACKNUMBER.
    pub tracknumber: String,
    pub title: String,
    pub artist: String,
    pub albumartist: String,
    /// [`flacmeta::file_disc`].
    pub disc: Option<i64>,
    /// The file carries DISCNUMBER, DISCTOTAL or TOTALDISCS.
    pub has_disc_tags: bool,
    /// Every value of the numbering tags a conversion reads and writes.
    pub numbering: NumberingTags,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryKind {
    /// A cover file name the scan looks for, with its content.
    Cover(Vec<u8>),
    /// The per-album track-tag sidecar.
    Sidecar,
    /// Operating-system clutter (`.DS_Store`, `Thumbs.db`, AppleDouble).
    Junk,
    /// Anything else that is not a FLAC.
    Other,
}

/// A folder entry that is not one of the member's FLACs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryFacts {
    pub name: String,
    pub kind: EntryKind,
}

/// One member album as found on disk now.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MemberFacts {
    pub album_id: String,
    /// The folder's name.
    pub dir_name: String,
    /// The disc the folder name gives (a folder set); `None` for a folder
    /// that holds several discs.
    pub folder_disc: Option<i64>,
    pub files: Vec<FileFacts>,
    pub entries: Vec<EntryFacts>,
    /// `Some(blocker)` when the folder could not be looked at.
    pub problem: Option<Blocker>,
    /// Entries named as a rename's temporary file (`.<name>.renaming.tmp`):
    /// a rename stopped half-way and a file may be under such a name.
    pub stranded: Vec<String>,
}

/// What [`plan_conversion`] plans from.
#[derive(Debug, Clone)]
pub struct PlanInput<'a> {
    /// In disc order; the first is the destination (disc 1's folder, or
    /// the one folder).
    pub members: &'a [MemberFacts],
    /// The destination's album row: album, year and artist fallbacks for
    /// the names, and the folder name. Its disc fields are ignored.
    pub target_row: &'a RenamePlanRow,
    pub templates: &'a Templates,
}

/// One file of the plan, old → new.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedTrack {
    pub album_id: String,
    pub filename: String,
    pub title: String,
    pub disc: Option<i64>,
    /// The raw TRACKNUMBER and its number.
    pub tracknumber: String,
    pub track: Option<i64>,
    /// Earlier discs' track count plus the track's number on its disc.
    pub new_track: Option<i64>,
    pub new_filename: Option<String>,
    pub has_disc_tags: bool,
    /// The numbering tags on disk now.
    pub numbering: NumberingTags,
    /// The file already has its new numbering and no disc tags (an
    /// interrupted conversion wrote it); `disc`, `tracknumber` and
    /// `track` are then the values its record kept from before.
    pub tags_done: bool,
}

/// What happens to one cover file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverAction {
    /// The destination's own copy: it stays.
    Kept,
    /// The first copy of this name, from another disc's folder: it moves
    /// into the destination.
    Moved,
    /// The same bytes as the copy kept under this name: it is not needed.
    Identical,
    /// Other bytes than the copy kept under this name: it goes to the
    /// app's trash (the destination keeps its own).
    Different,
}

impl CoverAction {
    pub fn as_str(self) -> &'static str {
        match self {
            CoverAction::Kept => "kept",
            CoverAction::Moved => "moved",
            CoverAction::Identical => "identical",
            CoverAction::Different => "different",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverPlan {
    pub name: String,
    pub action: CoverAction,
}

/// The non-FLAC files of one member folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderPlan {
    pub album_id: String,
    pub dir_name: String,
    pub disc: Option<i64>,
    pub destination: bool,
    pub covers: Vec<CoverPlan>,
    /// The folder has a track-tag sidecar (its tags are merged into the
    /// destination's).
    pub sidecar: bool,
    /// Files that are neither FLACs, covers, the sidecar nor clutter: they
    /// stay where they are.
    pub other_files: Vec<String>,
}

/// [`plan_conversion`]'s result.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Planned {
    /// In disc order, then track order.
    pub tracks: Vec<PlannedTrack>,
    /// The new TRACKTOTAL.
    pub track_total: usize,
    /// The destination folder's new name (equal to its current one when
    /// it already fits).
    pub target_name: Option<String>,
    pub folders: Vec<FolderPlan>,
    pub blockers: Vec<Blocker>,
}

fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

fn join_numbers(v: &[i64]) -> String {
    v.iter().map(i64::to_string).collect::<Vec<_>>().join(", ")
}

/// Missing and repeated numbers of `tracks` against `first..first+n`.
fn describe_run(tracks: &[i64], first: i64) -> String {
    let n = tracks.len() as i64;
    let have: BTreeSet<i64> = tracks.iter().copied().collect();
    let missing: Vec<i64> = (first..first + n).filter(|t| !have.contains(t)).collect();
    let mut seen = BTreeSet::new();
    let repeated: Vec<i64> = tracks
        .iter()
        .copied()
        .filter(|t| !seen.insert(*t))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut parts = Vec::new();
    if !missing.is_empty() {
        parts.push(format!("no track {}", join_numbers(&missing)));
    }
    if !repeated.is_empty() {
        parts.push(format!("track {} more than once", join_numbers(&repeated)));
    }
    if parts.is_empty() {
        parts.push("numbers outside the run".into());
    }
    parts.join(", ")
}

/// The conversion plan for members as they are on disk now. Pure: the
/// caller gathers the facts and adds what only the disk can say (the
/// destination name being taken, the volumes, running jobs).
pub fn plan_conversion(input: &PlanInput<'_>) -> Planned {
    let mut out = Planned::default();
    let one_folder = input.members.len() == 1 && input.members[0].folder_disc.is_none();
    for m in input.members {
        if let Some(p) = &m.problem {
            out.blockers.push(p.clone());
        }
        for name in &m.stranded {
            out.blockers.push(Blocker {
                code: blocker::STRANDED_TEMP,
                message: format!(
                    "{name} in {} is a file left under a temporary name by a rename that did not \
finish. Give it back its own name (without the leading dot and the .renaming ending), then try again.",
                    m.dir_name
                ),
            });
        }
    }
    let (template, dir_template) = input.templates.for_row(input.target_row);

    // Every file with its member and track number, grouped by disc.
    type OnDisc<'m> = (&'m MemberFacts, &'m FileFacts, Option<i64>);
    let mut by_disc: BTreeMap<i64, Vec<OnDisc<'_>>> = BTreeMap::new();
    let mut unknown_disc = Vec::new();
    for m in input.members {
        for f in &m.files {
            if !f.readable {
                out.blockers.push(Blocker {
                    code: blocker::MEMBER_UNREADABLE,
                    message: format!("{} in {} could not be read.", f.name, m.dir_name),
                });
                continue;
            }
            let disc = match m.folder_disc {
                Some(folder) => {
                    if f.disc.is_some_and(|d| d != folder) {
                        out.blockers.push(Blocker {
                            code: blocker::DISC_MISMATCH,
                            message: format!(
                                "{} is tagged as disc {} but is in the folder of disc {folder}.",
                                f.name,
                                f.disc.unwrap_or_default()
                            ),
                        });
                    }
                    Some(folder)
                }
                None => f.disc,
            };
            match disc {
                Some(d) => {
                    by_disc
                        .entry(d)
                        .or_default()
                        .push((m, f, parse_tracknum(Some(&f.tracknumber))))
                }
                None => unknown_disc.push(f.name.clone()),
            }
        }
    }
    if !unknown_disc.is_empty() {
        out.blockers.push(Blocker {
            code: blocker::DISC_UNKNOWN,
            message: format!(
                "{} {} no disc number, so {} disc {} on cannot be told: {}.",
                plural(unknown_disc.len(), "file"),
                if unknown_disc.len() == 1 {
                    "has"
                } else {
                    "have"
                },
                if unknown_disc.len() == 1 {
                    "its"
                } else {
                    "their"
                },
                if unknown_disc.len() == 1 {
                    "it is"
                } else {
                    "they are"
                },
                unknown_disc.join(", ")
            ),
        });
    }
    if one_folder {
        let discs: Vec<i64> = by_disc.keys().copied().collect();
        let gaps: Vec<i64> = (1..=discs.last().copied().unwrap_or(0))
            .filter(|d| !by_disc.contains_key(d))
            .collect();
        if !gaps.is_empty() {
            out.blockers.push(Blocker {
                code: blocker::DISC_GAP,
                message: format!(
                    "The files name discs {} but no disc {}: the numbering would be wrong once it is added.",
                    join_numbers(&discs),
                    join_numbers(&gaps)
                ),
            });
        }
    }

    // New numbers: each disc must run 1..n, or already continue the
    // earlier discs (a conversion that stopped half-way).
    let mut offset = 0i64;
    let mut planned_names: Vec<(String, String)> = Vec::new();
    for (disc, files) in &by_disc {
        let mut files = files.clone();
        files.sort_by(|a, b| (a.2.is_none(), a.2, &a.1.name).cmp(&(b.2.is_none(), b.2, &b.1.name)));
        let numbers: Vec<i64> = files.iter().filter_map(|f| f.2).collect();
        let n = files.len() as i64;
        let runs = |first: i64| {
            numbers.len() as i64 == n
                && numbers
                    .iter()
                    .enumerate()
                    .all(|(i, t)| *t == first + i as i64)
        };
        let shift = if runs(1) {
            Some(offset)
        } else if offset > 0 && runs(offset + 1) {
            Some(0)
        } else {
            let unnumbered = files.iter().filter(|f| f.2.is_none()).count();
            let why = if unnumbered > 0 {
                format!("{} without a track number", plural(unnumbered, "file"))
            } else {
                describe_run(&numbers, 1)
            };
            out.blockers.push(Blocker {
                code: blocker::TRACK_NUMBERS,
                message: format!(
                    "Disc {disc}'s tracks are not numbered 1 to {n} ({why}). Fix the track gaps first."
                ),
            });
            None
        };
        for (m, f, track) in files {
            let new_track = shift.and_then(|s| track.map(|t| t + s));
            let new_filename = match new_track {
                Some(t) if !f.title.is_empty() => {
                    let tags = TrackTags {
                        title: &f.title,
                        artist: &f.artist,
                        albumartist: &f.albumartist,
                    };
                    match render_track_stem(template, &tags, input.target_row, &t.to_string(), None)
                    {
                        Ok(stem) if !stem.is_empty() => Some(format!("{stem}.flac")),
                        // A conversion never renames a file to nothing.
                        Ok(_) => {
                            out.blockers.push(Blocker {
                                code: blocker::RENDER_ERROR,
                                message: format!(
                                    "The filename template gives no name for {}.",
                                    f.name
                                ),
                            });
                            None
                        }
                        Err(e) => {
                            out.blockers.push(Blocker {
                                code: blocker::RENDER_ERROR,
                                message: format!(
                                    "The filename template could not be applied to {}: {e}.",
                                    f.name
                                ),
                            });
                            None
                        }
                    }
                }
                _ => None,
            };
            if new_track.is_some() && f.title.is_empty() {
                out.blockers.push(Blocker {
                    code: blocker::MISSING_TITLE,
                    message: format!(
                        "{} has no title, so its new name cannot be made from the filename template.",
                        f.name
                    ),
                });
            }
            if let Some(n) = &new_filename {
                planned_names.push((n.clone(), f.name.clone()));
            }
            out.tracks.push(PlannedTrack {
                album_id: m.album_id.clone(),
                filename: f.name.clone(),
                title: f.title.clone(),
                disc: Some(*disc),
                tracknumber: f.tracknumber.clone(),
                track,
                new_track,
                new_filename,
                has_disc_tags: f.has_disc_tags,
                numbering: f.numbering.clone(),
                tags_done: false,
            });
        }
        offset += n;
    }
    out.track_total = out.tracks.len();

    // Two files, or a file and something already in the destination, must
    // not end up with one name as an insensitive volume compares them.
    let mut taken: HashMap<String, String> = HashMap::new();
    for (new, old) in &planned_names {
        if let Some(first) = taken.insert(fold(new), old.clone()) {
            out.blockers.push(Blocker {
                code: blocker::NAME_COLLISION,
                message: format!("{first} and {old} would both be named {new}."),
            });
        }
    }
    if let Some(dest) = input.members.first() {
        for e in &dest.entries {
            if let Some(old) = taken.get(&fold(&e.name)) {
                out.blockers.push(Blocker {
                    code: blocker::NAME_COLLISION,
                    message: format!(
                        "{old} would be renamed to {}, which is already taken in {}.",
                        e.name, dest.dir_name
                    ),
                });
            }
        }
    }

    // The destination folder's name: the directory template, no disc.
    let mut row = input.target_row.clone();
    row.disc_number = None;
    row.disc_total = None;
    match render_dir_name(dir_template, &row) {
        Ok(Some(name)) => {
            let current = input.members.first().map(|m| m.dir_name.as_str());
            out.target_name = Some(match current {
                Some(c) if same_nfc(c, &name) => c.to_string(),
                _ => name,
            });
        }
        Ok(None) => out.blockers.push(Blocker {
            code: blocker::FOLDER_NAME,
            message: "The folder template gives no name for this album (it has no album title)."
                .into(),
        }),
        Err(e) => out.blockers.push(Blocker {
            code: blocker::RENDER_ERROR,
            message: format!("The folder template could not be applied: {e}."),
        }),
    }

    if !one_folder {
        out.folders = plan_folders(input.members);
    }
    out
}

/// Covers, sidecars and other files of each folder: the destination keeps
/// its own copy of a name; another folder's copy moves when the name is
/// new, and otherwise is a duplicate (same bytes) or goes to the trash.
fn plan_folders(members: &[MemberFacts]) -> Vec<FolderPlan> {
    let mut kept: HashMap<String, &[u8]> = HashMap::new();
    let mut out = Vec::new();
    for (i, m) in members.iter().enumerate() {
        let mut covers = Vec::new();
        let mut other_files = Vec::new();
        let mut sidecar = false;
        for e in &m.entries {
            match &e.kind {
                EntryKind::Cover(bytes) => {
                    let key = fold(&e.name);
                    let action = match kept.get(&key) {
                        None => {
                            kept.insert(key, bytes);
                            if i == 0 {
                                CoverAction::Kept
                            } else {
                                CoverAction::Moved
                            }
                        }
                        Some(k) if *k == bytes.as_slice() => CoverAction::Identical,
                        Some(_) => CoverAction::Different,
                    };
                    covers.push(CoverPlan {
                        name: e.name.clone(),
                        action,
                    });
                }
                EntryKind::Sidecar => sidecar = true,
                EntryKind::Junk => {}
                EntryKind::Other => other_files.push(e.name.clone()),
            }
        }
        out.push(FolderPlan {
            album_id: m.album_id.clone(),
            dir_name: m.dir_name.clone(),
            disc: m.folder_disc,
            destination: i == 0,
            covers,
            sidecar,
            other_files,
        });
    }
    out
}

/// How a folder entry that is not a FLAC is treated.
pub fn entry_kind_of(name: &str) -> Option<EntryKind> {
    let lower = name.to_lowercase();
    if name == SIDECAR_FILENAME {
        return Some(EntryKind::Sidecar);
    }
    if name.starts_with("._") || [".ds_store", "thumbs.db", "desktop.ini"].contains(&lower.as_str())
    {
        return Some(EntryKind::Junk);
    }
    if COVER_FILENAMES.contains(&fold(name).as_str()) {
        return None; // the caller reads the bytes
    }
    Some(EntryKind::Other)
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Read one member folder now.
pub fn gather_member(album_id: &str, folder: &Path, folder_disc: Option<i64>) -> MemberFacts {
    let dir_name = file_name(folder);
    let mut facts = MemberFacts {
        album_id: album_id.to_string(),
        dir_name: dir_name.clone(),
        folder_disc,
        ..Default::default()
    };
    if !folder.is_dir() {
        facts.problem = Some(Blocker {
            code: blocker::MEMBER_MISSING,
            message: format!("The folder {dir_name} is not on disk right now."),
        });
        return facts;
    }
    let names: Vec<String> = match std::fs::read_dir(folder) {
        Ok(rd) => {
            let mut v = Vec::new();
            for e in rd {
                match e {
                    Ok(e) => v.push(e.file_name().to_string_lossy().into_owned()),
                    Err(e) => {
                        facts.problem = Some(unreadable(&dir_name, &e));
                        return facts;
                    }
                }
            }
            v.sort();
            v
        }
        Err(e) => {
            facts.problem = Some(unreadable(&dir_name, &e));
            return facts;
        }
    };
    let flacs = list_flacs(folder);
    let flac_names: BTreeSet<String> = flacs.iter().map(|p| file_name(p)).collect();
    for path in &flacs {
        let name = file_name(path);
        let mut f = FileFacts {
            name,
            ..Default::default()
        };
        if let Ok(meta) = flacmeta::read(path) {
            let first = |k: &str| meta.values(k).next().unwrap_or("").to_string();
            f.readable = true;
            f.tracknumber = first("tracknumber");
            f.title = first("title");
            f.artist = first("artist");
            f.albumartist = first("albumartist");
            f.disc = flacmeta::file_disc(meta.values("discnumber"), meta.values("tracknumber"));
            f.has_disc_tags = ["discnumber", "disctotal", "totaldiscs"]
                .iter()
                .any(|k| meta.values(k).next().is_some());
            f.numbering = numbering_of(|k| meta.values(k).collect());
        }
        facts.files.push(f);
    }
    for name in names {
        if flac_names.contains(&name) {
            continue;
        }
        if is_rename_temp(&name) {
            facts.stranded.push(name.clone());
        }
        let kind = match entry_kind_of(&name) {
            Some(k) => k,
            None => match std::fs::read(folder.join(&name)) {
                Ok(bytes) => EntryKind::Cover(bytes),
                // A directory named like a cover, or one that cannot be
                // read: not a cover to compare.
                Err(_) => EntryKind::Other,
            },
        };
        facts.entries.push(EntryFacts { name, kind });
    }
    facts
}

/// A name the rename code gives a file while it moves it
/// (`fsutil::temp_name`): `.<name>.renaming.tmp`, `.<name>.renaming.<n>.tmp`
/// or `.musaic-<n>.tmp`.
fn is_rename_temp(name: &str) -> bool {
    let Some(rest) = name.strip_prefix('.') else {
        return false;
    };
    let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    if let Some(n) = rest
        .strip_prefix("musaic-")
        .and_then(|r| r.strip_suffix(".tmp"))
    {
        return digits(n);
    }
    let Some(stem) = rest.strip_suffix(".tmp") else {
        return false;
    };
    if stem.ends_with(".renaming") {
        return true;
    }
    stem.rsplit_once(".renaming.")
        .is_some_and(|(_, n)| digits(n))
}

fn unreadable(dir_name: &str, e: &std::io::Error) -> Blocker {
    Blocker {
        code: blocker::MEMBER_UNREADABLE,
        message: format!("The folder {dir_name} could not be read ({e})."),
    }
}

/// What the preview shows for a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewState {
    OneFolder,
    Folders,
    Incomplete,
    /// The albums no longer form a set this check reports.
    NotASet,
}

impl PreviewState {
    pub fn as_str(self) -> &'static str {
        match self {
            PreviewState::OneFolder => "one_folder",
            PreviewState::Folders => "folders",
            PreviewState::Incomplete => "incomplete",
            PreviewState::NotASet => "not_a_set",
        }
    }
}

/// The conversion preview of one finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversionPreview {
    pub album_id: String,
    pub state: PreviewState,
    /// Why no conversion is offered (an incomplete set, or no set).
    pub reason: Option<String>,
    pub numbering: Option<Numbering>,
    pub disc_total: Option<i64>,
    pub present_discs: Vec<i64>,
    pub absent_discs: Vec<i64>,
    pub members: Vec<SetMember>,
    pub is_compilation: bool,
    pub template: String,
    pub dir_template: String,
    /// The destination folder's name now.
    pub target_current: Option<String>,
    /// The destination folder's path now (as the album row stores it).
    pub target_path: Option<String>,
    /// The library the destination's row belongs to.
    pub library_path: String,
    pub plan: Planned,
    /// The record of an interrupted conversion of this album, when one is
    /// what the plan was re-derived from.
    pub resume: Option<Record>,
}

impl ConversionPreview {
    /// A conversion could run as planned.
    pub fn convertible(&self) -> bool {
        matches!(self.state, PreviewState::OneFolder | PreviewState::Folders)
            && self.plan.blockers.is_empty()
    }

    /// A conversion can be started from the preview: a convertible set in
    /// one folder (sets in several folders cannot be converted yet).
    pub fn conversion_available(&self) -> bool {
        self.state == PreviewState::OneFolder && self.convertible()
    }

    /// A digest of exactly what a conversion would do now: every file's
    /// name, numbering tags and planned result, and the folder's old and
    /// new name. The conversion is refused unless the client sends back
    /// the digest of the preview the user saw, so a plan that changed in
    /// between is never carried out.
    pub fn digest(&self) -> String {
        use sha2::{Digest, Sha256};
        let tracks: Vec<serde_json::Value> = self
            .plan
            .tracks
            .iter()
            .map(|t| {
                serde_json::json!([
                    t.album_id,
                    t.filename,
                    t.disc,
                    t.track,
                    t.new_track,
                    t.new_filename,
                    t.tags_done,
                    t.numbering
                        .iter()
                        .map(|(k, v)| serde_json::json!([k, v]))
                        .collect::<Vec<_>>(),
                ])
            })
            .collect();
        let doc = serde_json::json!({
            "album_id": self.album_id,
            "state": self.state.as_str(),
            "target_path": self.target_path,
            "target_current": self.target_current,
            "target_new": self.plan.target_name,
            "track_total": self.plan.track_total,
            "tracks": tracks,
            "blockers": self.plan.blockers.iter().map(|b| b.code).collect::<Vec<_>>(),
            "resume": self.resume.as_ref().map(|r| r.path.to_string_lossy().into_owned()),
        });
        let hash = Sha256::digest(doc.to_string().as_bytes());
        hash.iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// Whether any of `ids` has an AI job queued or running.
fn active_jobs(conn: &Connection, ids: &[&str]) -> musaic_db::Result<Vec<String>> {
    let json = serde_json::Value::from(ids.to_vec()).to_string();
    let mut stmt = conn.prepare(
        "SELECT DISTINCT album_id FROM ai_jobs WHERE status IN ('queued', 'running') \
         AND album_id IN (SELECT value FROM json_each(?1)) ORDER BY album_id",
    )?;
    let v = stmt
        .query_map([json], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(v)
}

#[cfg(unix)]
fn volume_of(p: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(p).ok().map(|m| m.dev())
}

// Elsewhere the same-parent test stands in: siblings share a volume unless
// one is a mount point of its own.
#[cfg(not(unix))]
fn volume_of(_: &Path) -> Option<u64> {
    None
}

/// Checks only the disk can answer: every member folder in the
/// destination's parent and on its volume, the destination's new name
/// free (or already the destination itself, same-file).
fn disk_blockers(folders: &[(String, PathBuf)], target_name: Option<&str>) -> Vec<Blocker> {
    let mut out = Vec::new();
    let Some((_, dest)) = folders.first() else {
        return out;
    };
    let parent = dest.parent().map(Path::to_path_buf).unwrap_or_default();
    for (name, path) in folders.iter().skip(1) {
        let same_parent = path
            .parent()
            .is_some_and(|p| same_file::is_same_file(p, &parent).unwrap_or(false));
        if !same_parent {
            out.push(Blocker {
                code: blocker::DIFFERENT_PARENT,
                message: format!("{name} is not in the same folder as {}.", file_name(dest)),
            });
        } else if let (Some(a), Some(b)) = (volume_of(path), volume_of(dest))
            && a != b
        {
            out.push(Blocker {
                code: blocker::DIFFERENT_VOLUME,
                message: format!("{name} is on another drive than {}.", file_name(dest)),
            });
        }
    }
    if let Some(target) = target_name {
        let folded = fold(target);
        match std::fs::read_dir(&parent) {
            Ok(rd) => {
                for e in rd.flatten() {
                    let n = e.file_name().to_string_lossy().into_owned();
                    if fold(&n) == folded
                        && !same_file::is_same_file(parent.join(&n), dest).unwrap_or(false)
                    {
                        out.push(Blocker {
                            code: blocker::TARGET_TAKEN,
                            message: format!(
                                "The new folder name {target} is already used by another folder ({n})."
                            ),
                        });
                    }
                }
            }
            Err(e) => out.push(Blocker {
                code: blocker::MEMBER_UNREADABLE,
                message: format!(
                    "The folder that holds {} could not be read ({e}).",
                    file_name(dest)
                ),
            }),
        }
    }
    out
}

fn incomplete_reason(total: Option<i64>, present: &[i64], absent: &[i64]) -> String {
    let of = total.map(|t| format!(" of {t}")).unwrap_or_default();
    format!(
        "Only disc{} {}{of} {} here; disc{} {} {} not. No conversion is offered for a set with a \
disc missing: the track numbers would be wrong once the missing disc is added.",
        if present.len() == 1 { "" } else { "s" },
        join_numbers(present),
        if present.len() == 1 { "is" } else { "are" },
        if absent.len() == 1 { "" } else { "s" },
        join_numbers(absent),
        if absent.len() == 1 { "is" } else { "are" },
    )
}

/// The conversion preview for the finding stored on `album_id`,
/// re-derived from the current database and disk. `None` when the album
/// row is gone.
///
/// `logs` is the folder of conversion records ([`conversion_log`]). When it
/// holds an unfinished conversion of this album, the plan is re-derived
/// with the record's help ([`derive_resumed`]): the files it already wrote
/// no longer carry their disc, so the disk alone cannot say where they
/// belong.
pub fn derive_conversion(
    conn: &Connection,
    album_id: &str,
    logs: Option<&Path>,
) -> musaic_db::Result<Option<ConversionPreview>> {
    if let Some(dir) = logs {
        let records = conversion_log::unfinished_of(dir, album_id);
        if !records.is_empty() {
            return derive_resumed(conn, album_id, records);
        }
    }
    derive_fresh(conn, album_id)
}

fn derive_fresh(conn: &Connection, album_id: &str) -> musaic_db::Result<Option<ConversionPreview>> {
    let library: Option<String> = conn
        .query_row(
            "SELECT library_path FROM albums WHERE id = ?1",
            [album_id],
            |r| r.get(0),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            e => Err(e),
        })?;
    let Some(library) = library else {
        return Ok(None);
    };
    let templates = Templates::from_settings(conn)?;
    let rows = load_rows(conn, &library)?;
    let detection = detect(&rows);
    let set = detection
        .sets
        .iter()
        .find(|s| s.anchor == album_id)
        .or_else(|| {
            detection
                .sets
                .iter()
                .find(|s| s.members.iter().any(|m| m.album_id == album_id))
        });
    let row_of = |id: &str| rows.iter().find(|r| r.id == id);
    let mut preview = ConversionPreview {
        album_id: album_id.to_string(),
        state: PreviewState::NotASet,
        reason: None,
        numbering: None,
        disc_total: None,
        present_discs: Vec::new(),
        absent_discs: Vec::new(),
        members: Vec::new(),
        is_compilation: row_of(album_id).is_some_and(|r| r.is_compilation),
        template: String::new(),
        dir_template: String::new(),
        target_current: None,
        target_path: None,
        library_path: library.clone(),
        plan: Planned::default(),
        resume: None,
    };
    let Some(set) = set else {
        preview.reason = Some(
            "These folders no longer form a multi-disc set that needs converting. The finding \
clears the next time the check runs."
                .into(),
        );
        return Ok(Some(preview));
    };
    preview.members = set.members.clone();
    let anchor = row_of(&set.anchor);
    preview.is_compilation = anchor.is_some_and(|r| r.is_compilation);
    match &set.kind {
        SetKind::Incomplete {
            total,
            present,
            absent,
        } => {
            preview.state = PreviewState::Incomplete;
            preview.disc_total = *total;
            preview.present_discs = present.clone();
            preview.absent_discs = absent.clone();
            preview.reason = Some(incomplete_reason(*total, present, absent));
            return Ok(Some(preview));
        }
        SetKind::OneFolder { discs, numbering } => {
            preview.state = PreviewState::OneFolder;
            preview.numbering = Some(*numbering);
            preview.present_discs = discs.clone();
        }
        SetKind::Folders { total } => {
            preview.state = PreviewState::Folders;
            preview.disc_total = Some(*total);
            preview.present_discs = (1..=*total).collect();
        }
    }
    let one_folder = preview.state == PreviewState::OneFolder;
    let ids = set.album_ids();
    let mut facts = Vec::new();
    let mut folders = Vec::new();
    for id in &ids {
        let Some(r) = row_of(id) else { continue };
        let disc = if one_folder { None } else { r.disc_number };
        let path = PathBuf::from(&r.path);
        let mut f = gather_member(id, &path, disc);
        if r.missing {
            f.problem = Some(Blocker {
                code: blocker::MEMBER_MISSING,
                message: format!(
                    "{} is marked Missing: its folder was not found by the last scan.",
                    r.dir_name.clone().unwrap_or_else(|| r.id.clone())
                ),
            });
        }
        folders.push((f.dir_name.clone(), path));
        facts.push(f);
    }
    let target_row = rename_plan_row(conn, &set.anchor)?;
    let mut plan = match &target_row {
        Some(row) => {
            let (t, d) = templates.for_row(row);
            preview.template = t.to_string();
            preview.dir_template = d.to_string();
            plan_conversion(&PlanInput {
                members: &facts,
                target_row: row,
                templates: &templates,
            })
        }
        // The destination is Missing: its member blocker says so.
        None => Planned {
            blockers: facts.iter().filter_map(|f| f.problem.clone()).collect(),
            ..Default::default()
        },
    };
    preview.target_current = facts.first().map(|f| f.dir_name.clone());
    preview.target_path = row_of(&set.anchor).map(|r| r.path.clone());
    if facts.iter().all(|f| f.problem.is_none()) {
        let target = plan
            .target_name
            .as_deref()
            .filter(|t| preview.target_current.as_deref() != Some(*t));
        plan.blockers.extend(disk_blockers(&folders, target));
    }
    for id in active_jobs(conn, &ids)? {
        let name = row_of(&id).and_then(|r| r.dir_name.clone()).unwrap_or(id);
        plan.blockers.push(Blocker {
            code: blocker::JOB_ACTIVE,
            message: format!("A job is queued or running on {name}. Wait for it to finish."),
        });
    }
    preview.plan = plan;
    Ok(Some(preview))
}

/// The preview of an album that has an unfinished conversion record.
///
/// Every FLAC in the folder must be one of the record's files, under its
/// old or its new name, with either its old numbering tags (not written
/// yet) or exactly its new ones (written). For planning only, a file
/// already written gets back the disc and number the record kept, so the
/// plan is made as for the untouched set; it must then come out as the
/// record says. Any difference (a file added, gone or retagged, a template
/// or a title changed) is a blocker, never a guess.
fn derive_resumed(
    conn: &Connection,
    album_id: &str,
    records: Vec<Record>,
) -> musaic_db::Result<Option<ConversionPreview>> {
    let library: Option<String> = conn
        .query_row(
            "SELECT library_path FROM albums WHERE id = ?1",
            [album_id],
            |r| r.get(0),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            e => Err(e),
        })?;
    let Some(library) = library else {
        return Ok(None);
    };
    let rows = load_rows(conn, &library)?;
    let Some(row) = rows.iter().find(|r| r.id == album_id) else {
        return Ok(None);
    };
    let templates = Templates::from_settings(conn)?;
    let record = records.last().cloned().expect("at least one record");
    let discs: Vec<i64> = record
        .files
        .iter()
        .map(|f| f.disc)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut preview = ConversionPreview {
        album_id: album_id.to_string(),
        state: PreviewState::OneFolder,
        reason: None,
        numbering: None,
        disc_total: None,
        present_discs: discs.clone(),
        absent_discs: Vec::new(),
        members: discs
            .iter()
            .map(|d| SetMember {
                album_id: album_id.to_string(),
                dir_name: row.dir_name.clone(),
                disc: *d,
                missing: row.missing,
            })
            .collect(),
        is_compilation: row.is_compilation,
        template: String::new(),
        dir_template: String::new(),
        target_current: None,
        target_path: Some(row.path.clone()),
        library_path: library.clone(),
        plan: Planned::default(),
        resume: Some(record.clone()),
    };
    if records.len() > 1 {
        preview.plan.blockers.push(Blocker {
            code: blocker::RESUME_AMBIGUOUS,
            message: format!(
                "{} unfinished conversions name this album, so Musaic cannot tell which one to \
finish. The records are in {}.",
                records.len(),
                record
                    .path
                    .parent()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            ),
        });
        return Ok(Some(preview));
    }
    let path = PathBuf::from(&row.path);
    let mut facts = gather_member(album_id, &path, None);
    if row.missing {
        facts.problem = Some(Blocker {
            code: blocker::MEMBER_MISSING,
            message: format!(
                "{} is marked Missing: its folder was not found by the last scan.",
                row.dir_name.clone().unwrap_or_else(|| row.id.clone())
            ),
        });
    }
    preview.target_current = Some(facts.dir_name.clone());

    let mut mismatch: Vec<Blocker> = Vec::new();
    let mut used = vec![false; record.files.len()];
    // File name -> (record entry, already written).
    let mut matched: HashMap<String, (usize, bool)> = HashMap::new();
    for f in facts.files.iter_mut() {
        let find = |exact: bool| {
            record.files.iter().enumerate().position(|(i, e)| {
                !used[i]
                    && if exact {
                        e.new_name == f.name || e.old_name == f.name
                    } else {
                        same_nfc(&e.new_name, &f.name) || same_nfc(&e.old_name, &f.name)
                    }
            })
        };
        let Some(i) = find(true).or_else(|| find(false)) else {
            mismatch.push(Blocker {
                code: blocker::RESUME_MISMATCH,
                message: format!(
                    "{} is in the folder but not in the record of the interrupted conversion.",
                    f.name
                ),
            });
            continue;
        };
        used[i] = true;
        let e = &record.files[i];
        if !f.readable {
            // plan_conversion reports it.
            matched.insert(f.name.clone(), (i, false));
            continue;
        }
        if f.numbering == e.old_tags {
            matched.insert(f.name.clone(), (i, false));
        } else if f.numbering == e.new_tags {
            let old = |k: &str| values_of(&e.old_tags, k).to_vec();
            f.tracknumber = old("TRACKNUMBER").first().cloned().unwrap_or_default();
            f.disc = flacmeta::file_disc(
                old("DISCNUMBER").iter().map(String::as_str),
                old("TRACKNUMBER").iter().map(String::as_str),
            );
            f.has_disc_tags = ["DISCNUMBER", "DISCTOTAL", "TOTALDISCS"]
                .iter()
                .any(|k| !values_of(&e.old_tags, k).is_empty());
            matched.insert(f.name.clone(), (i, true));
        } else {
            mismatch.push(Blocker {
                code: blocker::RESUME_MISMATCH,
                message: format!(
                    "The track number or disc tags of {} changed since the interrupted conversion.",
                    f.name
                ),
            });
        }
    }
    for (i, e) in record.files.iter().enumerate() {
        if !used[i] {
            mismatch.push(Blocker {
                code: blocker::RESUME_MISMATCH,
                message: format!(
                    "{} (before the conversion: {}) is no longer in the folder.",
                    e.new_name, e.old_name
                ),
            });
        }
    }

    let target_row = rename_plan_row(conn, album_id)?;
    let mut plan = match &target_row {
        Some(r) => {
            let (t, d) = templates.for_row(r);
            preview.template = t.to_string();
            preview.dir_template = d.to_string();
            plan_conversion(&PlanInput {
                members: std::slice::from_ref(&facts),
                target_row: r,
                templates: &templates,
            })
        }
        None => Planned {
            blockers: facts.problem.iter().cloned().collect(),
            ..Default::default()
        },
    };
    let mut changed = Vec::new();
    for t in plan.tracks.iter_mut() {
        let Some((i, done)) = matched.get(&t.filename).copied() else {
            continue;
        };
        t.tags_done = done;
        let e = &record.files[i];
        let same = t.disc == Some(e.disc)
            && t.track == Some(e.track)
            && t.new_track == Some(e.new_track)
            && t.new_filename
                .as_deref()
                .is_some_and(|n| same_nfc(n, &e.new_name));
        if !same {
            changed.push(t.filename.clone());
        }
    }
    if !changed.is_empty() || (mismatch.is_empty() && plan.track_total != record.track_total) {
        plan.blockers.push(Blocker {
            code: blocker::RESUME_CHANGED,
            message: format!(
                "The plan made now is not the one the interrupted conversion recorded{}: a \
template, a title or a track number changed since. Musaic will not finish it with other numbers or \
names. The record is {}.",
                if changed.is_empty() {
                    String::new()
                } else {
                    format!(" (for {})", changed.join(", "))
                },
                record.path.display()
            ),
        });
    }
    if let Some(t) = &plan.target_name
        && !same_nfc(t, &record.folder_name_after)
    {
        plan.blockers.push(Blocker {
            code: blocker::RESUME_CHANGED,
            message: format!(
                "The folder name made now ({t}) is not the one the interrupted conversion \
recorded ({}). The record is {}.",
                record.folder_name_after,
                record.path.display()
            ),
        });
    }
    plan.blockers.extend(mismatch);
    if facts.problem.is_none() {
        let target = plan
            .target_name
            .as_deref()
            .filter(|t| preview.target_current.as_deref() != Some(*t));
        plan.blockers
            .extend(disk_blockers(&[(facts.dir_name.clone(), path)], target));
    }
    for id in active_jobs(conn, &[album_id])? {
        let name = row.dir_name.clone().unwrap_or(id);
        plan.blockers.push(Blocker {
            code: blocker::JOB_ACTIVE,
            message: format!("A job is queued or running on {name}. Wait for it to finish."),
        });
    }
    let pairs: Vec<(Option<i64>, Option<i64>)> =
        plan.tracks.iter().map(|t| (t.disc, t.track)).collect();
    preview.numbering = Some(classify_numbering(&pairs));
    preview.plan = plan;
    Ok(Some(preview))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, dir: &str) -> DiscRow {
        let parsed = musaic_scan::parse_album_dir(dir);
        DiscRow {
            id: id.into(),
            path: format!("/lib/{dir}"),
            dir_name: Some(dir.into()),
            dir_artist: parsed.dir_artist,
            dir_album: parsed.dir_album,
            is_compilation: parsed.is_compilation,
            disc_number: parsed.disc_number,
            disc_total: parsed.disc_total,
            track_numbers: vec![Some(1), Some(2)],
            track_discs: Some(vec![None, None]),
            ..Default::default()
        }
    }

    fn with_tracks(mut r: DiscRow, discs: &[Option<i64>], tracks: &[i64]) -> DiscRow {
        r.track_numbers = tracks.iter().map(|t| Some(*t)).collect();
        r.track_discs = Some(discs.to_vec());
        r
    }

    #[test]
    fn numbering_kinds() {
        let p = |v: &[(i64, i64)]| -> Vec<(Option<i64>, Option<i64>)> {
            v.iter().map(|(d, t)| (Some(*d), Some(*t))).collect()
        };
        assert_eq!(
            classify_numbering(&p(&[(1, 1), (1, 2), (2, 1), (2, 2)])),
            Numbering::PerDisc
        );
        assert_eq!(
            classify_numbering(&p(&[(1, 1), (1, 2), (2, 3), (2, 4)])),
            Numbering::Continuous
        );
        assert_eq!(
            classify_numbering(&p(&[(1, 1), (1, 3), (2, 1)])),
            Numbering::Irregular
        );
        assert_eq!(
            classify_numbering(&[(Some(1), Some(1)), (None, Some(2))]),
            Numbering::Irregular
        );
    }

    #[test]
    fn one_folder_with_two_discs_is_found() {
        let r = with_tracks(
            row("a", "Band - Record [FLAC]"),
            &[Some(1), Some(1), Some(2), Some(2)],
            &[1, 2, 1, 2],
        );
        let d = detect(&[r]);
        assert_eq!(d.sets.len(), 1);
        assert_eq!(
            d.sets[0].kind,
            SetKind::OneFolder {
                discs: vec![1, 2],
                numbering: Numbering::PerDisc
            }
        );
        assert_eq!(d.sets[0].members.len(), 2);
    }

    #[test]
    fn unread_discs_are_unknown_only_where_they_matter() {
        let mut repeats = row("a", "Band - Record [FLAC]");
        repeats.track_numbers = vec![Some(1), Some(2), Some(1), Some(2)];
        repeats.track_discs = None;
        let mut plain = row("b", "Band - Other [FLAC]");
        plain.track_discs = None;
        let d = detect(&[repeats, plain]);
        assert!(d.sets.is_empty());
        assert_eq!(d.unknown, vec!["a".to_string()]);
    }

    #[test]
    fn folder_set_and_its_rules() {
        let d1 = row("d1", "Band - Record (disc 1 of 2) [FLAC]");
        let d2 = row("d2", "Band - Record (disc 2 of 2) [FLAC]");
        let d = detect(&[d1.clone(), d2.clone()]);
        assert_eq!(d.sets.len(), 1);
        assert_eq!(d.sets[0].anchor, "d1");
        assert_eq!(d.sets[0].kind, SetKind::Folders { total: 2 });

        // Folder artist compared folded.
        let mut d2b = d2.clone();
        d2b.dir_artist = Some("BAND".into());
        assert_eq!(detect(&[d1.clone(), d2b]).sets.len(), 1);

        // Different parents: not one set, and neither half is reported as
        // incomplete while the other disc is elsewhere.
        let mut elsewhere = d2.clone();
        elsewhere.path = "/other/Band - Record (disc 2 of 2) [FLAC]".into();
        assert!(detect(&[d1.clone(), elsewhere]).sets.is_empty());

        // Two release groups: not one set.
        let mut a = d1.clone();
        a.release_group = Some("rg-1".into());
        let mut b = d2.clone();
        b.release_group = Some("rg-2".into());
        assert!(detect(&[a.clone(), b]).sets.is_empty());
        // One anchored, one not: fine.
        assert_eq!(detect(&[a, d2.clone()]).sets.len(), 1);

        // A disc tag that names another disc than the folder: no folder
        // set (that folder itself holds two discs, its own finding).
        let bad = with_tracks(d2.clone(), &[Some(1), Some(2)], &[1, 2]);
        let sets = detect(&[d1.clone(), bad]).sets;
        assert_eq!(sets.len(), 1);
        assert!(matches!(sets[0].kind, SetKind::OneFolder { .. }));
        assert_eq!(sets[0].anchor, "d2");

        // Totals that disagree.
        let mut odd = d2.clone();
        odd.disc_total = Some(3);
        assert!(detect(&[d1.clone(), odd]).sets.is_empty());

        // Album artists or years that disagree.
        let mut x = d1.clone();
        x.year = Some("1990".into());
        let mut y = d2.clone();
        y.year = Some("1991".into());
        assert!(detect(&[x, y]).sets.is_empty());

        // Two folders for one disc.
        let mut dup = d2.clone();
        dup.id = "d2b".into();
        dup.path = "/lib/Band - Record (Disc 2 of 2) [FLAC]".into();
        assert!(detect(&[d1, d2, dup]).sets.is_empty());
    }

    #[test]
    fn incomplete_sets() {
        let d1 = row("d1", "Band - Record (disc 1 of 3) [FLAC]");
        let d3 = row("d3", "Band - Record (disc 3 of 3) [FLAC]");
        let d = detect(&[d1, d3]);
        assert_eq!(
            d.sets[0].kind,
            SetKind::Incomplete {
                total: Some(3),
                present: vec![1, 3],
                absent: vec![2]
            }
        );
        // No total: disc 2 without disc 1.
        let lone = row("x", "Band - Other (disc 2) [FLAC]");
        let d = detect(&[lone]);
        assert_eq!(
            d.sets[0].kind,
            SetKind::Incomplete {
                total: None,
                present: vec![2],
                absent: vec![1]
            }
        );
        // No total, discs 1 and 2: a set of two.
        let a = row("a", "Band - Third (disc 1) [FLAC]");
        let b = row("b", "Band - Third (disc 2) [FLAC]");
        assert_eq!(detect(&[a, b]).sets[0].kind, SetKind::Folders { total: 2 });
        // A lone "(disc 1)" or "disc 1 of 1": nothing.
        assert!(
            detect(&[row("c", "Band - Fourth (disc 1) [FLAC]")])
                .sets
                .is_empty()
        );
        assert!(
            detect(&[row("e", "Band - Fifth (disc 1 of 1) [FLAC]")])
                .sets
                .is_empty()
        );
    }

    #[test]
    fn missing_members_are_flagged_not_dropped() {
        let d1 = row("d1", "Band - Record (disc 1 of 2) [FLAC]");
        let mut d2 = row("d2", "Band - Record (disc 2 of 2) [FLAC]");
        d2.missing = true;
        let d = detect(&[d1, d2]);
        assert_eq!(d.sets[0].kind, SetKind::Folders { total: 2 });
        assert!(d.sets[0].has_missing());
    }

    #[test]
    fn grouping_needs_disc_markers_in_folder_names() {
        // Same tags, no disc in the folder names: never grouped.
        let a = row("a", "Band - Record [FLAC]");
        let mut b = row("b", "Band - Record [FLAC]");
        b.path = "/lib/Band - Record (bonus) [FLAC]".into();
        assert!(detect(&[a, b]).sets.is_empty());
    }

    fn templates() -> Templates {
        Templates {
            filename_single: "{tracknumber:02} - {title}".into(),
            filename_compilation: "{tracknumber:02} - {artist} - {title}".into(),
            dir_single: "{albumartist} - {album} [FLAC]".into(),
            dir_compilation: "Various Artists - {album} [FLAC]".into(),
        }
    }

    fn target_row() -> RenamePlanRow {
        RenamePlanRow {
            id: "d1".into(),
            path: Some("/lib/Band - Record (disc 1 of 3) [FLAC]".into()),
            dir_name: Some("Band - Record (disc 1 of 3) [FLAC]".into()),
            album_artist: Some("Band".into()),
            artist: Some("Band".into()),
            dir_artist: Some("Band".into()),
            title: Some("Record".into()),
            year: Some("1999".into()),
            is_compilation: false,
            disc_number: Some(1),
            disc_total: Some(3),
        }
    }

    fn file(name: &str, track: &str, title: &str, disc: Option<i64>) -> FileFacts {
        FileFacts {
            name: name.into(),
            readable: true,
            tracknumber: track.into(),
            title: title.into(),
            artist: "Band".into(),
            albumartist: "Band".into(),
            disc,
            has_disc_tags: disc.is_some(),
            numbering: Vec::new(),
        }
    }

    fn member(id: &str, disc: i64, n: usize) -> MemberFacts {
        MemberFacts {
            album_id: id.into(),
            dir_name: format!("Band - Record (disc {disc} of 3) [FLAC]"),
            folder_disc: Some(disc),
            files: (1..=n)
                .map(|k| {
                    file(
                        &format!("{k:02} - Song {disc}.{k}.flac"),
                        &k.to_string(),
                        &format!("Song {disc}.{k}"),
                        None,
                    )
                })
                .collect(),
            ..Default::default()
        }
    }

    fn codes(p: &Planned) -> Vec<&'static str> {
        p.blockers.iter().map(|b| b.code).collect()
    }

    #[test]
    fn three_folders_get_continuous_numbers() {
        let members = vec![member("d1", 1, 3), member("d2", 2, 2), member("d3", 3, 4)];
        let row = target_row();
        let t = templates();
        let p = plan_conversion(&PlanInput {
            members: &members,
            target_row: &row,
            templates: &t,
        });
        assert!(p.blockers.is_empty(), "{:?}", p.blockers);
        assert_eq!(p.track_total, 9);
        let numbers: Vec<(Option<i64>, Option<i64>, Option<i64>)> = p
            .tracks
            .iter()
            .map(|t| (t.disc, t.track, t.new_track))
            .collect();
        assert_eq!(
            numbers,
            vec![
                (Some(1), Some(1), Some(1)),
                (Some(1), Some(2), Some(2)),
                (Some(1), Some(3), Some(3)),
                (Some(2), Some(1), Some(4)),
                (Some(2), Some(2), Some(5)),
                (Some(3), Some(1), Some(6)),
                (Some(3), Some(2), Some(7)),
                (Some(3), Some(3), Some(8)),
                (Some(3), Some(4), Some(9)),
            ]
        );
        assert_eq!(
            p.tracks[3].new_filename.as_deref(),
            Some("04 - Song 2.1.flac")
        );
        assert_eq!(
            p.tracks[8].new_filename.as_deref(),
            Some("09 - Song 3.4.flac")
        );
        assert_eq!(p.target_name.as_deref(), Some("Band - Record [FLAC]"));
        assert_eq!(p.folders.len(), 3);
        assert!(p.folders[0].destination);
    }

    #[test]
    fn a_stopped_conversion_numbering_is_accepted() {
        let mut d2 = member("d2", 2, 2);
        for (i, f) in d2.files.iter_mut().enumerate() {
            f.tracknumber = (4 + i).to_string();
        }
        let members = vec![member("d1", 1, 3), d2];
        let (row, t) = (target_row(), templates());
        let p = plan_conversion(&PlanInput {
            members: &members,
            target_row: &row,
            templates: &t,
        });
        assert!(p.blockers.is_empty(), "{:?}", p.blockers);
        assert_eq!(p.tracks[3].new_track, Some(4));
    }

    #[test]
    fn gaps_block() {
        let mut d2 = member("d2", 2, 3);
        d2.files[1].tracknumber = "3".into();
        let members = vec![member("d1", 1, 2), d2];
        let (row, t) = (target_row(), templates());
        let p = plan_conversion(&PlanInput {
            members: &members,
            target_row: &row,
            templates: &t,
        });
        assert_eq!(codes(&p), vec![blocker::TRACK_NUMBERS]);
        assert!(p.blockers[0].message.contains("Fix the track gaps first"));
        assert!(
            p.blockers[0].message.contains("no track 2"),
            "{}",
            p.blockers[0].message
        );
    }

    #[test]
    fn collisions_fold_case_and_normalisation() {
        let mut t = templates();
        t.filename_single = "{title}".into();
        let mut d1 = member("d1", 1, 1);
        d1.files[0].title = "Caf\u{e9}".into();
        let mut d2 = member("d2", 2, 1);
        d2.files[0].title = "CAFE\u{301}".into();
        let mut d3 = member("d3", 3, 1);
        d3.files[0].title = "Intro".into();
        d1.entries.push(EntryFacts {
            name: "intro.FLAC".into(),
            kind: EntryKind::Other,
        });
        let members = vec![d1, d2, d3];
        let row = target_row();
        let p = plan_conversion(&PlanInput {
            members: &members,
            target_row: &row,
            templates: &t,
        });
        assert_eq!(
            codes(&p),
            vec![blocker::NAME_COLLISION, blocker::NAME_COLLISION]
        );
    }

    #[test]
    fn covers_identical_different_moved() {
        let mut d1 = member("d1", 1, 1);
        d1.entries = vec![
            EntryFacts {
                name: "cover.jpg".into(),
                kind: EntryKind::Cover(vec![1, 2]),
            },
            EntryFacts {
                name: SIDECAR_FILENAME.into(),
                kind: EntryKind::Sidecar,
            },
            EntryFacts {
                name: "notes.txt".into(),
                kind: EntryKind::Other,
            },
        ];
        let mut d2 = member("d2", 2, 1);
        d2.entries = vec![
            EntryFacts {
                name: "Cover.JPG".into(),
                kind: EntryKind::Cover(vec![1, 2]),
            },
            EntryFacts {
                name: "folder.jpg".into(),
                kind: EntryKind::Cover(vec![9]),
            },
            EntryFacts {
                name: ".DS_Store".into(),
                kind: EntryKind::Junk,
            },
        ];
        let mut d3 = member("d3", 3, 1);
        d3.entries = vec![
            EntryFacts {
                name: "cover.jpg".into(),
                kind: EntryKind::Cover(vec![3]),
            },
            EntryFacts {
                name: "folder.jpg".into(),
                kind: EntryKind::Cover(vec![9]),
            },
        ];
        let members = vec![d1, d2, d3];
        let (row, t) = (target_row(), templates());
        let p = plan_conversion(&PlanInput {
            members: &members,
            target_row: &row,
            templates: &t,
        });
        let acts = |i: usize| -> Vec<(&str, CoverAction)> {
            p.folders[i]
                .covers
                .iter()
                .map(|c| (c.name.as_str(), c.action))
                .collect()
        };
        assert_eq!(acts(0), vec![("cover.jpg", CoverAction::Kept)]);
        assert_eq!(
            acts(1),
            vec![
                ("Cover.JPG", CoverAction::Identical),
                ("folder.jpg", CoverAction::Moved)
            ]
        );
        assert_eq!(
            acts(2),
            vec![
                ("cover.jpg", CoverAction::Different),
                ("folder.jpg", CoverAction::Identical)
            ]
        );
        assert!(p.folders[0].sidecar);
        assert_eq!(p.folders[0].other_files, vec!["notes.txt".to_string()]);
        assert!(p.folders[1].other_files.is_empty());
    }

    #[test]
    fn one_folder_blockers() {
        let mut m = MemberFacts {
            album_id: "a".into(),
            dir_name: "Band - Record [FLAC]".into(),
            files: vec![
                file("1-01 A.flac", "1-01", "A", Some(1)),
                file("1-02 B.flac", "1-02", "B", Some(1)),
                file("3-01 C.flac", "3-01", "C", Some(3)),
                file("x.flac", "4", "", None),
            ],
            ..Default::default()
        };
        m.files.push(FileFacts {
            name: "broken.flac".into(),
            ..Default::default()
        });
        let members = vec![m];
        let (row, t) = (target_row(), templates());
        let p = plan_conversion(&PlanInput {
            members: &members,
            target_row: &row,
            templates: &t,
        });
        let c = codes(&p);
        assert!(c.contains(&blocker::MEMBER_UNREADABLE), "{c:?}");
        assert!(c.contains(&blocker::DISC_UNKNOWN), "{c:?}");
        assert!(c.contains(&blocker::DISC_GAP), "{c:?}");
        assert!(p.folders.is_empty());
        // Disc 3 continues after disc 1's two tracks.
        assert_eq!(p.tracks[2].new_track, Some(3));
        assert_eq!(p.target_name.as_deref(), Some("Band - Record [FLAC]"));
    }

    #[test]
    fn rename_temporaries_are_recognised_and_block() {
        for n in [
            ".01 - A.flac.renaming.tmp",
            ".01 - A.flac.renaming.12.tmp",
            ".musaic-3.tmp",
        ] {
            assert!(is_rename_temp(n), "{n}");
        }
        for n in [
            "01 - A.flac",
            ".DS_Store",
            ".media-manager-tags.json",
            ".x.renaming.y.tmp",
            ".musaic-.tmp",
            "a.renaming.tmp",
        ] {
            assert!(!is_rename_temp(n), "{n}");
        }
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(".01 - A.flac.renaming.tmp"), b"x").unwrap();
        let m = gather_member("a", tmp.path(), None);
        assert_eq!(m.stranded, vec![".01 - A.flac.renaming.tmp".to_string()]);
        let p = plan_conversion(&PlanInput {
            members: &[m],
            target_row: &target_row(),
            templates: &templates(),
        });
        assert!(
            codes(&p).contains(&blocker::STRANDED_TEMP),
            "{:?}",
            codes(&p)
        );
    }

    #[test]
    fn disk_blockers_parent_and_taken_target() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let d1 = root.join("lib/Band - Record (disc 1 of 2) [FLAC]");
        let d2 = root.join("elsewhere/Band - Record (disc 2 of 2) [FLAC]");
        std::fs::create_dir_all(&d1).unwrap();
        std::fs::create_dir_all(&d2).unwrap();
        // Another folder already has the target name, in another case.
        std::fs::create_dir_all(root.join("lib/band - record [flac]")).unwrap();
        let folders = vec![("d1".to_string(), d1.clone()), ("d2".to_string(), d2)];
        let codes: Vec<&str> = disk_blockers(&folders, Some("Band - Record [FLAC]"))
            .iter()
            .map(|b| b.code)
            .collect();
        assert_eq!(
            codes,
            vec![blocker::DIFFERENT_PARENT, blocker::TARGET_TAKEN]
        );
        // The destination itself under its own name is not "taken".
        let only = vec![("d1".to_string(), d1)];
        assert!(disk_blockers(&only, Some("Band - Record (disc 1 of 2) [FLAC]")).is_empty());
    }

    #[test]
    fn missing_title_and_disc_mismatch_block() {
        let mut d2 = member("d2", 2, 1);
        d2.files[0].title.clear();
        d2.files[0].disc = Some(3);
        let members = vec![member("d1", 1, 1), d2];
        let (row, t) = (target_row(), templates());
        let p = plan_conversion(&PlanInput {
            members: &members,
            target_row: &row,
            templates: &t,
        });
        assert_eq!(
            codes(&p),
            vec![blocker::DISC_MISMATCH, blocker::MISSING_TITLE]
        );
    }
}
