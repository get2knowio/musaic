use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{MAX_CHECK_IDS, Validate, check_id_list, check_idempotency_key};
use crate::errors::ToolError;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChecksStartInput {
    #[schemars(length(max = 500))]
    pub album_ids: Option<Vec<String>>,
    #[schemars(length(max = 20))]
    pub check_ids: Option<Vec<String>>,
    #[schemars(regex(pattern = r"^[A-Za-z0-9._:-]{8,128}$"))]
    pub idempotency_key: String,
}

impl Validate for ChecksStartInput {
    fn validate(&self) -> Result<(), ToolError> {
        if let Some(ids) = &self.album_ids {
            check_id_list("albumIds", ids, 0)?;
        }
        if let Some(ids) = &self.check_ids {
            if ids.len() > MAX_CHECK_IDS {
                return Err(ToolError::invalid("checkIds", "must hold at most 20 ids"));
            }
            if ids.iter().any(|c| c.is_empty() || c.len() > 64) {
                return Err(ToolError::invalid(
                    "checkIds",
                    "each id is 1 to 64 characters",
                ));
            }
        }
        check_idempotency_key(&self.idempotency_key)
    }
}
