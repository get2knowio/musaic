//! Both MCP profiles over in-memory pipes with a fake backend (T037).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use musaic_commands::{REGISTRY, ToolError, ToolErrorCode};
use musaic_mcp::{Backend, serve_io};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

#[derive(Default)]
struct Fake {
    calls: Mutex<Vec<(String, Value)>>,
}

impl Backend for Fake {
    fn call_tool(&self, tool_id: &str, input: Value) -> Result<Value, ToolError> {
        self.calls
            .lock()
            .unwrap()
            .push((tool_id.to_string(), input));
        match tool_id {
            "musaic_capabilities" => Ok(json!({
                "appVersion": "t", "contractVersion": "1", "registryDigest": "d", "instanceId": "i_1",
                "scopes": ["library.read"], "policy": {"library.read": "allow"},
                "profiles": ["mcp-2026-07-28"], "extensions": [], "configuredRoots": []
            })),
            _ => Err(ToolError::new(ToolErrorCode::NotFound, "nothing here")),
        }
    }
}

struct Client {
    w: DuplexStream,
    r: BufReader<DuplexStream>,
    next: u64,
    server: tokio::task::JoinHandle<Result<(), musaic_mcp::ServeError>>,
    backend: Arc<Fake>,
}

fn start() -> Client {
    let (cw, sr) = tokio::io::duplex(1 << 20);
    let (sw, cr) = tokio::io::duplex(1 << 20);
    let backend = Arc::new(Fake::default());
    let b: Arc<dyn Backend> = backend.clone();
    let server = tokio::spawn(async move { serve_io(b, "9.9.9", sr, sw).await });
    Client {
        w: cw,
        r: BufReader::new(cr),
        next: 1,
        server,
        backend,
    }
}

impl Client {
    async fn send(&mut self, v: Value) {
        let mut line = v.to_string();
        line.push('\n');
        self.w.write_all(line.as_bytes()).await.unwrap();
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await;
        loop {
            let mut line = String::new();
            let n = tokio::time::timeout(Duration::from_secs(10), self.r.read_line(&mut line))
                .await
                .expect("a reply in time")
                .unwrap();
            assert!(n > 0, "the server closed the stream");
            let v: Value = serde_json::from_str(line.trim()).expect("every line is JSON");
            assert_eq!(v["jsonrpc"], "2.0", "{v}");
            if v["id"] == json!(id) {
                return v;
            }
        }
    }
}

fn modern_meta(version: &str) -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": version,
        "io.modelcontextprotocol/clientCapabilities": {},
        "io.modelcontextprotocol/clientInfo": {"name": "t", "version": "1"}
    })
}

#[tokio::test]
async fn legacy_initialize_list_and_call() {
    let mut c = start();
    let init = c
        .request("initialize", json!({"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"t","version":"1"}}))
        .await;
    assert_eq!(init["result"]["protocolVersion"], "2025-11-25", "{init}");
    assert_eq!(init["result"]["serverInfo"]["name"], "musaic");
    assert!(
        init["result"].get("resultType").is_none(),
        "legacy replies carry no resultType"
    );
    c.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
        .await;

    let list = c.request("tools/list", json!({})).await;
    let tools = list["result"]["tools"].as_array().expect("tools");
    assert_eq!(tools.len(), 21);
    let ids: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(ids, REGISTRY.iter().map(|t| t.id).collect::<Vec<_>>());
    for t in tools {
        assert_eq!(t["inputSchema"]["type"], "object");
        assert_eq!(t["outputSchema"]["type"], "object");
        assert!(t["annotations"]["readOnlyHint"].is_boolean());
    }
    assert_eq!(tools[1]["annotations"]["readOnlyHint"], true);
    assert_eq!(tools[6]["annotations"]["destructiveHint"], true);

    let call = c
        .request(
            "tools/call",
            json!({"name":"musaic_capabilities","arguments":{}}),
        )
        .await;
    assert_eq!(call["result"]["isError"], false, "{call}");
    // structuredContent validates against the tool's output schema.
    let schema = (REGISTRY[0].output_schema)();
    let v = jsonschema::validator_for(&schema).unwrap();
    assert!(v.is_valid(&call["result"]["structuredContent"]), "{call}");
    assert_eq!(c.backend.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn modern_discover_then_stateless_calls() {
    let mut c = start();
    let d = c
        .request(
            "server/discover",
            json!({"_meta": modern_meta("2026-07-28")}),
        )
        .await;
    assert!(d.get("error").is_none(), "{d}");
    assert_eq!(d["result"]["resultType"], "complete", "{d}");
    let versions = d["result"]["supportedVersions"].to_string();
    assert!(
        versions.contains("2026-07-28") && versions.contains("2025-11-25"),
        "{d}"
    );

    // The server is still there after discover.
    let list = c
        .request("tools/list", json!({"_meta": modern_meta("2026-07-28")}))
        .await;
    assert_eq!(
        list["result"]["tools"].as_array().unwrap().len(),
        21,
        "{list}"
    );
    assert_eq!(list["result"]["resultType"], "complete");
    assert!(list["result"]["ttlMs"].is_number(), "{list}");
    assert!(list["result"]["cacheScope"].is_string(), "{list}");

    let call = c
        .request(
            "tools/call",
            json!({"name":"musaic_capabilities","arguments":{},"_meta": modern_meta("2026-07-28")}),
        )
        .await;
    assert_eq!(call["result"]["resultType"], "complete", "{call}");
    assert_eq!(call["result"]["isError"], false);
    assert!(!c.server.is_finished(), "discover must not end the server");
}

#[tokio::test]
async fn modern_request_without_meta_is_invalid_params() {
    let mut c = start();
    let r = c.request("server/discover", json!({})).await;
    assert_eq!(r["error"]["code"], -32602, "{r}");
    // Still alive.
    let ok = c
        .request(
            "server/discover",
            json!({"_meta": modern_meta("2026-07-28")}),
        )
        .await;
    assert!(ok.get("error").is_none(), "{ok}");
}

#[tokio::test]
async fn unsupported_version_is_32022_with_supported_list() {
    let mut c = start();
    let r = c
        .request("tools/list", json!({"_meta": modern_meta("2031-01-01")}))
        .await;
    assert_eq!(r["error"]["code"], -32022, "{r}");
    let supported = r["error"]["data"]["supported"].to_string();
    assert!(
        supported.contains("2026-07-28") && supported.contains("2025-11-25"),
        "{r}"
    );
}

#[tokio::test]
async fn unknown_tool_is_a_protocol_error_and_domain_error_is_is_error() {
    let mut c = start();
    let r = c
        .request(
            "tools/call",
            json!({"name":"musaic_nope","arguments":{},"_meta": modern_meta("2026-07-28")}),
        )
        .await;
    assert_eq!(r["error"]["code"], -32602, "{r}");

    let r = c
        .request("tools/call", json!({"name":"musaic_findings_list","arguments":{},"_meta": modern_meta("2026-07-28")}))
        .await;
    assert!(r.get("error").is_none(), "{r}");
    assert_eq!(r["result"]["isError"], true);
    assert_eq!(r["result"]["structuredContent"]["code"], "NOT_FOUND");
    assert_eq!(r["result"]["structuredContent"]["message"], "nothing here");
}

#[tokio::test]
async fn initialize_switches_to_legacy_for_the_process() {
    let mut c = start();
    c.request("initialize", json!({"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"t","version":"1"}}))
        .await;
    c.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
        .await;
    // After initialize, replies stay in the legacy envelope even if the request
    // carries no modern _meta.
    let list = c.request("tools/list", json!({})).await;
    assert!(list["result"].get("resultType").is_none(), "{list}");
    assert!(list["result"].get("ttlMs").is_none(), "{list}");
    let call = c
        .request(
            "tools/call",
            json!({"name":"musaic_capabilities","arguments":{}}),
        )
        .await;
    assert!(call["result"].get("resultType").is_none(), "{call}");
}

#[tokio::test]
async fn eof_ends_the_server_cleanly() {
    let c = start();
    drop(c.w);
    let res = tokio::time::timeout(Duration::from_secs(10), c.server)
        .await
        .expect("ends")
        .unwrap();
    assert!(res.is_ok(), "{res:?}");
}
