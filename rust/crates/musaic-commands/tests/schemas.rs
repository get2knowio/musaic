//! The registry's schemas, bounds and annotations (T012, FR-006).

use musaic_commands::{PermissionKind, REGISTRY, ToolErrorCode, parse_call};
use serde_json::{Value, json};

fn walk(v: &Value, f: &mut dyn FnMut(&Value)) {
    f(v);
    match v {
        Value::Object(m) => m.values().for_each(|c| walk(c, f)),
        Value::Array(a) => a.iter().for_each(|c| walk(c, f)),
        _ => {}
    }
}

#[test]
fn registry_has_twenty_three_tools_with_unique_names() {
    assert_eq!(REGISTRY.len(), 23);
    let mut ids = std::collections::BTreeSet::new();
    let mut cli = std::collections::BTreeSet::new();
    for t in &REGISTRY {
        assert!(ids.insert(t.id));
        assert!(cli.insert(t.cli_name));
        assert_eq!(
            t.cli_name,
            t.id.strip_prefix("musaic_").unwrap().replace('_', "-")
        );
    }
}

#[test]
fn inputs_forbid_extra_properties_and_outputs_are_objects() {
    for t in &REGISTRY {
        let input = (t.input_schema)();
        assert_eq!(input["type"], "object", "{}", t.id);
        assert_eq!(
            input["additionalProperties"],
            json!(false),
            "{} input",
            t.id
        );
        let output = (t.output_schema)();
        assert_eq!(output["type"], "object", "{} output", t.id);
        // Every object schema with named properties in an input denies extras.
        walk(&input, &mut |v| {
            if let Some(m) = v.as_object()
                && m.contains_key("properties")
                && m.get("type") == Some(&json!("object"))
            {
                assert_eq!(
                    m.get("additionalProperties"),
                    Some(&json!(false)),
                    "{}: {v}",
                    t.id
                );
            }
        });
    }
}

#[test]
fn annotations_match_kinds() {
    for t in &REGISTRY {
        let only_read = t.kinds == [PermissionKind::LibraryRead];
        let expected = only_read && t.id != "musaic_operation_cancel";
        assert_eq!(t.annotations.read_only, expected, "{}", t.id);
        if t.annotations.read_only {
            assert!(!t.annotations.destructive, "{}", t.id);
        }
    }
}

fn code(tool: &str, body: Value) -> ToolErrorCode {
    parse_call(tool, body.to_string().as_bytes())
        .unwrap_err()
        .code
}

#[test]
fn unknown_fields_are_rejected_for_every_tool() {
    let valid = json!({});
    for t in &REGISTRY {
        let mut body = valid.clone();
        body["surprise"] = json!(1);
        assert_eq!(code(t.id, body), ToolErrorCode::InvalidInput, "{}", t.id);
    }
    assert_eq!(
        code(
            "musaic_fix_preview",
            json!({
                "albumId": "0123456789abcdef",
                "baseRevision": "0123456789abcdef0123456789abcdef",
                "proposedPatch": {"rename": true, "shell": "rm"}
            })
        ),
        ToolErrorCode::InvalidInput
    );
}

#[test]
fn bounds_are_enforced_with_the_field_name() {
    let id = "0123456789abcdef";
    let key = "abcdefgh12";
    let cases: Vec<(&str, Value, &str)> = vec![
        ("musaic_library_list", json!({"limit": 0}), "limit"),
        ("musaic_library_list", json!({"limit": 101}), "limit"),
        (
            "musaic_library_list",
            json!({"query": "x".repeat(201)}),
            "query",
        ),
        (
            "musaic_library_list",
            json!({"cursor": "x".repeat(513)}),
            "cursor",
        ),
        (
            "musaic_album_get",
            json!({"albumId": "ABCDEF0123456789"}),
            "albumId",
        ),
        ("musaic_album_get", json!({"albumId": "short"}), "albumId"),
        ("musaic_findings_list", json!({"limit": 500}), "limit"),
        (
            "musaic_scan_start",
            json!({"configuredRootId": id, "idempotencyKey": "short"}),
            "idempotencyKey",
        ),
        (
            "musaic_scan_start",
            json!({"configuredRootId": id, "idempotencyKey": "has space ok1"}),
            "idempotencyKey",
        ),
        (
            "musaic_scan_start",
            json!({"configuredRootId": id, "idempotencyKey": "x".repeat(129)}),
            "idempotencyKey",
        ),
        (
            "musaic_checks_start",
            json!({"albumIds": vec![id; 501], "idempotencyKey": key}),
            "albumIds",
        ),
        (
            "musaic_checks_start",
            json!({"checkIds": vec!["a"; 21], "idempotencyKey": key}),
            "checkIds",
        ),
        (
            "musaic_analysis_start",
            json!({"albumIds": [], "kinds": ["bpm"], "idempotencyKey": key}),
            "albumIds",
        ),
        (
            "musaic_analysis_start",
            json!({"albumIds": [id], "kinds": [], "idempotencyKey": key}),
            "kinds",
        ),
        (
            "musaic_fix_preview",
            json!({"albumId": id, "baseRevision": "0123456789abcdef0123456789abcdef", "proposedPatch": {}}),
            "proposedPatch",
        ),
        (
            "musaic_fix_preview",
            json!({"albumId": id, "baseRevision": "0123456789abcdef0123456789abcdef", "proposedPatch": {"rename": false}}),
            "proposedPatch.rename",
        ),
        (
            "musaic_fix_preview",
            json!({"albumId": id, "baseRevision": "0123456789abcdef0123456789abcdef", "proposedPatch": {"tags": {"album": {"TITLE": "x".repeat(1001)}}}}),
            "proposedPatch.tags",
        ),
        (
            "musaic_fix_preview",
            json!({"albumId": id, "baseRevision": "0123456789abcdef0123456789abcdef", "proposedPatch": {"cover": {"source": "url", "url": "http://x/y.jpg"}}}),
            "proposedPatch.cover.url",
        ),
        (
            "musaic_fix_preview",
            json!({"albumId": id, "baseRevision": "zz", "proposedPatch": {"rename": true}}),
            "baseRevision",
        ),
    ];
    for (tool, body, field) in cases {
        let err = parse_call(tool, body.to_string().as_bytes()).unwrap_err();
        assert_eq!(err.code, ToolErrorCode::InvalidInput, "{tool} {field}");
        assert_eq!(
            err.details
                .as_ref()
                .and_then(|d| d.get("field"))
                .map(String::as_str),
            Some(field),
            "{tool}"
        );
    }
    // The upper bounds themselves are accepted.
    assert!(
        parse_call(
            "musaic_library_list",
            json!({"limit": 100, "query": "x".repeat(200)})
                .to_string()
                .as_bytes()
        )
        .is_ok()
    );
    assert!(
        parse_call(
            "musaic_checks_start",
            json!({"albumIds": vec![id; 500], "idempotencyKey": "x".repeat(128)})
                .to_string()
                .as_bytes()
        )
        .is_ok()
    );
    // A null clears a tag.
    assert!(parse_call("musaic_fix_preview", json!({"albumId": id, "baseRevision": "0123456789abcdef0123456789abcdef", "proposedPatch": {"tags": {"album": {"GENRE": null}}}}).to_string().as_bytes()).is_ok());
}

#[test]
fn schema_validators_agree_with_the_typed_validation() {
    // A body that fails the generated schema must also fail parse_call.
    let bad = json!({"limit": 1000});
    let schema = (REGISTRY[1].input_schema)();
    let validator = jsonschema::validator_for(&schema).unwrap();
    assert!(!validator.is_valid(&bad));
    assert!(parse_call("musaic_library_list", bad.to_string().as_bytes()).is_err());
    let good = json!({"limit": 5});
    assert!(validator.is_valid(&good));
}

#[test]
fn the_catalog_states_what_is_not_offered() {
    let md = musaic_commands::catalog::catalog_markdown();
    for w in [
        "raw SQL",
        "shell execution",
        "trash or delete",
        "approving its own requests",
    ] {
        assert!(md.contains(w), "{w}");
    }
    assert!(
        !REGISTRY
            .iter()
            .any(|t| t.id.contains("approve") || t.id.contains("policy"))
    );
}

/// MCP hosts warn on a `format` JSON Schema 2020-12 does not define, such as
/// the integer widths schemars emits (#356).
#[test]
fn schemas_use_only_standard_formats() {
    for t in &REGISTRY {
        for (side, s) in [
            ("input", (t.input_schema)()),
            ("output", (t.output_schema)()),
        ] {
            walk(&s, &mut |v| {
                if let Some(Value::String(f)) = v.get("format") {
                    assert!(
                        musaic_commands::STANDARD_FORMATS.contains(&f.as_str()),
                        "{} {side}: format {f:?} in {v}",
                        t.id
                    );
                }
            });
        }
    }
}

fn is_integer(v: &Value) -> bool {
    match v.get("type") {
        Some(Value::String(s)) => s == "integer",
        Some(Value::Array(a)) => a.contains(&json!("integer")),
        _ => false,
    }
}

/// Dropping `uint32` must not lose the bound: every integer in the contract
/// is unsigned, so each carries a non-negative `minimum`.
#[test]
fn unsigned_integers_keep_a_lower_bound() {
    let mut seen = 0;
    for t in &REGISTRY {
        for (side, s) in [
            ("input", (t.input_schema)()),
            ("output", (t.output_schema)()),
        ] {
            walk(&s, &mut |v| {
                if is_integer(v) {
                    seen += 1;
                    let min = v.get("minimum").and_then(Value::as_f64);
                    assert!(min.is_some_and(|m| m >= 0.0), "{} {side}: {v}", t.id);
                }
            });
        }
    }
    assert!(seen > 0);
    assert_eq!(REGISTRY[1].id, "musaic_library_list");
    let out = (REGISTRY[1].output_schema)();
    assert_eq!(out["properties"]["total"]["minimum"], json!(0));
    let mut track_count = false;
    walk(&out, &mut |v| {
        if let Some(tc) = v.get("properties").and_then(|p| p.get("trackCount")) {
            assert_eq!(tc["minimum"], json!(0));
            assert!(tc.get("format").is_none());
            track_count = true;
        }
    });
    assert!(track_count);
}

#[test]
fn delegated_tool_inputs_round_trip_and_bound_evidence() {
    let work_id = "99eafac2-9d45-41da-93c5-24e420afea21";
    let digest = "a".repeat(64);
    let lease = json!({"workId":work_id, "claimGeneration":1, "leaseToken":"b".repeat(64)});
    let mut done = lease.clone();
    done["idempotencyKey"] = json!("complete-key");
    done["inputDigest"] = json!(digest);
    let evidence = json!({"source":"fixture:liner","retrievedAt":"2026-11-22T16:00:00Z","claim":"The liner lists the title","limitations":[]});
    done["result"] = json!({
        "schemaVersion":1,"workId":work_id,"kind":"metadata.correct","inputDigest":digest,
        "outcome":"proposal","confidence":0.5,"uncertainties":[],"evidence":[evidence],
        "recommendation":{"type":"metadata","identifiers":[],"albumFields":{"title":"Liner title"},"tracks":[]}
    });
    let bodies = [
        ("musaic_work_list", json!({"state":"queued","limit":50})),
        ("musaic_work_get", json!({"workId":work_id})),
        (
            "musaic_work_claim",
            json!({"workId":work_id,"idempotencyKey":"claim-key"}),
        ),
        ("musaic_work_renew", lease.clone()),
        (
            "musaic_work_progress",
            with_fields(
                &lease,
                json!({"sequence":1,"stage":"reviewing","completed":1,"total":2}),
            ),
        ),
        ("musaic_work_complete", done.clone()),
        (
            "musaic_work_fail",
            with_fields(
                &lease,
                json!({"code":"source_missing","message":"Source is unavailable","retryable":true}),
            ),
        ),
        (
            "musaic_work_release",
            with_fields(&lease, json!({"reason":"Another source is needed"})),
        ),
        (
            "musaic_work_cancel",
            json!({"workId":work_id,"expectedRevision":1}),
        ),
    ];
    for (id, body) in bodies {
        let registry = musaic_commands::find_tool(id).unwrap();
        let schema = (registry.input_schema)();
        let validator = jsonschema::validator_for(&schema).unwrap();
        assert!(
            validator.is_valid(&body),
            "{id}: {:?}",
            validator.iter_errors(&body).collect::<Vec<_>>()
        );
        let call = parse_call(id, body.to_string().as_bytes()).unwrap();
        assert_eq!(call.tool_id(), id);
    }
    done["result"]["evidence"] = json!(vec![evidence.clone(); 51]);
    assert_eq!(
        code("musaic_work_complete", done.clone()),
        ToolErrorCode::InvalidResult
    );
    done["result"]["evidence"] = json!([evidence]);
    done["result"]["evidence"][0]["retrievedAt"] = json!("not a timestamp");
    assert_eq!(
        code("musaic_work_complete", done.clone()),
        ToolErrorCode::InvalidResult
    );
    done["result"]["schemaVersion"] = json!("potential secret");
    let error = parse_call("musaic_work_complete", done.to_string().as_bytes()).unwrap_err();
    assert_eq!(error.code, ToolErrorCode::InvalidResult);
    assert!(!error.message.contains("potential secret"));
}

fn with_fields(base: &Value, fields: Value) -> Value {
    let mut result = base.as_object().unwrap().clone();
    result.extend(fields.as_object().unwrap().clone());
    Value::Object(result)
}

const WORK_ID: &str = "99eafac2-9d45-41da-93c5-24e420afea21";
const BATCH_ID: &str = "6f0c1d2e-3a4b-4c5d-8e6f-708192a3b4c5";

fn lease_for(work_id: &str) -> Value {
    json!({"workId": work_id, "claimGeneration": 1, "leaseToken": "b".repeat(64)})
}

fn field(tool: &str, body: Value) -> (ToolErrorCode, Option<String>) {
    let err = parse_call(tool, body.to_string().as_bytes()).unwrap_err();
    let f = err.details.as_ref().and_then(|d| d.get("field")).cloned();
    (err.code, f)
}

fn valid_against_schema(tool: &str, body: &Value) -> bool {
    let schema = (musaic_commands::find_tool(tool).unwrap().input_schema)();
    jsonschema::validator_for(&schema).unwrap().is_valid(body)
}

/// The two multi-item tools sit right after their single-item tools, need the
/// research scope and carry the annotations of contracts/agent-tools.md.
#[test]
fn the_multi_item_tools_are_registered_with_their_kinds_and_annotations() {
    let ids: Vec<&str> = REGISTRY.iter().map(|t| t.id).collect();
    let at = |id: &str| ids.iter().position(|i| *i == id).unwrap();
    assert_eq!(at("musaic_work_claim_many"), at("musaic_work_claim") + 1);
    assert_eq!(at("musaic_work_renew_many"), at("musaic_work_renew") + 1);
    for (id, cli, idempotent) in [
        ("musaic_work_claim_many", "work-claim-many", true),
        ("musaic_work_renew_many", "work-renew-many", false),
    ] {
        let t = musaic_commands::find_tool(id).unwrap();
        assert_eq!(t.cli_name, cli);
        assert_eq!(
            musaic_commands::find_by_cli_name(cli).map(|t| t.id),
            Some(id)
        );
        assert_eq!(
            t.kinds,
            [PermissionKind::LibraryRead, PermissionKind::ProposalSubmit]
        );
        assert!(!t.annotations.read_only, "{id}");
        assert!(!t.annotations.destructive, "{id}");
        assert_eq!(t.annotations.idempotent, idempotent, "{id}");
        assert!(!t.annotations.open_world, "{id}");
        assert_eq!((t.input_schema)()["additionalProperties"], json!(false));
        let lower = t.description.to_lowercase();
        for w in ["token", "secret", "password", "credential"] {
            assert!(!lower.contains(w), "{id}: {w}");
        }
    }
    let renew = (musaic_commands::find_tool("musaic_work_renew_many")
        .unwrap()
        .input_schema)();
    let mut lease_objects = 0;
    walk(&renew, &mut |v| {
        if v.get("properties")
            .is_some_and(|p| p.get("leaseToken").is_some())
        {
            assert_eq!(v["additionalProperties"], json!(false));
            lease_objects += 1;
        }
    });
    assert_eq!(lease_objects, 1);
}

#[test]
fn claim_many_bounds_max_and_requires_a_key() {
    let key = "burst-2026-11-23T10:00:00Z";
    let ok =
        json!({"batchId": BATCH_ID, "kind": "cover.research", "max": 25, "idempotencyKey": key});
    assert!(valid_against_schema("musaic_work_claim_many", &ok));
    match parse_call("musaic_work_claim_many", ok.to_string().as_bytes()).unwrap() {
        musaic_commands::ToolCall::WorkClaimMany(input) => {
            assert_eq!(input.max(), 25);
            assert_eq!(input.batch_id.as_deref(), Some(BATCH_ID));
        }
        other => panic!("{other:?}"),
    }
    let minimal = json!({"idempotencyKey": key});
    match parse_call("musaic_work_claim_many", minimal.to_string().as_bytes()).unwrap() {
        musaic_commands::ToolCall::WorkClaimMany(input) => {
            assert_eq!(
                input.max(),
                musaic_commands::tools::work::WorkClaimManyInput::MAX
            );
            assert!(input.batch_id.is_none() && input.kind.is_none());
        }
        other => panic!("{other:?}"),
    }
    assert!(
        parse_call(
            "musaic_work_claim_many",
            json!({"max": 1, "idempotencyKey": key})
                .to_string()
                .as_bytes()
        )
        .is_ok()
    );
    for max in [0, 26] {
        let body = json!({"max": max, "idempotencyKey": key});
        assert!(
            !valid_against_schema("musaic_work_claim_many", &body),
            "{max}"
        );
        assert_eq!(
            field("musaic_work_claim_many", body),
            (ToolErrorCode::InvalidInput, Some("max".into())),
            "{max}"
        );
    }
    assert_eq!(
        field("musaic_work_claim_many", json!({"idempotencyKey": "short"})),
        (ToolErrorCode::InvalidInput, Some("idempotencyKey".into()))
    );
    assert_eq!(
        code("musaic_work_claim_many", json!({})),
        ToolErrorCode::InvalidInput
    );
    let bad_batch = json!({"batchId": "not-a-batch", "idempotencyKey": key});
    assert!(!valid_against_schema("musaic_work_claim_many", &bad_batch));
    assert_eq!(
        field("musaic_work_claim_many", bad_batch),
        (ToolErrorCode::InvalidInput, Some("batchId".into()))
    );
}

#[test]
fn renew_many_takes_one_to_twenty_five_distinct_leases() {
    let ids: Vec<String> = (0..26)
        .map(|i| format!("99eafac2-9d45-41da-93c5-24e420af{i:04x}"))
        .collect();
    let leases = |n: usize| -> Value { ids[..n].iter().map(|id| lease_for(id)).collect() };
    for n in [1, 25] {
        let body = json!({"leases": leases(n)});
        assert!(valid_against_schema("musaic_work_renew_many", &body), "{n}");
        assert!(
            parse_call("musaic_work_renew_many", body.to_string().as_bytes()).is_ok(),
            "{n}"
        );
    }
    for n in [0, 26] {
        let body = json!({"leases": leases(n)});
        assert!(
            !valid_against_schema("musaic_work_renew_many", &body),
            "{n}"
        );
        assert_eq!(
            field("musaic_work_renew_many", body),
            (ToolErrorCode::InvalidInput, Some("leases".into())),
            "{n}"
        );
    }
    let duplicate = json!({"leases": [lease_for(WORK_ID), lease_for(WORK_ID)]});
    assert_eq!(
        field("musaic_work_renew_many", duplicate),
        (ToolErrorCode::InvalidInput, Some("leases".into()))
    );
    let mut bad_token = lease_for(WORK_ID);
    bad_token["leaseToken"] = json!("nope");
    assert_eq!(
        field("musaic_work_renew_many", json!({"leases": [bad_token]})),
        (ToolErrorCode::InvalidInput, Some("leaseToken".into()))
    );
    let mut extra = lease_for(WORK_ID);
    extra["surprise"] = json!(1);
    assert_eq!(
        code("musaic_work_renew_many", json!({"leases": [extra]})),
        ToolErrorCode::InvalidInput
    );
    // The token never reaches a debug print.
    let call = parse_call(
        "musaic_work_renew_many",
        json!({"leases": [lease_for(WORK_ID)]})
            .to_string()
            .as_bytes(),
    )
    .unwrap();
    assert!(!format!("{call:?}").contains(&"b".repeat(64)));
}

#[test]
fn work_list_filters_by_batch_and_summaries_omit_a_missing_batch() {
    let body = json!({"state": "queued", "batchId": BATCH_ID});
    assert!(valid_against_schema("musaic_work_list", &body));
    assert!(parse_call("musaic_work_list", body.to_string().as_bytes()).is_ok());
    for bad in ["6F0C1D2E-3A4B-4C5D-8E6F-708192A3B4C5", "short", ""] {
        let body = json!({"batchId": bad});
        assert!(!valid_against_schema("musaic_work_list", &body), "{bad}");
        assert_eq!(
            field("musaic_work_list", body),
            (ToolErrorCode::InvalidInput, Some("batchId".into())),
            "{bad}"
        );
    }
    let list = (musaic_commands::find_tool("musaic_work_list")
        .unwrap()
        .input_schema)();
    assert_eq!(
        list["properties"]["batchId"]["pattern"],
        json!("^[0-9a-f-]{36}$")
    );

    use musaic_commands::tools::work::{WorkKind, WorkState, WorkSummary};
    let mut summary = WorkSummary {
        work_id: WORK_ID.into(),
        album_id: "0123456789abcdef".into(),
        kind: WorkKind::CoverResearch,
        state: WorkState::Queued,
        revision: 1,
        attempts: 0,
        max_attempts: 3,
        created_at: "2026-11-23T10:00:00Z".into(),
        updated_at: "2026-11-23T10:00:00Z".into(),
        available_at: "2026-11-23T10:00:00Z".into(),
        result_id: None,
        batch_id: None,
    };
    let v = serde_json::to_value(&summary).unwrap();
    assert!(v.get("batchId").is_none(), "{v}");
    assert!(v.get("resultId").is_some(), "{v}");
    summary.batch_id = Some(BATCH_ID.into());
    let v = serde_json::to_value(&summary).unwrap();
    assert_eq!(v["batchId"], json!(BATCH_ID));
}

#[test]
fn renew_many_output_names_each_refusal_by_its_code() {
    use musaic_commands::tools::work::{WorkRenewManyOutput, WorkRenewOutput, WorkRenewRefusal};
    let out = musaic_commands::ToolOutput::WorkRenewMany(WorkRenewManyOutput {
        renewed: vec![WorkRenewOutput {
            work_id: WORK_ID.into(),
            lease_expires_at: "2026-11-23T10:05:00Z".into(),
        }],
        refused: vec![WorkRenewRefusal {
            work_id: BATCH_ID.into(),
            code: ToolErrorCode::LeaseExpired,
        }],
    });
    let v = out.to_json();
    assert_eq!(
        v,
        json!({
            "renewed": [{"workId": WORK_ID, "leaseExpiresAt": "2026-11-23T10:05:00Z"}],
            "refused": [{"workId": BATCH_ID, "code": "LEASE_EXPIRED"}],
        })
    );
    let schema = (musaic_commands::find_tool("musaic_work_renew_many")
        .unwrap()
        .output_schema)();
    assert!(jsonschema::validator_for(&schema).unwrap().is_valid(&v));
}
