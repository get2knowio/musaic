//! The plain types the hub hands around.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::SystemTime;

use musaic_commands::tools::operation::{Counts, OperationResult, OperationState};
use musaic_commands::{PermissionKind, ToolError};

/// Who is calling: resolved from a token, passed into every tool operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub pairing_id: String,
    pub pairing_name: String,
    pub scopes: Vec<PermissionKind>,
    pub instance_id: String,
}

/// An operation an agent started, in the hub's registry.
#[derive(Debug)]
pub struct Operation {
    pub id: String,
    pub pairing_id: String,
    pub tool: &'static str,
    pub state: OperationState,
    pub counts: Counts,
    pub result: Option<OperationResult>,
    pub error: Option<ToolError>,
    pub approval_id: Option<String>,
    /// Cooperative cancellation: workers check it at item boundaries.
    pub cancel: Arc<AtomicBool>,
    pub created_at: SystemTime,
}

impl Operation {
    pub fn new(id: &str, pairing_id: &str, tool: &'static str, now: SystemTime) -> Self {
        Self {
            id: id.to_string(),
            pairing_id: pairing_id.to_string(),
            tool,
            state: OperationState::Running,
            counts: Counts::default(),
            result: None,
            error: None,
            approval_id: None,
            cancel: Arc::new(AtomicBool::new(false)),
            created_at: now,
        }
    }

    pub fn snapshot(&self) -> OperationSnapshot {
        OperationSnapshot {
            id: self.id.clone(),
            pairing_id: self.pairing_id.clone(),
            tool: self.tool,
            state: self.state,
            counts: self.counts,
            result: self.result.clone(),
            error: self.error.clone(),
            approval_id: self.approval_id.clone(),
            cancel: self.cancel.clone(),
        }
    }
}

/// A copy of an [`Operation`] to read without holding the hub's lock.
#[derive(Debug, Clone)]
pub struct OperationSnapshot {
    pub id: String,
    pub pairing_id: String,
    pub tool: &'static str,
    pub state: OperationState,
    pub counts: Counts,
    pub result: Option<OperationResult>,
    pub error: Option<ToolError>,
    pub approval_id: Option<String>,
    pub cancel: Arc<AtomicBool>,
}
