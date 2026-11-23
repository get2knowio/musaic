//! The Events contract (spec 005): the extension ids, the two event types and
//! their schemas, the wire records `events/list` and `events/poll` use, and
//! the generated catalog (`docs/agent-events.{json,md}`). No database type is
//! here; the app turns its history rows into these records.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::errors::{ToolError, ToolErrorCode};
use crate::tools::work::{WorkKind, WorkState};

/// The Skills extension (Beta) of the 2026-07-28 revision.
pub const SKILLS_EXTENSION_ID: &str = "io.modelcontextprotocol/skills";
/// The Events extension (Beta) of the 2026-07-28 revision.
pub const EVENTS_EXTENSION_ID: &str = "io.modelcontextprotocol/events";

/// The extensions the server offers, as `musaic_capabilities` lists them.
/// What a host negotiated is in the desktop diagnostics, not here.
pub const OFFERED_EXTENSIONS: [&str; 3] = [
    crate::tools::task::EXTENSION_ID,
    SKILLS_EXTENSION_ID,
    EVENTS_EXTENSION_ID,
];

/// Work became available: created, requeued, released or retried.
pub const EVENT_REQUESTED: &str = "musaic.work.requested.v1";
/// Any other change of a work item's state.
pub const EVENT_UPDATED: &str = "musaic.work.updated.v1";
pub const EVENT_NAMES: [&str; 2] = [EVENT_REQUESTED, EVENT_UPDATED];

/// Waiting-poll bound of the bridge, in milliseconds.
pub const MAX_WAIT_MS: u32 = 25_000;
pub const DEFAULT_MAX_EVENTS: u32 = 50;
pub const MAX_EVENTS: u32 = 100;
/// The suggested pause between polls, in milliseconds.
pub const NEXT_POLL_MS: u32 = 30_000;
/// Heartbeat interval of a stream: default and the clamp.
pub const HEARTBEAT_DEFAULT_MS: u32 = 30_000;
pub const HEARTBEAT_MIN_MS: u32 = 5_000;
pub const HEARTBEAT_MAX_MS: u32 = 60_000;
/// Concurrent waiting polls per pairing, and streams per process.
pub const MAX_WAITING_POLLS: usize = 4;
pub const MAX_STREAMS: usize = 8;

/// JSON-RPC codes of the Events errors, as the Events specification's Error
/// Handling table assigns them (`ext-events`, `specification/beta/events`).
/// `CallbackEndpointError` (-32027) is webhook-only and never sent here.
pub mod rpc {
    pub const INVALID_PARAMS: i32 = -32602;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const NOT_FOUND: i32 = -32023;
    pub const FORBIDDEN: i32 = -32024;
    pub const RESOURCE_EXHAUSTED: i32 = -32025;
    pub const UNSUPPORTED: i32 = -32026;
}

/// The JSON-RPC code a bridge error maps to; `None` for a non-event error
/// (the adapter reports those as an internal error). A missing pairing is an
/// authorization failure like a revoked one, so a host that is told to stop
/// does not read it as a server fault.
pub fn rpc_code(code: ToolErrorCode) -> Option<i32> {
    match code {
        ToolErrorCode::EventInvalid | ToolErrorCode::InvalidInput => Some(rpc::INVALID_PARAMS),
        ToolErrorCode::EventNotFound => Some(rpc::NOT_FOUND),
        ToolErrorCode::EventForbidden
        | ToolErrorCode::Forbidden
        | ToolErrorCode::PairingRevoked
        | ToolErrorCode::NotPaired => Some(rpc::FORBIDDEN),
        ToolErrorCode::EventLimit => Some(rpc::RESOURCE_EXHAUSTED),
        _ => None,
    }
}

/// `data` of `NotFound` for an unknown or removed event type.
pub fn not_found_data() -> Value {
    json!({"kind": "event"})
}

/// `data` of `ResourceExhausted`: which limit, and its ceiling.
pub fn limit_data(limit: &str, max: usize) -> Value {
    json!({"limit": limit, "max": max})
}

/// The limit a bridge `EVENT_LIMIT` names (the app's only event limit).
pub const LIMIT_WAITING_POLLS: &str = "waitingPolls";
/// The adapter's own limit on open streams.
pub const LIMIT_STREAMS: &str = "streams";

/// `data` of `Unsupported` for `events/subscribe` and `events/unsubscribe`.
pub fn webhook_unsupported_data() -> Value {
    json!({"feature": "deliveryMode", "value": "webhook"})
}

/// The typed `data` the specification attaches to an error of this code.
pub fn rpc_data(code: ToolErrorCode) -> Option<Value> {
    match code {
        ToolErrorCode::EventNotFound => Some(not_found_data()),
        ToolErrorCode::EventLimit => Some(limit_data(LIMIT_WAITING_POLLS, MAX_WAITING_POLLS)),
        _ => None,
    }
}

const KINDS: [&str; 3] = ["metadata.identify", "metadata.correct", "cover.research"];
const STATES: [&str; 8] = [
    "queued",
    "claimed",
    "running",
    "awaiting_review",
    "applied",
    "discarded",
    "failed",
    "cancelled",
];

/// A kind of event, as `events/list` returns it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventDescriptor {
    pub name: String,
    pub description: String,
    pub delivery: Vec<String>,
    pub input_schema: Value,
    pub payload_schema: Value,
}

/// The arguments an event subscription takes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventArguments {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kinds: Option<Vec<WorkKind>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub states: Option<Vec<WorkState>>,
}

/// The only content of an event (references and counters, never text).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkEventData {
    pub work_id: String,
    pub kind: WorkKind,
    pub state: WorkState,
    pub revision: i64,
    pub library_epoch: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventOccurrence {
    pub event_id: String,
    pub name: String,
    /// RFC 3339, UTC.
    pub timestamp: String,
    pub data: WorkEventData,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// What `events/poll` takes. `wait_ms` is the bridge's own and is never part
/// of the MCP request.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PollParams {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<EventArguments>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_age_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_events: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_ms: Option<u32>,
}

impl PollParams {
    pub fn validate(&self) -> Result<(), ToolError> {
        if !EVENT_NAMES.contains(&self.name.as_str()) {
            return Err(ToolError::new(
                ToolErrorCode::EventNotFound,
                "no such event type",
            ));
        }
        if self
            .max_events
            .is_some_and(|n| !(1..=MAX_EVENTS).contains(&n))
        {
            return Err(ToolError::new(
                ToolErrorCode::EventInvalid,
                "maxEvents must be 1 to 100",
            ));
        }
        if self.wait_ms.is_some_and(|n| n > MAX_WAIT_MS) {
            return Err(ToolError::new(
                ToolErrorCode::EventInvalid,
                "waitMs must be 0 to 25000",
            ));
        }
        if self.cursor.as_ref().is_some_and(|c| c.len() > 512) {
            return Err(ToolError::new(
                ToolErrorCode::EventInvalid,
                "the cursor is too long",
            ));
        }
        Ok(())
    }

    pub fn max_events(&self) -> u32 {
        self.max_events.unwrap_or(DEFAULT_MAX_EVENTS)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PollResult {
    pub events: Vec<EventOccurrence>,
    pub cursor: String,
    pub truncated: bool,
    pub has_more: bool,
    pub next_poll_ms: u32,
}

/// What `events/list` returns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListResult {
    pub events: Vec<EventDescriptor>,
}

/// What a host told the stdio process: its name and version as reported, the
/// profile it speaks and which extensions it declared or used.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostObservation {
    #[serde(default)]
    pub client_name: String,
    #[serde(default)]
    pub client_version: String,
    #[serde(default)]
    pub profile: String,
    #[serde(default)]
    pub tasks: bool,
    #[serde(default)]
    pub skills: bool,
    #[serde(default)]
    pub events: bool,
}

/// What `GET session` returns to the CLI's `doctor`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<HostObservation>,
    pub skill_version: String,
    pub subscription_active: bool,
}

fn input_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "kinds": {"type": "array", "items": {"enum": KINDS}},
            "states": {"type": "array", "items": {"enum": STATES}}
        }
    })
}

fn payload_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["workId", "kind", "state", "revision", "libraryEpoch"],
        "properties": {
            "workId": {"type": "string"},
            "kind": {"enum": KINDS},
            "state": {"enum": STATES},
            "revision": {"type": "integer"},
            "libraryEpoch": {"type": "string"}
        }
    })
}

/// The two event types. `push` is listed only when the adapter can stream.
pub fn descriptors(push: bool) -> Vec<EventDescriptor> {
    let delivery: Vec<String> = if push {
        vec!["poll".into(), "push".into()]
    } else {
        vec!["poll".into()]
    };
    [
        (
            EVENT_REQUESTED,
            "Work became available to claim: it was created, requeued, released or retried.",
        ),
        (
            EVENT_UPDATED,
            "Delegated work changed state in any other way (claimed, running, awaiting review, applied, discarded, failed, cancelled).",
        ),
    ]
    .into_iter()
    .map(|(name, description)| EventDescriptor {
        name: name.into(),
        description: description.into(),
        delivery: delivery.clone(),
        input_schema: input_schema(),
        payload_schema: payload_schema(),
    })
    .collect()
}

/// Whether the adapter streams (`events/stream`). Recorded in one place so
/// the catalog, `events/list` and the docs agree; see research D7.
pub const STREAM_SUPPORTED: bool = true;

/// `docs/agent-events.json`: deterministic, trailing newline.
pub fn events_catalog_json() -> String {
    let doc = json!({
        "extension": EVENTS_EXTENSION_ID,
        "revision": "2026-07-28",
        "events": descriptors(STREAM_SUPPORTED),
        "errors": [
            {"name": "InvalidParams", "code": rpc::INVALID_PARAMS},
            {"name": "NotFound", "code": rpc::NOT_FOUND, "data": not_found_data()},
            {"name": "Forbidden", "code": rpc::FORBIDDEN},
            {"name": "ResourceExhausted", "code": rpc::RESOURCE_EXHAUSTED, "data": [
                limit_data(LIMIT_WAITING_POLLS, MAX_WAITING_POLLS),
                limit_data(LIMIT_STREAMS, MAX_STREAMS),
            ]},
            {"name": "Unsupported", "code": rpc::UNSUPPORTED, "data": webhook_unsupported_data()},
        ],
        "limits": {
            "maxEvents": MAX_EVENTS,
            "defaultMaxEvents": DEFAULT_MAX_EVENTS,
            "waitingPollsPerPairing": MAX_WAITING_POLLS,
            "streamsPerProcess": MAX_STREAMS,
            "heartbeatMs": {"default": HEARTBEAT_DEFAULT_MS, "min": HEARTBEAT_MIN_MS, "max": HEARTBEAT_MAX_MS},
            "retentionDays": 7,
        },
    });
    let mut s = serde_json::to_string_pretty(&doc).expect("catalog is JSON");
    s.push('\n');
    s
}

/// `docs/agent-events.md`, also served as the skill's `references/events.md`.
pub fn events_catalog_markdown() -> String {
    let mut out = String::from(
        "# Musaic events\n\nGenerated from the event catalog; do not edit by hand.\n\n\
         Extension `io.modelcontextprotocol/events` on protocol revision `2026-07-28`. \
         An event says that work changed; read the work with `musaic_work_get`. \
         Delivery is at least once: deduplicate by `eventId` and rely on the atomic claim.\n\n",
    );
    for d in descriptors(STREAM_SUPPORTED) {
        out.push_str(&format!("## `{}`\n\n{}\n\n", d.name, d.description));
        out.push_str(&format!("Delivery: {}.\n\n", d.delivery.join(", ")));
        out.push_str("`data` holds exactly:\n\n");
        out.push_str("- `workId` (string)\n");
        out.push_str(&format!("- `kind` (one of {})\n", list(&KINDS)));
        out.push_str(&format!("- `state` (one of {})\n", list(&STATES)));
        out.push_str("- `revision` (integer)\n- `libraryEpoch` (string)\n\n");
        out.push_str("Arguments (all optional): `kinds`, `states`; any other key is refused.\n\n");
    }
    out.push_str("## Polling\n\n");
    out.push_str(&format!(
        "`events/poll` takes `name`, optional `arguments`, `cursor`, `maxAgeMs` and `maxEvents` (1 to {MAX_EVENTS}, default {DEFAULT_MAX_EVENTS}). \
         A missing cursor returns no events and the current cursor. A cursor that is unusable (undecodable, from another library, older than the 7 days kept, or older than `maxAgeMs`) returns `truncated: true`, no events and a fresh cursor: read the work list once, then continue from that cursor. \
         The cursor is at the response level (events in a poll carry none). `nextPollMs` suggests the next poll; with `hasMore: true`, poll again at once.\n\n",
    ));
    out.push_str("## Streaming\n\n");
    out.push_str(&format!(
        "`events/stream` sends `notifications/events/active`, then `event`, `heartbeat` (granted `heartbeatMs`, {HEARTBEAT_MIN_MS} to {HEARTBEAT_MAX_MS}, default {HEARTBEAT_DEFAULT_MS}) and, on a revoked pairing, `terminated`. A cancelled request stops delivery. A host that cannot hold a stream polls instead.\n\n"
    ));
    out.push_str("## Errors\n\n| Name | Code | When |\n| --- | --- | --- |\n");
    for (name, code, when) in [
        (
            "InvalidParams",
            rpc::INVALID_PARAMS,
            "arguments do not match the schema, or a bad `maxEvents`",
        ),
        (
            "NotFound",
            rpc::NOT_FOUND,
            "an unknown event name; `data`: `{\"kind\": \"event\"}`",
        ),
        (
            "Forbidden",
            rpc::FORBIDDEN,
            "the pairing is missing, revoked or may not read work",
        ),
        (
            "ResourceExhausted",
            rpc::RESOURCE_EXHAUSTED,
            "more than 4 waiting polls for the pairing (`data.limit`: `waitingPolls`), or 8 streams in the process (`streams`); `data.max` is the ceiling",
        ),
        (
            "Unsupported",
            rpc::UNSUPPORTED,
            "`events/subscribe` and `events/unsubscribe` (webhook delivery is not offered); `data`: `{\"feature\": \"deliveryMode\", \"value\": \"webhook\"}`",
        ),
    ] {
        out.push_str(&format!("| {name} | {code} | {when} |\n"));
    }
    out
}

fn list(items: &[&str]) -> String {
    items
        .iter()
        .map(|i| format!("`{i}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_has_exactly_the_five_required_keys() {
        let schema = payload_schema();
        assert_eq!(schema["additionalProperties"], false);
        let required: Vec<&str> = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(
            required,
            ["workId", "kind", "state", "revision", "libraryEpoch"]
        );
        let props: Vec<&String> = schema["properties"].as_object().unwrap().keys().collect();
        assert_eq!(props.len(), 5);
        let data = WorkEventData {
            work_id: "w".into(),
            kind: WorkKind::CoverResearch,
            state: WorkState::Queued,
            revision: 1,
            library_epoch: "e".into(),
        };
        let keys: std::collections::BTreeSet<_> = serde_json::to_value(&data)
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        let want: std::collections::BTreeSet<_> = required.iter().map(|s| s.to_string()).collect();
        assert_eq!(keys, want);
    }

    #[test]
    fn enums_in_the_schemas_match_the_work_enums() {
        for k in KINDS {
            assert!(WorkKind::parse(k).is_some(), "{k}");
        }
        for s in STATES {
            assert!(WorkState::parse(s).is_some(), "{s}");
        }
        assert_eq!(STATES.len(), 8);
    }

    #[test]
    fn input_schema_refuses_unknown_arguments() {
        assert_eq!(input_schema()["additionalProperties"], false);
        let bad = json!({"kinds": ["cover.research"], "other": 1});
        assert!(serde_json::from_value::<EventArguments>(bad).is_err());
        assert!(serde_json::from_value::<EventArguments>(json!({"states": ["queued"]})).is_ok());
    }

    #[test]
    fn two_types_poll_and_push_or_poll_only() {
        let d = descriptors(true);
        assert_eq!(
            d.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            EVENT_NAMES
        );
        assert!(d.iter().all(|e| e.delivery == ["poll", "push"]));
        assert!(descriptors(false).iter().all(|e| e.delivery == ["poll"]));
    }

    #[test]
    fn poll_params_are_bounded() {
        let ok = PollParams {
            name: EVENT_REQUESTED.into(),
            ..Default::default()
        };
        assert!(ok.validate().is_ok());
        assert_eq!(ok.max_events(), 50);
        for bad in [
            PollParams {
                max_events: Some(0),
                ..ok.clone()
            },
            PollParams {
                max_events: Some(101),
                ..ok.clone()
            },
            PollParams {
                wait_ms: Some(25_001),
                ..ok.clone()
            },
        ] {
            assert_eq!(
                bad.validate().unwrap_err().code,
                ToolErrorCode::EventInvalid
            );
        }
        let unknown = PollParams {
            name: "nope".into(),
            ..ok
        };
        assert_eq!(
            unknown.validate().unwrap_err().code,
            ToolErrorCode::EventNotFound
        );
    }

    #[test]
    fn the_error_codes_map_to_the_documented_numbers() {
        // The numbers of the Events specification's Error Handling table.
        assert_eq!(rpc_code(ToolErrorCode::EventNotFound), Some(-32023));
        assert_eq!(rpc_code(ToolErrorCode::EventForbidden), Some(-32024));
        assert_eq!(rpc_code(ToolErrorCode::NotPaired), Some(-32024));
        assert_eq!(rpc_code(ToolErrorCode::PairingRevoked), Some(-32024));
        assert_eq!(rpc_code(ToolErrorCode::EventLimit), Some(-32025));
        assert_eq!(rpc::UNSUPPORTED, -32026);
        assert_eq!(rpc_code(ToolErrorCode::EventInvalid), Some(-32602));
        assert_eq!(rpc_code(ToolErrorCode::Internal), None);
    }

    #[test]
    fn the_error_data_has_the_specified_discriminators() {
        assert_eq!(
            rpc_data(ToolErrorCode::EventNotFound),
            Some(json!({"kind": "event"}))
        );
        assert_eq!(
            rpc_data(ToolErrorCode::EventLimit),
            Some(json!({"limit": "waitingPolls", "max": 4}))
        );
        assert_eq!(rpc_data(ToolErrorCode::EventForbidden), None);
        assert_eq!(
            webhook_unsupported_data(),
            json!({"feature": "deliveryMode", "value": "webhook"})
        );
    }

    #[test]
    fn catalogs_are_deterministic_and_name_both_events() {
        assert_eq!(events_catalog_json(), events_catalog_json());
        let md = events_catalog_markdown();
        assert!(md.contains(EVENT_REQUESTED) && md.contains(EVENT_UPDATED));
        assert!(events_catalog_json().ends_with('\n'));
    }
}
