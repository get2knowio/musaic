use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Validate, check_idempotency_key};
use crate::errors::ToolError;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScanStartInput {
    /// The `rootId` of an entry in `capabilities.configuredRoots`.
    pub configured_root_id: String,
    #[schemars(regex(pattern = r"^[A-Za-z0-9._:-]{8,128}$"))]
    pub idempotency_key: String,
}

impl Validate for ScanStartInput {
    fn validate(&self) -> Result<(), ToolError> {
        super::check_album_id("configuredRootId", &self.configured_root_id)?;
        check_idempotency_key(&self.idempotency_key)
    }
}
