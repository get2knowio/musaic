//! The generated catalog: `docs/agent-tools.json` and `docs/agent-tools.md`.
//! Both are deterministic (object keys sorted, registry order kept) so a diff
//! means the contract changed.

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::errors::ToolErrorCode;
use crate::{CONTRACT_VERSION, NOT_OFFERED, REGISTRY, ToolDef};

fn tool_json(t: &ToolDef) -> Value {
    json!({
        "id": t.id,
        "cliName": t.cli_name,
        "title": t.title,
        "description": t.description,
        "kinds": t.kinds.iter().map(|k| k.as_str()).collect::<Vec<_>>(),
        "annotations": {
            "readOnlyHint": t.annotations.read_only,
            "destructiveHint": t.annotations.destructive,
            "idempotentHint": t.annotations.idempotent,
            "openWorldHint": t.annotations.open_world,
        },
        "inputSchema": (t.input_schema)(),
        "outputSchema": (t.output_schema)(),
    })
}

/// The catalog as JSON text, with a trailing newline. Built once per process:
/// it depends only on the code, and `capabilities` and the catalog route ask
/// for it (and its digest) on every call.
pub fn catalog_json() -> String {
    static TEXT: std::sync::LazyLock<String> = std::sync::LazyLock::new(build_catalog_json);
    TEXT.clone()
}

fn build_catalog_json() -> String {
    let doc = json!({
        "contractVersion": CONTRACT_VERSION,
        "tools": REGISTRY.iter().map(tool_json).collect::<Vec<_>>(),
        "errorCodes": ToolErrorCode::ALL
            .iter()
            .map(|c| json!({"code": c.as_str(), "transport": c.is_transport()}))
            .collect::<Vec<_>>(),
        "notOffered": NOT_OFFERED,
    });
    let mut s = serde_json::to_string_pretty(&doc).expect("catalog is JSON");
    s.push('\n');
    s
}

/// SHA-256 (hex) of [`catalog_json`]: the registry digest `capabilities` reports.
pub fn registry_digest() -> String {
    static DIGEST: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        crate::auth::hex_encode(&Sha256::digest(catalog_json().as_bytes()))
    });
    DIGEST.clone()
}

/// The properties of an object schema as `name (type, required)` lines.
fn properties(schema: &Value, root: &Value, out: &mut Vec<String>, indent: usize) {
    let schema = resolve(schema, root);
    let Some(props) = schema.get("properties").and_then(Value::as_object) else {
        return;
    };
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let pad = "  ".repeat(indent);
    for (name, p) in props {
        let req = if required.contains(&name.as_str()) {
            "required"
        } else {
            "optional"
        };
        out.push(format!("{pad}- `{name}`: {}, {req}", type_text(p, root)));
        let inner = resolve(p, root);
        if inner.get("properties").is_some() {
            properties(inner, root, out, indent + 1);
        } else if let Some(items) = inner.get("items") {
            let items = resolve(items, root);
            if items.get("properties").is_some() {
                properties(items, root, out, indent + 1);
            }
        }
    }
}

fn resolve<'a>(schema: &'a Value, root: &'a Value) -> &'a Value {
    if let Some(r) = schema.get("$ref").and_then(Value::as_str)
        && let Some(name) = r.strip_prefix("#/$defs/")
        && let Some(def) = root.get("$defs").and_then(|d| d.get(name))
    {
        return def;
    }
    schema
}

fn type_text(schema: &Value, root: &Value) -> String {
    let s = resolve(schema, root);
    if let Some(e) = s.get("enum").and_then(Value::as_array) {
        return e
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" | ");
    }
    if let Some(o) = s.get("oneOf").and_then(Value::as_array) {
        let parts: Vec<String> = o
            .iter()
            .filter_map(|v| v.get("const").and_then(Value::as_str).map(str::to_string))
            .collect();
        if !parts.is_empty() {
            return parts.join(" | ");
        }
    }
    let mut t = match s.get("type") {
        Some(Value::String(t)) => t.clone(),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .filter(|t| *t != "null")
            .collect::<Vec<_>>()
            .join(" | "),
        _ => "object".to_string(),
    };
    if t == "array"
        && let Some(items) = s.get("items")
    {
        t = format!("array of {}", type_text(items, root));
    }
    for (k, label) in [
        ("minimum", "min"),
        ("maximum", "max"),
        ("minLength", "minLength"),
        ("maxLength", "maxLength"),
        ("minItems", "minItems"),
        ("maxItems", "maxItems"),
    ] {
        if let Some(v) = s.get(k) {
            t.push_str(&format!(" ({label} {v})"));
        }
    }
    t
}

/// One tool's input and output as `name (type, required)` lines, for the
/// skill's generated tool reference.
pub(crate) fn tool_fields(t: &ToolDef) -> (Vec<String>, Vec<String>) {
    let input = (t.input_schema)();
    let output = (t.output_schema)();
    let mut i = Vec::new();
    properties(&input, &input, &mut i, 0);
    let mut o = Vec::new();
    properties(&output, &output, &mut o, 0);
    (i, o)
}

/// The catalog as Markdown, with a trailing newline.
pub fn catalog_markdown() -> String {
    let mut m = String::new();
    m.push_str("# Agent tools\n\n");
    m.push_str("Generated by `musaic-cli catalog --write`; do not edit by hand. `musaic-cli catalog --check` fails when this file or `agent-tools.json` differs from the code.\n\n");
    m.push_str(&format!("Contract version `{CONTRACT_VERSION}`, {} tools. Every input object rejects unknown fields; every output is an object. Tool results carry no filesystem path, only bare file and folder names.\n\n", REGISTRY.len()));
    m.push_str("| Tool | CLI name | Needs | Read-only | Destructive | Idempotent | Open world |\n|---|---|---|---|---|---|---|\n");
    for t in &REGISTRY {
        let kinds = t
            .kinds
            .iter()
            .map(|k| format!("`{k}`"))
            .collect::<Vec<_>>()
            .join(", ");
        m.push_str(&format!(
            "| `{}` | `{}` | {} | {} | {} | {} | {} |\n",
            t.id,
            t.cli_name,
            kinds,
            yn(t.annotations.read_only),
            yn(t.annotations.destructive),
            yn(t.annotations.idempotent),
            yn(t.annotations.open_world),
        ));
    }
    m.push('\n');
    for t in &REGISTRY {
        m.push_str(&format!("## `{}`\n\n{}\n\n", t.id, t.description));
        let input = (t.input_schema)();
        let output = (t.output_schema)();
        let mut lines = Vec::new();
        properties(&input, &input, &mut lines, 0);
        m.push_str("Input:\n\n");
        if lines.is_empty() {
            m.push_str("- (no fields)\n");
        }
        for l in &lines {
            m.push_str(l);
            m.push('\n');
        }
        let mut lines = Vec::new();
        properties(&output, &output, &mut lines, 0);
        m.push_str("\nOutput:\n\n");
        for l in &lines {
            m.push_str(l);
            m.push('\n');
        }
        m.push('\n');
    }
    m.push_str("## Error codes\n\nTool errors come back as a tool result with `isError` and `{code, message, details?}`.\n\n");
    for c in ToolErrorCode::ALL.iter().filter(|c| !c.is_transport()) {
        m.push_str(&format!("- `{c}`\n"));
    }
    m.push_str("\nTransport errors are reported by the CLI, not by a tool:\n\n");
    for c in ToolErrorCode::ALL.iter().filter(|c| c.is_transport()) {
        m.push_str(&format!("- `{c}`\n"));
    }
    m.push_str("\n## Deliberately not offered\n\n");
    for n in NOT_OFFERED {
        m.push_str(&format!("- {n}\n"));
    }
    m
}

fn yn(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

/// The catalog document with the registry digest added, as the app serves it.
pub fn catalog_with_digest() -> Value {
    with_digest(&catalog_json(), &registry_digest())
}

/// A catalog text (as [`catalog_json`] gives it) with `digest` added as
/// `registryDigest`: the document the catalog route renders.
pub fn with_digest(catalog: &str, digest: &str) -> Value {
    let mut doc: Map<String, Value> = serde_json::from_str(catalog).expect("catalog parses");
    doc.insert("registryDigest".into(), Value::String(digest.to_string()));
    Value::Object(doc)
}
