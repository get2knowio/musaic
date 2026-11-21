use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Validate;
use crate::errors::{ToolError, ToolErrorCode};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    AwaitingApproval,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Denied,
    Expired,
}

impl OperationState {
    pub fn is_finished(self) -> bool {
        !matches!(self, Self::AwaitingApproval | Self::Running)
    }
}

/// What a start tool returns: a handle to poll with `musaic_operation_get`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OperationHandle {
    pub operation_id: String,
    pub state: OperationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Counts {
    pub done: u32,
    pub total: u32,
    pub failed: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FileStatus {
    Ok,
    Failed,
    Skipped,
}

/// One file's outcome, by bare file name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FileOutcome {
    pub file_name: String,
    pub status: FileStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApplyResult {
    pub files: Vec<FileOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<FileOutcome>,
    /// Whether the rename step ran and finished.
    pub renamed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_id_after: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CheckRunSummary {
    pub check_id: String,
    pub findings: u32,
    pub failed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisItem {
    pub album_id: String,
    pub kind: String,
    pub status: FileStatus,
}

/// The result of a finished operation; only the part its tool produces is set.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OperationResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apply: Option<ApplyResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scan_albums: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks: Option<Vec<CheckRunSummary>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analysis: Option<Vec<AnalysisItem>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationGetInput {
    pub operation_id: String,
}

fn check_operation_id(id: &str) -> Result<(), ToolError> {
    if id.starts_with("op_") && id.len() <= 64 {
        Ok(())
    } else {
        Err(ToolError::new(
            ToolErrorCode::InvalidInput,
            "operationId is not an operation id",
        )
        .with_detail("field", "operationId"))
    }
}

impl Validate for OperationGetInput {
    fn validate(&self) -> Result<(), ToolError> {
        check_operation_id(&self.operation_id)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OperationGetOutput {
    pub operation_id: String,
    pub tool: String,
    pub state: OperationState,
    pub counts: Counts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<OperationResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ToolError>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationCancelInput {
    pub operation_id: String,
}

impl Validate for OperationCancelInput {
    fn validate(&self) -> Result<(), ToolError> {
        check_operation_id(&self.operation_id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Cancellation {
    Requested,
    NotCancellable,
    AlreadyFinished,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OperationCancelOutput {
    pub operation_id: String,
    pub state: OperationState,
    pub cancellation: Cancellation,
}
