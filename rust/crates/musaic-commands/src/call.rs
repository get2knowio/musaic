//! A parsed tool call and a typed tool output, so JSON parsing and rendering
//! stay in the faces and the app works on types.

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::errors::{ToolError, ToolErrorCode};
use crate::tools::capabilities::{CapabilitiesInput, CapabilitiesOutput};
use crate::tools::operation::{
    OperationCancelInput, OperationCancelOutput, OperationGetInput, OperationGetOutput,
    OperationHandle,
};
use crate::tools::work::*;
use crate::tools::{
    Validate, album::*, analysis::AnalysisStartInput, checks::ChecksStartInput, findings::*,
    fix::*, library::*, resolution::*, scan::ScanStartInput,
};

/// One variant per tool, holding its validated input.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolCall {
    Capabilities(CapabilitiesInput),
    LibraryList(LibraryListInput),
    AlbumGet(AlbumGetInput),
    ResolutionGet(ResolutionGetInput),
    FindingsList(FindingsListInput),
    FixPreview(FixPreviewInput),
    FixApply(FixApplyInput),
    ScanStart(ScanStartInput),
    ChecksStart(ChecksStartInput),
    AnalysisStart(AnalysisStartInput),
    OperationGet(OperationGetInput),
    OperationCancel(OperationCancelInput),
    WorkList(WorkListInput),
    WorkGet(WorkGetInput),
    WorkClaim(WorkClaimInput),
    WorkClaimMany(WorkClaimManyInput),
    WorkRenew(WorkRenewInput),
    WorkRenewMany(WorkRenewManyInput),
    WorkProgress(WorkProgressInput),
    WorkComplete(WorkCompleteInput),
    WorkFail(WorkFailInput),
    WorkRelease(WorkReleaseInput),
    WorkCancel(WorkCancelInput),
}

impl ToolCall {
    /// The registry id of the tool.
    pub fn tool_id(&self) -> &'static str {
        match self {
            Self::Capabilities(_) => "musaic_capabilities",
            Self::LibraryList(_) => "musaic_library_list",
            Self::AlbumGet(_) => "musaic_album_get",
            Self::ResolutionGet(_) => "musaic_resolution_get",
            Self::FindingsList(_) => "musaic_findings_list",
            Self::FixPreview(_) => "musaic_fix_preview",
            Self::FixApply(_) => "musaic_fix_apply",
            Self::ScanStart(_) => "musaic_scan_start",
            Self::ChecksStart(_) => "musaic_checks_start",
            Self::AnalysisStart(_) => "musaic_analysis_start",
            Self::OperationGet(_) => "musaic_operation_get",
            Self::OperationCancel(_) => "musaic_operation_cancel",
            Self::WorkList(_) => "musaic_work_list",
            Self::WorkGet(_) => "musaic_work_get",
            Self::WorkClaim(_) => "musaic_work_claim",
            Self::WorkClaimMany(_) => "musaic_work_claim_many",
            Self::WorkRenew(_) => "musaic_work_renew",
            Self::WorkRenewMany(_) => "musaic_work_renew_many",
            Self::WorkProgress(_) => "musaic_work_progress",
            Self::WorkComplete(_) => "musaic_work_complete",
            Self::WorkFail(_) => "musaic_work_fail",
            Self::WorkRelease(_) => "musaic_work_release",
            Self::WorkCancel(_) => "musaic_work_cancel",
        }
    }
}

/// One variant per tool, holding its output.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolOutput {
    Capabilities(CapabilitiesOutput),
    LibraryList(LibraryListOutput),
    AlbumGet(AlbumGetOutput),
    ResolutionGet(ResolutionGetOutput),
    FindingsList(FindingsListOutput),
    FixPreview(FixPreviewOutput),
    FixApply(FixApplyOutput),
    /// The three start tools.
    Handle(OperationHandle),
    OperationGet(OperationGetOutput),
    OperationCancel(OperationCancelOutput),
    WorkList(WorkListOutput),
    WorkGet(WorkGetOutput),
    WorkClaim(WorkClaimOutput),
    WorkClaimMany(WorkClaimManyOutput),
    WorkRenew(WorkRenewOutput),
    WorkRenewMany(WorkRenewManyOutput),
    WorkStatus(WorkSummary),
}

impl ToolOutput {
    /// The output as a JSON object.
    pub fn to_json(&self) -> Value {
        fn v<T: Serialize>(t: &T) -> Value {
            serde_json::to_value(t).expect("an output is JSON")
        }
        match self {
            Self::Capabilities(o) => v(o),
            Self::LibraryList(o) => v(o),
            Self::AlbumGet(o) => v(o),
            Self::ResolutionGet(o) => v(o),
            Self::FindingsList(o) => v(o),
            Self::FixPreview(o) => v(o),
            Self::FixApply(o) => v(o),
            Self::Handle(o) => v(o),
            Self::OperationGet(o) => v(o),
            Self::OperationCancel(o) => v(o),
            Self::WorkList(o) => v(o),
            Self::WorkGet(o) => v(o),
            Self::WorkClaim(o) => v(o),
            Self::WorkClaimMany(o) => v(o),
            Self::WorkRenew(o) => v(o),
            Self::WorkRenewMany(o) => v(o),
            Self::WorkStatus(o) => v(o),
        }
    }
}

fn input<T: DeserializeOwned + Validate>(body: &[u8]) -> Result<T, ToolError> {
    let body = if body.iter().all(u8::is_ascii_whitespace) {
        b"{}".as_slice()
    } else {
        body
    };
    let mut decoder = serde_json::Deserializer::from_slice(body);
    let parsed: T = serde_path_to_error::deserialize(&mut decoder).map_err(|error| {
        // The deserializer's display may quote an untrusted string.
        let result = matches!(
            error.path().iter().next(),
            Some(serde_path_to_error::Segment::Map { key }) if key == "result"
        );
        if result {
            ToolError::new(
                ToolErrorCode::InvalidResult,
                "the result does not match the schema",
            )
        } else {
            ToolError::new(
                ToolErrorCode::InvalidInput,
                "the input does not match the schema",
            )
        }
    })?;
    decoder
        .end()
        .map_err(|_| ToolError::new(ToolErrorCode::InvalidInput, "the input has extra data"))?;
    parsed.validate()?;
    Ok(parsed)
}

/// Parse and validate the body of a call to `tool_id`. An unknown tool is
/// `NOT_FOUND`; a schema or bounds failure is `INVALID_INPUT`.
pub fn parse_call(tool_id: &str, body: &[u8]) -> Result<ToolCall, ToolError> {
    Ok(match tool_id {
        "musaic_capabilities" => ToolCall::Capabilities(input(body)?),
        "musaic_library_list" => ToolCall::LibraryList(input(body)?),
        "musaic_album_get" => ToolCall::AlbumGet(input(body)?),
        "musaic_resolution_get" => ToolCall::ResolutionGet(input(body)?),
        "musaic_findings_list" => ToolCall::FindingsList(input(body)?),
        "musaic_fix_preview" => ToolCall::FixPreview(input(body)?),
        "musaic_fix_apply" => ToolCall::FixApply(input(body)?),
        "musaic_scan_start" => ToolCall::ScanStart(input(body)?),
        "musaic_checks_start" => ToolCall::ChecksStart(input(body)?),
        "musaic_analysis_start" => ToolCall::AnalysisStart(input(body)?),
        "musaic_operation_get" => ToolCall::OperationGet(input(body)?),
        "musaic_operation_cancel" => ToolCall::OperationCancel(input(body)?),
        "musaic_work_list" => ToolCall::WorkList(input(body)?),
        "musaic_work_get" => ToolCall::WorkGet(input(body)?),
        "musaic_work_claim" => ToolCall::WorkClaim(input(body)?),
        "musaic_work_claim_many" => ToolCall::WorkClaimMany(input(body)?),
        "musaic_work_renew" => ToolCall::WorkRenew(input(body)?),
        "musaic_work_renew_many" => ToolCall::WorkRenewMany(input(body)?),
        "musaic_work_progress" => ToolCall::WorkProgress(input(body)?),
        "musaic_work_complete" => ToolCall::WorkComplete(input(body)?),
        "musaic_work_fail" => ToolCall::WorkFail(input(body)?),
        "musaic_work_release" => ToolCall::WorkRelease(input(body)?),
        "musaic_work_cancel" => ToolCall::WorkCancel(input(body)?),
        _ => return Err(ToolError::new(ToolErrorCode::NotFound, "no such tool")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_tools_parse_and_name_themselves() {
        let bodies: [(&str, &str); 12] = [
            ("musaic_capabilities", "{}"),
            ("musaic_library_list", "{}"),
            ("musaic_album_get", r#"{"albumId":"0123456789abcdef"}"#),
            ("musaic_resolution_get", r#"{"albumId":"0123456789abcdef"}"#),
            ("musaic_findings_list", "{}"),
            (
                "musaic_fix_preview",
                r#"{"albumId":"0123456789abcdef","baseRevision":"0123456789abcdef0123456789abcdef","proposedPatch":{"rename":true}}"#,
            ),
            (
                "musaic_fix_apply",
                r#"{"proposalId":"pr_0123456789abcdef","digest":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","idempotencyKey":"abcdefgh12"}"#,
            ),
            (
                "musaic_scan_start",
                r#"{"configuredRootId":"0123456789abcdef","idempotencyKey":"abcdefgh12"}"#,
            ),
            ("musaic_checks_start", r#"{"idempotencyKey":"abcdefgh12"}"#),
            (
                "musaic_analysis_start",
                r#"{"albumIds":["0123456789abcdef"],"kinds":["bpm"],"idempotencyKey":"abcdefgh12"}"#,
            ),
            ("musaic_operation_get", r#"{"operationId":"op_1"}"#),
            ("musaic_operation_cancel", r#"{"operationId":"op_1"}"#),
        ];
        for (id, body) in bodies {
            let call = parse_call(id, body.as_bytes()).unwrap_or_else(|e| panic!("{id}: {e}"));
            assert_eq!(call.tool_id(), id);
        }
        assert_eq!(
            parse_call("musaic_nope", b"{}").unwrap_err().code,
            ToolErrorCode::NotFound
        );
    }

    #[test]
    fn empty_body_is_an_empty_object() {
        assert!(parse_call("musaic_capabilities", b"").is_ok());
    }
}
