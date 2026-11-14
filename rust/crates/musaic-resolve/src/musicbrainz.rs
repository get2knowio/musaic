//! The two MusicBrainz responses a name search uses, reduced to what the
//! resolver needs: search candidates, and each release's shape (track counts
//! and total durations) from a release browse with `inc=media+recordings`.

use serde::Deserialize;

use crate::name_search::{MediumShape, ReleaseShape};

/// One hit of `/ws/2/release-group/?query=...`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchCandidate {
    pub release_group_id: String,
    pub title: String,
    /// Lucene score, 0-100.
    pub score: u32,
}

#[derive(Deserialize)]
struct SearchDoc {
    #[serde(rename = "release-groups", default)]
    release_groups: Option<Vec<SearchGroup>>,
}

#[derive(Deserialize)]
struct SearchGroup {
    id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    score: Option<serde_json::Value>,
}

/// Search hits in response order (the Python app takes the first). Hits without
/// an id are dropped; a missing or non-numeric score reads as 0.
pub fn parse_search(json: &[u8]) -> serde_json::Result<Vec<SearchCandidate>> {
    let doc: SearchDoc = serde_json::from_slice(json)?;
    Ok(doc
        .release_groups
        .unwrap_or_default()
        .into_iter()
        .filter_map(|g| {
            let score = match g.score {
                Some(serde_json::Value::Number(n)) => n.as_u64().unwrap_or(0) as u32,
                Some(serde_json::Value::String(s)) => s.trim().parse().unwrap_or(0),
                _ => 0,
            };
            Some(SearchCandidate {
                release_group_id: g.id?,
                title: g.title.unwrap_or_default(),
                score,
            })
        })
        .collect())
}

#[derive(Deserialize)]
struct BrowseDoc {
    #[serde(default)]
    releases: Option<Vec<BrowseRelease>>,
}

#[derive(Deserialize)]
struct BrowseRelease {
    id: Option<String>,
    #[serde(default)]
    media: Option<Vec<BrowseMedium>>,
}

#[derive(Deserialize)]
struct BrowseMedium {
    #[serde(rename = "track-count", default)]
    track_count: Option<u64>,
    #[serde(default)]
    tracks: Option<Vec<BrowseTrack>>,
}

#[derive(Deserialize)]
struct BrowseTrack {
    /// Milliseconds; null when MusicBrainz has no length for the track.
    #[serde(default)]
    length: Option<f64>,
}

/// `(release id, shape)` for every release in a browse response. A disc's
/// duration is known only when every one of its tracks has a length, and a
/// release's only when every disc's is.
pub fn parse_release_browse(json: &[u8]) -> serde_json::Result<Vec<(String, ReleaseShape)>> {
    let doc: BrowseDoc = serde_json::from_slice(json)?;
    Ok(doc
        .releases
        .unwrap_or_default()
        .into_iter()
        .map(|r| {
            let mediums: Vec<MediumShape> = r
                .media
                .unwrap_or_default()
                .into_iter()
                .map(|m| {
                    let tracks = m.tracks.unwrap_or_default();
                    let duration_s = (!tracks.is_empty()
                        && tracks.iter().all(|t| t.length.is_some()))
                    .then(|| tracks.iter().filter_map(|t| t.length).sum::<f64>() / 1000.0);
                    MediumShape {
                        track_count: m.track_count.unwrap_or(tracks.len() as u64),
                        duration_s,
                    }
                })
                .collect();
            let duration_s = (!mediums.is_empty()
                && mediums.iter().all(|m| m.duration_s.is_some()))
            .then(|| mediums.iter().filter_map(|m| m.duration_s).sum());
            let shape = ReleaseShape {
                track_count: mediums.iter().map(|m| m.track_count).sum(),
                duration_s,
                mediums,
            };
            (r.id.unwrap_or_default(), shape)
        })
        .collect())
}
