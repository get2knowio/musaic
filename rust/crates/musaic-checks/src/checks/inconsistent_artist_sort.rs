//! `inconsistent-artist-sort` (port of `checks/inconsistent_artist_sort.py`):
//! clusters of album- and track-level credits with the same primary artist
//! whose sort tags disagree. Also the bulk-apply plan (`apply_cluster_sort`
//! without the tag writes) and the API's member normalisation.
//!
//! Deliberate change: an album without ALBUMARTIST joins at album level,
//! credited with its ARTIST through the shared cascade
//! ([`musaic_core::album_fields::display_artist`]), when it is a
//! single-artist album (not a compilation, every track by the same primary
//! artist). The Python app clustered albums on ALBUMARTIST only, so the
//! ALBUMARTISTSORT of an ARTIST-only album was never compared.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use musaic_core::album_fields::display_artist;
use musaic_core::pycompat::strip;
use musaic_events::{Bus, Event};
use musaic_fsops::py_repr;
use regex::Regex;
use serde_json::{Value, json};

use crate::base::{
    Check, CheckCtx, CheckError, CheckMeta, CheckScope, Cost, Emit, Finding, FixKind, Severity,
};
use crate::pyjson::{PyJson, dict, py_float_repr};

pub struct InconsistentArtistSort;

pub static META: CheckMeta = CheckMeta {
    id: "inconsistent-artist-sort",
    label: "Inconsistent artist sort",
    description: "Clusters of albums whose album_artist values are credit permutations of the same primary \
artist (\"Ray Conniff\" / \"Ray Conniff & His Orchestra\" / \"Ray Conniff, his Orchestra and Chorus\") but \
whose sort tags don't agree. Setting the same album_artist_sort across the cluster groups them under one \
entry in Plex / Roon sidebars.",
    category: "metadata",
    cost: Cost::Cheap,
    runs_during_scan: true,
    fix_kind: FixKind::Deterministic,
    bulk_action_label: Some("Propagate where possible"),
};

static CREDIT_SUFFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\s*(?:,\s*(?:his|her)\s+\S+(?:\s+(?:and|with)\s+\S+)?|\s*&\s*(?:his|her)\s+\S+(?:\s+\S+)*|\s+and\s+(?:his|her)\s+\S+(?:\s+\S+)*|\s+(?:featuring|feat\.|ft\.|with)\s+.+|\s+and\s+the\s+\S+(?:\s+\S+)?|\s*&\s*the\s+\S+(?:\s+\S+)*)\s*$",
    )
    .expect("credit suffix regex")
});
static LEADING_ARTICLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(?:the|a|an)\s+").expect("article regex"));
static WS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("ws regex"));

const NEVER_NORMALIZE: [&str; 8] = [
    "various artists",
    "various",
    "various composers",
    "various performers",
    "soundtrack",
    "original soundtrack",
    "no artist",
    "unknown artist",
];

/// `_is_never_normalize`: already-canonical credits (Various Artists, ...).
pub fn is_never_normalize(credit: Option<&str>) -> bool {
    match credit {
        Some(c) if !c.is_empty() => NEVER_NORMALIZE.contains(&strip(c).to_lowercase().as_str()),
        _ => false,
    }
}

/// `_primary_token`: strip credit suffixes (up to three passes) and a
/// leading article, collapse whitespace, lower-case.
pub fn primary_token(credit: Option<&str>) -> String {
    let Some(c) = credit.filter(|c| !c.is_empty()) else {
        return String::new();
    };
    let mut s = strip(c).to_string();
    for _ in 0..3 {
        let new = strip(&CREDIT_SUFFIX.replace_all(&s, "")).to_string();
        if new == s {
            break;
        }
        s = new;
    }
    let s = strip(&LEADING_ARTICLE.replace(&s, "")).to_string();
    WS.replace_all(&s, " ").to_lowercase()
}

/// A cluster's state (`_classify_cluster`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClusterState {
    Consistent,
    Propagatable,
    Unsorted,
    Conflicting,
}

impl ClusterState {
    pub fn as_str(self) -> &'static str {
        match self {
            ClusterState::Consistent => "consistent",
            ClusterState::Propagatable => "propagatable",
            ClusterState::Unsorted => "unsorted",
            ClusterState::Conflicting => "conflicting",
        }
    }
}

/// `_classify_cluster` over the members' sort values (`None` = absent).
/// Only "empty vs non-empty, and do the non-empty ones agree" matters.
pub fn classify_cluster<'a>(
    sorts: impl IntoIterator<Item = Option<&'a str>>,
) -> (ClusterState, Option<String>) {
    let mut distinct: BTreeSet<String> = BTreeSet::new();
    let mut has_empty = false;
    for s in sorts {
        let v = strip(s.unwrap_or(""));
        if v.is_empty() {
            has_empty = true;
        } else {
            distinct.insert(v.to_string());
        }
    }
    match distinct.len() {
        0 => (ClusterState::Unsorted, None),
        1 => {
            let v = distinct.into_iter().next();
            if has_empty {
                (ClusterState::Propagatable, v)
            } else {
                (ClusterState::Consistent, v)
            }
        }
        _ => (ClusterState::Conflicting, None),
    }
}

/// `_canonical_display`: the shortest non-empty credit, ties alphabetical.
pub fn canonical_display(credits: &[String]) -> String {
    credits
        .iter()
        .filter(|c| !c.is_empty())
        .min_by(|a, b| (a.chars().count(), *a).cmp(&(b.chars().count(), *b)))
        .cloned()
        .unwrap_or_default()
}

/// Python `str(x)` for a JSON scalar.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        Value::Number(n) => match n.as_i64() {
            Some(i) => i.to_string(),
            None => py_float_repr(n.as_f64().unwrap_or(0.0)),
        },
        Value::Null => "None".into(),
        other => other.to_string(),
    }
}

/// Python truthiness of a JSON value.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// `str(item.get(key) or "")`.
fn str_or_empty(item: &serde_json::Map<String, Value>, key: &str) -> String {
    let v = item.get(key);
    if truthy(v) {
        py_str(v.unwrap_or(&Value::Null))
    } else {
        String::new()
    }
}

/// One `track_artists_json` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackArtist {
    pub filename: String,
    pub artist: String,
    pub artist_sort: String,
}

/// `_parse_track_artists_json`: tolerant; entries without a filename dropped.
pub fn parse_track_artists_json(raw: Option<&str>) -> Vec<TrackArtist> {
    let Some(raw) = raw.filter(|r| !r.is_empty()) else {
        return Vec::new();
    };
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(Value::as_object)
        .filter_map(|o| {
            let filename = str_or_empty(o, "filename");
            (!filename.is_empty()).then(|| TrackArtist {
                filename,
                artist: str_or_empty(o, "artist"),
                artist_sort: str_or_empty(o, "artist_sort"),
            })
        })
        .collect()
}

/// A cluster member (`finding.detail.members[]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Member {
    Album {
        album_id: String,
        dir_name: Option<String>,
        album_artist: String,
        sort: String,
    },
    Track {
        album_id: String,
        dir_name: Option<String>,
        filenames: Vec<String>,
        artist: String,
        sort: String,
    },
}

impl Member {
    pub fn album_id(&self) -> &str {
        match self {
            Member::Album { album_id, .. } | Member::Track { album_id, .. } => album_id,
        }
    }
    pub fn sort(&self) -> &str {
        match self {
            Member::Album { sort, .. } | Member::Track { sort, .. } => sort,
        }
    }
    /// `_credit_of`.
    pub fn credit(&self) -> &str {
        match self {
            Member::Album { album_artist, .. } => album_artist,
            Member::Track { artist, .. } => artist,
        }
    }
    fn to_pyjson(&self) -> PyJson {
        match self {
            Member::Album {
                album_id,
                dir_name,
                album_artist,
                sort,
            } => dict([
                ("kind", "album".into()),
                ("album_id", album_id.into()),
                ("dir_name", dir_name.clone().into()),
                ("album_artist", album_artist.into()),
                ("sort", sort.into()),
            ]),
            Member::Track {
                album_id,
                dir_name,
                filenames,
                artist,
                sort,
            } => dict([
                ("kind", "track".into()),
                ("album_id", album_id.into()),
                ("dir_name", dir_name.clone().into()),
                ("filenames", filenames.clone().into()),
                ("artist", artist.into()),
                ("sort", sort.into()),
            ]),
        }
    }
}

/// Python `str.title()` (only reachable when every credit is empty).
fn py_title(s: &str) -> String {
    let mut out = String::new();
    let mut prev_cased = false;
    for c in s.chars() {
        if prev_cased {
            out.extend(c.to_lowercase());
        } else {
            out.extend(c.to_uppercase());
        }
        prev_cased = c.is_alphabetic();
    }
    out
}

fn build_finding(
    token: &str,
    members: &[Member],
    state: ClusterState,
    canonical: Option<String>,
) -> Finding {
    let representative = members
        .iter()
        .map(Member::album_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .next()
        .unwrap_or_default()
        .to_string();
    let credits: Vec<String> = members
        .iter()
        .map(|m| m.credit().to_string())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let n_album = members
        .iter()
        .filter(|m| matches!(m, Member::Album { .. }))
        .count();
    let n_track = members.len() - n_album;
    let track_count: usize = members
        .iter()
        .map(|m| match m {
            Member::Track { filenames, .. } => filenames.len(),
            Member::Album { .. } => 0,
        })
        .sum();
    let head = if n_track > 0 {
        format!("{n_album} album-level + {n_track} track-level ({track_count} tracks)")
    } else {
        format!("{} credit instances", members.len())
    };
    let mut summary = format!("{head} for {}", py_repr(&credits[0]));
    if credits.len() > 1 {
        summary.push_str(&format!(" / {}", py_repr(&credits[1])));
    }
    if credits.len() > 2 {
        summary.push_str(&format!(" + {} more variation(s)", credits.len() - 2));
    }
    let mut headline = canonical_display(&credits);
    if headline.is_empty() {
        headline = py_title(token);
    }
    if credits.len() > 1 {
        headline = format!("{headline} ({} credit variations)", credits.len());
    }
    Finding {
        album_id: Some(representative),
        summary,
        severity: Severity::Info,
        detail: Some(dict([
            ("primary_token", token.into()),
            ("state", state.as_str().into()),
            ("canonical_sort", canonical.into()),
            ("headline", headline.into()),
            (
                "members",
                PyJson::List(members.iter().map(Member::to_pyjson).collect()),
            ),
        ])),
    }
}

/// One album row as the check reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortRow {
    pub id: String,
    pub dir_name: Option<String>,
    pub album_artist: Option<String>,
    /// ARTIST of the album's sample track.
    pub artist: Option<String>,
    pub is_compilation: bool,
    pub album_artist_sort: Option<String>,
    pub track_artists_json: Option<String>,
}

/// Whether an album without ALBUMARTIST is credited at album level with its
/// ARTIST ([`display_artist`]). Only a single-artist album is: not a
/// compilation, and every track ARTIST has the same primary artist as the
/// album's. Otherwise the sample track's ARTIST stands for one track, and a
/// cluster apply would write that artist's sort as ALBUMARTISTSORT on every
/// file of a various-artists album.
fn artist_only_album_credit(r: &SortRow, tracks: &[TrackArtist]) -> bool {
    if r.is_compilation {
        return false;
    }
    let token = primary_token(r.artist.as_deref());
    !token.is_empty()
        && tracks
            .iter()
            .filter(|t| !strip(&t.artist).is_empty())
            .all(|t| primary_token(Some(&t.artist)) == token)
}

/// Cluster the rows (in row order) and return one finding per cluster of two
/// or more members that is not already consistent.
pub fn findings_for(rows: &[SortRow]) -> Vec<Finding> {
    let mut clusters: Vec<(String, Vec<Member>)> = Vec::new();
    let mut add = |token: String, m: Member| match clusters.iter_mut().find(|(t, _)| *t == token) {
        Some((_, ms)) => ms.push(m),
        None => clusters.push((token, vec![m])),
    };
    for r in rows {
        let tracks = parse_track_artists_json(r.track_artists_json.as_deref());
        // The album's credit through the shared cascade (ALBUMARTIST, then
        // ARTIST), so an ARTIST-only album joins at album level too. The
        // stored member keeps ALBUMARTIST as tagged (unstripped), as the
        // Python app's detail did.
        let has_album_artist = r
            .album_artist
            .as_deref()
            .is_some_and(|a| !strip(a).is_empty());
        let album_artist = if has_album_artist {
            r.album_artist.clone().unwrap_or_default()
        } else if artist_only_album_credit(r, &tracks) {
            display_artist(None, r.artist.as_deref(), None, false)
        } else {
            String::new()
        };
        let aa_sort = strip(r.album_artist_sort.as_deref().unwrap_or("")).to_string();
        if !strip(&album_artist).is_empty() && !is_never_normalize(Some(&album_artist)) {
            let token = primary_token(Some(&album_artist));
            if !token.is_empty() {
                add(
                    token,
                    Member::Album {
                        album_id: r.id.clone(),
                        dir_name: r.dir_name.clone(),
                        album_artist: album_artist.clone(),
                        sort: aa_sort.clone(),
                    },
                );
            }
        }
        let mut grouped: Vec<((String, String), Vec<String>)> = Vec::new();
        for t in tracks {
            let artist = strip(&t.artist).to_string();
            if artist.is_empty() || is_never_normalize(Some(&artist)) {
                continue;
            }
            let key = (artist, strip(&t.artist_sort).to_string());
            match grouped.iter_mut().find(|(k, _)| *k == key) {
                Some((_, fs)) => fs.push(t.filename),
                None => grouped.push((key, vec![t.filename])),
            }
        }
        for ((artist, sort), mut filenames) in grouped {
            if artist == strip(&album_artist) && sort == aa_sort {
                continue;
            }
            let token = primary_token(Some(&artist));
            if token.is_empty() {
                continue;
            }
            filenames.sort();
            add(
                token,
                Member::Track {
                    album_id: r.id.clone(),
                    dir_name: r.dir_name.clone(),
                    filenames,
                    artist,
                    sort,
                },
            );
        }
    }
    clusters
        .into_iter()
        .filter(|(_, ms)| ms.len() >= 2)
        .filter_map(|(token, ms)| {
            let (state, canonical) = classify_cluster(ms.iter().map(|m| Some(m.sort())));
            (state != ClusterState::Consistent)
                .then(|| build_finding(&token, &ms, state, canonical))
        })
        .collect()
}

impl Check for InconsistentArtistSort {
    fn meta(&self) -> &CheckMeta {
        &META
    }

    /// Clusters span albums and the finding is stored on one member, so a
    /// run scoped to another member must still resolve it; every run reads
    /// all of the library's Present albums, so it can.
    fn sees_whole_library(&self) -> bool {
        true
    }

    fn run(
        &self,
        ctx: &CheckCtx<'_>,
        scope: &CheckScope,
        emit: &mut Emit<'_>,
    ) -> Result<(), CheckError> {
        let rows: Vec<SortRow> = {
            let mut stmt = ctx.conn.prepare(
                "SELECT id, dir_name, album_artist, album_artist_sort, track_artists_json, \
                 artist, is_compilation \
                 FROM albums WHERE library_path = ?1 AND missing_since IS NULL",
            )?;
            stmt.query_map([&scope.library_path], |r| {
                Ok(SortRow {
                    id: r.get(0)?,
                    dir_name: r.get(1)?,
                    album_artist: r.get(2)?,
                    album_artist_sort: r.get(3)?,
                    track_artists_json: r.get(4)?,
                    artist: r.get(5)?,
                    is_compilation: r.get::<_, Option<i64>>(6)?.unwrap_or(0) != 0,
                })
            })?
            .collect::<rusqlite::Result<_>>()?
        };
        for f in findings_for(&rows) {
            emit(f)?;
        }
        Ok(())
    }
}

// ---------- the bulk apply (plan + driver; the tag writes are injected) ----------

/// `api/health.py::_normalize_cluster_member`: coerce a stored member (v11+
/// shape, or the legacy v10 album shape) for the apply endpoint. `None` for
/// entries too malformed to act on.
pub fn normalize_cluster_member(m: &Value) -> Option<Value> {
    let o = m.as_object()?;
    if o.contains_key("kind") {
        if o.get("kind").and_then(Value::as_str) == Some("track") {
            let fns: Vec<Value> = match o.get("filenames") {
                Some(Value::Array(a)) => a.clone(),
                other if truthy(other) => vec![other.cloned().unwrap_or(Value::Null)],
                _ => Vec::new(),
            };
            let fns: Vec<Value> = fns
                .iter()
                .filter(|x| truthy(Some(x)))
                .map(|x| Value::String(py_str(x)))
                .collect();
            let mut out = o.clone();
            out.insert("filenames".into(), Value::Array(fns));
            return Some(Value::Object(out));
        }
        return Some(m.clone());
    }
    let album_id = [o.get("id"), o.get("album_id")]
        .into_iter()
        .find(|v| truthy(*v))
        .flatten()?
        .clone();
    let sort = match o.get("album_artist_sort") {
        v if truthy(v) => v.cloned().unwrap_or(Value::Null),
        _ => json!(""),
    };
    Some(json!({
        "kind": "album",
        "album_id": album_id,
        "dir_name": o.get("dir_name").cloned().unwrap_or(Value::Null),
        "album_artist": o.get("album_artist").cloned().unwrap_or(Value::Null),
        "sort": sort,
    }))
}

/// `field -> value` for the album (`apply_direct(suggestions=...)`).
pub type AlbumSuggestions = Vec<(&'static str, String)>;
/// `(filename, field, value)` (`apply_direct(track_suggestions=...)`).
pub type TrackSuggestions = Vec<(String, &'static str, String)>;

/// One album's share of a cluster apply: ALBUMARTISTSORT on every FLAC when
/// `album_level`, ARTISTSORT on `track_filenames`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortWrite {
    pub album_id: String,
    pub album_level: bool,
    /// Sorted (the Python app iterates a set, so its order is arbitrary).
    pub track_filenames: Vec<String>,
    pub sort_value: String,
}

impl SortWrite {
    /// The `apply_direct` arguments: album suggestions and per-track ones.
    pub fn suggestions(&self) -> (AlbumSuggestions, TrackSuggestions) {
        let album = if self.album_level {
            vec![("album_artist_sort", self.sort_value.clone())]
        } else {
            Vec::new()
        };
        let tracks = self
            .track_filenames
            .iter()
            .map(|f| (f.clone(), "artist_sort", self.sort_value.clone()))
            .collect();
        (album, tracks)
    }
}

/// `apply_cluster_sort`'s plan: one write per album (first-appearance
/// order), album-level and track-level members coalesced. Empty when there
/// are no members or `sort_value` is empty; members without an album id and
/// albums with nothing to write are dropped.
pub fn plan_cluster_sort(members: &[Value], sort_value: &str) -> Vec<SortWrite> {
    if members.is_empty() || sort_value.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<(String, bool, BTreeSet<String>)> = Vec::new();
    for m in members {
        let Some(aid) = m.get("album_id").filter(|v| truthy(Some(v))).map(py_str) else {
            continue;
        };
        let i = match out.iter().position(|(a, _, _)| *a == aid) {
            Some(i) => i,
            None => {
                out.push((aid, false, BTreeSet::new()));
                out.len() - 1
            }
        };
        if m.get("kind").and_then(Value::as_str) == Some("track") {
            if let Some(Value::Array(fs)) = m.get("filenames") {
                out[i].2.extend(fs.iter().map(py_str));
            }
        } else {
            out[i].1 = true;
        }
    }
    out.into_iter()
        .filter(|(_, album_level, fs)| *album_level || !fs.is_empty())
        .map(|(album_id, album_level, fs)| SortWrite {
            album_id,
            album_level,
            track_filenames: fs.into_iter().collect(),
            sort_value: sort_value.to_string(),
        })
        .collect()
}

/// `try_quick_fix`'s gate: only a `propagatable` cluster with a canonical
/// sort and members is applied. `Err(message)` = skipped.
pub fn propagate_plan(detail: Option<&Value>) -> Result<(String, Vec<SortWrite>), String> {
    let state = detail.and_then(|d| d.get("state"));
    let canonical = detail.and_then(|d| d.get("canonical_sort"));
    let members = detail.and_then(|d| d.get("members"));
    if state.and_then(Value::as_str) != Some("propagatable")
        || !truthy(canonical)
        || !truthy(members)
    {
        return Err(format!(
            "Skipped (state={}); needs user input via Resolve dialog",
            py_str(state.unwrap_or(&Value::Null))
        ));
    }
    let canonical = py_str(canonical.unwrap_or(&Value::Null));
    let members = members
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let plan = plan_cluster_sort(&members, &canonical);
    Ok((canonical, plan))
}

/// The quick-fix success message.
pub fn propagate_message(canonical: &str, applied: usize) -> String {
    format!("Propagated sort '{canonical}' to {applied} member(s) in cluster")
}

/// Run a cluster apply: every write under `in_bulk_run` (so the per-album
/// rerun subscribers stand down), then one `BulkBatchCompleted` if any write
/// succeeded. Returns the number of albums written.
///
/// Deliberate change: the Python app also ran its own
/// library-wide sweep before publishing; with bus reruns really running, the
/// event's sweep is the only one.
///
/// "also on error": when `apply` panics, the flag is reset, the
/// writes that already succeeded still get their `BulkBatchCompleted`, and
/// the panic then continues to the caller.
pub fn apply_cluster_sort(
    bus: &Bus,
    writes: &[SortWrite],
    mut apply: impl FnMut(&SortWrite) -> bool,
) -> usize {
    let mut succeeded = 0;
    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        musaic_events::context::with_bulk_run(|| {
            for w in writes {
                if apply(w) {
                    succeeded += 1;
                }
            }
        })
    }));
    if succeeded > 0 {
        bus.publish(&Event::BulkBatchCompleted);
    }
    if let Err(p) = run {
        std::panic::resume_unwind(p);
    }
    succeeded
}
