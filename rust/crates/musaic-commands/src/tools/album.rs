use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Validate, check_album_id};
use crate::errors::ToolError;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AlbumGetInput {
    #[schemars(regex(pattern = r"^[0-9a-f]{16}$"))]
    pub album_id: String,
}

impl Validate for AlbumGetInput {
    fn validate(&self) -> Result<(), ToolError> {
        check_album_id("albumId", &self.album_id)
    }
}

/// The summary fields plus the folder's bare name, the stored tags and the
/// disc count. Never a path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AlbumDetail {
    pub album_id: String,
    pub artist: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<String>,
    pub track_count: u32,
    pub missing: bool,
    pub has_cover: bool,
    pub folder_name: String,
    pub tags: BTreeMap<String, String>,
    pub disc_count: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    pub file_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_number: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disc_number: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<f64>,
    pub tags: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AlbumGetOutput {
    pub album: AlbumDetail,
    pub tracks: Vec<Track>,
    pub revision: String,
}
