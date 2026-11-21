use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Validate;
use crate::errors::ToolError;
use crate::kinds::{Decision, PermissionKind};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CapabilitiesInput {}

impl Validate for CapabilitiesInput {
    fn validate(&self) -> Result<(), ToolError> {
        Ok(())
    }
}

/// A library folder the app is configured with, by id and base name only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConfiguredRoot {
    pub root_id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CapabilitiesOutput {
    pub app_version: String,
    /// Always "1".
    pub contract_version: String,
    pub registry_digest: String,
    pub instance_id: String,
    pub scopes: Vec<PermissionKind>,
    pub policy: BTreeMap<PermissionKind, Decision>,
    pub profiles: Vec<String>,
    pub configured_roots: Vec<ConfiguredRoot>,
}
