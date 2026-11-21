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
fn registry_has_twelve_tools_with_unique_names() {
    assert_eq!(REGISTRY.len(), 12);
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
