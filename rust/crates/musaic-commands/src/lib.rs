//! The agent tool contract. A leaf crate: the app (which serves the tools),
//! the MCP adapter and the bundled CLI (which calls them) all use these types,
//! and none of this knows a database, an HTTP stack or an async runtime.

pub mod auth;
pub mod call;
pub mod catalog;
pub mod descriptor;
pub mod errors;
pub mod events;
pub mod kinds;
pub mod skill;
pub mod tools;
#[cfg(any(windows, test))]
pub mod windows_acl;

pub use call::{ToolCall, ToolOutput, parse_call};
pub use errors::{ToolError, ToolErrorCode};
pub use kinds::{Decision, PermissionKind, decide};
pub use tools::Validate;

use serde_json::Value;

/// The contract version reported by `musaic_capabilities`.
pub const CONTRACT_VERSION: &str = "1";

/// The MCP profiles the server speaks (the descriptor lists them too).
pub const PROTOCOL_PROFILES: [&str; 2] = ["mcp-2026-07-28", "mcp-2025-11-25"];

/// MCP tool annotations of a tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Annotations {
    pub read_only: bool,
    pub destructive: bool,
    pub idempotent: bool,
    pub open_world: bool,
}

const fn ann(
    read_only: bool,
    destructive: bool,
    idempotent: bool,
    open_world: bool,
) -> Annotations {
    Annotations {
        read_only,
        destructive,
        idempotent,
        open_world,
    }
}

/// One tool of the registry.
#[derive(Clone, Copy)]
pub struct ToolDef {
    pub id: &'static str,
    pub cli_name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    /// The kinds the tool needs. `musaic_fix_apply` needs the kinds of its
    /// proposal; the list here is the union it may need.
    pub kinds: &'static [PermissionKind],
    pub annotations: Annotations,
    pub input_schema: fn() -> Value,
    pub output_schema: fn() -> Value,
}

impl std::fmt::Debug for ToolDef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolDef").field("id", &self.id).finish()
    }
}

fn schema<T: schemars::JsonSchema>() -> Value {
    let mut v = serde_json::to_value(schemars::schema_for!(T)).expect("a schema is JSON");
    standard_formats_only(&mut v);
    v
}

/// The `format` values JSON Schema 2020-12 defines; any other is dropped.
pub const STANDARD_FORMATS: [&str; 19] = [
    "date-time",
    "date",
    "time",
    "duration",
    "email",
    "idn-email",
    "hostname",
    "idn-hostname",
    "ipv4",
    "ipv6",
    "uri",
    "uri-reference",
    "iri",
    "iri-reference",
    "uuid",
    "uri-template",
    "json-pointer",
    "relative-json-pointer",
    "regex",
];

/// schemars annotates numbers with their Rust width (`uint32`, `double`, …),
/// which is not a 2020-12 format and which MCP hosts warn about. Drop every
/// non-standard `format`; an unsigned width keeps its lower bound as
/// `minimum: 0` (schemars adds it, but a field without one gets it here).
fn standard_formats_only(v: &mut Value) {
    match v {
        Value::Object(m) => {
            if let Some(Value::String(f)) = m.get("format")
                && !STANDARD_FORMATS.contains(&f.as_str())
            {
                let unsigned = f.starts_with("uint");
                m.remove("format");
                if unsigned && !m.contains_key("minimum") && !m.contains_key("exclusiveMinimum") {
                    m.insert("minimum".into(), Value::from(0));
                }
            }
            m.values_mut().for_each(standard_formats_only);
        }
        Value::Array(a) => a.iter_mut().for_each(standard_formats_only),
        _ => {}
    }
}

use PermissionKind::*;
use tools::*;

/// The tools, in catalog order.
pub static REGISTRY: [ToolDef; 21] = [
    ToolDef {
        id: "musaic_capabilities",
        cli_name: "capabilities",
        title: "Capabilities",
        description: "What this pairing may do: its scopes and policy, the contract version, the registry digest and the configured library roots. Start here.",
        kinds: &[LibraryRead],
        annotations: ann(true, false, true, false),
        input_schema: schema::<capabilities::CapabilitiesInput>,
        output_schema: schema::<capabilities::CapabilitiesOutput>,
    },
    ToolDef {
        id: "musaic_library_list",
        cli_name: "library-list",
        title: "List albums",
        description: "Page through the albums of the library, optionally filtered by a text query on the artist or title. Returns ids and display fields, never paths.",
        kinds: &[LibraryRead],
        annotations: ann(true, false, true, false),
        input_schema: schema::<library::LibraryListInput>,
        output_schema: schema::<library::LibraryListOutput>,
    },
    ToolDef {
        id: "musaic_album_get",
        cli_name: "album-get",
        title: "Get an album",
        description: "One album: its tags, its tracks (file names and tags) and its revision, which a fix proposal is bound to.",
        kinds: &[LibraryRead],
        annotations: ann(true, false, true, false),
        input_schema: schema::<album::AlbumGetInput>,
        output_schema: schema::<album::AlbumGetOutput>,
    },
    ToolDef {
        id: "musaic_resolution_get",
        cli_name: "resolution-get",
        title: "Get the stored resolution",
        description: "The metadata resolution already stored for an album. It never starts a lookup; with nothing stored it answers not_resolved.",
        kinds: &[LibraryRead],
        annotations: ann(true, false, true, false),
        input_schema: schema::<resolution::ResolutionGetInput>,
        output_schema: schema::<resolution::ResolutionGetOutput>,
    },
    ToolDef {
        id: "musaic_findings_list",
        cli_name: "findings-list",
        title: "List findings",
        description: "Open health-check findings, by reference (check, album, severity, title), optionally for one album.",
        kinds: &[LibraryRead],
        annotations: ann(true, false, true, false),
        input_schema: schema::<findings::FindingsListInput>,
        output_schema: schema::<findings::FindingsListOutput>,
    },
    ToolDef {
        id: "musaic_fix_preview",
        cli_name: "fix-preview",
        title: "Preview a fix",
        description: "Validate a proposed change to one album's tags, cover or file names against its current revision and store it as a proposal. Writes nothing to the collection.",
        kinds: &[ProposalSubmit],
        annotations: ann(false, false, false, false),
        input_schema: schema::<fix::FixPreviewInput>,
        output_schema: schema::<fix::FixPreviewOutput>,
    },
    ToolDef {
        id: "musaic_fix_apply",
        cli_name: "fix-apply",
        title: "Apply a fix",
        description: "Apply a previewed proposal. Depending on the pairing's policy the change runs now or waits for the user to approve it in Musaic; poll musaic_operation_get.",
        kinds: &[TagsWrite, CoversWrite, FilesRename],
        annotations: ann(false, true, true, true),
        input_schema: schema::<fix::FixApplyInput>,
        output_schema: schema::<fix::FixApplyOutput>,
    },
    ToolDef {
        id: "musaic_scan_start",
        cli_name: "scan-start",
        title: "Start a library scan",
        description: "Scan the configured library folder. Returns a handle; if a scan is already running the handle follows it.",
        kinds: &[ComputeScan],
        annotations: ann(false, false, true, false),
        input_schema: schema::<scan::ScanStartInput>,
        output_schema: schema::<operation::OperationHandle>,
    },
    ToolDef {
        id: "musaic_checks_start",
        cli_name: "checks-start",
        title: "Start health checks",
        description: "Run health checks over some or all albums. Findings are written; no file is touched. Returns a handle.",
        kinds: &[ComputeScan],
        annotations: ann(false, false, true, true),
        input_schema: schema::<checks::ChecksStartInput>,
        output_schema: schema::<operation::OperationHandle>,
    },
    ToolDef {
        id: "musaic_analysis_start",
        cli_name: "analysis-start",
        title: "Start analysis",
        description: "Measure BPM or ReplayGain for some albums and write the result to their tags. Returns a handle.",
        kinds: &[ComputeAnalyze, TagsWrite],
        annotations: ann(false, true, true, false),
        input_schema: schema::<analysis::AnalysisStartInput>,
        output_schema: schema::<operation::OperationHandle>,
    },
    ToolDef {
        id: "musaic_operation_get",
        cli_name: "operation-get",
        title: "Get an operation",
        description: "The state, counts and result of an operation this pairing started.",
        kinds: &[LibraryRead],
        annotations: ann(true, false, true, false),
        input_schema: schema::<operation::OperationGetInput>,
        output_schema: schema::<operation::OperationGetOutput>,
    },
    ToolDef {
        id: "musaic_operation_cancel",
        cli_name: "operation-cancel",
        title: "Cancel an operation",
        description: "Ask an operation this pairing started to stop. Honoured between items, and for an apply only while it waits for approval.",
        kinds: &[LibraryRead],
        annotations: ann(false, false, true, false),
        input_schema: schema::<operation::OperationCancelInput>,
        output_schema: schema::<operation::OperationCancelOutput>,
    },
    ToolDef {
        id: "musaic_work_list",
        cli_name: "work-list",
        title: "List delegated work",
        description: "List available research requests and your claimed work.",
        kinds: &[LibraryRead],
        annotations: ann(true, false, true, false),
        input_schema: schema::<work::WorkListInput>,
        output_schema: schema::<work::WorkListOutput>,
    },
    ToolDef {
        id: "musaic_work_get",
        cli_name: "work-get",
        title: "Get delegated work",
        description: "Read the immutable album context and input digest for a research request.",
        kinds: &[LibraryRead],
        annotations: ann(true, false, true, false),
        input_schema: schema::<work::WorkGetInput>,
        output_schema: schema::<work::WorkGetOutput>,
    },
    ToolDef {
        id: "musaic_work_claim",
        cli_name: "work-claim",
        title: "Claim delegated work",
        description: "Reserve this request for five minutes using an idempotency key.",
        kinds: &[LibraryRead, ProposalSubmit],
        annotations: ann(false, false, true, false),
        input_schema: schema::<work::WorkClaimInput>,
        output_schema: schema::<work::WorkClaimOutput>,
    },
    ToolDef {
        id: "musaic_work_renew",
        cli_name: "work-renew",
        title: "Renew a work lease",
        description: "Extend the expiry of your current claim. Renew once a minute while you work.",
        kinds: &[LibraryRead, ProposalSubmit],
        annotations: ann(false, false, false, false),
        input_schema: schema::<work::WorkRenewInput>,
        output_schema: schema::<work::WorkRenewOutput>,
    },
    ToolDef {
        id: "musaic_work_progress",
        cli_name: "work-progress",
        title: "Report work progress",
        description: "Update the current attempt using a monotonically increasing sequence.",
        kinds: &[LibraryRead, ProposalSubmit],
        annotations: ann(false, false, true, false),
        input_schema: schema::<work::WorkProgressInput>,
        output_schema: schema::<work::WorkSummary>,
    },
    ToolDef {
        id: "musaic_work_complete",
        cli_name: "work-complete",
        title: "Submit a research result",
        description: "Submit bounded evidence and a recommendation for desktop review.",
        kinds: &[LibraryRead, ProposalSubmit],
        annotations: ann(false, false, true, true),
        input_schema: schema::<work::WorkCompleteInput>,
        output_schema: schema::<work::WorkSummary>,
    },
    ToolDef {
        id: "musaic_work_fail",
        cli_name: "work-fail",
        title: "Report failed research",
        description: "End this attempt and allow Musaic to decide whether to retry.",
        kinds: &[LibraryRead, ProposalSubmit],
        annotations: ann(false, false, false, false),
        input_schema: schema::<work::WorkFailInput>,
        output_schema: schema::<work::WorkSummary>,
    },
    ToolDef {
        id: "musaic_work_release",
        cli_name: "work-release",
        title: "Release delegated work",
        description: "Give your current claim back without submitting a result.",
        kinds: &[LibraryRead, ProposalSubmit],
        annotations: ann(false, false, false, false),
        input_schema: schema::<work::WorkReleaseInput>,
        output_schema: schema::<work::WorkSummary>,
    },
    ToolDef {
        id: "musaic_work_cancel",
        cli_name: "work-cancel",
        title: "Cancel delegated work",
        description: "Cancel work by revision when explicitly allowed by the pairing policy.",
        kinds: &[LibraryRead, WorkCancel],
        annotations: ann(false, false, true, false),
        input_schema: schema::<work::WorkCancelInput>,
        output_schema: schema::<work::WorkSummary>,
    },
];

/// What the contract deliberately does not offer (FR-005), stated in the
/// catalog.
pub const NOT_OFFERED: [&str; 9] = [
    "raw SQL",
    "shell execution",
    "arbitrary filesystem paths",
    "reading or writing keys and secrets",
    "trash or delete",
    "database restore or backup download",
    "settings changes",
    "setting or changing agent permissions and policies, including approving its own requests",
    "undoing an applied change (only the person at the desktop can)",
];

/// The tool with this id.
pub fn find_tool(id: &str) -> Option<&'static ToolDef> {
    REGISTRY.iter().find(|t| t.id == id)
}

/// The tool with this CLI name.
pub fn find_by_cli_name(name: &str) -> Option<&'static ToolDef> {
    REGISTRY.iter().find(|t| t.cli_name == name)
}
