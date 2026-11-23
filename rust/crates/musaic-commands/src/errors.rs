//! The stable error codes of the contract (contracts/tools.md). Tool errors
//! come back as a tool result; transport errors are the CLI's own.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

macro_rules! codes {
    ($( $(#[$m:meta])* $variant:ident => $text:literal ),+ $(,)?) => {
        /// A stable error code.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
        pub enum ToolErrorCode {
            $( $(#[$m])* #[serde(rename = $text)] $variant ),+
        }

        impl ToolErrorCode {
            pub const ALL: &'static [ToolErrorCode] = &[ $( Self::$variant ),+ ];

            pub fn as_str(self) -> &'static str {
                match self { $( Self::$variant => $text ),+ }
            }

            pub fn parse(s: &str) -> Option<Self> {
                match s { $( $text => Some(Self::$variant), )+ _ => None }
            }
        }
    };
}

codes! {
    // Tool errors (a tool result with `isError`).
    InvalidInput => "INVALID_INPUT",
    NotFound => "NOT_FOUND",
    MissingAlbum => "MISSING_ALBUM",
    StaleRevision => "STALE_REVISION",
    ProposalExpired => "PROPOSAL_EXPIRED",
    DigestMismatch => "DIGEST_MISMATCH",
    /// The scope or a Refuse policy denies it.
    Forbidden => "FORBIDDEN",
    /// The user denied it.
    Denied => "DENIED",
    ApprovalExpired => "APPROVAL_EXPIRED",
    IdempotencyConflict => "IDEMPOTENCY_CONFLICT",
    CursorStale => "CURSOR_STALE",
    Busy => "BUSY",
    Restoring => "RESTORING",
    NoLibrary => "NO_LIBRARY",
    Unavailable => "UNAVAILABLE",
    AlreadyClaimed => "ALREADY_CLAIMED",
    LeaseExpired => "LEASE_EXPIRED",
    StaleGeneration => "STALE_GENERATION",
    StaleContext => "STALE_CONTEXT",
    InvalidResult => "INVALID_RESULT",
    PolicyDenied => "POLICY_DENIED",
    RateLimited => "RATE_LIMITED",
    TaskExpired => "TASK_EXPIRED",
    Internal => "INTERNAL",
    // Transport errors (reported by the CLI, not by a tool).
    AppNotRunning => "APP_NOT_RUNNING",
    DescriptorInvalid => "DESCRIPTOR_INVALID",
    NotPaired => "NOT_PAIRED",
    PairingRevoked => "PAIRING_REVOKED",
    InstanceMismatch => "INSTANCE_MISMATCH",
    LockedOut => "LOCKED_OUT",
    ProfileUnsupported => "PROFILE_UNSUPPORTED",
}

impl ToolErrorCode {
    /// The codes the CLI reports itself (everything after `INTERNAL`).
    pub fn is_transport(self) -> bool {
        matches!(
            self,
            Self::AppNotRunning
                | Self::DescriptorInvalid
                | Self::NotPaired
                | Self::PairingRevoked
                | Self::InstanceMismatch
                | Self::LockedOut
                | Self::ProfileUnsupported
        )
    }
}

impl std::fmt::Display for ToolErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A failure a tool reports: `{code, message, details?}`. The message never
/// holds a path, a secret or an argument value.
#[derive(
    Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, thiserror::Error,
)]
#[error("{code}: {message}")]
pub struct ToolError {
    pub code: ToolErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<BTreeMap<String, String>>,
}

impl ToolError {
    pub fn new(code: ToolErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
        }
    }

    pub fn with_detail(mut self, key: &str, value: impl Into<String>) -> Self {
        self.details
            .get_or_insert_with(BTreeMap::new)
            .insert(key.to_string(), value.into());
        self
    }

    /// `INVALID_INPUT` naming the offending field.
    pub fn invalid(field: &str, message: impl Into<String>) -> Self {
        Self::new(ToolErrorCode::InvalidInput, message).with_detail("field", field)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_round_trip_and_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for c in ToolErrorCode::ALL {
            assert!(seen.insert(c.as_str()));
            assert_eq!(ToolErrorCode::parse(c.as_str()), Some(*c));
            assert_eq!(
                serde_json::to_string(c).unwrap(),
                format!("\"{}\"", c.as_str())
            );
        }
        assert_eq!(ToolErrorCode::ALL.len(), 31);
    }

    #[test]
    fn invalid_names_the_field() {
        let e = ToolError::invalid("limit", "must be 1..100");
        assert_eq!(e.code, ToolErrorCode::InvalidInput);
        assert_eq!(e.details.unwrap()["field"], "limit");
    }
}
