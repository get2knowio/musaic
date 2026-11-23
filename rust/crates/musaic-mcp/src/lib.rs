//! The MCP face of the agent tools: an adapter from the registry in
//! `musaic-commands` to MCP over stdio, built on rmcp. It knows no HTTP and no
//! database; a [`Backend`] (the CLI's client of the running app) does the
//! work, and a domain failure becomes a tool result with `isError`.

mod events;
pub mod profile;
mod prompts;
mod skills;

use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use musaic_commands::events::{
    EVENTS_EXTENSION_ID, HostObservation, ListResult, PollParams, PollResult,
};
use musaic_commands::tools::operation::{OperationHandle, OperationState, POLL_INTERVAL_MS};
use musaic_commands::tools::task::{TaskCreateInput, TaskInput, TaskSnapshot, TaskUpdateInput};
use musaic_commands::{REGISTRY, ToolDef, ToolError, ToolErrorCode, find_tool};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, CancelTaskMethod, CancelTaskParams,
    ClientCapabilities, CreateTaskResult, CustomRequest, CustomResult, DetailedTask,
    GetPromptRequestParams, GetPromptResponse, GetSkillRequestMethod, GetSkillRequestParams,
    GetSkillResult, GetTaskMethod, GetTaskParams, GetTaskResult, Implementation, JsonObject,
    ListPromptsResult, ListResourcesResult, ListSkillsRequestMethod, ListSkillsResult,
    ListToolsResult, MetaObject, PaginatedRequestParams, ProtocolVersion,
    ReadResourceRequestParams, ReadResourceResponse, ServerCapabilities, ServerConfig, Task,
    TaskPayload, TaskStatus, Tool, ToolAnnotations, UpdateTaskMethod, UpdateTaskParams,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, ServiceExt};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};

/// What the adapter calls for a tool: blocking, one JSON object in and out.
pub trait Backend: Send + Sync + 'static {
    fn call_tool(&self, tool_id: &str, input: Value) -> Result<Value, ToolError>;

    fn supports_tasks(&self) -> bool {
        false
    }

    fn create_task(&self, _input: TaskCreateInput) -> Result<TaskSnapshot, ToolError> {
        Err(tasks_unavailable())
    }

    fn get_task(&self, _input: TaskInput) -> Result<TaskSnapshot, ToolError> {
        Err(tasks_unavailable())
    }

    fn update_task(&self, _input: TaskUpdateInput) -> Result<(), ToolError> {
        Err(tasks_unavailable())
    }

    fn cancel_task(&self, _input: TaskInput) -> Result<(), ToolError> {
        Err(tasks_unavailable())
    }

    /// The event types the app offers (`events/list`).
    fn events_list(&self) -> Result<ListResult, ToolError> {
        Err(events_unavailable())
    }

    /// One poll of the delegated-work history. `wait_ms` may park the call
    /// in the app for up to 25 seconds, so the implementation's transport
    /// timeout must exceed it.
    fn events_poll(&self, _params: PollParams) -> Result<PollResult, ToolError> {
        Err(events_unavailable())
    }

    /// Tell the app what this host negotiated (diagnostics only). A failure
    /// is never a failure of the host's own request.
    fn observe_host(&self, _observation: HostObservation) -> Result<(), ToolError> {
        Ok(())
    }
}

/// The error of running the server (transport setup or a broken pipe).
pub type ServeError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Clone)]
struct Server {
    backend: Arc<dyn Backend>,
    version: Cow<'static, str>,
    /// The last observation recorded, so the app hears of a change only. A
    /// send that fails clears it, so the next request sends it again.
    observed: Arc<Mutex<Option<HostObservation>>>,
    /// One observation send at a time, so an older one never lands last.
    observe_sends: Arc<Mutex<()>>,
    /// The host fetched a skill (list, get or a skill resource).
    skills_used: Arc<AtomicBool>,
    /// The host called an `events/*` method (declaring the extension is
    /// optional for a client, so use counts as much as a declaration).
    events_used: Arc<AtomicBool>,
    /// Streams open in this process (the cap is in `events`).
    streams: Arc<std::sync::atomic::AtomicUsize>,
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

fn events_unavailable() -> ToolError {
    ToolError::new(ToolErrorCode::Unavailable, "the app does not offer events")
}

fn tasks_unavailable() -> ToolError {
    ToolError::new(
        ToolErrorCode::Unavailable,
        "the app does not support native Tasks",
    )
}

fn request_declares_tasks(context: &RequestContext<RoleServer>) -> bool {
    context
        .meta
        .client_capabilities()
        .is_some_and(|caps| caps.supports_tasks())
}

fn modern(context: &RequestContext<RoleServer>) -> bool {
    context
        .peer
        .peer_info()
        .is_none_or(|info| info.protocol_version == ProtocolVersion::V_2026_07_28)
        && context.protocol_version() == Some(ProtocolVersion::V_2026_07_28)
}

fn task_error(error: ToolError) -> McpError {
    let data = Some(serde_json::to_value(&error).expect("a tool error is JSON"));
    match error.code {
        ToolErrorCode::Internal => McpError::internal_error(error.message, data),
        code if code.is_transport() => McpError::internal_error(error.message, data),
        _ => McpError::invalid_params(error.message, data),
    }
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, ToolError> + Send + 'static,
) -> Result<Result<T, ToolError>, McpError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| McpError::internal_error("the request could not be executed", None))
}

fn operation_meta(id: &str) -> MetaObject {
    let mut value = JsonObject::new();
    value.insert("musaic/operationId".into(), Value::String(id.to_string()));
    MetaObject(value)
}

fn protocol_task(snapshot: &TaskSnapshot) -> Task {
    let status = if snapshot.execution_failed {
        TaskStatus::Failed
    } else if snapshot.operation.state == OperationState::Cancelled {
        TaskStatus::Cancelled
    } else if snapshot.operation.state.is_finished() {
        TaskStatus::Completed
    } else if !snapshot.pending_inputs.is_empty() {
        TaskStatus::InputRequired
    } else {
        TaskStatus::Working
    };
    let mut task = Task::new(
        &snapshot.task_id,
        status,
        &snapshot.created_at,
        &snapshot.updated_at,
    )
    .with_poll_interval_ms(POLL_INTERVAL_MS);
    if let Some(ttl) = snapshot.ttl_ms {
        task = task.with_ttl_ms(ttl);
    }
    if snapshot.operation.state == OperationState::AwaitingApproval && status == TaskStatus::Working
    {
        task = task.with_status_message("Waiting for a decision in the Musaic desktop app");
    }
    task
}

fn detailed_task(snapshot: TaskSnapshot) -> Result<GetTaskResult, McpError> {
    let task = protocol_task(&snapshot);
    let payload = match task.status {
        TaskStatus::Working => TaskPayload::Working,
        TaskStatus::Cancelled => TaskPayload::Cancelled,
        TaskStatus::Failed => TaskPayload::Failed {
            error: serde_json::to_value(McpError::internal_error(
                "the worker stopped unexpectedly",
                None,
            ))
            .expect("a protocol error is JSON")
            .as_object()
            .cloned()
            .expect("an error is an object"),
        },
        TaskStatus::Completed => {
            let is_error = snapshot.operation.state != OperationState::Succeeded;
            let handle = OperationHandle::completed(snapshot.operation.clone());
            let mut value = serde_json::to_value(handle).expect("an operation is JSON");
            let result = if is_error {
                if let Some(error) = &snapshot.operation.error {
                    value["code"] = serde_json::json!(error.code);
                    value["message"] = serde_json::json!(error.message);
                }
                CallToolResult::structured_error(value)
            } else {
                CallToolResult::structured(value)
            };
            TaskPayload::Completed {
                result: serde_json::to_value(result)
                    .expect("a tool result is JSON")
                    .as_object()
                    .cloned()
                    .expect("a tool result is an object"),
            }
        }
        TaskStatus::InputRequired => TaskPayload::InputRequired {
            input_requests: serde_json::from_value(
                serde_json::to_value(snapshot.pending_inputs)
                    .expect("the pending requests are JSON"),
            )
            .map_err(|_| {
                McpError::internal_error("the Task's input requests are malformed", None)
            })?,
        },
        _ => return Err(McpError::internal_error("unknown Task state", None)),
    };
    let mut result = GetTaskResult::new(DetailedTask::new(task, payload));
    result.meta = Some(operation_meta(&snapshot.operation.operation_id));
    Ok(result)
}

impl Server {
    /// Remember what this request shows of the host and tell the app when it
    /// changed. Best effort: never an error of the request.
    fn observe(&self, context: &RequestContext<RoleServer>) {
        let info = context.client_info();
        let caps = context.client_capabilities();
        let has = |id: &str| {
            caps.as_ref()
                .and_then(|c| c.extensions.as_ref())
                .is_some_and(|e| e.contains_key(id))
        };
        let observation = HostObservation {
            client_name: info.as_ref().map(|i| i.name.clone()).unwrap_or_default(),
            client_version: info.as_ref().map(|i| i.version.clone()).unwrap_or_default(),
            profile: context
                .protocol_version()
                .map(|v| v.as_str().to_string())
                .unwrap_or_default(),
            tasks: request_declares_tasks(context),
            skills: self.skills_used.load(Ordering::Relaxed)
                || has(musaic_commands::events::SKILLS_EXTENSION_ID),
            events: self.events_used.load(Ordering::Relaxed) || has(EVENTS_EXTENSION_ID),
        };
        let changed = {
            let mut last = self
                .observed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if last.as_ref() == Some(&observation) {
                false
            } else {
                *last = Some(observation.clone());
                true
            }
        };
        if changed {
            let backend = Arc::clone(&self.backend);
            let observed = Arc::clone(&self.observed);
            let sends = Arc::clone(&self.observe_sends);
            // Fire and forget on the blocking pool: the host's request never
            // waits on, or fails because of, a diagnostics call.
            drop(tokio::task::spawn_blocking(move || {
                let _one = sends
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let latest = |o: &HostObservation| {
                    observed
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .as_ref()
                        == Some(o)
                };
                // A newer observation was recorded meanwhile: its own send
                // carries it, and this older one must not land after it.
                if !latest(&observation) {
                    return;
                }
                if backend.observe_host(observation.clone()).is_err() {
                    // The app may not be running yet: forget the observation
                    // so the next request of the host sends it again.
                    let mut last = observed
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if last.as_ref() == Some(&observation) {
                        *last = None;
                    }
                }
            }));
        }
    }

    fn require_modern<M: rmcp::model::ConstString>(
        &self,
        context: &RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        if modern(context) {
            Ok(())
        } else {
            Err(McpError::method_not_found::<M>())
        }
    }

    fn require_tasks<M: rmcp::model::ConstString>(
        &self,
        context: &RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        if !modern(context) || !self.backend.supports_tasks() {
            return Err(McpError::method_not_found::<M>());
        }
        if !request_declares_tasks(context) {
            return Err(McpError::missing_required_client_capability(
                ClientCapabilities::builder().enable_tasks().build(),
            ));
        }
        Ok(())
    }
}

impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        let caps = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .enable_prompts()
            .enable_skills();
        let caps = if self.backend.supports_tasks() {
            caps.enable_tasks().build()
        } else {
            caps.build()
        };
        // The extensions are declared once; the handlers answer `skills/*` and
        // `events/*` on the 2026-07-28 profile only (research D1). Events
        // settings `{}`: the two event types never change at run time, so
        // `listChanged` stays at its default, false.
        let mut caps = caps;
        caps.extensions
            .get_or_insert_with(Default::default)
            .insert(EVENTS_EXTENSION_ID.to_string(), JsonObject::new());
        ServerConfig::new(caps)
            .with_server_info(Implementation::new("musaic", self.version.to_string()))
            .with_instructions(
                "Musaic agent tools. Call musaic_capabilities first. Changes to files wait for the user's approval in the Musaic app.",
            )
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [rmcp::model::ProtocolVersion]> {
        Cow::Owned(profile::supported())
    }

    async fn list_skills(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListSkillsResult, McpError> {
        self.require_modern::<ListSkillsRequestMethod>(&context)?;
        self.skills_used.store(true, Ordering::Relaxed);
        self.observe(&context);
        Ok(skills::list())
    }

    async fn get_skill(
        &self,
        request: GetSkillRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetSkillResult, McpError> {
        self.require_modern::<GetSkillRequestMethod>(&context)?;
        self.skills_used.store(true, Ordering::Relaxed);
        self.observe(&context);
        skills::get(&request.uri)
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(skills::resources())
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        self.skills_used.store(true, Ordering::Relaxed);
        self.observe(&context);
        Ok(skills::read(&request.uri)?.into())
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        Ok(prompts::list())
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, McpError> {
        Ok(prompts::get(&request)?.into())
    }

    async fn on_custom_request(
        &self,
        request: CustomRequest,
        context: RequestContext<RoleServer>,
    ) -> Result<CustomResult, McpError> {
        events::handle(self, request, context).await
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        self.observe(&context);
        Ok(ListToolsResult::with_all_items(
            REGISTRY.iter().map(tool_of).collect(),
        ))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        self.observe(&context);
        let Some(tool) = find_tool(&request.name) else {
            return Err(McpError::invalid_params("unknown tool", None));
        };
        let input = Value::Object(request.arguments.unwrap_or_default());
        let backend = Arc::clone(&self.backend);
        let id = tool.id;
        let negotiated =
            modern(&context) && request_declares_tasks(&context) && self.backend.supports_tasks();
        let long = matches!(
            id,
            "musaic_scan_start" | "musaic_analysis_start" | "musaic_checks_start"
        );
        match blocking(move || backend.call_tool(id, input)).await? {
            Ok(value) if negotiated && long => {
                let handle: OperationHandle = serde_json::from_value(value).map_err(|_| {
                    McpError::internal_error("the operation did not return a handle", None)
                })?;
                let backend = Arc::clone(&self.backend);
                let input = TaskCreateInput {
                    operation_id: handle.operation_id.clone(),
                };
                match blocking(move || backend.create_task(input)).await? {
                    Ok(snapshot) => Ok(CreateTaskResult::new(protocol_task(&snapshot))
                        .with_meta(operation_meta(&handle.operation_id))
                        .into()),
                    // The terminal application operation remains available after
                    // the Task's retention window, using its ordinary handle.
                    Err(e) if e.code == ToolErrorCode::TaskExpired => {
                        Ok(CallToolResult::structured(
                            serde_json::to_value(handle).expect("a handle is JSON"),
                        )
                        .into())
                    }
                    Err(e) => Err(task_error(e)),
                }
            }
            Ok(value) => Ok(CallToolResult::structured(value).into()),
            Err(error) if modern(&context) && error.code.is_transport() => Err(task_error(error)),
            Err(error) => Ok(CallToolResult::structured_error(
                serde_json::to_value(error).expect("a tool error is JSON"),
            )
            .into()),
        }
    }

    async fn get_task(
        &self,
        request: GetTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetTaskResult, McpError> {
        self.require_tasks::<GetTaskMethod>(&context)?;
        let backend = Arc::clone(&self.backend);
        let input = TaskInput {
            task_id: request.task_id,
        };
        detailed_task(
            blocking(move || backend.get_task(input))
                .await?
                .map_err(task_error)?,
        )
    }

    async fn update_task(
        &self,
        request: UpdateTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        self.require_tasks::<UpdateTaskMethod>(&context)?;
        let backend = Arc::clone(&self.backend);
        let input = TaskUpdateInput {
            task_id: request.task_id,
            input_responses: request.input_responses,
        };
        blocking(move || backend.update_task(input))
            .await?
            .map_err(task_error)
    }

    async fn cancel_task(
        &self,
        request: CancelTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        self.require_tasks::<CancelTaskMethod>(&context)?;
        let backend = Arc::clone(&self.backend);
        let input = TaskInput {
            task_id: request.task_id,
        };
        blocking(move || backend.cancel_task(input))
            .await?
            .map_err(task_error)
    }
}

/// The service rmcp runs: the server with one check in front. rmcp refuses a
/// `skills/get` whose URI is not a `SKILL.md` with a message that repeats the
/// URI; a refusal here never echoes the request (contracts/skills.md), so the
/// check runs before rmcp's own.
struct Gate(Server);

impl rmcp::Service<RoleServer> for Gate {
    async fn handle_request(
        &self,
        request: rmcp::model::ClientRequest,
        context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::ServerResult, McpError> {
        if let rmcp::model::ClientRequest::GetSkillRequest(r) = &request {
            // On the older profile skills are not offered at all; rmcp's own
            // URI check would otherwise answer first, echoing the URI.
            if !modern(&context) {
                return Err(McpError::method_not_found::<
                    rmcp::model::GetSkillRequestMethod,
                >());
            }
            if !skills::is_skill_md(&r.params.uri) {
                return Err(McpError::invalid_params(
                    musaic_commands::skill::UNKNOWN_RESOURCE,
                    None,
                ));
            }
        }
        rmcp::Service::handle_request(&self.0, request, context).await
    }

    async fn handle_notification(
        &self,
        notification: rmcp::model::ClientNotification,
        context: rmcp::service::NotificationContext<RoleServer>,
    ) -> Result<(), McpError> {
        rmcp::Service::handle_notification(&self.0, notification, context).await
    }

    fn get_info(&self) -> ServerConfig {
        rmcp::Service::get_info(&self.0)
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        rmcp::Service::supported_protocol_versions(&self.0)
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
        observed: Arc::default(),
        observe_sends: Arc::default(),
        skills_used: Arc::default(),
        events_used: Arc::default(),
        streams: Arc::default(),
    };
    let running = match Gate(server).serve((reader, writer)).await {
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
