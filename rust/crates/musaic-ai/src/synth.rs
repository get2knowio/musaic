//! The resolver-to-suggestion synthesizers and the rule that decides when
//! the resolver settled an album on its own:
//! `ai.synthesize_metadata_from_resolution`,
//! `ai.synthesize_cover_from_resolution`, and the two `can_skip_ai` /
//! cover tests of the Python app's `services/worker.run_one_job`.

use musaic_resolve::{AlbumResolution, FieldFact};

use crate::parse::{CoverSuggestion, MetadataSuggestion, TrackSuggestion};
use crate::pyjson::{Py, py_strip};

/// `anchor.confidence >= 0.85` / `cover_url.confidence >= 0.85`
/// (services/worker.py:276-280, 366).
pub const SKIP_CONFIDENCE: f64 = 0.85;

/// How many tracks the skip rule counts as unresolved.
///
/// Tracks Tier 1 aligns by position (confidence
/// `musaic_resolve::resolution::POSITIONAL_CONFIDENCE`, 0.8) carry a
/// canonical title and are not counted in `unresolved_track_count`, so an
/// anchored album aligned that way is settled like any fully aligned one.
/// The Python app has no positional alignment and would not count it as
/// settled. Pinned by tests/positional_skip.rs.
pub fn unresolved_for_skip_rule(r: &AlbumResolution) -> usize {
    r.unresolved_track_count
}

/// `can_skip_ai`: an anchor, at confidence >= 0.85, and no unresolved
/// track ([`unresolved_for_skip_rule`]). Then the metadata suggestion is
/// synthesized and no research is needed.
pub fn resolver_settles_metadata(r: &AlbumResolution) -> bool {
    r.anchor
        .as_ref()
        .is_some_and(|a| a.confidence >= SKIP_CONFIDENCE)
        && unresolved_for_skip_rule(r) == 0
}

/// The cover half: a resolver cover URL at confidence >= 0.85 is used as is.
pub fn cover_from_resolver(r: &AlbumResolution) -> bool {
    r.cover_url
        .as_ref()
        .is_some_and(|c| c.confidence >= SKIP_CONFIDENCE)
}

fn current(d: &Py, key: &str) -> String {
    // `str(album_dict.get(key) or "").strip()`
    match d.get(key) {
        Some(v) if v.truthy() => py_strip(&v.py_str()).to_string(),
        _ => String::new(),
    }
}

fn diff(out: &mut Vec<(String, String)>, field: &str, cur: &str, fact: Option<&FieldFact>) {
    let Some(f) = fact else { return };
    let proposed = py_strip(&f.value).to_string();
    if !proposed.is_empty() && proposed != cur {
        match out.iter_mut().find(|(k, _)| k == field) {
            Some(slot) => slot.1 = proposed,
            None => out.push((field.to_string(), proposed)),
        }
    }
}

/// The `notes` of a suggestion synthesized from the resolver (shown as
/// "Metadata notes" / the cover notes; stored with the result). The Python
/// app's text also said that no AI call was needed.
pub const DERIVED_NOTES: &str = "Derived from MusicBrainz.";

/// `ai.synthesize_metadata_from_resolution(album, tracks, resolution)`:
/// the canonical fields diffed against the current tags, only fields that
/// would change.
pub fn synthesize_metadata(album: &Py, tracks: &Py, r: &AlbumResolution) -> MetadataSuggestion {
    let mut suggestions = Vec::new();
    diff(
        &mut suggestions,
        "title",
        &current(album, "title"),
        r.canonical_title.as_ref(),
    );
    diff(
        &mut suggestions,
        "album_artist",
        &current(album, "album_artist"),
        r.canonical_album_artist.as_ref(),
    );
    // `artist` mirrors album_artist when MB only gave the album-level credit.
    diff(
        &mut suggestions,
        "artist",
        &current(album, "artist"),
        r.canonical_album_artist.as_ref(),
    );
    diff(
        &mut suggestions,
        "year",
        &current(album, "year"),
        r.canonical_year.as_ref(),
    );
    diff(
        &mut suggestions,
        "genre",
        &current(album, "genre"),
        r.canonical_genre.as_ref(),
    );
    diff(
        &mut suggestions,
        "album_artist_sort",
        &current(album, "album_artist_sort"),
        r.canonical_album_artist_sort.as_ref(),
    );

    // `{t.get("filename"): t for t in tracks}`: a later duplicate wins.
    let empty = Py::Dict(Vec::new());
    let by_filename = |name: &str| -> &Py {
        let mut found = &empty;
        if let Py::List(items) = tracks {
            for t in items {
                if t.get("filename").and_then(Py::as_str) == Some(name) {
                    found = t;
                }
            }
        }
        found
    };
    let mut track_suggestions = Vec::new();
    for tr in &r.tracks {
        let ft = by_filename(&tr.filename);
        let mut cleaned = Vec::new();
        diff(
            &mut cleaned,
            "title",
            &current(ft, "title"),
            tr.canonical_title.as_ref(),
        );
        diff(
            &mut cleaned,
            "artist",
            &current(ft, "artist"),
            tr.canonical_artist.as_ref(),
        );
        diff(
            &mut cleaned,
            "tracknumber",
            &current(ft, "tracknumber"),
            tr.canonical_tracknumber.as_ref(),
        );
        if !cleaned.is_empty() {
            track_suggestions.push(TrackSuggestion {
                filename: tr.filename.clone(),
                suggestions: cleaned,
            });
        }
    }
    MetadataSuggestion {
        suggestions,
        tracks: track_suggestions,
        notes: DERIVED_NOTES.into(),
        confidence: "high".into(),
        mb_release_group_mbid: r.anchor.as_ref().map(|a| a.value.clone()),
    }
}

/// `ai.synthesize_cover_from_resolution(resolution)`.
pub fn synthesize_cover(r: &AlbumResolution) -> CoverSuggestion {
    match &r.cover_url {
        None => CoverSuggestion {
            cover_url: None,
            source: "MusicBrainz / Cover Art Archive".into(),
            notes: "No cover in CAA for the anchored release-group.".into(),
            confidence: "low".into(),
        },
        Some(c) => CoverSuggestion {
            cover_url: Some(c.value.clone()),
            source: "Cover Art Archive (anchored release-group)".into(),
            notes: DERIVED_NOTES.into(),
            confidence: "high".into(),
        },
    }
}
