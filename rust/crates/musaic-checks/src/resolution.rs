//! What the checks ask of a metadata resolution.
//!
//! The resolver lives in `musaic-resolve` (being written separately). The
//! checks only need this narrow view of `services.resolver.AlbumResolution`:
//! whether there is an anchor, four canonical album fields, and per-track
//! canonical artists. [`ResolutionSource`] is the seam; the real resolver gets
//! an adapter, tests feed the Python app-recorded resolutions through
//! [`Resolution::from_python_json`].

use rusqlite::Connection;
use serde_json::Value;

/// `FieldFact` without the confidence (no check reads it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fact {
    pub value: String,
    pub source: String,
}

/// One track of the resolution (`TrackResolution`), reduced to what
/// tags-out-of-sync reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackMatch {
    pub filename: String,
    pub canonical_artist: Option<Fact>,
}

/// The parts of `AlbumResolution` the checks use.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Resolution {
    /// The MusicBrainz release-group anchor; `None` = unanchored.
    pub anchor: Option<Fact>,
    pub canonical_title: Option<Fact>,
    pub canonical_album_artist: Option<Fact>,
    pub canonical_year: Option<Fact>,
    pub canonical_album_artist_sort: Option<Fact>,
    /// The top-voted MusicBrainz tag of the release group, as the resolver
    /// gives it (`pick_primary_genre`; not re-cased).
    pub canonical_genre: Option<Fact>,
    pub tracks: Vec<TrackMatch>,
    /// A MusicBrainz request failed while resolving (network down,
    /// offline, a server error): the canonical values may be missing for
    /// that reason rather than because MusicBrainz has none, so a check
    /// must not take this answer as the album's state.
    pub network_failed: bool,
}

fn fact(v: &Value) -> Option<Fact> {
    let o = v.as_object()?;
    let value = match o.get("value") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    };
    Some(Fact {
        value,
        source: o
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    })
}

impl Resolution {
    /// From `AlbumResolution.to_dict()` (the `/api/albums/{id}/resolution`
    /// body). `None` when `v` is not an object.
    pub fn from_python_json(v: &Value) -> Option<Resolution> {
        let o = v.as_object()?;
        let f = |k: &str| o.get(k).and_then(fact);
        let tracks = o
            .get("tracks")
            .and_then(Value::as_array)
            .map(|ts| {
                ts.iter()
                    .filter_map(|t| {
                        Some(TrackMatch {
                            filename: t.get("filename")?.as_str()?.to_string(),
                            canonical_artist: t.get("canonical_artist").and_then(fact),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(Resolution {
            anchor: f("anchor"),
            canonical_title: f("canonical_title"),
            canonical_album_artist: f("canonical_album_artist"),
            canonical_year: f("canonical_year"),
            canonical_album_artist_sort: f("canonical_album_artist_sort"),
            canonical_genre: f("canonical_genre"),
            tracks,
            network_failed: false,
        })
    }
}

/// `services.resolver.resolve_album(album_id)` as the checks call it. An
/// `Err` is what the Python app's checks catch and treat as "no resolution".
pub trait ResolutionSource: Send + Sync {
    fn resolve(&self, conn: &Connection, album_id: &str) -> Result<Resolution, String>;

    /// `true` once this source has been told to stop (the background sweep
    /// is shutting down or yielding to a restore): an answer it gave since
    /// may be a refusal rather than a real "no resolution", so a check that
    /// sees it ends its run as failed instead of recording findings from it.
    fn interrupted(&self) -> bool {
        false
    }
}

/// A source that never resolves (every call is an error, so anchored albums
/// get no canonical comparison). Placeholder until `musaic-resolve` is wired.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoResolver;

impl ResolutionSource for NoResolver {
    fn resolve(&self, _conn: &Connection, album_id: &str) -> Result<Resolution, String> {
        Err(format!("no resolver configured (album {album_id})"))
    }
}
