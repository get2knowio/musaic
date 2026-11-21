//! Typed input and output of the twelve tools (contracts/tools.md). Every
//! input denies unknown fields and has a `validate()` that enforces the
//! bounds; every output is an object.

pub mod album;
pub mod analysis;
pub mod capabilities;
pub mod checks;
pub mod findings;
pub mod fix;
pub mod library;
pub mod operation;
pub mod resolution;
pub mod scan;

use crate::errors::ToolError;

/// An input that checks its own bounds.
pub trait Validate {
    fn validate(&self) -> Result<(), ToolError>;
}

pub const MAX_CURSOR: usize = 512;
pub const MAX_QUERY: usize = 200;
pub const MAX_IDS: usize = 500;
pub const MAX_CHECK_IDS: usize = 20;
pub const MAX_TAG_VALUE: usize = 1000;
pub const DEFAULT_LIMIT: u32 = 50;
pub const MAX_LIMIT: u32 = 100;

pub const ALBUM_ID_PATTERN: &str = "^[0-9a-f]{16}$";
pub const IDEMPOTENCY_PATTERN: &str = "^[A-Za-z0-9._:-]{8,128}$";

pub fn check_album_id(field: &str, s: &str) -> Result<(), ToolError> {
    if s.len() == 16
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        Ok(())
    } else {
        Err(ToolError::invalid(
            field,
            "must be 16 lowercase hex characters",
        ))
    }
}

pub fn check_cursor(s: &Option<String>) -> Result<(), ToolError> {
    match s {
        Some(c) if c.is_empty() || c.chars().count() > MAX_CURSOR => {
            Err(ToolError::invalid("cursor", "must be 1 to 512 characters"))
        }
        _ => Ok(()),
    }
}

pub fn check_limit(limit: Option<u32>) -> Result<(), ToolError> {
    match limit {
        Some(l) if !(1..=MAX_LIMIT).contains(&l) => {
            Err(ToolError::invalid("limit", "must be 1 to 100"))
        }
        _ => Ok(()),
    }
}

pub fn check_idempotency_key(s: &str) -> Result<(), ToolError> {
    let n = s.chars().count();
    if (8..=128).contains(&n)
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
    {
        Ok(())
    } else {
        Err(ToolError::invalid(
            "idempotencyKey",
            "must be 8 to 128 characters from A-Z a-z 0-9 . _ : -",
        ))
    }
}

pub fn check_id_list(field: &str, ids: &[String], min: usize) -> Result<(), ToolError> {
    if ids.len() < min || ids.len() > MAX_IDS {
        return Err(ToolError::invalid(
            field,
            format!("must hold {min} to {MAX_IDS} ids"),
        ));
    }
    for id in ids {
        check_album_id(field, id)?;
    }
    Ok(())
}
