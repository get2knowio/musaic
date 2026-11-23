//! Private, authenticated bridge messages for the modern MCP Task mapping.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::Validate;
use super::operation::{OperationGetInput, OperationGetOutput};
use crate::ToolError;

pub const EXTENSION_ID: &str = "io.modelcontextprotocol/tasks";
pub const PROFILE: &str = "mcp-2026-07-28";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskInput {
    pub task_id: String,
}

impl Validate for TaskInput {
    fn validate(&self) -> Result<(), ToolError> {
        if self
            .task_id
            .strip_prefix("task_")
            .is_some_and(|id| id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            Ok(())
        } else {
            Err(ToolError::invalid("taskId", "invalid task identifier"))
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskUpdateInput {
    pub task_id: String,
    pub input_responses: BTreeMap<String, Value>,
}

impl Validate for TaskUpdateInput {
    fn validate(&self) -> Result<(), ToolError> {
        TaskInput {
            task_id: self.task_id.clone(),
        }
        .validate()?;
        if self.input_responses.len() > 32
            || self
                .input_responses
                .keys()
                .any(|key| key.is_empty() || key.len() > 128)
        {
            return Err(ToolError::invalid(
                "inputResponses",
                "invalid input response keys",
            ));
        }
        Ok(())
    }
}

pub type TaskCreateInput = OperationGetInput;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskSnapshot {
    pub task_id: String,
    pub created_at: String,
    pub updated_at: String,
    pub ttl_ms: Option<u64>,
    pub execution_failed: bool,
    pub operation: OperationGetOutput,
    pub pending_inputs: BTreeMap<String, Value>,
}
