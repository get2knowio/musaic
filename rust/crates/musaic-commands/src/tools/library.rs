use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{MAX_QUERY, Validate, check_cursor, check_limit};
use crate::errors::ToolError;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LibraryListInput {
    #[schemars(length(min = 1, max = 512))]
    pub cursor: Option<String>,
    #[schemars(range(min = 1, max = 100))]
    pub limit: Option<u32>,
    /// Case-insensitive substring of the artist or the title.
    #[schemars(length(max = 200))]
    pub query: Option<String>,
}

impl Validate for LibraryListInput {
    fn validate(&self) -> Result<(), ToolError> {
        check_cursor(&self.cursor)?;
        check_limit(self.limit)?;
        if self
            .query
            .as_ref()
            .is_some_and(|q| q.chars().count() > MAX_QUERY)
        {
            return Err(ToolError::invalid(
                "query",
                "must be at most 200 characters",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AlbumSummary {
    pub album_id: String,
    pub artist: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<String>,
    pub track_count: u32,
    pub missing: bool,
    pub has_cover: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryListOutput {
    pub items: Vec<AlbumSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub total: u32,
}
