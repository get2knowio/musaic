//! Folding several album rows into one: the database side of gathering a
//! multi-disc set kept in several folders into one folder.
//!
//! The caller (the conversion of a set kept in several folders) moves the
//! files of every other member ("from" albums) into the folder of the
//! member that survives ("into"), then folds what is left of the emptied
//! rows into the survivor, then renames the survivor's folder (which
//! re-keys it through [`crate::renames::move_album_rows`]). This module
//! is the first two steps' database half; it touches no file and
//! publishes nothing.
//!
//! # The calls, and the state each one leaves
//!
//! 1. [`move_track_rows`], once per file, right after that file moved on
//!    disk: the file's rows follow it to the survivor under the name the
//!    target folder lists. One transaction per call. Afterwards every row
//!    of that file is under `(into, into_name)` and none under
//!    `(from, from_name)`. Calling it again is a no-op.
//! 2. [`merge_album_rows`], once every file has moved: one transaction
//!    that carries over what hangs off the emptied rows and deletes them.
//!    Afterwards no album row has a from id; nothing refers to a from id but
//!    history (resolved findings, `check_runs.scope`); the survivor holds
//!    the union of the members' per-file rows and blocklists, the agreed
//!    anchor, the latest AI review stamp, and no `check_inputs` row (so the
//!    background sweep checks it again). Calling it again finds no from row
//!    and changes nothing ([`MergeReport::already_merged`]).
//! 3. The folder rename, by the existing rename code.
//!
//! # Interruption
//!
//! Every call is one transaction, so a crash leaves the database as it was
//! before the call or after it. Between steps the database is consistent:
//!
//! * during step 1, some files' rows are under the survivor, the rest under
//!   their member, each matching where its file is (or where it was, if the
//!   crash fell between the file move and its rows' move: calling
//!   [`move_track_rows`] again for that file repairs it, and for a file
//!   whose rows already moved it does nothing);
//! * after step 1 and before step 2, the from rows exist with no per-file
//!   rows. A scan in between sees their folders hold no FLAC and may mark
//!   them Missing; [`MergeRequest::emptied`] is how the caller says that is
//!   its own doing (see there);
//! * after step 2, the survivor is one album with every file's rows.
//!
//! The order "move every file, then fold, then rename" is what makes a
//! resumed run converge: each call only looks at what is still left.
//!
//! # What this does not decide
//!
//! Whether the members are one set at all (same parent, disc markers,
//! names): that is the caller's detection. Here only what the database can
//! check is checked, and every unclear case refuses
//! ([`MergeRefusal`]), changing nothing.

use std::collections::BTreeMap;
use std::fmt;

use rusqlite::{Connection, OptionalExtension, Transaction, params, params_from_iter};

use crate::renames::{FILE_ROW_TABLES, FileRowsMoved, move_file_rows};
use crate::{Error, Result};

/// Why a merge call refused. Nothing was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeRefusal {
    /// An album id or a file name was empty.
    Empty(&'static str),
    /// No from-album was given.
    NoMembers,
    /// The same from-album was listed twice.
    ListedTwice(String),
    /// The surviving album is also listed among the albums to fold into it.
    IntoAmongFrom(String),
    /// A per-file move within one album is a rename
    /// ([`crate::renames::rename_track_files`]).
    SameAlbum(String),
    /// [`MergeRequest::emptied`] names an album that is not a from-album.
    EmptiedNotAMember(String),
    /// No album row has this id (and, for a from-album, it is not one an
    /// earlier fold removed: rows of a removed album still refer to it, or
    /// other from-albums still exist).
    UnknownAlbum(String),
    /// A member is in another library than the survivor.
    OtherLibrary {
        album_id: String,
        library: String,
        into_library: String,
    },
    /// A member is Missing (its folder was not found by the last scan).
    /// The survivor never may be; a from-album only when the caller lists it
    /// in [`MergeRequest::emptied`].
    Missing { album_id: String, survivor: bool },
    /// A from-album still has per-file rows: a file that was not moved.
    FileRowsLeft {
        album_id: String,
        /// `(table, rows)` for each table that still has rows.
        rows: Vec<(&'static str, usize)>,
    },
    /// A member has an AI job that is queued, running, or completed and not
    /// yet acted on (its suggestion was made for that album alone).
    ActiveJob {
        album_id: String,
        job_id: String,
        status: String,
    },
    /// Members are anchored to different MusicBrainz release groups.
    ReleaseGroupsDiffer { anchors: Vec<(String, String)> },
    /// The one release group the members are anchored to is blocked as a
    /// wrong match on a member: the survivor would hold it both ways.
    ReleaseGroupBlocked {
        release_group: String,
        blocked_on: String,
    },
}

impl fmt::Display for MergeRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use MergeRefusal::*;
        match self {
            Empty(what) => write!(f, "cannot merge: the {what} is empty"),
            NoMembers => write!(f, "cannot merge: no album to merge was given"),
            ListedTwice(id) => write!(f, "cannot merge: album {id} is listed twice"),
            IntoAmongFrom(id) => write!(
                f,
                "cannot merge: album {id} is both the one kept and one merged into it"
            ),
            SameAlbum(id) => write!(
                f,
                "cannot move a file's rows from album {id} to itself; that is a rename"
            ),
            EmptiedNotAMember(id) => write!(
                f,
                "cannot merge: album {id} is said to be emptied by this merge but is not merged"
            ),
            UnknownAlbum(id) => write!(f, "cannot merge: no album has id {id}"),
            OtherLibrary {
                album_id,
                library,
                into_library,
            } => write!(
                f,
                "cannot merge: album {album_id} is in library {library}, not {into_library}"
            ),
            Missing {
                album_id,
                survivor: true,
            } => write!(
                f,
                "cannot merge into album {album_id}: its folder is missing"
            ),
            Missing { album_id, .. } => write!(
                f,
                "cannot merge album {album_id}: its folder is missing, and this merge did not empty it"
            ),
            FileRowsLeft { album_id, rows } => {
                let list: Vec<String> = rows.iter().map(|(t, n)| format!("{n} in {t}")).collect();
                write!(
                    f,
                    "cannot merge album {album_id}: it still has rows of files that were not moved ({})",
                    list.join(", ")
                )
            }
            ActiveJob {
                album_id,
                job_id,
                status,
            } => write!(
                f,
                "cannot merge: album {album_id} has an AI job ({job_id}) that is {status}; \
                 finish or discard it first"
            ),
            ReleaseGroupsDiffer { anchors } => {
                let list: Vec<String> = anchors
                    .iter()
                    .map(|(id, rg)| format!("{id}: {rg}"))
                    .collect();
                write!(
                    f,
                    "cannot merge: the albums are matched to different MusicBrainz release groups ({})",
                    list.join("; ")
                )
            }
            ReleaseGroupBlocked {
                release_group,
                blocked_on,
            } => write!(
                f,
                "cannot merge: release group {release_group} is the match of one album and \
                 marked a wrong match on album {blocked_on}"
            ),
        }
    }
}

fn refuse<T>(r: MergeRefusal) -> Result<T> {
    Err(Error::MergeRefused(r))
}

/// The album columns a merge reads.
struct Member {
    id: String,
    library_path: String,
    missing: bool,
    release_group: Option<String>,
    release: Option<String>,
    score: Option<i64>,
    ai_acted_on_at: Option<String>,
}

fn non_empty(s: Option<String>) -> Option<String> {
    s.filter(|s| !s.is_empty())
}

fn read_member(conn: &Connection, id: &str) -> Result<Option<Member>> {
    Ok(conn
        .query_row(
            "SELECT library_path, missing_since, mb_release_group_mbid, mb_release_mbid, \
                    mb_match_score, metadata_ai_acted_on_at \
             FROM albums WHERE id = ?1",
            [id],
            |r| {
                Ok(Member {
                    id: id.to_string(),
                    library_path: r.get(0)?,
                    // The scan's own test: NULL or empty is Present.
                    missing: non_empty(r.get(1)?).is_some(),
                    release_group: non_empty(r.get(2)?),
                    release: non_empty(r.get(3)?),
                    score: r.get(4)?,
                    ai_acted_on_at: non_empty(r.get(5)?),
                })
            },
        )
        .optional()?)
}

fn count(conn: &Connection, sql: &str, id: &str) -> Result<usize> {
    let n: i64 = conn.query_row(sql, [id], |r| r.get(0))?;
    Ok(usize::try_from(n).unwrap_or(0))
}

/// Move one file's rows (`track_fingerprints`, `track_tags`,
/// `fingerprint_rejects`: every table keyed by album id and file name,
/// [`FILE_ROW_TABLES`]) from one album to another, possibly under a new
/// name, in one transaction. Called right after the file itself moved,
/// with the name the target folder lists.
///
/// The rule for rows already under the target name is
/// [`crate::renames::rename_track_files`]'s (the same code): tags are
/// united; a fingerprint or reject row of the source replaces the target's,
/// which described a file no longer there; without a source row the
/// target's is kept. So a second call, or a call for a file whose rows a
/// fingerprint pass already wrote under the target, changes nothing.
///
/// Refuses (nothing written) an empty id or name, `from == into`, an
/// `into` with no row or that is Missing, and a from-album in another
/// library. A from-album with no row has no rows to move (its rows went
/// with it): the call does nothing, which is what a call repeated after
/// the fold finds. A Missing from-album is allowed: a scan may have marked
/// it so after its files left.
pub fn move_track_rows(
    conn: &Connection,
    from_album: &str,
    from_filename: &str,
    into_album: &str,
    into_filename: &str,
) -> Result<FileRowsMoved> {
    for (v, what) in [
        (from_album, "album id to move from"),
        (into_album, "album id to move into"),
        (from_filename, "file name to move from"),
        (into_filename, "file name to move into"),
    ] {
        if v.is_empty() {
            return refuse(MergeRefusal::Empty(what));
        }
    }
    if from_album == into_album {
        return refuse(MergeRefusal::SameAlbum(from_album.to_string()));
    }
    let tx = crate::immediate(conn)?;
    let Some(into) = read_member(&tx, into_album)? else {
        return refuse(MergeRefusal::UnknownAlbum(into_album.to_string()));
    };
    if into.missing {
        return refuse(MergeRefusal::Missing {
            album_id: into.id,
            survivor: true,
        });
    }
    let Some(from) = read_member(&tx, from_album)? else {
        return Ok(FileRowsMoved::default());
    };
    if from.library_path != into.library_path {
        return refuse(MergeRefusal::OtherLibrary {
            album_id: from.id,
            library: from.library_path,
            into_library: into.library_path,
        });
    }
    let moved = move_file_rows(&tx, from_album, from_filename, into_album, into_filename)?;
    tx.commit()?;
    Ok(moved)
}

/// What [`merge_album_rows`] is asked to do.
#[derive(Debug, Clone, Copy)]
pub struct MergeRequest<'a> {
    /// The albums folded into `into_id` and then deleted.
    pub from_ids: &'a [String],
    /// The album that survives.
    pub into_id: &'a str,
    /// From-albums the caller vouches were emptied by this very merge, so
    /// that being Missing is expected: a scan that ran between the file
    /// moves and the fold (after a crash, say) found their folders without
    /// a FLAC. The caller proves it from its own record of the merge, which
    /// lists the members and the files it moved out of each, and by the
    /// files now being in the survivor's folder; the fold itself then
    /// checks that no file row is left under the member. Any other Missing
    /// member refuses: its files may be on a drive that is not mounted.
    pub emptied: &'a [String],
    /// The time stamped on the findings this resolves.
    pub now: &'a str,
}

/// What [`merge_album_rows`] did, per table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergeReport {
    /// No from-album row existed, and nothing but history refers to them:
    /// a fold already done (or ids no album ever had). Nothing was written
    /// and every other field is zero / `None`.
    pub already_merged: bool,
    /// From-album rows deleted.
    pub albums_deleted: usize,
    /// Blocklist rows the survivor gained.
    pub album_blocked_mb_release_groups: usize,
    /// Open or ignored findings of the from-albums resolved in place.
    pub check_findings_resolved: usize,
    /// Finished AI jobs repointed to the survivor.
    pub ai_jobs: usize,
    /// `check_inputs` rows deleted (from-albums and survivor).
    pub check_inputs: usize,
    /// The survivor's release group after the fold.
    pub release_group: Option<String>,
    /// The survivor's release after the fold.
    pub release: Option<String>,
    /// Members named different releases, so none was kept.
    pub release_cleared: bool,
    /// The survivor's `metadata_ai_acted_on_at` after the fold.
    pub metadata_ai_acted_on_at: Option<String>,
}

/// [`merge_album_rows_tx`] in a transaction of its own (`BEGIN
/// IMMEDIATE`), committed on success and rolled back on a refusal.
pub fn merge_album_rows(conn: &Connection, req: &MergeRequest<'_>) -> Result<MergeReport> {
    let tx = crate::immediate(conn)?;
    let report = merge_album_rows_tx(&tx, req)?;
    tx.commit()?;
    Ok(report)
}

/// Fold the from-albums into the survivor and delete their rows, on the
/// caller's transaction. Every precondition is checked before the first
/// write, so a refusal ([`Error::MergeRefused`]) leaves the transaction
/// untouched.
///
/// # Refused
///
/// * no from-album, an empty id, an id listed twice, `into` among `from`,
///   an [`MergeRequest::emptied`] id that is not a from-album;
/// * `into` has no row or is Missing;
/// * a from-album has no row while another one does (no fold leaves that),
///   or none has a row but something besides history still refers to one
///   (it was removed some other way);
/// * a member in another library than `into`;
/// * a Missing from-album not listed in [`MergeRequest::emptied`];
/// * a from-album with any per-file row left ([`FILE_ROW_TABLES`]): the
///   caller moves every file first ([`move_track_rows`]); a file it did not
///   move must not lose its fingerprint or tags silently;
/// * any member (the survivor included) with an AI job that is queued,
///   running, or completed but not acted on
///   ([`crate::ai_jobs::ACTIVE_STATUSES`]): its suggestion was made for one
///   disc, and the one-active-job-per-album rule could not hold for the
///   survivor;
/// * members anchored to two release groups, or the one group blocked on
///   any member.
///
/// # Per table
///
/// * `track_fingerprints`, `track_tags`, `fingerprint_rejects`: already
///   moved file by file; none left (refused otherwise).
/// * `album_blocked_mb_release_groups`: the union, onto the survivor
///   (`INSERT OR IGNORE`: a group the survivor already blocks keeps its own
///   stamp). A wrong match on one disc is a wrong match for the set.
/// * `check_findings`: not moved. Every unresolved finding of a from-album,
///   ignored or not, is resolved in place under its old id and kept as
///   history. Moving them would collide on `UNIQUE(check_id, album_id,
///   summary)` (both discs "have no cover") and would carry an ignore made
///   for one disc onto the whole set. Resolved now rather than by a later
///   run because a run never resolves an ignored finding, and an open one
///   would show as an unknown album until a library-wide run of its check.
///   A resolved row under a dead id is what the user's "Remove from
///   library" leaves too: nothing lists it by default, and if a folder
///   ever appears at the old path again its id is the same and a check
///   that reports the same summary reopens the row, ignore included, as it
///   does after a removal. The survivor's own findings stay.
/// * `ai_jobs`: finished jobs (failed, acted on) repointed to the survivor
///   so its history lists them; `album_path` is a display copy and keeps
///   the old text, as in a rename. Active ones refuse (above).
/// * `check_inputs`: the rows of every from-album and of the survivor are
///   deleted, so the background sweep checks the survivor again (its files
///   changed). Whole-library rows (`album_id = ''`) are left: their digest
///   covers the album list and changes by itself.
/// * `check_runs.scope`: left alone, a record of what a past run covered.
/// * `albums`: the from rows are deleted (their per-file rows and
///   blocklist rows are gone or copied by then; the cascades find nothing
///   to lose). On the survivor:
///   * `mb_release_group_mbid`: the one group any member is anchored to,
///     or none;
///   * `mb_release_mbid`: kept only when every member that names a release
///     names the same one, otherwise cleared ([`MergeReport::release_cleared`]);
///     the next resolve picks the best-fitting release of the group;
///   * `mb_match_score`: the lowest score among the members anchored to
///     that group (the set is no more certain than its least certain disc);
///   * `mb_last_checked_at`: cleared, so the next resolve re-confirms the
///     group against the whole set. A stale anchor is refreshed by its
///     group id and never replaced by a name search, so this cannot lose
///     the group. With no group at all, every anchor column is cleared:
///     an earlier failed lookup was for one disc, not for the set;
///   * `metadata_ai_acted_on_at`: the latest of the members';
///   * `missing_since`: NULL (the survivor must be Present);
///   * every scanner-owned column: left for the rescan the caller runs
///     once the folder is final (`albums::SCANNER_OWNED_COLUMNS`).
pub fn merge_album_rows_tx(tx: &Transaction<'_>, req: &MergeRequest<'_>) -> Result<MergeReport> {
    let into_id = req.into_id;
    if into_id.is_empty() {
        return refuse(MergeRefusal::Empty("album id to merge into"));
    }
    if req.from_ids.is_empty() {
        return refuse(MergeRefusal::NoMembers);
    }
    let mut seen = std::collections::BTreeSet::new();
    for id in req.from_ids {
        if id.is_empty() {
            return refuse(MergeRefusal::Empty("album id to merge"));
        }
        if !seen.insert(id.as_str()) {
            return refuse(MergeRefusal::ListedTwice(id.clone()));
        }
        if id == into_id {
            return refuse(MergeRefusal::IntoAmongFrom(id.clone()));
        }
    }
    if let Some(id) = req.emptied.iter().find(|e| !seen.contains(e.as_str())) {
        return refuse(MergeRefusal::EmptiedNotAMember(id.clone()));
    }

    let Some(into) = read_member(tx, into_id)? else {
        return refuse(MergeRefusal::UnknownAlbum(into_id.to_string()));
    };
    if into.missing {
        return refuse(MergeRefusal::Missing {
            album_id: into.id,
            survivor: true,
        });
    }
    let mut from = Vec::with_capacity(req.from_ids.len());
    let mut absent = Vec::new();
    for id in req.from_ids {
        match read_member(tx, id)? {
            Some(m) => from.push(m),
            None => absent.push(id),
        }
    }
    if from.is_empty() {
        // What a fold leaves under a from id: resolved findings and run
        // scopes, nothing else. Anything more is an album removed some
        // other way, and folding "nothing" into the survivor would hide it.
        for id in absent {
            let leftovers = count(
                tx,
                "SELECT (SELECT COUNT(*) FROM check_findings WHERE album_id = ?1 AND resolved_at IS NULL) \
                      + (SELECT COUNT(*) FROM ai_jobs WHERE album_id = ?1) \
                      + (SELECT COUNT(*) FROM check_inputs WHERE album_id = ?1)",
                id,
            )?;
            if leftovers > 0 {
                return refuse(MergeRefusal::UnknownAlbum(id.clone()));
            }
        }
        return Ok(MergeReport {
            already_merged: true,
            ..MergeReport::default()
        });
    }
    if let Some(id) = absent.first() {
        return refuse(MergeRefusal::UnknownAlbum((*id).clone()));
    }

    for m in &from {
        if m.library_path != into.library_path {
            return refuse(MergeRefusal::OtherLibrary {
                album_id: m.id.clone(),
                library: m.library_path.clone(),
                into_library: into.library_path.clone(),
            });
        }
        if m.missing && !req.emptied.contains(&m.id) {
            return refuse(MergeRefusal::Missing {
                album_id: m.id.clone(),
                survivor: false,
            });
        }
    }
    for m in &from {
        let mut rows = Vec::new();
        for table in FILE_ROW_TABLES {
            let n = count(
                tx,
                &format!("SELECT COUNT(*) FROM {table} WHERE album_id = ?1"),
                &m.id,
            )?;
            if n > 0 {
                rows.push((table, n));
            }
        }
        if !rows.is_empty() {
            return refuse(MergeRefusal::FileRowsLeft {
                album_id: m.id.clone(),
                rows,
            });
        }
    }
    let members: Vec<&Member> = std::iter::once(&into).chain(from.iter()).collect();
    let active = crate::ai_jobs::ACTIVE_STATUSES;
    for m in &members {
        let job: Option<(String, String)> = tx
            .query_row(
                &format!(
                    "SELECT id, status FROM ai_jobs WHERE album_id = ? AND status IN ({}) \
                     ORDER BY created_at, id LIMIT 1",
                    vec!["?"; active.len()].join(",")
                ),
                params_from_iter(std::iter::once(m.id.as_str()).chain(active)),
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((job_id, status)) = job {
            return refuse(MergeRefusal::ActiveJob {
                album_id: m.id.clone(),
                job_id,
                status,
            });
        }
    }

    let mut groups: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for m in &members {
        if let Some(rg) = &m.release_group {
            groups.entry(rg.as_str()).or_default().push(&m.id);
        }
    }
    if groups.len() > 1 {
        return refuse(MergeRefusal::ReleaseGroupsDiffer {
            anchors: members
                .iter()
                .filter_map(|m| Some((m.id.clone(), m.release_group.clone()?)))
                .collect(),
        });
    }
    let release_group = groups.keys().next().map(|s| s.to_string());
    if let Some(rg) = &release_group {
        for m in &members {
            let blocked: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM album_blocked_mb_release_groups \
                 WHERE album_id = ?1 AND release_group_mbid = ?2)",
                params![m.id, rg],
                |r| r.get(0),
            )?;
            if blocked {
                return refuse(MergeRefusal::ReleaseGroupBlocked {
                    release_group: rg.clone(),
                    blocked_on: m.id.clone(),
                });
            }
        }
    }
    // Every precondition holds: from here on, write.

    let mut releases: Vec<&str> = members
        .iter()
        .filter_map(|m| m.release.as_deref())
        .collect();
    releases.sort_unstable();
    releases.dedup();
    let (release, release_cleared) = match (&release_group, releases.as_slice()) {
        (None, _) => (None, false),
        (Some(_), []) => (None, false),
        (Some(_), [one]) => (Some(one.to_string()), false),
        (Some(_), _) => (None, true),
    };
    let score = release_group.as_ref().and_then(|rg| {
        members
            .iter()
            .filter(|m| m.release_group.as_ref() == Some(rg))
            .filter_map(|m| m.score)
            .min()
    });
    let ai_acted_on_at = members
        .iter()
        .filter_map(|m| m.ai_acted_on_at.clone())
        .max();

    let ids: Vec<&str> = from.iter().map(|m| m.id.as_str()).collect();
    let in_from = vec!["?"; ids.len()].join(",");
    let with_into = |first: &str| -> Vec<String> {
        std::iter::once(first.to_string())
            .chain(ids.iter().map(|s| s.to_string()))
            .collect()
    };

    let album_blocked_mb_release_groups = tx.execute(
        &format!(
            "INSERT OR IGNORE INTO album_blocked_mb_release_groups \
             (album_id, release_group_mbid, blocked_at) \
             SELECT ?, release_group_mbid, blocked_at FROM album_blocked_mb_release_groups \
             WHERE album_id IN ({in_from}) ORDER BY blocked_at, release_group_mbid"
        ),
        params_from_iter(with_into(into_id)),
    )?;
    let check_findings_resolved = tx.execute(
        &format!(
            "UPDATE check_findings SET resolved_at = ? \
             WHERE album_id IN ({in_from}) AND resolved_at IS NULL"
        ),
        params_from_iter(with_into(req.now)),
    )?;
    let ai_jobs = tx.execute(
        &format!("UPDATE ai_jobs SET album_id = ? WHERE album_id IN ({in_from})"),
        params_from_iter(with_into(into_id)),
    )?;
    let check_inputs = tx.execute(
        &format!("DELETE FROM check_inputs WHERE album_id IN (?,{in_from})"),
        params_from_iter(with_into(into_id)),
    )?;
    tx.execute(
        "UPDATE albums SET mb_release_group_mbid = ?1, mb_release_mbid = ?2, \
         mb_match_score = ?3, mb_last_checked_at = NULL, metadata_ai_acted_on_at = ?4 \
         WHERE id = ?5",
        params![release_group, release, score, ai_acted_on_at, into_id],
    )?;
    let albums_deleted = tx.execute(
        &format!("DELETE FROM albums WHERE id IN ({in_from})"),
        params_from_iter(ids.iter()),
    )?;
    Ok(MergeReport {
        already_merged: false,
        albums_deleted,
        album_blocked_mb_release_groups,
        check_findings_resolved,
        ai_jobs,
        check_inputs,
        release_group,
        release,
        release_cleared,
        metadata_ai_acted_on_at: ai_acted_on_at,
    })
}
