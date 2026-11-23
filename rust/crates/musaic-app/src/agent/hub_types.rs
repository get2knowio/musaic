//! The authenticated caller passed to agent operations.

use musaic_commands::PermissionKind;

/// A principal resolved from a bearer token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub pairing_id: String,
    pub pairing_name: String,
    pub scopes: Vec<PermissionKind>,
    pub instance_id: String,
}
