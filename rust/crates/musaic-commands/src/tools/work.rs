//! Versioned, bounded delegation inputs and outputs.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{DEFAULT_LIMIT, Validate, check_idempotency_key, check_limit};
use crate::{ToolError, ToolErrorCode};

pub const SCHEMA_VERSION: u8 = 1;
pub const MAX_RESULT_BYTES: usize = 256 * 1024;

macro_rules! string_enum {
    ($name:ident { $($variant:ident => $value:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
        pub enum $name {
            $(#[serde(rename = $value)] $variant),+
        }
        impl $name {
            pub const fn as_str(self) -> &'static str {
                match self { $( Self::$variant => $value ),+ }
            }
            pub fn parse(value: &str) -> Option<Self> {
                match value { $( $value => Some(Self::$variant), )+ _ => None }
            }
        }
    };
}

string_enum!(WorkKind {
    MetadataIdentify => "metadata.identify",
    MetadataCorrect => "metadata.correct",
    CoverResearch => "cover.research",
});

string_enum!(WorkState {
    Queued => "queued",
    Claimed => "claimed",
    Running => "running",
    AwaitingReview => "awaiting_review",
    Applied => "applied",
    Discarded => "discarded",
    Failed => "failed",
    Cancelled => "cancelled",
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkOutcome {
    Proposal,
    NoMatch,
    NeedsInput,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Evidence {
    pub source: String,
    #[schemars(schema_with = "date_time_schema")]
    pub retrieved_at: String,
    pub claim: String,
    pub limitations: Vec<String>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum MusicBrainzKind {
    ReleaseGroup,
    Release,
    Recording,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MusicBrainzId {
    #[serde(rename = "type")]
    pub kind: MusicBrainzKind,
    #[schemars(regex(
        pattern = r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"
    ))]
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TrackChange {
    pub track_id: String,
    pub fields: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MetadataRecommendation {
    #[schemars(length(max = 100))]
    pub identifiers: Vec<MusicBrainzId>,
    pub album_fields: BTreeMap<String, String>,
    #[schemars(length(max = 500))]
    pub tracks: Vec<TrackChange>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CoverRecommendation {
    pub url: String,
    pub source_page: String,
    #[schemars(range(min = 0))]
    pub width: Option<u32>,
    #[schemars(range(min = 0))]
    pub height: Option<u32>,
    pub rationale: String,
    pub rights_uncertainty: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UnresolvedRecommendation {
    pub explanation: String,
    pub question: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Recommendation {
    Metadata(MetadataRecommendation),
    Cover(CoverRecommendation),
    Unresolved(UnresolvedRecommendation),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkResult {
    #[schemars(range(min = 1, max = 1))]
    pub schema_version: u8,
    pub work_id: String,
    pub kind: WorkKind,
    pub input_digest: String,
    pub outcome: WorkOutcome,
    #[schemars(length(max = 50))]
    pub evidence: Vec<Evidence>,
    #[schemars(range(min = 0, max = 1))]
    pub confidence: f64,
    #[schemars(length(max = 20))]
    pub uncertainties: Vec<String>,
    pub recommendation: Recommendation,
}

fn date_time_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"type": "string", "format": "date-time"})
}

fn bounded(value: &str, max: usize) -> bool {
    !value.trim().is_empty()
        && value.len() <= max
        && !value
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
}

fn hex(value: &str, size: usize) -> bool {
    value.len() == size
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_digit() || (b'a'..=b'f').contains(&b),
        })
}

fn work_id(value: &str) -> Result<(), ToolError> {
    is_uuid(value)
        .then_some(())
        .ok_or_else(|| ToolError::invalid("workId", "must be a work UUID"))
}

fn invalid_result(message: &str) -> ToolError {
    ToolError::new(ToolErrorCode::InvalidResult, message)
}

fn valid_fields(fields: &BTreeMap<String, String>) -> bool {
    fields.len() <= 32
        && fields
            .iter()
            .all(|(key, value)| bounded(key, 64) && bounded(value, 1000))
}

impl Validate for WorkResult {
    fn validate(&self) -> Result<(), ToolError> {
        if self.schema_version != SCHEMA_VERSION
            || !is_uuid(&self.work_id)
            || !hex(&self.input_digest, 64)
            || !(0.0..=1.0).contains(&self.confidence)
            || self.evidence.len() > 50
            || self.uncertainties.len() > 20
            || self.uncertainties.iter().any(|s| !bounded(s, 1000))
            || serde_json::to_vec(self).is_ok_and(|s| s.len() > MAX_RESULT_BYTES)
        {
            return Err(invalid_result(
                "the result exceeds its bounds or uses an unsupported schema",
            ));
        }
        if self.evidence.iter().any(|e| {
            !bounded(&e.source, 2000)
                || !bounded(&e.retrieved_at, 64)
                || time::OffsetDateTime::parse(
                    &e.retrieved_at,
                    &time::format_description::well_known::Rfc3339,
                )
                .is_err()
                || !bounded(&e.claim, 2000)
                || e.limitations.len() > 10
                || e.limitations.iter().any(|s| !bounded(s, 1000))
        }) {
            return Err(invalid_result(
                "an evidence record is incomplete or exceeds its bounds",
            ));
        }
        if self.outcome == WorkOutcome::Proposal && self.evidence.is_empty() {
            return Err(invalid_result("a proposal requires evidence"));
        }
        let valid = match (&self.recommendation, self.kind, self.outcome) {
            (
                Recommendation::Metadata(r),
                WorkKind::MetadataIdentify | WorkKind::MetadataCorrect,
                WorkOutcome::Proposal,
            ) => {
                let mut tracks = BTreeSet::new();
                let mut ids = BTreeSet::new();
                r.identifiers.len() <= 100
                    && r.identifiers
                        .iter()
                        .all(|i| is_uuid(&i.id) && ids.insert((i.kind, &i.id)))
                    && r.tracks.len() <= 500
                    && r.tracks.iter().all(|t| {
                        hex(&t.track_id, 32)
                            && tracks.insert(&t.track_id)
                            && !t.fields.is_empty()
                            && valid_fields(&t.fields)
                    })
                    && valid_fields(&r.album_fields)
                    && (!r.album_fields.is_empty() || !r.tracks.is_empty())
            }
            (Recommendation::Cover(r), WorkKind::CoverResearch, WorkOutcome::Proposal) => {
                r.url.starts_with("https://")
                    && bounded(&r.url, 2000)
                    && r.source_page.starts_with("https://")
                    && bounded(&r.source_page, 2000)
                    && r.width.is_none_or(|v| v > 0)
                    && r.height.is_none_or(|v| v > 0)
                    && bounded(&r.rationale, 2000)
                    && bounded(&r.rights_uncertainty, 1000)
            }
            (Recommendation::Unresolved(r), _, WorkOutcome::NoMatch | WorkOutcome::NeedsInput) => {
                bounded(&r.explanation, 2000) && bounded(&r.question, 1000)
            }
            _ => false,
        };
        valid.then_some(()).ok_or_else(|| {
            invalid_result("the recommendation does not match the work kind or outcome")
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkTrack {
    pub track_id: String,
    pub file_name: String,
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkContext {
    pub schema_version: u8,
    pub album_id: String,
    pub title: String,
    pub artist: String,
    pub hint: Option<String>,
    pub tracks: Vec<WorkTrack>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkListInput {
    pub state: Option<WorkState>,
    pub kind: Option<WorkKind>,
    /// Only the items of this bulk run. An unknown batch lists nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(regex(pattern = r"^[0-9a-f-]{36}$"))]
    pub batch_id: Option<String>,
    pub cursor: Option<String>,
    #[schemars(range(min = 1, max = 100))]
    pub limit: Option<u32>,
}

impl WorkListInput {
    pub fn limit(&self) -> u32 {
        self.limit.unwrap_or(DEFAULT_LIMIT)
    }
}

fn batch_id(value: &Option<String>) -> Result<(), ToolError> {
    match value {
        Some(id) if !is_uuid(id) => Err(ToolError::invalid("batchId", "must be a batch UUID")),
        _ => Ok(()),
    }
}

impl Validate for WorkListInput {
    fn validate(&self) -> Result<(), ToolError> {
        super::check_cursor(&self.cursor)?;
        batch_id(&self.batch_id)?;
        check_limit(self.limit)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkGetInput {
    pub work_id: String,
}
impl Validate for WorkGetInput {
    fn validate(&self) -> Result<(), ToolError> {
        work_id(&self.work_id)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkClaimInput {
    pub work_id: String,
    #[schemars(regex(pattern = r"^[A-Za-z0-9._:-]{8,128}$"))]
    pub idempotency_key: String,
}
impl Validate for WorkClaimInput {
    fn validate(&self) -> Result<(), ToolError> {
        work_id(&self.work_id)?;
        check_idempotency_key(&self.idempotency_key)
    }
}

/// Claim several queued items at once; each claim is independent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkClaimManyInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(regex(pattern = r"^[0-9a-f-]{36}$"))]
    pub batch_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<WorkKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 25))]
    pub max: Option<u32>,
    #[schemars(regex(pattern = r"^[A-Za-z0-9._:-]{8,128}$"))]
    pub idempotency_key: String,
}

impl WorkClaimManyInput {
    /// The most items one call may claim, and the default.
    pub const MAX: u32 = 25;

    pub fn max(&self) -> u32 {
        self.max.unwrap_or(Self::MAX)
    }
}

impl Validate for WorkClaimManyInput {
    fn validate(&self) -> Result<(), ToolError> {
        batch_id(&self.batch_id)?;
        if self.max.is_some_and(|n| !(1..=Self::MAX).contains(&n)) {
            return Err(ToolError::invalid("max", "must be 1 to 25"));
        }
        check_idempotency_key(&self.idempotency_key)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkClaimManyOutput {
    pub claims: Vec<WorkClaimOutput>,
}

#[derive(Clone, Copy)]
pub struct LeaseRef<'a> {
    pub work_id: &'a str,
    pub claim_generation: i64,
    pub lease_token: &'a str,
}

impl Validate for LeaseRef<'_> {
    fn validate(&self) -> Result<(), ToolError> {
        work_id(self.work_id)?;
        if self.claim_generation < 1 {
            return Err(ToolError::invalid("claimGeneration", "must be positive"));
        }
        if !hex(self.lease_token, 64) {
            return Err(ToolError::invalid(
                "leaseToken",
                "must be the token from the claim",
            ));
        }
        Ok(())
    }
}

macro_rules! leased_input {
    ($name:ident { $($(#[$meta:meta])* $field:ident: $ty:ty),* $(,)? }) => {
        #[derive(Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        pub struct $name {
            pub work_id: String,
            #[schemars(range(min = 1))]
            pub claim_generation: i64,
            pub lease_token: String,
            $($(#[$meta])* pub $field: $ty),*
        }
        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_struct(stringify!($name))
                    .field("work_id", &self.work_id)
                    .field("claim_generation", &self.claim_generation)
                    .finish_non_exhaustive()
            }
        }
        impl $name {
            pub fn lease(&self) -> LeaseRef<'_> {
                LeaseRef {
                    work_id: &self.work_id,
                    claim_generation: self.claim_generation,
                    lease_token: &self.lease_token,
                }
            }
        }
    };
}

leased_input!(WorkRenewInput {});
leased_input!(WorkProgressInput {
    #[schemars(range(min = 1))]
    sequence: i64,
    #[schemars(length(min = 1, max = 80))]
    stage: String,
    #[schemars(range(min = 0))]
    completed: Option<i64>,
    #[schemars(range(min = 0))]
    total: Option<i64>,
    #[schemars(length(max = 500))]
    message: Option<String>,
});
leased_input!(WorkCompleteInput {
    #[schemars(regex(pattern = r"^[A-Za-z0-9._:-]{8,128}$"))]
    idempotency_key: String,
    input_digest: String,
    result: WorkResult,
});
leased_input!(WorkFailInput {
    code: String,
    message: String,
    retryable: bool,
});
leased_input!(WorkReleaseInput { reason: String });

/// One held lease, as `musaic_work_renew_many` lists it.
#[derive(Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LeaseInput {
    pub work_id: String,
    #[schemars(range(min = 1))]
    pub claim_generation: i64,
    pub lease_token: String,
}

impl std::fmt::Debug for LeaseInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeaseInput")
            .field("work_id", &self.work_id)
            .field("claim_generation", &self.claim_generation)
            .finish_non_exhaustive()
    }
}

impl LeaseInput {
    pub fn lease(&self) -> LeaseRef<'_> {
        LeaseRef {
            work_id: &self.work_id,
            claim_generation: self.claim_generation,
            lease_token: &self.lease_token,
        }
    }
}

/// Renew several leases; each is renewed or refused on its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkRenewManyInput {
    #[schemars(length(min = 1, max = 25))]
    pub leases: Vec<LeaseInput>,
}

impl WorkRenewManyInput {
    /// The most leases one call may renew.
    pub const MAX: usize = 25;
}

impl Validate for WorkRenewManyInput {
    fn validate(&self) -> Result<(), ToolError> {
        if !(1..=Self::MAX).contains(&self.leases.len()) {
            return Err(ToolError::invalid("leases", "must hold 1 to 25 leases"));
        }
        let mut seen = BTreeSet::new();
        for lease in &self.leases {
            lease.lease().validate()?;
            if !seen.insert(lease.work_id.as_str()) {
                return Err(ToolError::invalid("leases", "each workId must be distinct"));
            }
        }
        Ok(())
    }
}

impl Validate for WorkRenewInput {
    fn validate(&self) -> Result<(), ToolError> {
        self.lease().validate()
    }
}
impl Validate for WorkProgressInput {
    fn validate(&self) -> Result<(), ToolError> {
        self.lease().validate()?;
        if self.sequence < 1
            || !bounded(&self.stage, 80)
            || self.completed.is_some_and(|n| n < 0)
            || self.total.is_some_and(|n| n < 0)
            || self.completed.zip(self.total).is_some_and(|(n, t)| n > t)
            || self.message.as_ref().is_some_and(|s| !bounded(s, 500))
        {
            return Err(ToolError::invalid(
                "progress",
                "use a positive sequence and valid stage, counts, and message",
            ));
        }
        Ok(())
    }
}
impl Validate for WorkCompleteInput {
    fn validate(&self) -> Result<(), ToolError> {
        self.lease().validate()?;
        check_idempotency_key(&self.idempotency_key)?;
        if self.work_id != self.result.work_id || self.input_digest != self.result.input_digest {
            return Err(invalid_result(
                "the result references different work or input",
            ));
        }
        self.result.validate()
    }
}
impl Validate for WorkFailInput {
    fn validate(&self) -> Result<(), ToolError> {
        self.lease().validate()?;
        if !bounded(&self.code, 64) || !bounded(&self.message, 500) {
            return Err(ToolError::invalid(
                "failure",
                "provide a bounded code and message",
            ));
        }
        Ok(())
    }
}
impl Validate for WorkReleaseInput {
    fn validate(&self) -> Result<(), ToolError> {
        self.lease().validate()?;
        bounded(&self.reason, 500)
            .then_some(())
            .ok_or_else(|| ToolError::invalid("reason", "provide a bounded reason"))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkCancelInput {
    pub work_id: String,
    #[schemars(range(min = 1))]
    pub expected_revision: i64,
}
impl Validate for WorkCancelInput {
    fn validate(&self) -> Result<(), ToolError> {
        work_id(&self.work_id)?;
        (self.expected_revision > 0)
            .then_some(())
            .ok_or_else(|| ToolError::invalid("expectedRevision", "must be positive"))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkSummary {
    pub work_id: String,
    pub album_id: String,
    pub kind: WorkKind,
    pub state: WorkState,
    #[schemars(range(min = 0))]
    pub revision: i64,
    #[schemars(range(min = 0))]
    pub attempts: i64,
    #[schemars(range(min = 0))]
    pub max_attempts: i64,
    pub created_at: String,
    pub updated_at: String,
    pub available_at: String,
    pub result_id: Option<String>,
    /// The bulk run that made this request; omitted when none did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkListOutput {
    pub items: Vec<WorkSummary>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkGetOutput {
    pub work: WorkSummary,
    pub schema_version: u8,
    pub library_epoch: String,
    pub base_revision: String,
    pub input_digest: String,
    pub context: WorkContext,
    pub result_schema: String,
}

#[derive(Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkClaimOutput {
    pub work_id: String,
    #[schemars(range(min = 1))]
    pub claim_generation: i64,
    pub lease_token: String,
    pub lease_expires_at: String,
    pub input_digest: String,
}

impl std::fmt::Debug for WorkClaimOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkClaimOutput")
            .field("work_id", &self.work_id)
            .field("claim_generation", &self.claim_generation)
            .field("lease_expires_at", &self.lease_expires_at)
            .field("input_digest", &self.input_digest)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkRenewOutput {
    pub work_id: String,
    pub lease_expires_at: String,
}

/// A lease `musaic_work_renew_many` did not renew, and why.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkRenewRefusal {
    pub work_id: String,
    /// The code `musaic_work_renew` would answer for this lease.
    #[schemars(schema_with = "refusal_code_schema")]
    pub code: ToolErrorCode,
}

/// The derived schema of every error code renders badly in the catalog; a
/// refusal carries one of the lease codes in practice.
fn refusal_code_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "string",
        "description": "An error code of the contract, such as LEASE_EXPIRED, STALE_GENERATION or NOT_FOUND."
    })
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkRenewManyOutput {
    pub renewed: Vec<WorkRenewOutput>,
    pub refused: Vec<WorkRenewRefusal>,
}
