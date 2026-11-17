//! `mb_lookup.find_release` and its helpers.
//!
//! Parity with the Python app: query terms, blocklist filtering, hint reranking,
//! year fallback, genre pick, notes and `source` strings, CAA check and the
//! tracklist fetch. What is NOT here: persisting the anchor. The Python app
//! persists a confident hit inside `find_release`; this build leaves that to
//! the cascade, which first applies the Tier 0b acceptance check
//! ([`crate::name_search`]).
//!
//! Not in the Python app (deliberate changes): [`parse_listing`] and
//! [`best_fit_release`] (Tier 1 aligns against the best-fitting release, not
//! the earliest), [`parse_release_group`] and `MbMatch::artist_sort` (the
//! canonical album fields are filled whichever tier anchored).
//!
//! Golden: `testdata/golden/resolver/find_release.json` and `tracklists.json`
//! (recorded from the Python app).

use std::collections::BTreeSet;

use serde_json::Value;

use crate::pyfmt::repr_str;
use crate::source::{AlbumRecord, MusicBrainzSource, NetError};

/// `mb_lookup.HIGH_CONFIDENCE_SCORE`.
pub const HIGH_CONFIDENCE_SCORE: i64 = 88;
/// `mb_lookup.MEDIUM_CONFIDENCE_SCORE`.
pub const MEDIUM_CONFIDENCE_SCORE: i64 = 70;
/// `mb_lookup.CACHE_REFRESH_DAYS`.
pub const CACHE_REFRESH_DAYS: i64 = 90;

/// `CAA_RELEASE_GROUP_FRONT`.
pub fn cover_front_url(release_group: &str) -> String {
    format!("https://coverartarchive.org/release-group/{release_group}/front")
}

/// `mb_lookup.MBTrack`.
#[derive(Debug, Clone, PartialEq)]
pub struct MbTrack {
    /// 1-based, running on across discs.
    pub position: i64,
    pub title: String,
    pub length_ms: Option<i64>,
    pub artist: Option<String>,
    pub recording_mbid: Option<String>,
}

impl MbTrack {
    pub fn length_s(&self) -> Option<f64> {
        self.length_ms.map(|ms| ms as f64 / 1000.0)
    }
}

/// `mb_lookup.MusicBrainzMatch`.
#[derive(Debug, Clone, PartialEq)]
pub struct MbMatch {
    pub confident: bool,
    pub score: i64,
    pub confidence: &'static str,
    pub mbid: Option<String>,
    pub release_mbid: Option<String>,
    pub artist: Option<String>,
    /// Not in the Python app: the MusicBrainz sort-name of the first
    /// credited artist, for `canonical_album_artist_sort`.
    pub artist_sort: Option<String>,
    pub title: Option<String>,
    pub year: Option<String>,
    pub primary_genre: Option<String>,
    pub cover_url: Option<String>,
    pub cover_url_verified: bool,
    pub tracklist: Vec<MbTrack>,
    pub notes: String,
    pub source: String,
    /// Not in the Python app: the failure behind a "(network error)",
    /// "(HTTP error)" or "(parse error)" source, for the trace.
    pub failure: Option<NetError>,
    /// Not in the Python app: the release browse used for the tracklist, kept so
    /// the Tier 0b acceptance check can reuse it instead of asking again.
    pub browse: Option<Result<Vec<u8>, NetError>>,
}

impl Default for MbMatch {
    fn default() -> Self {
        Self {
            confident: false,
            score: 0,
            confidence: "low",
            mbid: None,
            release_mbid: None,
            artist: None,
            artist_sort: None,
            title: None,
            year: None,
            primary_genre: None,
            cover_url: None,
            cover_url_verified: false,
            tracklist: Vec::new(),
            notes: String::new(),
            source: "MusicBrainz".to_owned(),
            failure: None,
            browse: None,
        }
    }
}

/// `_confidence_from_score`.
pub fn confidence_from_score(score: i64) -> &'static str {
    if score >= HIGH_CONFIDENCE_SCORE {
        "high"
    } else if score >= MEDIUM_CONFIDENCE_SCORE {
        "medium"
    } else {
        "low"
    }
}

/// Artist and title of the search (the one documented exception to the shared
/// album-field accessors: the folder name comes first): `dir_artist -> album_artist -> artist` and `dir_album -> title`,
/// stripped; "Various Artists" for a compilation. Either may come back empty.
pub fn search_terms(album: &AlbumRecord) -> (String, String) {
    let first = |opts: [&Option<String>; 3]| {
        opts.into_iter()
            .flatten()
            .find(|s| !s.is_empty())
            .map(|s| musaic_core::pycompat::strip(s).to_owned())
            .unwrap_or_default()
    };
    let mut artist = first([&album.dir_artist, &album.album_artist, &album.artist]);
    let title = first([&album.dir_album, &album.title, &None]);
    if album.is_compilation {
        artist = "Various Artists".to_owned();
    }
    (artist, title)
}

/// Python truthiness of a JSON value.
fn truthy(v: Option<&Value>) -> Option<&Value> {
    v.filter(|v| match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    })
}

/// `int(x.get(k, 0) or 0)` for the score / tag-count fields. A value Python's
/// `int()` would reject reads as 0 here (the Python app would raise).
fn py_int(v: Option<&Value>) -> i64 {
    match truthy(v) {
        None => 0,
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f.trunc() as i64))
            .unwrap_or(0),
        Some(Value::String(s)) => musaic_core::pycompat::int(s).unwrap_or(0),
        Some(Value::Bool(true)) => 1,
        Some(_) => 0,
    }
}

fn str_field<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

/// `" ".join(ac.get("name", "") for ac in credits)` / `", ".join(...)`.
fn credit_names(group: &Value) -> Vec<String> {
    truthy(group.get("artist-credit"))
        .and_then(Value::as_array)
        .map(|credits| {
            credits
                .iter()
                .map(|ac| str_field(ac, "name").unwrap_or("").to_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// The hint reranking of `find_release` (Tier 2 bias): among the groups whose
/// `title + " " + credit names`, lower-cased, contains the stripped and
/// lower-cased hint, the highest score wins (first on ties); with no hint or
/// no such group, the first group wins. `groups` is non-empty.
pub fn pick_with_hint(groups: &[Value], hint: &str) -> usize {
    let hint_q = py_lower(musaic_core::pycompat::strip(hint));
    if hint_q.is_empty() {
        return 0;
    }
    let mut best: Option<(usize, i64)> = None;
    for (i, g) in groups.iter().enumerate() {
        let text = py_lower(&format!(
            "{} {}",
            str_field(g, "title").unwrap_or(""),
            credit_names(g).join(" ")
        ));
        if !text.contains(&hint_q) {
            continue;
        }
        let score = py_int(g.get("score"));
        if best.is_none_or(|(_, s)| score > s) {
            best = Some((i, score));
        }
    }
    best.map_or(0, |(i, _)| i)
}

/// `str.lower()`. Rust's `to_lowercase` differs from Python only for a few
/// characters (final sigma in context, `İ`); neither matters for a substring test.
fn py_lower(s: &str) -> String {
    s.to_lowercase()
}

/// `_pick_primary_genre`.
fn pick_primary_genre(tags: Option<&Value>) -> Option<String> {
    let tags = truthy(tags)?.as_array()?;
    let mut best: Option<(&Value, i64)> = None;
    for t in tags {
        let c = py_int(t.get("count"));
        if best.is_none_or(|(_, bc)| c > bc) {
            best = Some((t, c));
        }
    }
    let (t, count) = best?;
    if count < 2 {
        return None;
    }
    let name = musaic_core::pycompat::strip(str_field(t, "name").unwrap_or(""));
    (!name.is_empty()).then(|| name.to_owned())
}

/// `first_release_date[:4]` when it is four digits.
fn year_of(date: &str) -> Option<String> {
    let head: String = date.chars().take(4).collect();
    (date.chars().count() >= 4 && head.chars().all(|c| c.is_ascii_digit())).then_some(head)
}

/// `_earliest_release_year` over a release browse body: the smallest
/// four-digit year among the releases' `date`s. `None` on any failure.
pub fn earliest_release_year(body: &[u8]) -> Option<String> {
    let doc: Value = serde_json::from_slice(body).ok()?;
    let releases = truthy(doc.get("releases"))?.as_array()?;
    let mut earliest: Option<String> = None;
    for r in releases {
        let date = truthy(r.get("date")).and_then(Value::as_str).unwrap_or("");
        if let Some(y) = year_of(date)
            && earliest.as_ref().is_none_or(|e| &y < e)
        {
            earliest = Some(y);
        }
    }
    earliest
}

/// `_artist_credit_string`.
fn artist_credit_string(credit: Option<&Value>) -> Option<String> {
    let credit = truthy(credit)?.as_array()?;
    let mut out = String::new();
    for entry in credit {
        let name = truthy(entry.get("name"))
            .and_then(Value::as_str)
            .or_else(|| entry.get("artist").and_then(|a| str_field(a, "name")))
            .unwrap_or("");
        let name = musaic_core::pycompat::strip(name);
        if !name.is_empty() {
            out.push_str(name);
            if let Some(j) = truthy(entry.get("joinphrase")).and_then(Value::as_str) {
                out.push_str(j);
            }
        }
    }
    let out = musaic_core::pycompat::strip(&out);
    (!out.is_empty()).then(|| out.to_owned())
}

/// `int(length_ms) if length_ms else None` after `t.length or rec.length`.
fn length_ms(t: &Value, rec: &Value) -> Option<i64> {
    let v = truthy(t.get("length")).or_else(|| truthy(rec.get("length")))?;
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f.trunc() as i64)),
        Value::String(s) => musaic_core::pycompat::int(s),
        _ => None,
    }
}

/// `_fetch_tracklist` over a release browse body: releases sorted by `date`
/// (undated last, stable), the first one whose every medium has tracks and
/// every track a title, linearised across discs. `("", [])` when none is
/// usable. `Err` when the body is not JSON (the Python app raises).
///
/// This is the Python app's choice (earliest release). Tier 1 of this build picks
/// with [`best_fit_release`] instead; this function stays for the
/// parity table `tracklists.json`.
pub fn parse_tracklist(body: &[u8]) -> Result<(String, Vec<MbTrack>), NetError> {
    let doc: Value = serde_json::from_slice(body).map_err(|e| NetError::Parse(e.to_string()))?;
    let mut releases: Vec<&Value> = browse_releases_of(&doc);
    releases.sort_by_key(|r| date_key(r));
    for release in releases {
        if let Some(tracks) = release_tracks(release) {
            return Ok((release_id(release), tracks));
        }
    }
    Ok((String::new(), Vec::new()))
}

fn browse_releases_of(doc: &Value) -> Vec<&Value> {
    truthy(doc.get("releases"))
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default()
}

/// `_first_release_date`: `date` or `"9999-99-99"`.
fn date_key(r: &Value) -> String {
    truthy(r.get("date"))
        .and_then(Value::as_str)
        .unwrap_or("9999-99-99")
        .to_owned()
}

fn release_id(r: &Value) -> String {
    truthy(r.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

/// One release's linear tracklist under the Python app's usability rule: every
/// medium has tracks and every track a title. `None` when unusable or empty.
fn release_tracks(release: &Value) -> Option<Vec<MbTrack>> {
    let media = truthy(release.get("media")).and_then(Value::as_array)?;
    let mut tracks = Vec::new();
    let mut position = 0;
    for medium in media {
        let mtracks = truthy(medium.get("tracks")).and_then(Value::as_array)?;
        for t in mtracks {
            position += 1;
            let empty = Value::Object(Default::default());
            let rec = truthy(t.get("recording")).unwrap_or(&empty);
            let title = truthy(t.get("title"))
                .or_else(|| truthy(rec.get("title")))
                .and_then(Value::as_str)
                .unwrap_or("");
            let title = musaic_core::pycompat::strip(title);
            if title.is_empty() {
                return None;
            }
            tracks.push(MbTrack {
                position,
                title: title.to_owned(),
                length_ms: length_ms(t, rec),
                artist: artist_credit_string(rec.get("artist-credit")),
                recording_mbid: str_field(rec, "id").map(str::to_owned),
            });
        }
    }
    (!tracks.is_empty()).then_some(tracks)
}

/// The sort-name of the first credited artist (`artist-credit[0].artist.sort-name`),
/// stripped; `None` when absent or empty. The library's convention reduces
/// every credit variant to the primary artist's sort-name.
fn first_credit_sort_name(credit: Option<&Value>) -> Option<String> {
    let first = truthy(credit)?.as_array()?.first()?;
    let s = musaic_core::pycompat::strip(
        first
            .get("artist")
            .and_then(|a| str_field(a, "sort-name"))?,
    );
    (!s.is_empty()).then(|| s.to_owned())
}

/// One usable release of a release browse, with what Tier 1 needs from it.
#[derive(Debug, Clone, PartialEq)]
pub struct ListedRelease {
    pub id: String,
    /// The Python app's sort key: `date`, or `"9999-99-99"` when undated.
    pub date_key: String,
    pub title: Option<String>,
    /// `_artist_credit_string` of the release's artist credit.
    pub artist: Option<String>,
    pub artist_sort: Option<String>,
    pub tracks: Vec<MbTrack>,
}

/// What a release browse says about the group as a whole.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Listing {
    /// Number of releases listed, usable or not.
    pub listed: usize,
    /// The usable ones, in response order.
    pub usable: Vec<ListedRelease>,
    /// [`earliest_release_year`] of the same body.
    pub earliest_year: Option<String>,
}

/// Parse a release browse body (`inc=media+recordings+artist-credits`).
/// `Err` when the body is not JSON.
pub fn parse_listing(body: &[u8]) -> Result<Listing, NetError> {
    let doc: Value = serde_json::from_slice(body).map_err(|e| NetError::Parse(e.to_string()))?;
    let releases = browse_releases_of(&doc);
    let usable = releases
        .iter()
        .filter_map(|r| {
            let tracks = release_tracks(r)?;
            let title = truthy(r.get("title"))
                .and_then(Value::as_str)
                .map(musaic_core::pycompat::strip)
                .filter(|t| !t.is_empty())
                .map(str::to_owned);
            Some(ListedRelease {
                id: release_id(r),
                date_key: date_key(r),
                title,
                artist: artist_credit_string(r.get("artist-credit")),
                artist_sort: first_credit_sort_name(r.get("artist-credit")),
                tracks,
            })
        })
        .collect();
    Ok(Listing {
        listed: releases.len(),
        usable,
        earliest_year: earliest_release_year(body),
    })
}

/// Tracks per medium, in listed order, of the first usable release of a
/// release browse whose id is `release_id` (the medium a "(disc N of M)"
/// folder is compared with by positional alignment). `None` when the body is not
/// JSON or no usable release has that id.
pub fn release_medium_sizes(body: &[u8], release_id: &str) -> Option<Vec<usize>> {
    let doc: Value = serde_json::from_slice(body).ok()?;
    browse_releases_of(&doc)
        .into_iter()
        .filter(|r| self::release_id(r) == release_id)
        .find(|r| release_tracks(r).is_some())
        .and_then(|r| truthy(r.get("media")).and_then(Value::as_array))
        .map(|media| {
            media
                .iter()
                .map(|m| {
                    truthy(m.get("tracks"))
                        .and_then(Value::as_array)
                        .map_or(0, Vec::len)
                })
                .collect()
        })
}

/// How well one release fits the album; smaller is better, compared field
/// by field in declaration order (see [`best_fit_release`]).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Fit {
    pub count_gap: u64,
    pub unpaired: usize,
    pub diff_ms: i64,
    pub date_key: String,
    pub id: String,
}

/// [`Fit`] of one release against the album's FLACs.
pub fn fit(release: &ListedRelease, flac: &[crate::source::FlacTrack]) -> Fit {
    let by_position: std::collections::HashMap<i64, &MbTrack> =
        release.tracks.iter().map(|t| (t.position, t)).collect();
    let mut unpaired = 0;
    let mut diff_ms = 0i64;
    for ft in flac {
        let pair = ft
            .tracknumber
            .as_position()
            .and_then(|p| by_position.get(&p))
            .and_then(|t| t.length_ms.filter(|l| *l != 0))
            .zip(ft.duration.filter(|d| *d != 0.0));
        match pair {
            Some((len, dur)) => diff_ms += ((dur * 1000.0).round() as i64 - len).abs(),
            None => unpaired += 1,
        }
    }
    Fit {
        count_gap: (release.tracks.len() as u64).abs_diff(flac.len() as u64),
        unpaired,
        diff_ms,
        date_key: release.date_key.clone(),
        id: release.id.clone(),
    }
}

/// Deliberate change from the Python app's "earliest release": the
/// release of the anchored group that Tier 1 aligns against.
///
/// Among the usable releases (every medium has tracks, every track a title —
/// the Python app's rule), the smallest [`Fit`], comparing in this order:
///
/// 1. `count_gap`: |release track count (all media, linearised) − number of
///    FLACs|; an exact count wins.
/// 2. `unpaired`: FLACs without a comparable partner at their position —
///    TRACKNUMBER does not parse (same rule as alignment), no release track at
///    that linear position, or either duration unknown or zero.
/// 3. `diff_ms`: Σ over the paired FLACs of
///    |round(FLAC duration × 1000) − release track length in ms|.
/// 4. `date_key`: the Python app's order (`date` as a string, undated last), so
///    when nothing above separates the releases the earliest still wins.
/// 5. `id`: the release MBID, as a string, so the pick never depends on the
///    order MusicBrainz lists releases in.
///
/// Uses only the release browse Tier 1 already fetches. The 2 s per-track
/// alignment tolerance is unchanged. `None` when no release is usable.
pub fn best_fit_release<'a>(
    listing: &'a Listing,
    flac: &[crate::source::FlacTrack],
) -> Option<&'a ListedRelease> {
    listing.usable.iter().min_by_key(|r| fit(r, flac))
}

/// The album-level fields of a release-group lookup
/// (`GET /ws/2/release-group/{mbid}?fmt=json&inc=artist-credits+tags`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GroupFields {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub artist_sort: Option<String>,
    pub year: Option<String>,
    pub primary_genre: Option<String>,
}

/// Parse a release-group lookup body. `Err` when it is not a JSON object.
pub fn parse_release_group(body: &[u8]) -> Result<GroupFields, NetError> {
    let doc: Value = serde_json::from_slice(body).map_err(|e| NetError::Parse(e.to_string()))?;
    if !doc.is_object() {
        return Err(NetError::Parse(
            "release-group lookup is not an object".into(),
        ));
    }
    Ok(GroupFields {
        title: truthy(doc.get("title"))
            .and_then(Value::as_str)
            .map(musaic_core::pycompat::strip)
            .filter(|t| !t.is_empty())
            .map(str::to_owned),
        artist: artist_credit_string(doc.get("artist-credit")),
        artist_sort: first_credit_sort_name(doc.get("artist-credit")),
        year: truthy(doc.get("first-release-date"))
            .and_then(Value::as_str)
            .and_then(year_of),
        primary_genre: pick_primary_genre(doc.get("tags")),
    })
}

/// Inputs of one `find_release` call.
#[derive(Debug, Clone, Copy)]
pub struct FindOptions<'a> {
    pub hint: &'a str,
    pub blocked: &'a BTreeSet<String>,
    pub fetch_cover: bool,
    pub fetch_tracklist: bool,
}

/// `find_release(album, hint=…, fetch_cover=…, fetch_tracklist=…,
/// exclude_release_group_mbids=…)` without the persistence step.
pub fn find_release(
    album: &AlbumRecord,
    opts: FindOptions<'_>,
    mb: &dyn MusicBrainzSource,
) -> MbMatch {
    let (artist, title) = search_terms(album);
    if title.is_empty() || artist.is_empty() {
        return MbMatch {
            notes: format!(
                "Need both artist and title; got artist={} title={}",
                repr_str(&artist),
                repr_str(&title)
            ),
            source: "MusicBrainz (insufficient input)".to_owned(),
            ..MbMatch::default()
        };
    }
    let body = match mb.search_release_groups(&artist, &title) {
        Ok(b) => b,
        Err(e) => {
            // Deliberate change: every failure of the search is logged. The
            // Python app logged only a `requests` exception
            // (mb_lookup.py:170), so a 503 (rate limiting) or an unreadable
            // body never reached the server log.
            if !matches!(e, NetError::Network(_) | NetError::Cancelled) {
                musaic_log::warning(
                    "musaic.mb_lookup",
                    &format!("MusicBrainz search failed: {e}"),
                );
            }
            let (notes, source) = match &e {
                NetError::Http(s) => (
                    format!("HTTP {s} from MusicBrainz"),
                    "MusicBrainz (HTTP error)",
                ),
                NetError::Parse(_) => (
                    "Search response was not valid JSON".to_owned(),
                    "MusicBrainz (parse error)",
                ),
                // The app was closing: say so, and log at most a debug line.
                NetError::Cancelled => {
                    musaic_log::debug(
                        "musaic.mb_lookup",
                        "MusicBrainz search cancelled before it was sent",
                    );
                    (
                        "Cancelled: Musaic was closing".to_owned(),
                        "MusicBrainz (cancelled)",
                    )
                }
                // The Python app writes `{type(e).__name__}: {e}` of the requests
                // exception; this build has no such type name.
                NetError::Network(d) => {
                    // mb_lookup.py:170 (its text has no "network error: ").
                    musaic_log::warning(
                        "musaic.mb_lookup",
                        &format!("MusicBrainz search failed: {d}"),
                    );
                    (format!("NetworkError: {d}"), "MusicBrainz (network error)")
                }
            };
            return MbMatch {
                notes,
                source: source.to_owned(),
                failure: Some(e),
                ..MbMatch::default()
            };
        }
    };
    let Ok(data) = serde_json::from_slice::<Value>(&body) else {
        // Logged too (see above); the Python app did not.
        musaic_log::warning(
            "musaic.mb_lookup",
            "MusicBrainz search failed: search response was not valid JSON",
        );
        return MbMatch {
            notes: "Search response was not valid JSON".to_owned(),
            source: "MusicBrainz (parse error)".to_owned(),
            failure: Some(NetError::Parse(
                "search response was not valid JSON".to_owned(),
            )),
            ..MbMatch::default()
        };
    };
    let mut groups: Vec<Value> = truthy(data.get("release-groups"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !opts.blocked.is_empty() {
        groups.retain(|g| !str_field(g, "id").is_some_and(|id| opts.blocked.contains(id)));
    }
    if groups.is_empty() {
        let mut notes = format!(
            "No release-group match for artist={} title={}",
            repr_str(&artist),
            repr_str(&title)
        );
        if !opts.hint.is_empty() {
            notes.push_str(&format!(" (your hint: \"{}\")", opts.hint));
        }
        if !opts.blocked.is_empty() {
            notes.push_str(&format!(
                " (after excluding {} blocklisted release-group(s))",
                opts.blocked.len()
            ));
        }
        return MbMatch {
            notes,
            source: "MusicBrainz (no match)".to_owned(),
            ..MbMatch::default()
        };
    }
    let top = &groups[pick_with_hint(&groups, opts.hint)];
    let mbid = truthy(top.get("id"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let score = py_int(top.get("score"));
    let rg_title = truthy(top.get("title"))
        .and_then(Value::as_str)
        .unwrap_or(&title)
        .to_owned();
    let names = credit_names(top);
    let rg_artist = if names.is_empty() {
        artist.clone()
    } else {
        let joined = names.join(", ");
        if joined.is_empty() {
            artist.clone()
        } else {
            joined
        }
    };
    let first_release_date = truthy(top.get("first-release-date"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let mut year = year_of(first_release_date);
    if year.is_none()
        && let Some(id) = &mbid
    {
        year = mb
            .browse_release_dates(id)
            .ok()
            .and_then(|b| earliest_release_year(&b));
    }
    let primary_genre = pick_primary_genre(top.get("tags"));
    let confidence = confidence_from_score(score);
    let Some(mbid) = mbid else {
        return MbMatch {
            score,
            confidence,
            notes: "Top match had no MBID".to_owned(),
            source: "MusicBrainz (malformed result)".to_owned(),
            ..MbMatch::default()
        };
    };
    let mut m = MbMatch {
        confident: score >= HIGH_CONFIDENCE_SCORE,
        score,
        confidence,
        mbid: Some(mbid.clone()),
        artist: (!rg_artist.is_empty()).then(|| rg_artist.clone()),
        artist_sort: first_credit_sort_name(top.get("artist-credit")),
        title: (!rg_title.is_empty()).then(|| rg_title.clone()),
        year,
        primary_genre,
        notes: format!(
            "Matched \"{rg_artist} \u{2014} {rg_title}\" (score {score}/100, MBID {mbid})"
        ),
        source: format!("MusicBrainz (release-group {mbid})"),
        ..MbMatch::default()
    };
    if opts.fetch_cover {
        check_cover(&mut m, &mbid, mb);
    }
    if opts.fetch_tracklist {
        let browse = mb.browse_releases(&mbid);
        match browse
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|b| parse_tracklist(b))
        {
            Ok((rid, tracks)) => {
                m.release_mbid = Some(rid);
                m.tracklist = tracks;
            }
            Err(e) => {
                // mb_lookup.py:283 (any exception of `_fetch_tracklist`).
                musaic_log::warning(
                    "musaic.mb_lookup",
                    &format!("MB tracklist fetch failed for {mbid}: {e}"),
                );
                m.notes.push_str(&format!("\nTracklist fetch failed: {e}"));
            }
        }
        m.browse = Some(browse);
    }
    m
}

/// `_head_cover` folded into the match, as both of the Python app's entry points do.
pub fn check_cover(m: &mut MbMatch, release_group: &str, mb: &dyn MusicBrainzSource) {
    match mb.cover_front_status(release_group) {
        Ok(200) => {
            m.cover_url = Some(cover_front_url(release_group));
            m.cover_url_verified = true;
        }
        Ok(404) => m
            .notes
            .push_str("\nCover not in CAA: no front-cover for this release"),
        Ok(s) => m.notes.push_str(&format!("\nCover not in CAA: HTTP {s}")),
        Err(e) => m.notes.push_str(&format!("\nCover not in CAA: {e}")),
    }
}

/// What the check-fix name search does with its hit's anchor.
#[derive(Debug, Clone, PartialEq)]
pub enum FixAnchor {
    /// Save it: `set_album_mb_anchor(album_id, release_group, release, score)`,
    /// then publish `AlbumMbAnchorSet(album_id, "musicbrainz")`.
    Save {
        album_id: String,
        release_group: String,
        release: Option<String>,
        score: i64,
    },
    /// Not saved: no confident hit, or the album has no id (as in the Python app).
    NotConfident,
    /// Not saved: the confident hit failed the Tier 0b acceptance check; the reason.
    Rejected(String),
}

/// [`find_release_for_fix`]'s result: the match the fix path uses as before,
/// and whether its anchor may be saved.
#[derive(Debug, Clone)]
pub struct FixSearch {
    pub matched: MbMatch,
    pub anchor: FixAnchor,
}

/// Deliberate change: the check fixes' MusicBrainz name search
/// (`mb_lookup.find_release` as the no-cover-art and track-gaps fix paths
/// call it). The Python app saved every confident hit of an album with an id as
/// its anchor (musaic/mb_lookup.py:286-312); here a confident hit is saved
/// only when it also passes the track-count and duration check
/// ([`crate::resolver::check_name_search_hit`], one release listing request
/// unless the search already fetched it), the rule Tier 0b applies.
/// The match itself (and so the fix's outcome) is the Python app's either way.
/// Persisting and publishing are the caller's
/// (`musaic_services::sources::find_release_for_fix`).
pub fn find_release_for_fix(
    album: &AlbumRecord,
    opts: FindOptions<'_>,
    mb: &dyn MusicBrainzSource,
) -> FixSearch {
    let matched = find_release(album, opts, mb);
    let anchor = match (&matched.mbid, matched.confident && !album.id.is_empty()) {
        (Some(rg), true) => match crate::resolver::check_name_search_hit(album, &matched, rg, mb) {
            Ok(()) => FixAnchor::Save {
                album_id: album.id.clone(),
                release_group: rg.clone(),
                release: matched.release_mbid.clone(),
                score: matched.score,
            },
            Err(why) => FixAnchor::Rejected(why),
        },
        _ => FixAnchor::NotConfident,
    };
    FixSearch { matched, anchor }
}
