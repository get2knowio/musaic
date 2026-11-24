//! The suggestion types a research result carries (`MetadataSuggestion`,
//! `CoverSuggestion`) and the fields a suggestion may name.

use crate::pyjson::Py;

/// `SUGGESTABLE_FIELDS`: the album-level keys a suggestion may carry.
pub const SUGGESTABLE_FIELDS: [&str; 8] = [
    "title",
    "album_artist",
    "artist",
    "year",
    "genre",
    "album_artist_sort",
    "artist_sort",
    "album_sort",
];

/// `TRACK_SUGGESTABLE_FIELDS`.
pub const TRACK_SUGGESTABLE_FIELDS: [&str; 3] = ["title", "artist", "tracknumber"];

/// `TrackSuggestion`.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackSuggestion {
    pub filename: String,
    pub suggestions: Vec<(String, String)>,
}

/// `MetadataSuggestion` (without `raw`).
#[derive(Debug, Clone, PartialEq)]
pub struct MetadataSuggestion {
    pub suggestions: Vec<(String, String)>,
    pub tracks: Vec<TrackSuggestion>,
    pub notes: String,
    pub confidence: String,
    pub mb_release_group_mbid: Option<String>,
}

impl MetadataSuggestion {
    /// `result["metadata"]` as the worker stores it.
    pub fn to_py(&self) -> Py {
        let pairs = |p: &[(String, String)]| {
            Py::Dict(
                p.iter()
                    .map(|(k, v)| (k.clone(), Py::str(v.clone())))
                    .collect(),
            )
        };
        Py::dict([
            ("suggestions", pairs(&self.suggestions)),
            (
                "tracks",
                Py::List(
                    self.tracks
                        .iter()
                        .map(|t| {
                            Py::dict([
                                ("filename", Py::str(t.filename.clone())),
                                ("suggestions", pairs(&t.suggestions)),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("notes", Py::str(self.notes.clone())),
            ("confidence", Py::str(self.confidence.clone())),
            (
                "mb_release_group_mbid",
                Py::opt_str(self.mb_release_group_mbid.clone()),
            ),
        ])
    }
}

/// `CoverSuggestion` (without `raw`).
#[derive(Debug, Clone, PartialEq)]
pub struct CoverSuggestion {
    pub cover_url: Option<String>,
    pub source: String,
    pub notes: String,
    pub confidence: String,
}

impl CoverSuggestion {
    /// `result["cover"]` as the worker stores it.
    pub fn to_py(&self) -> Py {
        Py::dict([
            ("cover_url", Py::opt_str(self.cover_url.clone())),
            ("source", Py::str(self.source.clone())),
            ("notes", Py::str(self.notes.clone())),
            ("confidence", Py::str(self.confidence.clone())),
        ])
    }
}
