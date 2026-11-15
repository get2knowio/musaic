//! A parsed AcoustID `/v2/lookup` response.
//!
//! Only the fields the anchoring rule reads are kept, mirroring `trim()` in
//! the Python reference rule. Every list that AcoustID may omit or send as
//! `null` deserialises to an empty `Vec` (Python: `x.get(k) or []`).

use serde::{Deserialize, Deserializer};

fn null_as_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// The whole response body.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LookupResponse {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub results: Vec<LookupResult>,
}

impl LookupResponse {
    /// Parse a raw AcoustID JSON body.
    pub fn from_json(body: &[u8]) -> serde_json::Result<Self> {
        serde_json::from_slice(body)
    }

    /// `true` when AcoustID answered `"status": "ok"`.
    pub fn is_ok(&self) -> bool {
        self.status.as_deref() == Some("ok")
    }
}

/// One fingerprint match.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LookupResult {
    #[serde(default)]
    pub id: Option<String>,
    /// Missing score counts as 0 (Python: `res.get("score") or 0`).
    #[serde(default)]
    pub score: Option<f64>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub recordings: Vec<Recording>,
}

impl LookupResult {
    /// The score with a missing value read as 0.
    pub fn score_or_zero(&self) -> f64 {
        self.score.unwrap_or(0.0)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Recording {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub duration: Option<f64>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub releasegroups: Vec<ReleaseGroup>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ReleaseGroup {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default, rename = "type")]
    pub primary_type: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub secondarytypes: Vec<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub releases: Vec<Release>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Release {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub track_count: Option<u32>,
    #[serde(default)]
    pub medium_count: Option<u32>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub mediums: Vec<Medium>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Medium {
    #[serde(default)]
    pub position: Option<u32>,
    #[serde(default)]
    pub track_count: Option<u32>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub tracks: Vec<MediumTrack>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct MediumTrack {
    #[serde(default)]
    pub position: Option<u32>,
}
