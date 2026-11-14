//! Per-track user tags (schema v9) and the tag vocabulary (v10): port of the
//! `track_tags` / vocabulary half of `musaic/cache.py` (lines 949-1496).
//!
//! The track-tag SQL is the Python app's, statement for statement. The
//! vocabulary half deliberately is not:
//!
//! # One case fold for vocabulary names
//!
//! The Python app folds vocabulary names three ways: Python `str.lower()` for
//! the usage counts and the orphan list, SQLite `LOWER()` (ASCII only) for
//! the track rows a rename / merge / strip touches, and the column's
//! `COLLATE NOCASE` (ASCII only) for uniqueness and for which entry a
//! patch / rename / delete addresses. So "Été", "été" and "ÉTÉ" were one
//! tag for the counts and three for everything else. Here every one of
//! those questions uses [`fold`] (Unicode lower-casing, the same as
//! the Python app's `str.lower()`, so every count and orphan list is unchanged),
//! computed in Rust over the rows: SQLite's own LOWER / NOCASE are never
//! relied on to decide whether two names are the same tag. (The column
//! keeps its `COLLATE NOCASE` constraint: two names equal under it are
//! also equal under [`fold`], so it never refuses something the fold
//! allows.)
//!
//! A database written under the old rules can hold two vocabulary entries
//! that are one tag under [`fold`] ("Été" and "été"). Nothing merges them
//! behind the user's back: both stay, each listed with the usage count of
//! the folded tag, and [`case_conflicts`] names them so the vocabulary
//! response reports them. An operation addresses an entry by its exact
//! spelling first, then by [`fold`] when exactly one entry matches; when
//! several match and none exactly it is refused ([`ambiguous`]), as are a
//! rename onto a name another entry folds to and a strip that would also
//! empty a twin. The user resolves twins with a merge of one into the
//! other (allowed between two existing entries that differ only in case).
//!
//! # Renames never lose rows
//!
//! A rename moves each track row whose tag folds to the old name and is not
//! already spelled the new way (insert the new spelling if absent, delete
//! the old row), so a case-only rename rewrites the rows in place and an
//! identical rename changes nothing. The Python app's INSERT OR IGNORE + DELETE
//! removed every row of the tag on a case-only or identical rename.
//!
//! Every mutation runs in one transaction. Timestamps come from the caller
//! (`now`, the Python app's `_now_iso()` format).
//!
//! Nothing here touches the filesystem or publishes events; the sidecar
//! rewrite after a mutation belongs to the caller (`musaic-services`).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use musaic_core::pycompat::strip;
use rusqlite::{Connection, params};

use crate::Result;

/// Python's `str.lower()` (Unicode lower-casing, as Rust's `to_lowercase`).
pub fn py_lower(s: &str) -> String {
    s.to_lowercase()
}

/// The one case fold for vocabulary names: Unicode lower-casing,
/// identical to the Python app's `str.lower()`.
pub fn fold(s: &str) -> String {
    py_lower(s)
}

// ---------- track tags ----------

/// `add_track_tag`: `true` if a row was inserted (exact-case uniqueness).
pub fn add_track_tag(
    conn: &mut Connection,
    album_id: &str,
    filename: &str,
    tag: &str,
    now: &str,
) -> Result<bool> {
    let tag = strip(tag);
    if tag.is_empty() {
        return Ok(false);
    }
    let tx = crate::immediate(conn)?;
    let n = tx.execute(
        "INSERT OR IGNORE INTO track_tags (album_id, filename, tag, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![album_id, filename, tag, now],
    )?;
    tx.commit()?;
    Ok(n > 0)
}

/// `remove_track_tag`: `true` if a row was deleted.
pub fn remove_track_tag(
    conn: &mut Connection,
    album_id: &str,
    filename: &str,
    tag: &str,
) -> Result<bool> {
    let tag = strip(tag);
    if tag.is_empty() {
        return Ok(false);
    }
    let tx = crate::immediate(conn)?;
    let n = tx.execute(
        "DELETE FROM track_tags WHERE album_id = ?1 AND filename = ?2 AND tag = ?3",
        params![album_id, filename, tag],
    )?;
    tx.commit()?;
    Ok(n > 0)
}

/// `get_album_track_tags`: `{filename: [tag, ...]}` in `ORDER BY filename,
/// tag` (BINARY) order, which is also a `BTreeMap`'s byte order.
pub fn album_track_tags(
    conn: &Connection,
    album_id: &str,
) -> Result<BTreeMap<String, Vec<String>>> {
    crate::renames::album_track_tags(conn, album_id)
}

/// `get_album_tag_intersection`: tags on at least `total_track_count`
/// distinct filenames of the album, sorted; empty when the count is <= 0.
pub fn album_tag_intersection(
    conn: &Connection,
    album_id: &str,
    total_track_count: i64,
) -> Result<Vec<String>> {
    if total_track_count <= 0 {
        return Ok(Vec::new());
    }
    let mut st = conn.prepare(
        "SELECT tag, COUNT(DISTINCT filename) AS n FROM track_tags WHERE album_id = ?1 \
         GROUP BY tag HAVING n >= ?2 ORDER BY tag",
    )?;
    let rows = st.query_map(params![album_id, total_track_count], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// `find_tracks_with_tag`: `(album_id, filename)` pairs carrying exactly `tag`
/// (stripped; empty -> none), ordered by album id then filename.
pub fn find_tracks_with_tag(conn: &Connection, tag: &str) -> Result<Vec<(String, String)>> {
    let tag = strip(tag);
    if tag.is_empty() {
        return Ok(Vec::new());
    }
    let mut st = conn.prepare(
        "SELECT album_id, filename FROM track_tags WHERE tag = ?1 ORDER BY album_id, filename",
    )?;
    let rows = st.query_map([tag], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// `list_all_tags`: `(tag, count)` by descending count, then tag (BINARY).
pub fn list_all_tags(conn: &Connection) -> Result<Vec<(String, i64)>> {
    let mut st = conn.prepare(
        "SELECT tag, COUNT(*) AS n FROM track_tags GROUP BY tag ORDER BY n DESC, tag ASC",
    )?;
    let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// `add_album_tag`: insert `tag` on each of `filenames` (no validation of
/// the names); returns the number of new rows.
pub fn add_album_tag(
    conn: &mut Connection,
    album_id: &str,
    tag: &str,
    filenames: &[String],
    now: &str,
) -> Result<usize> {
    let tag = strip(tag);
    if tag.is_empty() || filenames.is_empty() {
        return Ok(0);
    }
    let tx = crate::immediate(conn)?;
    let mut inserted = 0;
    for f in filenames {
        inserted += tx.execute(
            "INSERT OR IGNORE INTO track_tags (album_id, filename, tag, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![album_id, f, tag, now],
        )?;
    }
    tx.commit()?;
    Ok(inserted)
}

/// `remove_album_tag`: rows deleted.
pub fn remove_album_tag(conn: &mut Connection, album_id: &str, tag: &str) -> Result<usize> {
    let tag = strip(tag);
    if tag.is_empty() {
        return Ok(0);
    }
    let tx = crate::immediate(conn)?;
    let n = tx.execute(
        "DELETE FROM track_tags WHERE album_id = ?1 AND tag = ?2",
        params![album_id, tag],
    )?;
    tx.commit()?;
    Ok(n)
}

/// `replace_album_track_tags`: delete the album's rows, then insert each
/// `(filename, tags)` in the given order (empty filenames skipped, tags
/// stripped, empties skipped, exact duplicates ignored), `created_at = now`.
pub fn replace_album_track_tags(
    conn: &mut Connection,
    album_id: &str,
    track_tags: &[(String, Vec<String>)],
    now: &str,
) -> Result<()> {
    let tx = crate::immediate(conn)?;
    tx.execute("DELETE FROM track_tags WHERE album_id = ?1", [album_id])?;
    for (filename, tags) in track_tags {
        if filename.is_empty() {
            continue;
        }
        for tag in tags {
            let tag = strip(tag);
            if tag.is_empty() {
                continue;
            }
            tx.execute(
                "INSERT OR IGNORE INTO track_tags (album_id, filename, tag, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![album_id, filename, tag, now],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}

// ---------- vocabulary ----------

/// One `list_vocabulary` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VocabEntry {
    pub tag: String,
    pub description: String,
    pub color: Option<String>,
    pub sort_order: i64,
    pub created_at: String,
    /// Track rows whose tag folds ([`fold`]) to this entry's.
    pub count: i64,
}

fn tag_usage(conn: &Connection) -> Result<Vec<(String, i64)>> {
    let mut st = conn.prepare("SELECT tag, COUNT(*) AS n FROM track_tags GROUP BY tag")?;
    let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// `list_vocabulary`: entries by `sort_order`, then tag (NOCASE), with usage
/// counts summed over case variants.
pub fn list_vocabulary(conn: &Connection) -> Result<Vec<VocabEntry>> {
    let mut st = conn.prepare(
        "SELECT tag, description, color, sort_order, created_at FROM tag_vocabulary \
         ORDER BY sort_order ASC, tag ASC",
    )?;
    let vocab: Vec<VocabEntry> = st
        .query_map([], |r| {
            Ok(VocabEntry {
                tag: r.get(0)?,
                description: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                color: r.get(2)?,
                sort_order: r.get(3)?,
                created_at: r.get(4)?,
                count: 0,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut counts: HashMap<String, i64> = HashMap::new();
    for (tag, n) in tag_usage(conn)? {
        *counts.entry(fold(&tag)).or_default() += n;
    }
    Ok(vocab
        .into_iter()
        .map(|e| VocabEntry {
            count: counts.get(&fold(&e.tag)).copied().unwrap_or(0),
            ..e
        })
        .collect())
}

fn vocab_tags(conn: &Connection) -> Result<Vec<String>> {
    let mut st = conn.prepare("SELECT tag FROM tag_vocabulary ORDER BY tag COLLATE BINARY")?;
    let rows = st.query_map([], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Vocabulary entries that are one tag under [`fold`] (possible only in a
/// database written under the Python app's ASCII-only rules), as groups of
/// spellings, each sorted (BINARY), groups by their first spelling. Empty in
/// a consistent database.
pub fn case_conflicts(conn: &Connection) -> Result<Vec<Vec<String>>> {
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for t in vocab_tags(conn)? {
        groups.entry(fold(&t)).or_default().push(t);
    }
    let mut out: Vec<Vec<String>> = groups.into_values().filter(|g| g.len() > 1).collect();
    out.sort();
    Ok(out)
}

/// Which vocabulary entry a name addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    /// No entry folds to the name.
    None,
    /// The entry with this exact spelling, or the only one that folds to it.
    Entry(String),
    /// Several entries fold to the name and none is spelled exactly so.
    Ambiguous(Vec<String>),
}

fn target(conn: &Connection, name: &str) -> Result<Target> {
    let matches: Vec<String> = vocab_tags(conn)?
        .into_iter()
        .filter(|t| fold(t) == fold(name))
        .collect();
    Ok(if matches.iter().any(|t| t == name) {
        Target::Entry(name.to_string())
    } else {
        match matches.len() {
            0 => Target::None,
            1 => Target::Entry(matches.into_iter().next().unwrap_or_default()),
            _ => Target::Ambiguous(matches),
        }
    })
}

/// The refusal for a name that several entries fold to.
pub fn ambiguous(name: &str, entries: &[String]) -> VocabRefused {
    let list: Vec<String> = entries.iter().map(|e| repr(e)).collect();
    VocabRefused(format!(
        "{} vocabulary entries match {} ignoring case ({}); merge them into one first",
        entries.len(),
        repr(name),
        list.join(", ")
    ))
}

/// `add_vocabulary_entry`: `true` if a row landed (`false` when an entry
/// that folds to the same name exists). `sort_order` defaults to
/// max + 1.
pub fn add_vocabulary_entry(
    conn: &mut Connection,
    tag: &str,
    description: &str,
    color: Option<&str>,
    sort_order: Option<i64>,
    now: &str,
) -> Result<bool> {
    let tag = strip(tag);
    if tag.is_empty() {
        return Ok(false);
    }
    let tx = crate::immediate(conn)?;
    if target(&tx, tag)? != Target::None {
        return Ok(false);
    }
    let sort_order = match sort_order {
        Some(s) => s,
        None => tx.query_row(
            "SELECT COALESCE(MAX(sort_order), -1) + 1 AS n FROM tag_vocabulary",
            [],
            |r| r.get(0),
        )?,
    };
    let n = tx.execute(
        "INSERT OR IGNORE INTO tag_vocabulary (tag, description, color, sort_order, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![tag, description, color, sort_order, now],
    )?;
    tx.commit()?;
    Ok(n > 0)
}

/// `update_vocabulary_entry`: patch the given fields (an empty `color`
/// clears it) of the entry `tag` addresses ([`fold`]). `Ok(false)`
/// when nothing was given or no entry matched; refused when several entries
/// fold to `tag` and none is spelled exactly so.
pub fn update_vocabulary_entry(
    conn: &mut Connection,
    tag: &str,
    description: Option<&str>,
    color: Option<&str>,
    sort_order: Option<i64>,
) -> VocabResult<bool> {
    let mut sets: Vec<&str> = Vec::new();
    let mut args: Vec<rusqlite::types::Value> = Vec::new();
    if let Some(d) = description {
        sets.push("description = ?");
        args.push(d.to_string().into());
    }
    if let Some(c) = color {
        sets.push("color = ?");
        args.push(if c.is_empty() {
            rusqlite::types::Value::Null
        } else {
            c.to_string().into()
        });
    }
    if let Some(s) = sort_order {
        sets.push("sort_order = ?");
        args.push(s.into());
    }
    if sets.is_empty() {
        return Ok(Ok(false));
    }
    let tx = crate::immediate(conn)?;
    let entry = match target(&tx, tag)? {
        Target::None => return Ok(Ok(false)),
        Target::Ambiguous(e) => return Ok(Err(ambiguous(tag, &e))),
        Target::Entry(e) => e,
    };
    args.push(entry.into());
    let n = tx.execute(
        &format!(
            "UPDATE tag_vocabulary SET {} WHERE tag = ? COLLATE BINARY",
            sets.join(", ")
        ),
        rusqlite::params_from_iter(args),
    )?;
    tx.commit()?;
    Ok(Ok(n > 0))
}

/// Why a vocabulary operation refused (the Python app's `ValueError`; the
/// message is its `str(e)`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct VocabRefused(pub String);

/// A vocabulary mutation's result: refused, or done.
pub type VocabResult<T> = std::result::Result<std::result::Result<T, VocabRefused>, crate::Error>;

/// `rename_vocabulary_entry`'s return value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameResult {
    /// A vocabulary row was renamed (deleted, for a delete).
    pub vocab: bool,
    /// Track rows rewritten (deleted, for a strip).
    pub tag_rows: usize,
    /// Albums whose rows changed, sorted.
    pub albums: Vec<String>,
}

/// One `track_tags` row.
struct TagRow {
    album_id: String,
    filename: String,
    tag: String,
    created_at: String,
}

/// Every track row whose tag folds to `name` ([`fold`]).
fn rows_folding_to(conn: &Connection, name: &str) -> Result<Vec<TagRow>> {
    let want = fold(name);
    let mut st = conn.prepare("SELECT album_id, filename, tag, created_at FROM track_tags")?;
    let rows = st.query_map([], |r| {
        Ok(TagRow {
            album_id: r.get(0)?,
            filename: r.get(1)?,
            tag: r.get(2)?,
            created_at: r.get(3)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        let r = r?;
        if fold(&r.tag) == want {
            out.push(r);
        }
    }
    Ok(out)
}

/// Respell every row of `rows` that is not already spelled `to`: insert the
/// `to` row (kept when one exists) and delete the old one. Returns (rows
/// rewritten, rows inserted, albums touched).
fn respell(tx: &Connection, rows: &[TagRow], to: &str) -> Result<(usize, usize, BTreeSet<String>)> {
    let (mut rewritten, mut inserted) = (0, 0);
    let mut albums = BTreeSet::new();
    for r in rows.iter().filter(|r| r.tag != to) {
        inserted += tx.execute(
            "INSERT OR IGNORE INTO track_tags (album_id, filename, tag, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![r.album_id, r.filename, to, r.created_at],
        )?;
        rewritten += tx.execute(
            "DELETE FROM track_tags WHERE album_id = ?1 AND filename = ?2 AND tag = ?3",
            params![r.album_id, r.filename, r.tag],
        )?;
        albums.insert(r.album_id.clone());
    }
    Ok((rewritten, inserted, albums))
}

fn repr(s: &str) -> String {
    // `{x!r}` as Python prints a str.
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

/// `rename_vocabulary_entry`: respell every track row whose tag folds to
/// `old_tag` as `new_tag`, then rename the vocabulary entry `old_tag`
/// addresses (if any) in place. Refused when another entry already folds to
/// `new_tag` ("use merge instead", the Python app's text) or `old_tag` is
/// ambiguous.
///
/// Rows are respelled in place, so a
/// case-only rename keeps every assignment (rows already spelled `new_tag`
/// stay; two case variants on one track become one row), and a rename to
/// the identical name is a no-op: nothing is read or written and the result
/// is `{vocab: false, tag_rows: 0, albums: []}`. `tag_rows` counts the rows
/// whose spelling changed (for a rename across names, every row of the old
/// tag, as in the Python app); `albums` lists the albums of those rows.
pub fn rename_vocabulary_entry(
    conn: &mut Connection,
    old_tag: &str,
    new_tag: &str,
) -> VocabResult<RenameResult> {
    let old_tag = strip(old_tag);
    let new_tag = strip(new_tag);
    if old_tag.is_empty() || new_tag.is_empty() {
        return Ok(Err(VocabRefused("old_tag and new_tag are required".into())));
    }
    if old_tag == new_tag {
        return Ok(Ok(RenameResult {
            vocab: false,
            tag_rows: 0,
            albums: Vec::new(),
        }));
    }
    let tx = crate::immediate(conn)?;
    let source = match target(&tx, old_tag)? {
        Target::Ambiguous(e) => return Ok(Err(ambiguous(old_tag, &e))),
        Target::None => None,
        Target::Entry(e) => Some(e),
    };
    let clash = vocab_tags(&tx)?
        .into_iter()
        .any(|t| fold(&t) == fold(new_tag) && Some(&t) != source.as_ref());
    if clash {
        return Ok(Err(VocabRefused(format!(
            "vocabulary entry {} already exists; use merge instead",
            repr(new_tag)
        ))));
    }
    let rows = rows_folding_to(&tx, old_tag)?;
    let (tag_rows, _, albums) = respell(&tx, &rows, new_tag)?;
    let vocab = match &source {
        Some(src) => {
            tx.execute(
                "UPDATE tag_vocabulary SET tag = ?1 WHERE tag = ?2 COLLATE BINARY",
                params![new_tag, src],
            )? > 0
        }
        None => false,
    };
    tx.commit()?;
    Ok(Ok(RenameResult {
        vocab,
        tag_rows,
        albums: albums.into_iter().collect(),
    }))
}

/// `delete_vocabulary_entry`'s return value (same shape as a rename's).
pub type DeleteResult = RenameResult;

/// `delete_vocabulary_entry`: remove the entry `tag` addresses; with
/// `strip_from_tracks`, also every track row whose tag folds to it
/// Refused when `tag` is ambiguous, and for a strip while
/// another entry folds to the same tag (its rows would go too: merge the
/// twins first).
pub fn delete_vocabulary_entry(
    conn: &mut Connection,
    tag: &str,
    strip_from_tracks: bool,
) -> VocabResult<DeleteResult> {
    let tag = strip(tag);
    if tag.is_empty() {
        return Ok(Err(VocabRefused("tag is required".into())));
    }
    let tx = crate::immediate(conn)?;
    let entry = match target(&tx, tag)? {
        Target::Ambiguous(e) => return Ok(Err(ambiguous(tag, &e))),
        Target::None => None,
        Target::Entry(e) => Some(e),
    };
    let (albums, tag_rows) = if strip_from_tracks {
        let twins: Vec<String> = vocab_tags(&tx)?
            .into_iter()
            .filter(|t| fold(t) == fold(tag))
            .collect();
        if twins.len() > 1 {
            return Ok(Err(ambiguous(tag, &twins)));
        }
        let rows = rows_folding_to(&tx, tag)?;
        let mut albums = BTreeSet::new();
        let mut n = 0;
        for r in &rows {
            n += tx.execute(
                "DELETE FROM track_tags WHERE album_id = ?1 AND filename = ?2 AND tag = ?3",
                params![r.album_id, r.filename, r.tag],
            )?;
            albums.insert(r.album_id.clone());
        }
        (albums, n)
    } else {
        (BTreeSet::new(), 0)
    };
    let vocab = match &entry {
        Some(e) => {
            tx.execute(
                "DELETE FROM tag_vocabulary WHERE tag = ?1 COLLATE BINARY",
                [e],
            )? > 0
        }
        None => false,
    };
    tx.commit()?;
    Ok(Ok(RenameResult {
        vocab,
        tag_rows,
        albums: albums.into_iter().collect(),
    }))
}

/// `merge_vocabulary_entries`'s return value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeResult {
    /// Rows actually inserted at the destination.
    pub moved: usize,
    pub albums: Vec<String>,
}

/// `merge_vocabulary_entries`: respell every track row whose tag folds to
/// `from_tag` with the spelling of the vocabulary entry `into_tag` addresses
/// (the Python app wrote `into_tag` as typed), and delete the entry
/// `from_tag` addresses.
///
/// Names that fold alike are refused ("must differ", the Python app's
/// text) unless both are the exact spellings of two existing entries:
/// merging such twins (left by a database written under the old rules) is
/// how they are resolved. Rows of the destination's own spelling are never
/// touched, so nothing is lost either way.
pub fn merge_vocabulary_entries(
    conn: &mut Connection,
    from_tag: &str,
    into_tag: &str,
) -> VocabResult<MergeResult> {
    let from_tag = strip(from_tag);
    let into_tag = strip(into_tag);
    if from_tag.is_empty() || into_tag.is_empty() {
        return Ok(Err(VocabRefused(
            "from_tag and into_tag are required".into(),
        )));
    }
    let must_differ = || {
        Ok(Err(VocabRefused(
            "from_tag and into_tag must differ".into(),
        )))
    };
    if from_tag == into_tag {
        return must_differ();
    }
    let tx = crate::immediate(conn)?;
    let tags = vocab_tags(&tx)?;
    let twins = fold(from_tag) == fold(into_tag);
    if twins && !(tags.iter().any(|t| t == from_tag) && tags.iter().any(|t| t == into_tag)) {
        return must_differ();
    }
    let into_entry = if twins {
        Target::Entry(into_tag.to_string())
    } else {
        target(&tx, into_tag)?
    };
    // Rows take the existing entry's
    // spelling, not the name as typed (the Python app wrote `into_tag` as given,
    // so merging into "HOLIDAY" while the entry is "Holiday" left
    // case-variant rows).
    let into_spelling = match into_entry {
        Target::None => {
            return Ok(Err(VocabRefused(format!(
                "merge target {} does not exist in vocabulary",
                repr(into_tag)
            ))));
        }
        Target::Ambiguous(e) => return Ok(Err(ambiguous(into_tag, &e))),
        Target::Entry(e) => e,
    };
    let from_entry = if twins {
        Some(from_tag.to_string())
    } else {
        match target(&tx, from_tag)? {
            Target::Ambiguous(e) => return Ok(Err(ambiguous(from_tag, &e))),
            Target::None => None,
            Target::Entry(e) => Some(e),
        }
    };
    let rows = rows_folding_to(&tx, from_tag)?;
    let (_, moved, albums) = respell(&tx, &rows, &into_spelling)?;
    if let Some(e) = from_entry {
        tx.execute(
            "DELETE FROM tag_vocabulary WHERE tag = ?1 COLLATE BINARY",
            [e],
        )?;
    }
    tx.commit()?;
    Ok(Ok(MergeResult {
        moved,
        albums: albums.into_iter().collect(),
    }))
}

/// `find_orphan_tags`: track tags with no vocabulary entry ([`fold`]
/// comparison), by descending count, then folded tag.
pub fn find_orphan_tags(conn: &Connection) -> Result<Vec<(String, i64)>> {
    let vocab: HashSet<String> = vocab_tags(conn)?.iter().map(|t| fold(t)).collect();
    let mut out: Vec<(String, i64)> = tag_usage(conn)?
        .into_iter()
        .filter(|(t, _)| !vocab.contains(&fold(t)))
        .collect();
    // Python's sort is stable over the GROUP BY order; ties on both keys keep it.
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| fold(&a.0).cmp(&fold(&b.0))));
    Ok(out)
}

/// `get_albums_for_resync`: `(id, path, missing_since)` of the listed albums
/// that still have a row.
pub fn albums_for_resync(
    conn: &Connection,
    album_ids: &[String],
) -> Result<Vec<(String, String, Option<String>)>> {
    if album_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; album_ids.len()].join(", ");
    let mut st = conn.prepare(&format!(
        "SELECT id, path, missing_since FROM albums WHERE id IN ({placeholders})"
    ))?;
    let rows = st.query_map(rusqlite::params_from_iter(album_ids), |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Whether `album_id` has a `track_tags` row for `filename`.
pub fn has_rows_for_file(conn: &Connection, album_id: &str, filename: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM track_tags WHERE album_id = ?1 AND filename = ?2)",
        params![album_id, filename],
        |r| r.get(0),
    )?)
}
