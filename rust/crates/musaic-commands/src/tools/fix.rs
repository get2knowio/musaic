use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::operation::{ApplyResult, OperationState};
use super::{MAX_TAG_VALUE, Validate, check_album_id, check_idempotency_key};
use crate::errors::ToolError;
use crate::kinds::PermissionKind;

/// Tag changes: `null` clears a field.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TagsPatch {
    /// Album-level fields (`{FIELD: value|null}`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<BTreeMap<String, Option<String>>>,
    /// Track-level fields by file name, as `musaic_album_get` lists them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracks: Option<BTreeMap<String, BTreeMap<String, Option<String>>>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum CoverPatch {
    /// The cover candidate of the stored resolution.
    Resolution,
    /// A public `https` URL.
    Url { url: String },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProposedPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<TagsPatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<CoverPatch>,
    /// Only `true`: rename the files and folder to the template names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rename: Option<bool>,
}

impl ProposedPatch {
    /// The kinds the parts of the patch need.
    pub fn kinds(&self) -> Vec<PermissionKind> {
        let mut kinds = Vec::new();
        if self.tags.is_some() {
            kinds.push(PermissionKind::TagsWrite);
        }
        if self.cover.is_some() {
            kinds.push(PermissionKind::CoversWrite);
        }
        if self.rename == Some(true) {
            kinds.push(PermissionKind::FilesRename);
        }
        kinds
    }

    /// Shape and size checks that need no album: not empty, values bounded,
    /// `rename` only `true`, a URL is `https`. Names and the editable
    /// vocabulary are checked against the album by the app.
    pub fn validate_shape(&self) -> Result<(), ToolError> {
        let mut parts = 0;
        if let Some(t) = &self.tags {
            parts += 1;
            let mut any = false;
            let album = t.album.iter().flatten();
            let tracks = t.tracks.iter().flat_map(|m| m.values().flatten());
            for (field, value) in album.chain(tracks) {
                any = true;
                if field.is_empty() || field.len() > 64 {
                    return Err(ToolError::invalid(
                        "proposedPatch.tags",
                        "field names are 1 to 64 characters",
                    ));
                }
                if value
                    .as_ref()
                    .is_some_and(|v| v.chars().count() > MAX_TAG_VALUE)
                {
                    return Err(ToolError::invalid(
                        "proposedPatch.tags",
                        "values are at most 1000 characters",
                    ));
                }
            }
            if !any {
                return Err(ToolError::invalid("proposedPatch.tags", "holds no change"));
            }
        }
        if let Some(c) = &self.cover {
            parts += 1;
            if let CoverPatch::Url { url } = c
                && (!url.starts_with("https://") || url.len() > 2000)
            {
                return Err(ToolError::invalid(
                    "proposedPatch.cover.url",
                    "must be an https URL",
                ));
            }
        }
        if let Some(r) = self.rename {
            if !r {
                return Err(ToolError::invalid(
                    "proposedPatch.rename",
                    "may only be true",
                ));
            }
            parts += 1;
        }
        if parts == 0 {
            return Err(ToolError::invalid("proposedPatch", "must not be empty"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FixPreviewInput {
    #[schemars(regex(pattern = r"^[0-9a-f]{16}$"))]
    pub album_id: String,
    /// The `revision` from `musaic_album_get`.
    pub base_revision: String,
    pub proposed_patch: ProposedPatch,
}

impl Validate for FixPreviewInput {
    fn validate(&self) -> Result<(), ToolError> {
        check_album_id("albumId", &self.album_id)?;
        if self.base_revision.len() != 32
            || !self.base_revision.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(ToolError::invalid(
                "baseRevision",
                "must be a revision from musaic_album_get",
            ));
        }
        self.proposed_patch.validate_shape()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TagChange {
    /// `album`, or a file name.
    pub scope: String,
    pub field: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NameChange {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FixDiff {
    pub tags: Vec<TagChange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<String>,
    pub files: Vec<NameChange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<NameChange>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    WritesTags,
    WritesCover,
    RenamesFiles,
    RenamesFolder,
    FetchesNetwork,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FixPreviewOutput {
    pub proposal_id: String,
    pub digest: String,
    pub diff: FixDiff,
    pub effects: Vec<Effect>,
    pub kinds: Vec<PermissionKind>,
    pub expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FixApplyInput {
    pub proposal_id: String,
    pub digest: String,
    #[schemars(regex(pattern = r"^[A-Za-z0-9._:-]{8,128}$"))]
    pub idempotency_key: String,
}

impl Validate for FixApplyInput {
    fn validate(&self) -> Result<(), ToolError> {
        if !self.proposal_id.starts_with("pr_") || self.proposal_id.len() > 64 {
            return Err(ToolError::invalid("proposalId", "is not a proposal id"));
        }
        if self.digest.len() != 64 || !self.digest.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ToolError::invalid(
                "digest",
                "must be the digest from musaic_fix_preview",
            ));
        }
        check_idempotency_key(&self.idempotency_key)
    }
}

/// The handle, plus the result once the apply has finished.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FixApplyOutput {
    pub operation_id: String,
    pub state: OperationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<ApplyResult>,
}
