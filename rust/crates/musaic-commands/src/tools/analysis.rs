use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Validate, check_id_list, check_idempotency_key};
use crate::errors::ToolError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum AnalysisKind {
    Bpm,
    Replaygain,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnalysisStartInput {
    #[schemars(length(min = 1, max = 500))]
    pub album_ids: Vec<String>,
    #[schemars(length(min = 1, max = 2))]
    pub kinds: Vec<AnalysisKind>,
    #[schemars(regex(pattern = r"^[A-Za-z0-9._:-]{8,128}$"))]
    pub idempotency_key: String,
}

impl Validate for AnalysisStartInput {
    fn validate(&self) -> Result<(), ToolError> {
        check_id_list("albumIds", &self.album_ids, 1)?;
        if self.kinds.is_empty() || self.kinds.len() > 2 {
            return Err(ToolError::invalid("kinds", "must hold 1 or 2 kinds"));
        }
        check_idempotency_key(&self.idempotency_key)
    }
}
