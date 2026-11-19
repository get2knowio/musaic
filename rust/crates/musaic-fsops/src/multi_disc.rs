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

use crate::conversion_log::{self, LoggedFile, NumberingTags, Record, numbering_of, values_of};
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
    /// The discs are matched to MusicBrainz release groups that differ.
    pub const ANCHORS_DIFFER: &str = "anchors_differ";
    /// The release group the discs are matched to is marked a wrong match
    /// on one of them.
    pub const ANCHOR_BLOCKED: &str = "anchor_blocked";
    /// Musaic keeps data for a file that is no longer on disk under a name
    /// a file of the converted set would get.
    pub const ORPHAN_ROWS: &str = "orphan_rows";
    /// An album of an interrupted conversion is no longer in the library,
    /// and not because the conversion folded it.
    pub const MEMBER_GONE: &str = "member_gone";
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
    /// Any other file that is not a FLAC (a rip log, a cue sheet, a
    /// booklet).
    Other,
    /// A subfolder, or anything else that is not a regular file (a link).
    /// A conversion never looks inside one and never moves it.
    Folder,
}

/// A folder entry that is not one of the member's FLACs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryFacts {
    pub name: String,
    pub kind: EntryKind,
    /// The SHA-256 of an [`EntryKind::Other`] file's content, read only
    /// where another folder of the set has a file of the same name
    /// ([`digest_shared_strays`]): two such files are the same only when
    /// both digests are known and equal.
    pub digest: Option<String>,
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
    /// How far an interrupted conversion got with this file.
    pub state: TrackState,
    /// The name of the folder the file is in now (its member's folder, or
    /// the destination once a set in several folders moved it there).
    pub folder: String,
}

/// Where a file of a conversion is, as its record and the disk say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrackState {
    /// Its old name and its old tags, in its own folder.
    #[default]
    Untouched,
    /// Its new name, its old tags, in its own folder.
    Renamed,
    /// Its new name and its new tags, in its own folder.
    Tagged,
    /// Its new name and its new tags, in the destination folder (a disc of
    /// a set kept in several folders).
    Moved,
}

impl TrackState {
    pub fn as_str(self) -> &'static str {
        match self {
            TrackState::Untouched => "untouched",
            TrackState::Renamed => "renamed",
            TrackState::Tagged => "tagged",
            TrackState::Moved => "moved",
        }
    }
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
    /// An interrupted conversion already did it.
    pub done: bool,
}

impl CoverAction {
    pub fn parse(s: &str) -> Option<CoverAction> {
        Some(match s {
            "kept" => CoverAction::Kept,
            "moved" => CoverAction::Moved,
            "identical" => CoverAction::Identical,
            "different" => CoverAction::Different,
            _ => return None,
        })
    }
}

/// What happens to one other file (a rip log, a cue sheet, a booklet) of a
/// folder that is emptied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrayAction {
    /// It moves into the destination, under its own name or under
    /// [`StrayPlan::new_name`].
    Moved,
    /// The destination has the same content under this name: this copy
    /// goes to the app's trash.
    Identical,
}

impl StrayAction {
    pub fn as_str(self) -> &'static str {
        match self {
            StrayAction::Moved => "moved",
            StrayAction::Identical => "identical",
        }
    }

    pub fn parse(s: &str) -> Option<StrayAction> {
        Some(match s {
            "moved" => StrayAction::Moved,
            "identical" => StrayAction::Identical,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrayPlan {
    /// Its name in its own folder.
    pub name: String,
    pub action: StrayAction,
    /// The name it gets in the destination (`rip (disc 2).log`) when its
    /// own is taken there by a file with other content.
    pub new_name: Option<String>,
    /// An interrupted conversion already did it.
    pub done: bool,
}

impl StrayPlan {
    /// The name the file has in the destination once moved.
    pub fn target(&self) -> &str {
        self.new_name.as_deref().unwrap_or(&self.name)
    }
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
    /// Entries that stay where they are: in the destination everything
    /// that is neither a FLAC, a cover, the sidecar nor clutter; in another
    /// folder only its subfolders (and links), which a conversion never
    /// moves, so that folder is not removed.
    pub other_files: Vec<String>,
    /// The other files of a folder that is emptied (neither FLACs, covers,
    /// the sidecar nor clutter): each moves into the destination, or goes
    /// to the app trash when the destination has the same file already.
    /// Empty for the destination.
    pub strays: Vec<StrayPlan>,
    /// Operating-system clutter (`.DS_Store`, `Thumbs.db`, AppleDouble
    /// files) of a folder that is emptied: it goes to the app trash so the
    /// folder can be removed. `(name, done)`. The destination keeps its own.
    pub junk: Vec<(String, bool)>,
    /// The sidecar is gone already (an interrupted conversion moved it to
    /// the trash after merging it).
    pub sidecar_done: bool,
    /// The folder is still on disk (an interrupted conversion may have
    /// removed it once it was empty).
    pub exists: bool,
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
                state: TrackState::Untouched,
                folder: m.dir_name.clone(),
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
        out.folders = plan_folders(input.members, &out.tracks, &mut out.blockers);
    }
    out
}

/// `name` with ` (disc N)` before its extension: `rip.log` → `rip (disc
/// 2).log`. A name without an extension (or only a leading dot) gets it at
/// the end.
fn disc_name(name: &str, disc: i64) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => format!("{} (disc {disc}){}", &name[..i], &name[i..]),
        _ => format!("{name} (disc {disc})"),
    }
}

/// Covers, sidecars and other files of each folder.
///
/// Covers: the destination keeps every cover it has. Only when it has none
/// do the covers of the first other folder that has any (in disc order)
/// move in, so the set's picture is never replaced by another disc's. Any
/// other cover is a copy of one that is kept (the same bytes: not needed)
/// or a different picture; both go to the app trash, which can give them
/// back. Embedded pictures travel inside the files and are not touched.
///
/// Other files (a rip log, a cue sheet, a booklet) of a folder that is
/// emptied move into the destination, in disc order, so that the folder
/// can be removed: under their own name where it is free there; to the app
/// trash where the destination has the same content under that name; with
/// ` (disc N)` before the extension where it has other content. A name
/// that is taken even then is a blocker, never a guess. Subfolders are
/// never moved: a folder that has one stays.
fn plan_folders(
    members: &[MemberFacts],
    tracks: &[PlannedTrack],
    blockers: &mut Vec<Blocker>,
) -> Vec<FolderPlan> {
    let has_covers = |m: &MemberFacts| {
        m.entries
            .iter()
            .any(|e| matches!(e.kind, EntryKind::Cover(_)))
    };
    let source = members.iter().position(has_covers);
    let mut kept: Vec<&[u8]> = Vec::new();
    if let Some(s) = source {
        for e in &members[s].entries {
            if let EntryKind::Cover(bytes) = &e.kind {
                kept.push(bytes);
            }
        }
    }
    // What the destination holds under each name (as an insensitive volume
    // compares them) once the conversion's files, the covers it takes and
    // the strays planned so far are in it, with the content digest of an
    // other file where it is known.
    let mut taken: HashMap<String, Option<&str>> = HashMap::new();
    taken.insert(fold(SIDECAR_FILENAME), None);
    if let Some(dest) = members.first() {
        for e in &dest.entries {
            let digest = match e.kind {
                EntryKind::Other => e.digest.as_deref(),
                _ => None,
            };
            taken.insert(fold(&e.name), digest);
        }
    }
    for t in tracks {
        if let Some(n) = &t.new_filename {
            taken.insert(fold(n), None);
        }
    }
    if let Some(s) = source.filter(|s| *s > 0) {
        for e in &members[s].entries {
            if matches!(e.kind, EntryKind::Cover(_)) {
                taken.insert(fold(&e.name), None);
            }
        }
    }
    let dest_name = members.first().map_or("", |m| m.dir_name.as_str());
    let mut out = Vec::new();
    for (i, m) in members.iter().enumerate() {
        let mut covers = Vec::new();
        let mut other_files = Vec::new();
        let mut strays = Vec::new();
        let mut junk = Vec::new();
        let mut sidecar = false;
        for e in &m.entries {
            match &e.kind {
                EntryKind::Cover(bytes) => {
                    let action = if Some(i) == source {
                        if i == 0 {
                            CoverAction::Kept
                        } else {
                            CoverAction::Moved
                        }
                    } else if kept.contains(&bytes.as_slice()) {
                        CoverAction::Identical
                    } else {
                        CoverAction::Different
                    };
                    covers.push(CoverPlan {
                        name: e.name.clone(),
                        action,
                        done: false,
                    });
                }
                EntryKind::Sidecar => sidecar = true,
                EntryKind::Junk if i > 0 => junk.push((e.name.clone(), false)),
                EntryKind::Junk => {}
                EntryKind::Other if i > 0 => {
                    let disc = m.folder_disc.unwrap_or(i as i64 + 1);
                    let key = fold(&e.name);
                    let (action, new_name) = match taken.get(&key) {
                        None => {
                            taken.insert(key, e.digest.as_deref());
                            (StrayAction::Moved, None)
                        }
                        Some(Some(d)) if e.digest.as_deref() == Some(*d) => {
                            (StrayAction::Identical, None)
                        }
                        Some(_) => {
                            let alt = disc_name(&e.name, disc);
                            if taken.insert(fold(&alt), None).is_some() {
                                blockers.push(Blocker {
                                    code: blocker::NAME_COLLISION,
                                    message: format!(
                                        "{} in {} cannot move into {dest_name}: a different file \
named {} is already there, and so is {alt}, the name it would get instead. Rename or remove one of \
them, then try again.",
                                        e.name, m.dir_name, e.name
                                    ),
                                });
                            }
                            (StrayAction::Moved, Some(alt))
                        }
                    };
                    strays.push(StrayPlan {
                        name: e.name.clone(),
                        action,
                        new_name,
                        done: false,
                    });
                }
                EntryKind::Other | EntryKind::Folder => other_files.push(e.name.clone()),
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
            strays,
            junk,
            sidecar_done: false,
            exists: true,
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
        // A link is not followed: only a regular file is ever moved.
        let regular = std::fs::symlink_metadata(folder.join(&name)).is_ok_and(|m| m.is_file());
        let kind = match entry_kind_of(&name) {
            Some(EntryKind::Other) if !regular => EntryKind::Folder,
            Some(k) => k,
            None => match std::fs::read(folder.join(&name)) {
                Ok(bytes) => EntryKind::Cover(bytes),
                // A directory named like a cover: not a cover to compare.
                Err(_) if !regular => EntryKind::Folder,
                // A cover that cannot be read.
                Err(_) => EntryKind::Other,
            },
        };
        facts.entries.push(EntryFacts {
            name,
            kind,
            digest: None,
        });
    }
    facts
}

/// The SHA-256 of a file's content, read in pieces (a booklet can be
/// large); `None` when it cannot be read.
fn file_digest(path: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
    Some(
        hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    )
}

/// Read the content digest of every other file whose name more than one
/// folder of the set has (as an insensitive volume compares names): the
/// plan needs to know whether two such files are the same. A name only one
/// folder has is never read. `dirs` are the members' folders, in order.
pub fn digest_shared_strays(facts: &mut [MemberFacts], dirs: &[PathBuf]) {
    let mut count: HashMap<String, usize> = HashMap::new();
    for m in facts.iter() {
        for e in m.entries.iter().filter(|e| e.kind == EntryKind::Other) {
            *count.entry(fold(&e.name)).or_default() += 1;
        }
    }
    for (m, dir) in facts.iter_mut().zip(dirs) {
        for e in m.entries.iter_mut() {
            if e.kind == EntryKind::Other && count.get(&fold(&e.name)).is_some_and(|n| *n > 1) {
                e.digest = file_digest(&dir.join(&e.name));
            }
        }
    }
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
    /// A set kept in several folders: the member folders and how far an
    /// interrupted conversion got with the database and the folders.
    pub gather: Option<Gather>,
}

/// A member of a set kept in several folders, as the conversion finds it
/// now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatherMember {
    pub album_id: String,
    /// Its folder: the member's own, or the destination's as it is now.
    pub dir: PathBuf,
    pub dir_name: String,
    pub disc: i64,
    /// Disc 1's folder, which the others are gathered into.
    pub destination: bool,
    /// Its album row still exists (the fold removes the other members').
    pub has_row: bool,
    /// Its row is marked Missing.
    pub missing: bool,
    /// Its folder is on disk (an emptied one is removed at the end).
    pub exists: bool,
}

/// What a conversion of a set kept in several folders works on, besides
/// the files.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Gather {
    /// In disc order; the first is the destination.
    pub members: Vec<GatherMember>,
    /// Rows Musaic keeps for files no longer on disk (`(album id, file
    /// name)`), carried to the destination under the same name.
    pub orphan_rows: Vec<(String, String)>,
    /// The other members' rows are folded into the destination's already.
    pub folded: bool,
    /// The destination folder has its new name (and its row the new id).
    pub folder_renamed: bool,
    /// The destination folder has its new name on disk but its row still
    /// has the old path and id: the conversion stopped between the two.
    pub rekey_pending: bool,
}

impl ConversionPreview {
    /// A conversion could run as planned.
    pub fn convertible(&self) -> bool {
        matches!(self.state, PreviewState::OneFolder | PreviewState::Folders)
            && self.plan.blockers.is_empty()
    }

    /// A conversion can be started from the preview: a convertible set,
    /// in one folder or in several.
    pub fn conversion_available(&self) -> bool {
        self.convertible()
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
                    t.state.as_str(),
                    t.folder,
                ])
            })
            .collect();
        let folders: Vec<serde_json::Value> = self
            .plan
            .folders
            .iter()
            .map(|f| {
                serde_json::json!([
                    f.album_id,
                    f.dir_name,
                    f.covers
                        .iter()
                        .map(|c| serde_json::json!([c.name, c.action.as_str(), c.done]))
                        .collect::<Vec<_>>(),
                    f.junk,
                    f.sidecar,
                    f.sidecar_done,
                    f.other_files,
                    f.exists,
                    f.strays
                        .iter()
                        .map(|s| serde_json::json!([s.name, s.action.as_str(), s.new_name, s.done]))
                        .collect::<Vec<_>>(),
                ])
            })
            .collect();
        let gather = self.gather.as_ref().map(|g| {
            serde_json::json!({
                "members": g.members.iter().map(|m| serde_json::json!([
                    m.album_id, m.dir.to_string_lossy(), m.disc, m.has_row, m.missing, m.exists,
                ])).collect::<Vec<_>>(),
                "orphan_rows": g.orphan_rows,
                "folded": g.folded,
                "folder_renamed": g.folder_renamed,
                "rekey_pending": g.rekey_pending,
            })
        });
        let doc = serde_json::json!({
            "album_id": self.album_id,
            "state": self.state.as_str(),
            "target_path": self.target_path,
            "target_current": self.target_current,
            "target_new": self.plan.target_name,
            "track_total": self.plan.track_total,
            "tracks": tracks,
            "folders": folders,
            "gather": gather,
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
        let mut records = conversion_log::unfinished_of(dir, album_id);
        if records.len() == 1 && records[0].is_folders() {
            let record = records.pop().expect("one record");
            return derive_resumed_folders(conn, album_id, record);
        }
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
        gather: None,
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
    if !one_folder {
        let dirs: Vec<PathBuf> = folders.iter().map(|(_, p)| p.clone()).collect();
        digest_shared_strays(&mut facts, &dirs);
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
    if one_folder {
        for id in active_jobs(conn, &ids)? {
            let name = row_of(&id).and_then(|r| r.dir_name.clone()).unwrap_or(id);
            plan.blockers.push(Blocker {
                code: blocker::JOB_ACTIVE,
                message: format!("A job is queued or running on {name}. Wait for it to finish."),
            });
        }
    } else {
        let name_of = |id: &str| {
            row_of(id)
                .and_then(|r| r.dir_name.clone())
                .unwrap_or_else(|| id.to_string())
        };
        plan.blockers.extend(set_blockers(conn, &ids, &name_of)?);
        let (blockers, orphans) = orphan_rows(conn, &facts, &plan)?;
        plan.blockers.extend(blockers);
        plan.blockers.extend(gather_collisions(&facts, &plan));
        preview.gather = Some(Gather {
            members: ids
                .iter()
                .enumerate()
                .filter_map(|(i, id)| {
                    let r = row_of(id)?;
                    let dir = PathBuf::from(&r.path);
                    Some(GatherMember {
                        album_id: id.to_string(),
                        dir_name: file_name(&dir),
                        exists: dir.is_dir(),
                        dir,
                        disc: r.disc_number.unwrap_or_default(),
                        destination: i == 0,
                        has_row: true,
                        missing: r.missing,
                    })
                })
                .collect(),
            orphan_rows: orphans,
            ..Default::default()
        });
    }
    preview.plan = plan;
    Ok(Some(preview))
}

/// What the fold of a set's rows (`musaic_db::merge::merge_album_rows`)
/// would refuse, asked before anything moves so that the fold is never the
/// first to say no: a job on a member that is queued, running or waiting
/// with a suggestion made for that disc alone, members matched to two
/// release groups, or the one group marked a wrong match on a member.
fn set_blockers(
    conn: &Connection,
    ids: &[&str],
    name_of: &dyn Fn(&str) -> String,
) -> musaic_db::Result<Vec<Blocker>> {
    let mut out = Vec::new();
    let json = serde_json::Value::from(ids.to_vec()).to_string();
    let statuses =
        serde_json::Value::from(musaic_db::ai_jobs::ACTIVE_STATUSES.to_vec()).to_string();
    let mut stmt = conn.prepare(
        "SELECT album_id, status FROM ai_jobs WHERE album_id IN (SELECT value FROM json_each(?1)) \
         AND status IN (SELECT value FROM json_each(?2)) ORDER BY album_id, status",
    )?;
    let jobs = stmt
        .query_map([&json, &statuses], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut seen = BTreeSet::new();
    for (id, status) in jobs {
        if !seen.insert(id.clone()) {
            continue;
        }
        let name = name_of(&id);
        out.push(Blocker {
            code: blocker::JOB_ACTIVE,
            message: if status == "completed" {
                format!(
                    "A job on {name} has a suggestion waiting, made for that disc alone. Act on it \
or discard it on the Jobs page first."
                )
            } else {
                format!("A job is queued or running on {name}. Wait for it to finish.")
            },
        });
    }
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for id in ids {
        let rg: Option<String> = conn
            .query_row(
                "SELECT mb_release_group_mbid FROM albums WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .ok()
            .flatten()
            .filter(|s: &String| !s.is_empty());
        if let Some(rg) = rg {
            groups.entry(rg).or_default().push(name_of(id));
        }
    }
    if groups.len() > 1 {
        out.push(Blocker {
            code: blocker::ANCHORS_DIFFER,
            message: format!(
                "The discs are matched to different MusicBrainz releases ({}). Match them to the \
same release, or clear the match of the wrong one, first.",
                groups
                    .iter()
                    .map(|(rg, names)| format!("{} to {rg}", names.join(", ")))
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
        });
    } else if let Some(rg) = groups.keys().next() {
        for id in ids {
            let blocked: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM album_blocked_mb_release_groups \
                 WHERE album_id = ?1 AND release_group_mbid = ?2)",
                params![id, rg],
                |r| r.get(0),
            )?;
            if blocked {
                out.push(Blocker {
                    code: blocker::ANCHOR_BLOCKED,
                    message: format!(
                        "The set is matched to MusicBrainz release group {rg}, which is marked as \
a wrong match on {}. Fix the match first.",
                        name_of(id)
                    ),
                });
            }
        }
    }
    Ok(out)
}

/// `(album id, file name)` of rows kept for files no longer on disk.
type OrphanRows = Vec<(String, String)>;

/// The file names Musaic keeps rows for under `album_id` (fingerprints,
/// decode rejects, track tags).
fn row_filenames(conn: &Connection, album_id: &str) -> musaic_db::Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    for table in musaic_db::renames::FILE_ROW_TABLES {
        let mut stmt = conn.prepare(&format!(
            "SELECT DISTINCT filename FROM {table} WHERE album_id = ?1"
        ))?;
        for n in stmt.query_map([album_id], |r| r.get::<_, String>(0))? {
            out.insert(n?);
        }
    }
    Ok(out)
}

/// Rows of files that are no longer in their folder. The fold refuses a
/// member with any row left, so the other members' are carried to the
/// destination under their own name (data for a file that is gone stays
/// data for that name, as before). Where such a name is one a file of the
/// converted set gets, its rows would land on the wrong file: that blocks.
fn orphan_rows(
    conn: &Connection,
    facts: &[MemberFacts],
    plan: &Planned,
) -> musaic_db::Result<(Vec<Blocker>, OrphanRows)> {
    let finals: HashMap<String, String> = plan
        .tracks
        .iter()
        .filter_map(|t| Some((fold(t.new_filename.as_deref()?), t.filename.clone())))
        .collect();
    let mut blockers = Vec::new();
    let mut carried = Vec::new();
    let mut taken: HashMap<String, String> = HashMap::new();
    for (i, m) in facts.iter().enumerate() {
        let on_disk: Vec<&str> = m.files.iter().map(|f| f.name.as_str()).collect();
        for name in row_filenames(conn, &m.album_id)? {
            if on_disk.iter().any(|d| same_nfc(d, &name)) {
                continue;
            }
            let key = fold(&name);
            if let Some(file) = finals.get(&key) {
                blockers.push(Blocker {
                    code: blocker::ORPHAN_ROWS,
                    message: format!(
                        "Musaic keeps tags or a fingerprint for a file named {name} in {} that is \
no longer there, and {file} would get that name. Remove the tags of {name} first.",
                        m.dir_name
                    ),
                });
                continue;
            }
            if let Some(other) = taken.insert(key, m.dir_name.clone())
                && i > 0
            {
                blockers.push(Blocker {
                    code: blocker::ORPHAN_ROWS,
                    message: format!(
                        "Musaic keeps tags or a fingerprint for a file named {name} that is no \
longer there, in both {other} and {}.",
                        m.dir_name
                    ),
                });
                continue;
            }
            if i > 0 {
                carried.push((m.album_id.clone(), name));
            }
        }
    }
    Ok((blockers, carried))
}

/// Names that would meet when the files of the other discs are renamed in
/// their own folders and then moved: a renamed file onto something else in
/// its own folder, or a cover moving onto something in the destination.
/// (The other files that move are given a free name by the plan itself,
/// [`plan_folders`].)
fn gather_collisions(facts: &[MemberFacts], plan: &Planned) -> Vec<Blocker> {
    let mut out = Vec::new();
    for m in facts.iter().skip(1) {
        let others: HashMap<String, &str> = m
            .entries
            .iter()
            .map(|e| (fold(&e.name), e.name.as_str()))
            .collect();
        for t in plan.tracks.iter().filter(|t| t.album_id == m.album_id) {
            let Some(new) = &t.new_filename else { continue };
            if let Some(e) = others.get(&fold(new)) {
                out.push(Blocker {
                    code: blocker::NAME_COLLISION,
                    message: format!(
                        "{} would be renamed to {new}, which is already taken by {e} in {}.",
                        t.filename, m.dir_name
                    ),
                });
            }
        }
    }
    if let Some(dest) = facts.first() {
        let mut taken: HashMap<String, String> = dest
            .entries
            .iter()
            .map(|e| (fold(&e.name), e.name.clone()))
            .collect();
        for t in &plan.tracks {
            if let Some(n) = &t.new_filename {
                taken.insert(fold(n), n.clone());
            }
        }
        for f in plan.folders.iter().skip(1) {
            for c in f.covers.iter().filter(|c| c.action == CoverAction::Moved) {
                if let Some(e) = taken.insert(fold(&c.name), c.name.clone()) {
                    out.push(Blocker {
                        code: blocker::NAME_COLLISION,
                        message: format!(
                            "{} in {} would move into {}, which already has {e}.",
                            c.name, f.dir_name, dest.dir_name
                        ),
                    });
                }
            }
        }
    }
    out
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
        gather: None,
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
        if done {
            t.state = TrackState::Tagged;
        }
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

/// Whether `dir` lists an entry spelled `name` (up to NFC normalisation).
fn lists(dir: &Path, name: &str) -> bool {
    musaic_scan::dir_names(dir)
        .is_ok_and(|names| names.iter().any(|n| same_nfc(&n.to_string_lossy(), name)))
}

/// The preview of a set kept in several folders whose conversion did not
/// finish, from its record and the disk.
///
/// Every FLAC in the member folders and in the destination must be one of
/// the record's files in exactly one of the states it passes through: its
/// old name and old tags in its own folder (not started), its new name and
/// old tags there (renamed), its new name and new tags there (tagged), or
/// its new name and new tags in the destination (moved). A name alone
/// cannot tell two files apart where one file's new name is another's old
/// one, so the tags decide; a file that fits no state, or more than one
/// file of the record, is a blocker, never a guess. The plan is then made
/// as for the untouched set (the record gives a written file its old disc
/// and number back) and must come out as recorded. Covers, clutter, the
/// other files that move, the old sidecars and the folders themselves are
/// each either still to do or done, as the disk shows.
fn derive_resumed_folders(
    conn: &Connection,
    album_id: &str,
    record: Record,
) -> musaic_db::Result<Option<ConversionPreview>> {
    let rows = load_rows(conn, &record.library_path)?;
    let row_of = |id: &str| rows.iter().find(|r| r.id == id);
    if row_of(album_id).is_none() {
        return Ok(None);
    }
    let templates = Templates::from_settings(conn)?;
    let mut blockers: Vec<Blocker> = Vec::new();
    let mismatch = |message: String| Blocker {
        code: blocker::RESUME_MISMATCH,
        message,
    };

    // The destination: under its old id, or re-keyed by the folder rename.
    let dest = &record.members[0];
    let dest_row = [
        Some(record.album_id_before.as_str()),
        Some(record.album_id_expected.as_str()),
        record.album_id_after.as_deref(),
    ]
    .into_iter()
    .flatten()
    .find_map(row_of);
    let mut preview = ConversionPreview {
        album_id: album_id.to_string(),
        state: PreviewState::Folders,
        reason: None,
        numbering: None,
        disc_total: Some(record.members.len() as i64),
        present_discs: record.members.iter().map(|m| m.disc).collect(),
        absent_discs: Vec::new(),
        members: record
            .members
            .iter()
            .map(|m| SetMember {
                album_id: m.album_id.clone(),
                dir_name: Some(m.folder_name.clone()),
                disc: m.disc,
                missing: row_of(&m.album_id).is_some_and(|r| r.missing),
            })
            .collect(),
        is_compilation: dest_row.is_some_and(|r| r.is_compilation),
        template: String::new(),
        dir_template: String::new(),
        target_current: None,
        target_path: None,
        library_path: record.library_path.clone(),
        plan: Planned::default(),
        resume: Some(record.clone()),
        gather: None,
    };
    let Some(dest_row) = dest_row else {
        preview.plan.blockers.push(Blocker {
            code: blocker::MEMBER_GONE,
            message: format!(
                "{} is no longer in the library, so the conversion cannot be finished. The record \
is {}.",
                dest.folder_name,
                record.path.display()
            ),
        });
        return Ok(Some(preview));
    };
    let dest_id = dest_row.id.clone();
    let mut dest_dir = PathBuf::from(&dest_row.path);
    let after = Path::new(&record.folder_before)
        .parent()
        .map(|p| p.join(&record.folder_name_after))
        .unwrap_or_default();
    let renames_folder = !same_nfc(&record.folder_name_before, &record.folder_name_after);
    let mut rekey_pending = false;
    if renames_folder
        && dest_id == record.album_id_before
        && !dest_dir.is_dir()
        && after.is_dir()
        && row_of(&record.album_id_expected).is_none()
    {
        // The folder got its new name but the process stopped before its
        // row did.
        rekey_pending = true;
        dest_dir = after.clone();
    }
    let folder_renamed = rekey_pending
        || (renames_folder && same_nfc(&file_name(&dest_dir), &record.folder_name_after));

    // The members as they are now.
    let mut gm: Vec<GatherMember> = Vec::new();
    for (i, m) in record.members.iter().enumerate() {
        let (id, dir) = if i == 0 {
            (dest_id.clone(), dest_dir.clone())
        } else {
            (m.album_id.clone(), PathBuf::from(&m.folder))
        };
        let r = row_of(&id);
        gm.push(GatherMember {
            album_id: id,
            dir_name: file_name(&dir),
            exists: dir.is_dir(),
            dir,
            disc: m.disc,
            destination: i == 0,
            has_row: r.is_some(),
            missing: r.is_some_and(|r| r.missing),
        });
    }
    let others_with_row = gm.iter().skip(1).filter(|m| m.has_row).count();
    let folded = others_with_row == 0;
    if !folded && others_with_row < gm.len() - 1 {
        for m in gm.iter().skip(1).filter(|m| !m.has_row) {
            blockers.push(Blocker {
                code: blocker::MEMBER_GONE,
                message: format!(
                    "{} is no longer in the library, but the other discs were not merged yet. \
The record is {}.",
                    m.dir_name,
                    record.path.display()
                ),
            });
        }
    }
    if !gm[0].exists {
        blockers.push(Blocker {
            code: blocker::MEMBER_MISSING,
            message: format!("The folder {} is not on disk right now.", gm[0].dir_name),
        });
    }

    // Every FLAC where it is now.
    let facts_now: Vec<Option<MemberFacts>> = gm
        .iter()
        .map(|m| {
            m.exists
                .then(|| gather_member(&m.album_id, &m.dir, (!m.destination).then_some(m.disc)))
        })
        .collect();
    for f in facts_now.iter().flatten() {
        if let Some(p) = &f.problem {
            blockers.push(p.clone());
        }
        for name in &f.stranded {
            blockers.push(Blocker {
                code: blocker::STRANDED_TEMP,
                message: format!(
                    "{name} in {} is a file left under a temporary name by a rename that did not \
finish. Give it back its own name (without the leading dot and the .renaming ending), then try again.",
                    f.dir_name
                ),
            });
        }
    }
    let member_of = |e: &LoggedFile| record.members.iter().position(|m| m.album_id == e.album_id);
    // Per record file: (folder index, the file's facts now, its state).
    let mut found: Vec<Vec<(usize, FileFacts, TrackState)>> = vec![Vec::new(); record.files.len()];
    for (fi, facts) in facts_now.iter().enumerate() {
        let Some(facts) = facts else { continue };
        for f in &facts.files {
            if !f.readable {
                blockers.push(Blocker {
                    code: blocker::MEMBER_UNREADABLE,
                    message: format!("{} in {} could not be read.", f.name, facts.dir_name),
                });
                continue;
            }
            let mut fits: BTreeMap<usize, TrackState> = BTreeMap::new();
            for (i, e) in record.files.iter().enumerate() {
                let Some(mi) = member_of(e) else { continue };
                let old = same_nfc(&f.name, &e.old_name);
                let new = same_nfc(&f.name, &e.new_name);
                let mut states = Vec::new();
                if fi == mi {
                    if old && f.numbering == e.old_tags {
                        states.push(TrackState::Untouched);
                    }
                    if new && f.numbering == e.old_tags {
                        states.push(TrackState::Renamed);
                    }
                    if new && f.numbering == e.new_tags {
                        states.push(TrackState::Tagged);
                    }
                } else if fi == 0 && new && f.numbering == e.new_tags {
                    states.push(TrackState::Moved);
                }
                // A file whose name and tags do not change fits more than
                // one state of the same entry: it is as far as it can be.
                if let Some(s) = states.into_iter().max_by_key(|s| *s as u8) {
                    fits.insert(i, s);
                }
            }
            match fits.len() {
                1 => {
                    let (i, s) = fits.into_iter().next().expect("one");
                    found[i].push((fi, f.clone(), s));
                }
                0 => blockers.push(mismatch(format!(
                    "{} in {} is not one of the files of the interrupted conversion, or its track \
number or disc tags changed since.",
                    f.name, facts.dir_name
                ))),
                _ => blockers.push(mismatch(format!(
                    "{} in {} could be more than one file of the interrupted conversion, so \
Musaic cannot tell which it is.",
                    f.name, facts.dir_name
                ))),
            }
        }
    }
    let mut placed: Vec<Option<(usize, FileFacts, TrackState)>> = Vec::new();
    for (i, e) in record.files.iter().enumerate() {
        match found[i].len() {
            1 => placed.push(found[i].pop()),
            0 => {
                // A member that is gone with its folder: its files were
                // all moved before (the fold), so the destination has them.
                blockers.push(mismatch(format!(
                    "{} (before the conversion: {} in {}) is not in any of the set's folders.",
                    e.new_name,
                    e.old_name,
                    member_of(e)
                        .map(|m| record.members[m].folder_name.clone())
                        .unwrap_or_default()
                )));
                placed.push(None);
            }
            _ => {
                blockers.push(mismatch(format!(
                    "{} is in the set's folders more than once.",
                    e.new_name
                )));
                placed.push(None);
            }
        }
    }
    // A Missing member is only fine once every file of it has moved.
    for (k, m) in gm.iter().enumerate() {
        let pending =
            record.files.iter().zip(&placed).any(|(e, p)| {
                member_of(e) == Some(k) && !matches!(p, Some((_, _, TrackState::Moved)))
            });
        if (m.missing || !m.exists) && pending && k > 0 {
            blockers.push(Blocker {
                code: blocker::MEMBER_MISSING,
                message: format!(
                    "{} is not on disk right now, and some of its files have not been moved yet.",
                    m.dir_name
                ),
            });
        }
    }

    // The plan as for the untouched set, from what the record kept.
    let mut members: Vec<MemberFacts> = Vec::new();
    for (k, m) in record.members.iter().enumerate() {
        let mut facts = MemberFacts {
            album_id: m.album_id.clone(),
            dir_name: m.folder_name.clone(),
            folder_disc: Some(m.disc),
            ..Default::default()
        };
        if k == 0 {
            facts.album_id = dest_id.clone();
            facts.dir_name = gm[0].dir_name.clone();
            if let Some(now) = &facts_now[0] {
                facts.entries = now.entries.clone();
            }
        }
        for (e, p) in record.files.iter().zip(&placed) {
            if member_of(e) != Some(k) {
                continue;
            }
            let Some((_, f, s)) = p else { continue };
            let mut f = f.clone();
            if matches!(s, TrackState::Tagged | TrackState::Moved) {
                let old = |key: &str| values_of(&e.old_tags, key).to_vec();
                f.tracknumber = old("TRACKNUMBER").first().cloned().unwrap_or_default();
                f.disc = flacmeta::file_disc(
                    old("DISCNUMBER").iter().map(String::as_str),
                    old("TRACKNUMBER").iter().map(String::as_str),
                );
                f.has_disc_tags = ["DISCNUMBER", "DISCTOTAL", "TOTALDISCS"]
                    .iter()
                    .any(|key| !values_of(&e.old_tags, key).is_empty());
                f.numbering = e.old_tags.clone();
            }
            facts.files.push(f);
        }
        members.push(facts);
    }
    let target_row = rename_plan_row(conn, &dest_id)?;
    let mut plan = match &target_row {
        Some(r) => {
            let (t, d) = templates.for_row(r);
            preview.template = t.to_string();
            preview.dir_template = d.to_string();
            plan_conversion(&PlanInput {
                members: &members,
                target_row: r,
                templates: &templates,
            })
        }
        None => Planned::default(),
    };
    // The plan's member ids are the record's; the destination's is the one
    // it has now.
    let mut changed = Vec::new();
    for t in plan.tracks.iter_mut() {
        let Some((i, e)) = record.files.iter().enumerate().find(|(_, e)| {
            let mi = member_of(e);
            let id_now = if mi == Some(0) { &dest_id } else { &e.album_id };
            *id_now == t.album_id
                && placed_name(&placed, &record.files, e).is_some_and(|n| n == t.filename)
        }) else {
            continue;
        };
        let (fi, _, s) = placed[i].as_ref().expect("placed");
        t.state = *s;
        t.tags_done = matches!(s, TrackState::Tagged | TrackState::Moved);
        t.folder = gm[*fi].dir_name.clone();
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
    let complete = placed.iter().all(Option::is_some);
    if !changed.is_empty() || (complete && plan.track_total != record.track_total) {
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
        && !folder_renamed
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
    if folder_renamed {
        plan.target_name = Some(gm[0].dir_name.clone());
    }

    // Covers, clutter, other files, sidecars and folders: still to do, or
    // done.
    plan.folders = record
        .folders
        .iter()
        .map(|lf| {
            let k = record
                .members
                .iter()
                .position(|m| m.album_id == lf.album_id)
                .unwrap_or(0);
            let m = &gm[k];
            let here = |name: &str| m.exists && lists(&m.dir, name);
            let mut covers = Vec::new();
            for (name, action) in &lf.covers {
                let action = CoverAction::parse(action).unwrap_or(CoverAction::Kept);
                let done = match action {
                    CoverAction::Kept => false,
                    CoverAction::Moved => {
                        let in_dest = gm[0].exists && lists(&gm[0].dir, name);
                        match (here(name), in_dest) {
                            (true, false) => false,
                            (false, true) => true,
                            (true, true) => {
                                blockers.push(mismatch(format!(
                                    "{name} is both in {} and in {}, so Musaic cannot tell which \
one to keep.",
                                    m.dir_name, gm[0].dir_name
                                )));
                                false
                            }
                            (false, false) => {
                                blockers.push(mismatch(format!(
                                    "{name}, which was to move from {} into {}, is in neither.",
                                    m.dir_name, gm[0].dir_name
                                )));
                                false
                            }
                        }
                    }
                    CoverAction::Identical | CoverAction::Different => !here(name),
                };
                covers.push(CoverPlan {
                    name: name.clone(),
                    action,
                    done,
                });
            }
            // A file that moves is in exactly one of the two folders: its
            // own under its own name, or the destination under the name
            // the record gave it.
            let mut strays = Vec::new();
            for s in &lf.strays {
                let action = StrayAction::parse(&s.action).unwrap_or(StrayAction::Moved);
                let target = s.new_name.as_deref().unwrap_or(&s.name);
                let done = match action {
                    StrayAction::Identical => !here(&s.name),
                    StrayAction::Moved => {
                        let in_dest = gm[0].exists && lists(&gm[0].dir, target);
                        match (here(&s.name), in_dest) {
                            (true, false) => false,
                            (false, true) => true,
                            (true, true) => {
                                blockers.push(mismatch(format!(
                                    "{} is still in {} and {target} is in {} too, so Musaic \
cannot tell whether it was moved.",
                                    s.name, m.dir_name, gm[0].dir_name
                                )));
                                false
                            }
                            (false, false) => {
                                blockers.push(mismatch(format!(
                                    "{}, which was to move from {} into {}, is in neither.",
                                    s.name, m.dir_name, gm[0].dir_name
                                )));
                                false
                            }
                        }
                    }
                };
                strays.push(StrayPlan {
                    name: s.name.clone(),
                    action,
                    new_name: s.new_name.clone(),
                    done,
                });
            }
            FolderPlan {
                album_id: m.album_id.clone(),
                dir_name: lf.folder_name.clone(),
                disc: (!m.destination).then_some(m.disc),
                destination: m.destination,
                covers,
                sidecar: lf.sidecar,
                sidecar_done: lf.sidecar && !here(SIDECAR_FILENAME),
                other_files: lf.other_files.clone(),
                strays,
                junk: lf.junk.iter().map(|j| (j.clone(), !here(j))).collect(),
                exists: m.exists,
            }
        })
        .collect();

    preview.target_current = Some(gm[0].dir_name.clone());
    preview.target_path = Some(dest_row.path.clone());
    plan.blockers.extend(blockers);
    if gm[0].exists && !rekey_pending {
        let folders: Vec<(String, PathBuf)> = gm
            .iter()
            .filter(|m| m.exists)
            .map(|m| (m.dir_name.clone(), m.dir.clone()))
            .collect();
        let target = plan
            .target_name
            .as_deref()
            .filter(|t| !folder_renamed && preview.target_current.as_deref() != Some(*t));
        plan.blockers.extend(disk_blockers(&folders, target));
    }
    let with_rows: Vec<&str> = gm
        .iter()
        .filter(|m| m.has_row)
        .map(|m| m.album_id.as_str())
        .collect();
    let name_of = |id: &str| {
        gm.iter()
            .find(|m| m.album_id == id)
            .map(|m| m.dir_name.clone())
            .unwrap_or_else(|| id.to_string())
    };
    if !folded {
        plan.blockers
            .extend(set_blockers(conn, &with_rows, &name_of)?);
    } else {
        for id in active_jobs(conn, &[dest_id.as_str()])? {
            plan.blockers.push(Blocker {
                code: blocker::JOB_ACTIVE,
                message: format!(
                    "A job is queued or running on {}. Wait for it to finish.",
                    name_of(&id)
                ),
            });
        }
    }
    let pairs: Vec<(Option<i64>, Option<i64>)> =
        plan.tracks.iter().map(|t| (t.disc, t.track)).collect();
    preview.numbering = Some(classify_numbering(&pairs));
    preview.gather = Some(Gather {
        members: gm,
        orphan_rows: record.orphan_rows.clone(),
        folded,
        folder_renamed,
        rekey_pending,
    });
    preview.plan = plan;
    Ok(Some(preview))
}

/// The name a placed record file has now.
fn placed_name<'a>(
    placed: &'a [Option<(usize, FileFacts, TrackState)>],
    files: &[LoggedFile],
    e: &LoggedFile,
) -> Option<&'a str> {
    let i = files.iter().position(|x| std::ptr::eq(x, e))?;
    placed[i].as_ref().map(|(_, f, _)| f.name.as_str())
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
            digest: None,
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
                digest: None,
            },
            EntryFacts {
                name: SIDECAR_FILENAME.into(),
                kind: EntryKind::Sidecar,
                digest: None,
            },
            EntryFacts {
                name: "notes.txt".into(),
                kind: EntryKind::Other,
                digest: None,
            },
        ];
        let mut d2 = member("d2", 2, 1);
        d2.entries = vec![
            EntryFacts {
                name: "Cover.JPG".into(),
                kind: EntryKind::Cover(vec![1, 2]),
                digest: None,
            },
            EntryFacts {
                name: "folder.jpg".into(),
                kind: EntryKind::Cover(vec![9]),
                digest: None,
            },
            EntryFacts {
                name: ".DS_Store".into(),
                kind: EntryKind::Junk,
                digest: None,
            },
        ];
        let mut d3 = member("d3", 3, 1);
        d3.entries = vec![
            EntryFacts {
                name: "cover.jpg".into(),
                kind: EntryKind::Cover(vec![3]),
                digest: None,
            },
            EntryFacts {
                name: "folder.jpg".into(),
                kind: EntryKind::Cover(vec![9]),
                digest: None,
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
        // The destination has a cover, so no other picture moves in, under
        // whatever name: the set keeps its own picture.
        assert_eq!(
            acts(1),
            vec![
                ("Cover.JPG", CoverAction::Identical),
                ("folder.jpg", CoverAction::Different)
            ]
        );
        assert_eq!(
            acts(2),
            vec![
                ("cover.jpg", CoverAction::Different),
                ("folder.jpg", CoverAction::Different)
            ]
        );
        assert!(p.folders[0].sidecar);
        assert_eq!(p.folders[0].other_files, vec!["notes.txt".to_string()]);
        assert!(p.folders[1].other_files.is_empty());
        assert_eq!(p.folders[1].junk, vec![(".DS_Store".to_string(), false)]);

        // Without a cover of its own, the destination takes the covers of
        // the first disc that has any; a copy of one of them elsewhere is
        // not needed.
        let mut members = members;
        members[0]
            .entries
            .retain(|e| !matches!(e.kind, EntryKind::Cover(_)));
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
        assert!(acts(0).is_empty());
        assert_eq!(
            acts(1),
            vec![
                ("Cover.JPG", CoverAction::Moved),
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
    }

    fn other(name: &str, digest: Option<&str>) -> EntryFacts {
        EntryFacts {
            name: name.into(),
            kind: EntryKind::Other,
            digest: digest.map(str::to_string),
        }
    }

    /// `(name, action, new name)` of each stray of folder `i`.
    fn strays(p: &Planned, i: usize) -> Vec<(&str, StrayAction, Option<&str>)> {
        p.folders[i]
            .strays
            .iter()
            .map(|s| (s.name.as_str(), s.action, s.new_name.as_deref()))
            .collect()
    }

    #[test]
    fn other_files_move_with_the_set() {
        let mut d1 = member("d1", 1, 1);
        d1.entries = vec![other("rip.log", Some("aa")), other("info.txt", Some("cc"))];
        let mut d2 = member("d2", 2, 1);
        d2.entries = vec![
            other("Rip.LOG", Some("bb")),
            other("booklet.pdf", None),
            other("info.txt", Some("cc")),
            EntryFacts {
                name: "Scans".into(),
                kind: EntryKind::Folder,
                digest: None,
            },
        ];
        let mut d3 = member("d3", 3, 1);
        d3.entries = vec![
            other("booklet.pdf", None),
            other("rip.log", Some("aa")),
            other("README", Some("dd")),
        ];
        let members = vec![d1, d2, d3];
        let (row, t) = (target_row(), templates());
        let p = plan_conversion(&PlanInput {
            members: &members,
            target_row: &row,
            templates: &t,
        });
        assert!(p.blockers.is_empty(), "{:?}", p.blockers);
        // The destination's own files stay with the set and nothing of it
        // is planned to move.
        assert_eq!(p.folders[0].other_files, ["rip.log", "info.txt"]);
        assert!(p.folders[0].strays.is_empty());
        assert_eq!(
            strays(&p, 1),
            [
                // Another log under the same name (as an insensitive volume
                // compares names): it moves with the disc in its name.
                ("Rip.LOG", StrayAction::Moved, Some("Rip (disc 2).LOG")),
                // A free name: it moves as it is.
                ("booklet.pdf", StrayAction::Moved, None),
                // The same content as the destination's: not needed.
                ("info.txt", StrayAction::Identical, None),
            ]
        );
        // Only the subfolder stays in disc 2's folder.
        assert_eq!(p.folders[1].other_files, ["Scans"]);
        assert_eq!(
            strays(&p, 2),
            [
                // Disc 2's booklet has the name by now, and nothing says
                // the two are the same: it is never taken for a copy.
                (
                    "booklet.pdf",
                    StrayAction::Moved,
                    Some("booklet (disc 3).pdf")
                ),
                ("rip.log", StrayAction::Identical, None),
                ("README", StrayAction::Moved, None),
            ]
        );
        assert!(p.folders[2].other_files.is_empty());
    }

    #[test]
    fn a_stray_whose_second_name_is_taken_too_blocks() {
        let mut d1 = member("d1", 1, 1);
        d1.entries = vec![
            other("rip.log", Some("aa")),
            other("rip (disc 2).log", None),
        ];
        let mut d2 = member("d2", 2, 1);
        d2.entries = vec![other("rip.log", Some("bb"))];
        let members = vec![d1, d2];
        let (row, t) = (target_row(), templates());
        let p = plan_conversion(&PlanInput {
            members: &members,
            target_row: &row,
            templates: &t,
        });
        assert_eq!(codes(&p), [blocker::NAME_COLLISION], "{:?}", p.blockers);
        assert!(
            p.blockers[0].message.contains("rip (disc 2).log"),
            "{}",
            p.blockers[0].message
        );
    }

    #[test]
    fn a_stray_never_takes_a_name_the_set_uses() {
        // A cover the destination takes from disc 2, the sidecar's name and
        // a converted file's name are all taken before a stray is placed.
        let d1 = member("d1", 1, 1);
        let mut d2 = member("d2", 2, 1);
        d2.entries = vec![EntryFacts {
            name: "cover.jpg".into(),
            kind: EntryKind::Cover(vec![1]),
            digest: None,
        }];
        let mut d3 = member("d3", 3, 1);
        d3.entries = vec![other("COVER.JPG", None)];
        // (A name spelled like a cover is a cover; this one stands for any
        // file whose name the destination has by then.)
        let members = vec![d1, d2, d3];
        let (row, t) = (target_row(), templates());
        let p = plan_conversion(&PlanInput {
            members: &members,
            target_row: &row,
            templates: &t,
        });
        assert!(p.blockers.is_empty(), "{:?}", p.blockers);
        assert_eq!(
            strays(&p, 2),
            [("COVER.JPG", StrayAction::Moved, Some("COVER (disc 3).JPG"))]
        );
    }

    #[test]
    fn disc_names() {
        assert_eq!(disc_name("rip.log", 2), "rip (disc 2).log");
        assert_eq!(disc_name("a.tar.gz", 3), "a.tar (disc 3).gz");
        assert_eq!(disc_name("README", 2), "README (disc 2)");
        assert_eq!(disc_name(".hidden", 2), ".hidden (disc 2)");
    }

    #[test]
    fn only_names_more_than_one_folder_has_are_read() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs: Vec<PathBuf> = ["a", "b", "c"].iter().map(|d| tmp.path().join(d)).collect();
        for d in &dirs {
            std::fs::create_dir(d).unwrap();
        }
        std::fs::write(dirs[0].join("rip.log"), b"one").unwrap();
        std::fs::write(dirs[1].join("RIP.log"), b"two").unwrap();
        std::fs::write(dirs[2].join("rip.log"), b"one").unwrap();
        std::fs::write(dirs[1].join("alone.txt"), b"x").unwrap();
        std::fs::create_dir(dirs[2].join("Scans")).unwrap();
        let mut facts: Vec<MemberFacts> = dirs
            .iter()
            .enumerate()
            .map(|(i, d)| gather_member(&format!("m{i}"), d, Some(i as i64 + 1)))
            .collect();
        digest_shared_strays(&mut facts, &dirs);
        let entry = |i: usize, name: &str| {
            facts[i]
                .entries
                .iter()
                .find(|e| e.name == name)
                .unwrap()
                .clone()
        };
        assert!(entry(0, "rip.log").digest.is_some());
        assert_eq!(entry(0, "rip.log").digest, entry(2, "rip.log").digest);
        assert_ne!(entry(0, "rip.log").digest, entry(1, "RIP.log").digest);
        assert_eq!(entry(1, "alone.txt").digest, None);
        assert_eq!(entry(2, "Scans").kind, EntryKind::Folder);
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
