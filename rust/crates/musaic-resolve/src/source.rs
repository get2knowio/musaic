//! What the resolver needs from the outside world, as traits.
//!
//! `musaic-resolve` sits below `musaic-net` in the dependency graph (the net
//! crate uses this crate's parsers), so the resolver cannot call the HTTP
//! clients directly. Instead it asks a [`MusicBrainzSource`] for raw response
//! bodies and parses them itself; `musaic-net` (or the application crate) is
//! expected to implement the trait over `MusicBrainzClient` and its transport.
//! `musaic_services::sources` holds the production implementations; the
//! integration tests implement it over `musaic_net::ReplayTransport`.
//!
//! Every method answers `Err(NetError)` instead of failing the resolution: the
//! cascade turns each failure into "no anchor" or a partial result with a trace
//! entry.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::LazyLock;

use regex::Regex;

use crate::acoustid::LookupResult;

/// A failed request, in the three classes the Python app distinguishes
/// (`mb_lookup.find_release`: "network error", "HTTP error", "parse error").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetError {
    /// No answer at all: connection failure, timeout, or (replay) unobserved.
    Network(String),
    /// An answer with a status other than the one expected.
    Http(u16),
    /// A body that could not be read.
    Parse(String),
    /// The caller cancelled the request (the app is closing). Nothing
    /// failed and nothing was learned: not an outage, not "no match".
    Cancelled,
}

impl fmt::Display for NetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NetError::Network(d) => write!(f, "network error: {d}"),
            NetError::Http(s) => write!(f, "HTTP {s}"),
            NetError::Parse(d) => write!(f, "parse error: {d}"),
            NetError::Cancelled => write!(f, "request cancelled"),
        }
    }
}

impl std::error::Error for NetError {}

/// The MusicBrainz and Cover Art Archive requests of `musaic/mb_lookup.py`.
/// Each returns the body of a 200 answer; any other status is
/// `Err(NetError::Http(status))`.
pub trait MusicBrainzSource {
    /// `GET /ws/2/release-group/?query=artist:"…" AND release:"…"&fmt=json&limit=5`
    /// (`musaic_net::MusicBrainzClient::search_request`).
    fn search_release_groups(&self, artist: &str, title: &str) -> Result<Vec<u8>, NetError>;

    /// `GET /ws/2/release?release-group=…&fmt=json&inc=media+recordings+artist-credits&limit=25`
    /// (`musaic_net::MusicBrainzClient::browse_request`): the tracklist fetch
    /// and, for the Tier 0b acceptance check, every release's shape.
    fn browse_releases(&self, release_group: &str) -> Result<Vec<u8>, NetError>;

    /// `GET /ws/2/release?release-group=…&fmt=json&limit=25` (no `inc`): the
    /// year fallback when the search hit has no `first-release-date`
    /// (`musaic_net::MusicBrainzClient::release_dates_request`).
    fn browse_release_dates(&self, release_group: &str) -> Result<Vec<u8>, NetError>;

    /// `HEAD https://coverartarchive.org/release-group/{mbid}/front`,
    /// following redirects: the final status
    /// (`musaic_net::CoverArtClient::check_front`).
    fn cover_front_status(&self, release_group: &str) -> Result<u16, NetError>;

    /// `GET /ws/2/release-group/{mbid}?fmt=json&inc=artist-credits+tags`:
    /// the group's own title, artist credit (with sort-names),
    /// first-release-date and tags. Not a request the Python app made: Tier 1
    /// of this build asks it for every anchor that did not come from a name
    /// search, so the canonical album fields are filled whichever tier
    /// anchored. Never recorded yet
    /// (`musaic_net::MusicBrainzClient::release_group_request`). The default
    /// fails like an unreachable server so the cascade falls back to the
    /// release listing.
    fn lookup_release_group(&self, release_group: &str) -> Result<Vec<u8>, NetError> {
        let _ = release_group;
        Err(NetError::Network(
            "release-group lookup not implemented by this source".to_owned(),
        ))
    }
}

/// The album the resolver works on, as the Python app's `cached_album_dict` row
/// gives it (only the fields the cascade reads).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AlbumRecord {
    pub id: String,
    pub dir_name: Option<String>,
    pub dir_artist: Option<String>,
    pub dir_album: Option<String>,
    pub is_compilation: bool,
    /// `None` only for hand-made rows; the scanner always writes one.
    pub track_count: Option<i64>,
    pub total_seconds: Option<f64>,
    pub title: Option<String>,
    pub album_artist: Option<String>,
    pub artist: Option<String>,
    pub disc_number: Option<i64>,
    pub disc_total: Option<i64>,
    /// The stored anchor at the time the row was read (`prev_anchor`).
    pub mb_release_group_mbid: Option<String>,
}

impl AlbumRecord {
    /// From a scanned album plus its stored anchor column.
    pub fn from_album(album: &musaic_core::Album, mb_release_group_mbid: Option<String>) -> Self {
        Self {
            id: album.id.clone(),
            dir_name: Some(album.dir_name.clone()),
            dir_artist: album.dir_artist.clone(),
            dir_album: album.dir_album.clone(),
            is_compilation: album.is_compilation,
            track_count: Some(album.track_count as i64),
            total_seconds: Some(album.total_seconds),
            title: album.title.clone(),
            album_artist: album.album_artist.clone(),
            artist: album.artist.clone(),
            disc_number: album.disc_number,
            disc_total: album.disc_total,
            mb_release_group_mbid,
        }
    }

    /// The directory carries "(disc N of M)" (the reference rule's multi-disc
    /// test: `(?i)\(disc \d+ of \d+\)` on the dir name).
    pub fn is_disc_directory(&self) -> bool {
        self.dir_name.as_deref().is_some_and(has_disc_of_marker)
    }

    /// The album's own title for the tie-break: `dir_album`, else the ALBUM tag.
    pub fn own_title(&self) -> &str {
        self.dir_album
            .as_deref()
            .filter(|s| !s.is_empty())
            .or(self.title.as_deref())
            .unwrap_or("")
    }
}

/// The reference rule's `re.compile(r"\(disc \d+ of \d+\)", re.I).search`.
/// Python's `\d` is any Unicode decimal digit
/// (category Nd), as is the `regex` crate's. Python's `re.I` also lets `i`
/// match U+0130 and U+0131, which Unicode simple case folding (the `regex`
/// crate's) does not, hence the explicit class; `ſ` matches `s` in both.
/// An earlier hand-written matcher sliced the lower-cased
/// name at every byte index and panicked inside a multi-byte character
/// ("Café Orchestra"); a match over `char`s cannot.
static DISC_OF_MARKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\(d[i\x{130}\x{131}]sc \d+ of \d+\)").expect("static pattern")
});

fn has_disc_of_marker(s: &str) -> bool {
    DISC_OF_MARKER.is_match(s)
}

/// One FLAC as `services.albums.read_tracks` reports it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FlacTrack {
    pub filename: String,
    pub tracknumber: TrackNumber,
    /// `round(audio.info.length, 1)`, or `None`.
    pub duration: Option<f64>,
}

/// The TRACKNUMBER value. The app always reads a string; an integer appears
/// only in hand-built inputs and is kept for parity with the Python app's
/// `int(x) if x else None`.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum TrackNumber {
    #[default]
    Missing,
    Text(String),
    Int(i64),
}

impl TrackNumber {
    /// `int(x) if x else None` (Python truthiness, then `int()`).
    pub fn as_position(&self) -> Option<i64> {
        match self {
            TrackNumber::Missing => None,
            TrackNumber::Text(s) if s.is_empty() => None,
            TrackNumber::Text(s) => musaic_core::pycompat::int(s),
            TrackNumber::Int(0) => None,
            TrackNumber::Int(i) => Some(*i),
        }
    }
}

/// Album row, file list and tags.
pub trait AlbumSource {
    /// `cached_album_dict(album_id)`; `None` when the album is not in the cache.
    fn album(&self, album_id: &str) -> Option<AlbumRecord>;
    /// `list_album_filenames`: FLAC filenames in the album directory.
    fn filenames(&self, album: &AlbumRecord) -> Vec<String>;
    /// `read_tracks`: per-FLAC TRACKNUMBER and duration, in file order.
    fn tracks(&self, album: &AlbumRecord) -> Vec<FlacTrack>;
}

/// The stored anchor (`cache.get_album_mb_anchor`).
#[derive(Debug, Clone, PartialEq)]
pub struct StoredAnchor {
    pub release_group_mbid: String,
    pub release_mbid: Option<String>,
    pub score: Option<i64>,
    pub last_checked_at: Option<String>,
}

/// One `track_fingerprints` row as the resolver reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct FingerprintRow {
    pub filename: String,
    pub recording_mbid: Option<String>,
    pub score: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("store: {0}")]
pub struct StoreError(pub String);

/// Anchor columns, blocklist and fingerprint rows (`musaic/cache.py`).
pub trait AnchorStore {
    fn anchor(&self, album_id: &str) -> Result<Option<StoredAnchor>, StoreError>;
    /// `set_album_mb_anchor`; the store stamps `last_checked_at` itself.
    fn set_anchor(
        &self,
        album_id: &str,
        release_group_mbid: &str,
        release_mbid: Option<&str>,
        score: i64,
    ) -> Result<(), StoreError>;
    fn blocked(&self, album_id: &str) -> Result<BTreeSet<String>, StoreError>;
    fn fingerprints(&self, album_id: &str) -> Result<Vec<FingerprintRow>, StoreError>;
}

/// The AcoustID answer for one audio file.
#[derive(Debug, Clone)]
pub struct TrackLookup {
    pub filename: String,
    /// Every result AcoustID returned for the file's fingerprint, or
    /// the failure of that lookup.
    pub results: Result<Vec<LookupResult>, NetError>,
}

/// What Tier 0c gets to vote on.
#[derive(Debug, Clone)]
pub enum FingerprintLookups {
    /// No AcoustID key (local fingerprints are still computed, only the
    /// lookup is unavailable) or no audio decoder.
    Unavailable(String),
    /// One entry per audio file, in the album's file order.
    Tracks(Vec<TrackLookup>),
}

/// Fingerprints are computed by `musaic-dsp` and looked up by `musaic-net`;
/// the resolver only consumes the result.
pub trait FingerprintSource {
    fn lookups(&self, album: &AlbumRecord) -> FingerprintLookups;
}

#[cfg(test)]
mod tests {
    use super::*;

    // The "(disc N of M)" test the reference applies to directory names.
    #[test]
    fn disc_marker() {
        for (s, want) in [
            ("A - B (disc 1 of 2) [FLAC]", true),
            ("A - B (Disc 10 of 12)", true),
            ("A - B (disc 1) [FLAC]", false),
            ("A - B (1 of 2) [FLAC]", false),
            ("A - B disc 1 of 2", false),
            ("(disc  1 of 2)", false),
            ("Café Orchestra - Été (disc 1 of 2) [FLAC]", true),
            ("İstanbul - x (DİSC 1 OF 2)", true),
        ] {
            assert_eq!(has_disc_of_marker(s), want, "{s}");
        }
    }
}
