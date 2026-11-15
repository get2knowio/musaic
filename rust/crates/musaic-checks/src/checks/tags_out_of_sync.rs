//! `tags-out-of-sync` (port of `checks/tags_out_of_sync.py`): blank required
//! fields, drift from the resolver's canonical values, missing sort tags.
//! Detection only; the fix needs the apply pipeline and the AI queue.

use std::path::Path;

use musaic_core::album_fields::display_artist;
use musaic_core::pycompat::{is_py_space, strip};
use musaic_scan::flacmeta;
use rusqlite::Connection;

use crate::base::{
    Check, CheckCtx, CheckError, CheckMeta, CheckScope, Cost, Emit, Finding, FixKind, Severity,
};
use crate::pyjson::{PyJson, dict};
use crate::resolution::{Fact, Resolution, ResolutionSource};

pub struct TagsOutOfSync;

pub static META: CheckMeta = CheckMeta {
    id: "tags-out-of-sync",
    label: "Tags don't match canonical",
    description: "Albums where Vorbis tags drift from MusicBrainz's canonical values, are missing a \
required field (title / artist / year / genre), or are missing the *_SORT variants used by Plex and Roon \
for sidebar alphabetization. Fix applies all MusicBrainz-derivable corrections in one click and falls back \
to AI only for fields the resolver can't fill (genre, or anything on an unanchored album).",
    category: "metadata",
    cost: Cost::Medium,
    runs_during_scan: false,
    fix_kind: FixKind::AiJob,
    bulk_action_label: Some("Apply MusicBrainz canonical"),
};

const ARTICLES: [&str; 3] = ["the ", "a ", "an "];

/// `(primary column, sort column)` pairs, in the Python app's order.
pub const SORT_FIELD_PAIRS: [(&str, &str); 3] = [
    ("album_artist", "album_artist_sort"),
    ("artist", "artist_sort"),
    ("title", "album_sort"),
];

/// The album columns this check reads.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TagsRow {
    pub id: String,
    pub dir_name: Option<String>,
    pub path: Option<String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album_artist: Option<String>,
    pub year: Option<String>,
    pub genre: Option<String>,
    pub album_artist_sort: Option<String>,
    pub artist_sort: Option<String>,
    pub album_sort: Option<String>,
    pub is_compilation: bool,
    pub mb_release_group_mbid: Option<String>,
}

impl TagsRow {
    fn col(&self, name: &str) -> Option<&str> {
        match name {
            "title" => self.title.as_deref(),
            "artist" => self.artist.as_deref(),
            "album_artist" => self.album_artist.as_deref(),
            "album_artist_sort" => self.album_artist_sort.as_deref(),
            "artist_sort" => self.artist_sort.as_deref(),
            "album_sort" => self.album_sort.as_deref(),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumFieldDiff {
    pub field: &'static str,
    pub current: String,
    pub canonical: String,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackFieldDiff {
    pub filename: String,
    pub field: &'static str,
    pub current: String,
    pub canonical: String,
}

/// `AlbumDiffs`: everything the check found for one album.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumDiffs {
    pub missing_core: Vec<&'static str>,
    pub album_diffs: Vec<AlbumFieldDiff>,
    pub track_diffs: Vec<TrackFieldDiff>,
    /// sort column -> proposed value, in [`SORT_FIELD_PAIRS`] order.
    pub sort_proposals: Vec<(&'static str, String)>,
    pub ai_fallback_fields: Vec<&'static str>,
    /// field -> canonical value the deterministic apply would write.
    pub applicable_album: Vec<(&'static str, String)>,
    /// filename -> (field, value)s.
    pub applicable_tracks: Vec<(String, Vec<(&'static str, String)>)>,
    pub has_anchor: bool,
    pub summary: String,
}

impl AlbumDiffs {
    pub fn has_any(&self) -> bool {
        !(self.album_diffs.is_empty()
            && self.track_diffs.is_empty()
            && self.sort_proposals.is_empty()
            && self.missing_core.is_empty())
    }
}

/// Python `str.lower()` (close enough: Rust's full Unicode lowercase).
fn lower(s: &str) -> String {
    s.to_lowercase()
}

/// `_normalize`: lower-case and collapse whitespace.
pub fn normalize(s: &str) -> String {
    lower(s)
        .split(is_py_space)
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// `_starts_with_article`.
pub fn starts_with_article(value: Option<&str>) -> bool {
    let Some(v) = value.filter(|v| !v.is_empty()) else {
        return false;
    };
    let l = lower(v.trim_start_matches(is_py_space));
    ARTICLES.iter().any(|a| l.starts_with(a))
}

/// `_rotate_article`: "The Beatles" -> "Beatles, The" (original case kept).
pub fn rotate_article(value: &str) -> String {
    let stripped = value.trim_start_matches(is_py_space);
    let leading = &value[..value.len() - stripped.len()];
    let l = lower(stripped);
    for art in ARTICLES {
        if l.starts_with(art) {
            let n = art.len() - 1; // article chars (ASCII)
            let chars: Vec<char> = stripped.chars().collect();
            let original: String = chars[..n.min(chars.len())].iter().collect();
            let rest: String = chars[(n + 1).min(chars.len())..].iter().collect();
            return format!("{leading}{rest}, {original}");
        }
    }
    value.to_string()
}

/// `_build_summary`.
pub fn build_summary(
    missing_core: &[&str],
    n_album: usize,
    n_track: usize,
    n_sort: usize,
    has_anchor: bool,
) -> String {
    let mut parts = Vec::new();
    if !missing_core.is_empty() {
        parts.push(format!("missing: {}", missing_core.join(", ")));
    }
    if n_album > 0 {
        parts.push(format!("{n_album} canonical drift"));
    }
    if n_track > 0 {
        parts.push(format!("{n_track} track artist drift"));
    }
    if n_sort > 0 {
        parts.push(format!("{n_sort} sort tag"));
    }
    if parts.is_empty() {
        return "Tags need updating".into();
    }
    let detail = parts.join(" · ");
    if has_anchor {
        detail
    } else {
        format!("{detail}{NO_ANCHOR_SUFFIX}")
    }
}

/// The Python app's ending of a summary for an album with no anchor. Stored as
/// is: the summary is part of the finding's identity. [`wording`] rewords it
/// only where findings are shown ([`crate::wording::finding_summary`]).
pub const NO_ANCHOR_SUFFIX: &str = " (no MB anchor — AI required)";

/// [`NO_ANCHOR_SUFFIX`] when no AI provider is ready.
pub const NO_ANCHOR_SUFFIX_NEUTRAL: &str = " (no MusicBrainz match yet)";

fn blank(v: Option<&str>) -> bool {
    strip(v.unwrap_or("")).is_empty()
}

/// `_missing_core_fields`. The artist check uses the shared cascade without
/// the directory artist (a tag the files do not carry does not count).
pub fn missing_core_fields(a: &TagsRow) -> Vec<&'static str> {
    let mut out = Vec::new();
    if blank(a.title.as_deref()) {
        out.push("title");
    }
    if display_artist(a.album_artist.as_deref(), a.artist.as_deref(), None, false).is_empty() {
        out.push("album_artist");
    }
    if blank(a.year.as_deref()) {
        out.push("year");
    }
    if blank(a.genre.as_deref()) {
        out.push("genre");
    }
    out
}

fn album_diffs(a: &TagsRow, res: Option<&Resolution>) -> Vec<AlbumFieldDiff> {
    let Some(res) = res.filter(|r| r.anchor.is_some()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut add = |field: &'static str, current: Option<&str>, fact: Option<&Fact>| {
        let Some(fact) = fact else { return };
        let canonical = strip(&fact.value);
        let cur = strip(current.unwrap_or(""));
        if canonical.is_empty() || cur.is_empty() || normalize(canonical) == normalize(cur) {
            return;
        }
        out.push(AlbumFieldDiff {
            field,
            current: cur.to_string(),
            canonical: canonical.to_string(),
            source: fact.source.clone(),
        });
    };
    add("title", a.title.as_deref(), res.canonical_title.as_ref());
    add(
        "album_artist",
        a.album_artist.as_deref(),
        res.canonical_album_artist.as_ref(),
    );
    add("year", a.year.as_deref(), res.canonical_year.as_ref());
    out
}

/// Per-track artist drift for tracks the resolver matched, read from the
/// FLACs on disk.
fn track_diffs(a: &TagsRow, res: Option<&Resolution>) -> Vec<TrackFieldDiff> {
    let Some(res) = res.filter(|r| r.anchor.is_some()) else {
        return Vec::new();
    };
    let candidates: Vec<_> = res
        .tracks
        .iter()
        .filter(|t| t.canonical_artist.is_some())
        .collect();
    if candidates.is_empty() {
        return Vec::new();
    }
    let album_path = Path::new(a.path.as_deref().unwrap_or(""));
    if !album_path.is_dir() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for tr in candidates {
        let canonical = strip(
            tr.canonical_artist
                .as_ref()
                .map(|f| f.value.as_str())
                .unwrap_or(""),
        )
        .to_string();
        if canonical.is_empty() {
            continue;
        }
        let flac = album_path.join(&tr.filename);
        if !flac.is_file() {
            continue;
        }
        let Ok(meta) = flacmeta::read(&flac) else {
            continue;
        };
        let current = strip(meta.values("artist").next().unwrap_or("")).to_string();
        if current.is_empty() || normalize(&current) == normalize(&canonical) {
            continue;
        }
        out.push(TrackFieldDiff {
            filename: tr.filename.clone(),
            field: "artist",
            current,
            canonical,
        });
    }
    out
}

fn sort_proposals(a: &TagsRow, mb_album_artist_sort: Option<&str>) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    for (pcol, scol) in SORT_FIELD_PAIRS {
        let primary = a.col(pcol).unwrap_or("");
        if !starts_with_article(Some(primary)) || !blank(a.col(scol)) {
            continue;
        }
        match mb_album_artist_sort {
            Some(mb) if scol == "album_artist_sort" => out.push((scol, mb.to_string())),
            _ => out.push((scol, rotate_article(primary))),
        }
    }
    out
}

/// `compute_diffs(album)`. The resolver is asked only for albums with an
/// MBID; a resolver error counts as "no resolution".
pub fn compute_diffs(
    conn: &Connection,
    resolver: &dyn ResolutionSource,
    a: &TagsRow,
) -> AlbumDiffs {
    let missing_core = missing_core_fields(a);
    let resolution = match a.mb_release_group_mbid.as_deref() {
        Some(m) if !m.is_empty() => resolver.resolve(conn, &a.id).ok(),
        _ => None,
    };
    let res = resolution.as_ref();
    let album_diffs = album_diffs(a, res);
    let track_diffs = track_diffs(a, res);
    let mb_sort = res
        .and_then(|r| r.canonical_album_artist_sort.as_ref())
        .map(|f| strip(&f.value).to_string())
        .filter(|s| !s.is_empty());
    let sort_proposals = sort_proposals(a, mb_sort.as_deref());

    let mut applicable_album: Vec<(&'static str, String)> = album_diffs
        .iter()
        .map(|d| (d.field, d.canonical.clone()))
        .collect();
    if let Some(r) = res {
        for (field, fact) in [
            ("title", &r.canonical_title),
            ("album_artist", &r.canonical_album_artist),
            ("year", &r.canonical_year),
        ] {
            if missing_core.contains(&field)
                && let Some(f) = fact
                && !applicable_album.iter().any(|(k, _)| *k == field)
            {
                applicable_album.push((field, strip(&f.value).to_string()));
            }
        }
    }
    let mut applicable_tracks: Vec<(String, Vec<(&'static str, String)>)> = Vec::new();
    for td in &track_diffs {
        match applicable_tracks
            .iter_mut()
            .find(|(f, _)| *f == td.filename)
        {
            Some((_, fields)) => fields.push((td.field, td.canonical.clone())),
            None => applicable_tracks
                .push((td.filename.clone(), vec![(td.field, td.canonical.clone())])),
        }
    }
    let mut ai_fallback_fields: Vec<&'static str> = missing_core
        .iter()
        .copied()
        .filter(|f| !applicable_album.iter().any(|(k, _)| k == f) && *f != "album_artist")
        .collect();
    if missing_core.contains(&"genre") && !ai_fallback_fields.contains(&"genre") {
        ai_fallback_fields.push("genre");
    }
    let has_anchor = res.is_some_and(|r| r.anchor.is_some());
    let summary = build_summary(
        &missing_core,
        album_diffs.len(),
        track_diffs.len(),
        sort_proposals.len(),
        has_anchor,
    );
    AlbumDiffs {
        missing_core,
        album_diffs,
        track_diffs,
        sort_proposals,
        ai_fallback_fields,
        applicable_album,
        applicable_tracks,
        has_anchor,
        summary,
    }
}

fn strs(v: &[&'static str]) -> PyJson {
    PyJson::List(v.iter().map(|s| PyJson::from(*s)).collect())
}

/// The finding for one album, or `None` when there is nothing to report.
pub fn check_album(
    conn: &Connection,
    resolver: &dyn ResolutionSource,
    a: &TagsRow,
) -> Option<Finding> {
    let d = compute_diffs(conn, resolver, a);
    if !d.has_any() {
        return None;
    }
    let headline = a
        .dir_name
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| a.id.clone());
    Some(Finding {
        album_id: Some(a.id.clone()),
        severity: if d.missing_core.is_empty() {
            Severity::Info
        } else {
            Severity::Warn
        },
        summary: d.summary.clone(),
        detail: Some(dict([
            ("dir_name", a.dir_name.clone().into()),
            ("is_compilation", a.is_compilation.into()),
            ("headline", headline.into()),
            ("has_anchor", a.mb_release_group_mbid.is_some().into()),
            ("missing_core", strs(&d.missing_core)),
            (
                "album_diffs",
                PyJson::List(
                    d.album_diffs
                        .iter()
                        .map(|x| {
                            dict([
                                ("field", x.field.into()),
                                ("current", x.current.clone().into()),
                                ("canonical", x.canonical.clone().into()),
                                ("source", x.source.clone().into()),
                            ])
                        })
                        .collect(),
                ),
            ),
            (
                "track_diffs",
                PyJson::List(
                    d.track_diffs
                        .iter()
                        .map(|x| {
                            dict([
                                ("filename", x.filename.clone().into()),
                                ("field", x.field.into()),
                                ("current", x.current.clone().into()),
                                ("canonical", x.canonical.clone().into()),
                            ])
                        })
                        .collect(),
                ),
            ),
            (
                "sort_proposals",
                PyJson::Dict(
                    d.sort_proposals
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.clone().into()))
                        .collect(),
                ),
            ),
            ("ai_fallback_fields", strs(&d.ai_fallback_fields)),
        ])),
    })
}

impl Check for TagsOutOfSync {
    fn meta(&self) -> &CheckMeta {
        &META
    }

    fn run(
        &self,
        ctx: &CheckCtx<'_>,
        scope: &CheckScope,
        emit: &mut Emit<'_>,
    ) -> Result<(), CheckError> {
        let rows: Vec<TagsRow> = {
            let mut stmt = ctx.conn.prepare(
                "SELECT id, dir_name, path, title, artist, album_artist, year, genre, \
                        album_artist_sort, artist_sort, album_sort, is_compilation, \
                        mb_release_group_mbid, mb_release_mbid \
                 FROM albums WHERE library_path = ?1 AND missing_since IS NULL",
            )?;
            stmt.query_map([&scope.library_path], |r| {
                Ok(TagsRow {
                    id: r.get(0)?,
                    dir_name: r.get(1)?,
                    path: r.get(2)?,
                    title: r.get(3)?,
                    artist: r.get(4)?,
                    album_artist: r.get(5)?,
                    year: r.get(6)?,
                    genre: r.get(7)?,
                    album_artist_sort: r.get(8)?,
                    artist_sort: r.get(9)?,
                    album_sort: r.get(10)?,
                    is_compilation: r.get::<_, Option<i64>>(11)?.unwrap_or(0) != 0,
                    mb_release_group_mbid: r.get(12)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?
        };
        for row in &rows {
            if let Some(f) = check_album(ctx.conn, ctx.env.resolver.as_ref(), row) {
                emit(f)?;
            }
        }
        Ok(())
    }
}
