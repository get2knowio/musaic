use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Validate, check_album_id, check_cursor, check_limit};
use crate::errors::ToolError;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FindingsListInput {
    #[schemars(regex(pattern = r"^[0-9a-f]{16}$"))]
    pub album_id: Option<String>,
    #[schemars(length(min = 1, max = 512))]
    pub cursor: Option<String>,
    #[schemars(range(min = 1, max = 100))]
    pub limit: Option<u32>,
}

impl Validate for FindingsListInput {
    fn validate(&self) -> Result<(), ToolError> {
        if let Some(a) = &self.album_id {
            check_album_id("albumId", a)?;
        }
        check_cursor(&self.cursor)?;
        check_limit(self.limit)
    }
}

/// A finding by reference; the check-specific detail is left out because it
/// can hold paths.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    pub finding_id: String,
    pub check_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_id: Option<String>,
    pub severity: String,
    pub title: String,
    pub status: String,
    pub first_seen: String,
    pub last_seen: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FindingsListOutput {
    pub items: Vec<Finding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}
