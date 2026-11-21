//! The MCP face of the agent tools: an adapter from the registry in
//! `musaic-commands` to MCP over stdio, built on rmcp. It knows no HTTP and no
//! database; a [`Backend`] (the CLI's client of the running app) does the
//! work, and a domain failure becomes a tool result with `isError`.

pub mod profile;

use std::borrow::Cow;
use std::sync::Arc;

use musaic_commands::{REGISTRY, ToolDef, ToolError, ToolErrorCode, find_tool};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, Implementation, JsonObject,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
    ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, ServiceExt};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};

/// What the adapter calls for a tool: blocking, one JSON object in and out.
pub trait Backend: Send + Sync + 'static {
    fn call_tool(&self, tool_id: &str, input: Value) -> Result<Value, ToolError>;
}

/// The error of running the server (transport setup or a broken pipe).
pub type ServeError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Clone)]
struct Server {
    backend: Arc<dyn Backend>,
    version: Cow<'static, str>,
}

fn object(v: Value) -> Arc<JsonObject> {
    match v {
        Value::Object(m) => Arc::new(m),
        _ => Arc::new(JsonObject::new()),
    }
}

fn tool_of(t: &ToolDef) -> Tool {
    Tool::new(t.id, t.description, object((t.input_schema)()))
        .with_title(t.title)
        .with_raw_output_schema(object((t.output_schema)()))
        .with_annotations(ToolAnnotations::from_raw(
            None,
            Some(t.annotations.read_only),
            Some(t.annotations.destructive),
            Some(t.annotations.idempotent),
            Some(t.annotations.open_world),
        ))
}

impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("musaic", self.version.to_string()))
            .with_instructions(
                "Musaic agent tools. Call musaic_capabilities first. Changes to files wait for the user's approval in the Musaic app.",
            )
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [rmcp::model::ProtocolVersion]> {
        Cow::Owned(profile::supported())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(
            REGISTRY.iter().map(tool_of).collect(),
        ))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let Some(tool) = find_tool(&request.name) else {
            return Err(McpError::invalid_params("unknown tool", None));
        };
        let input = Value::Object(request.arguments.unwrap_or_default());
        let backend = Arc::clone(&self.backend);
        let id = tool.id;
        let outcome = tokio::task::spawn_blocking(move || backend.call_tool(id, input))
            .await
            .unwrap_or_else(|_| {
                Err(ToolError::new(
                    ToolErrorCode::Internal,
                    "the tool call failed",
                ))
            });
        Ok(match outcome {
            Ok(v) => CallToolResult::structured(v),
            Err(e) => CallToolResult::structured_error(
                serde_json::to_value(&e).expect("a tool error is JSON"),
            ),
        }
        .into())
    }
}

/// Serve MCP on a reader and writer until the reader reaches end of file.
/// Nothing but protocol messages is written to `writer`.
pub async fn serve_io<R, W>(
    backend: Arc<dyn Backend>,
    version: &str,
    reader: R,
    writer: W,
) -> Result<(), ServeError>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    let server = Server {
        backend,
        version: Cow::Owned(version.to_string()),
    };
    let running = match server.serve((reader, writer)).await {
        Ok(r) => r,
        // The host closed the pipe before saying anything: a clean end.
        Err(rmcp::service::ServerInitializeError::ConnectionClosed(_)) => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    running.waiting().await?;
    Ok(())
}

/// Serve MCP on this process's standard input and output.
pub async fn serve_stdio(backend: Arc<dyn Backend>, version: &str) -> Result<(), ServeError> {
    let (r, w) = rmcp::transport::stdio();
    serve_io(backend, version, r, w).await
}
