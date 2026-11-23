//! The guard and router of the agent route family, `/__musaic/agent/v1/*`.
//!
//! An agent's CLI calls these routes; the webview never does. They sit in
//! front of everything else: [`crate::router_with`] hands a request whose
//! path starts with [`PREFIX`] to [`agent_router`] before the Host / Origin
//! check and the launch gate run, so those two are untouched for every
//! existing route (FR-027) and a launch cookie is of no use here: this guard
//! never reads one.
//!
//! The checks, in order, first failure wins:
//!
//! 1. the peer is on the loopback interface;
//! 2. exactly one `Host` header, equal to `127.0.0.1:<the bound port>`;
//! 3. no `Origin` header, and no `Sec-Fetch-Site` other than `none`;
//! 4. a body of at most 256 KiB;
//! 5. a JSON content type on a body;
//! 6. except for `hello` and `exchange`, a valid bearer token.

use std::net::SocketAddr;

use axum::Router;
use axum::body::Body;
use axum::extract::connect_info::{ConnectInfo, Connected};
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::MethodFilter;
use musaic_app::ops::agent::{AgentError, Principal};
use musaic_commands::ToolErrorCode;
use serde_json::json;
use tokio::net::TcpListener;

use crate::routes::agent;
use crate::state::AppState;

/// Every agent route starts with this.
pub const PREFIX: &str = "/__musaic/agent/";
pub const HELLO: &str = "/__musaic/agent/v1/hello";
pub const EXCHANGE: &str = "/__musaic/agent/v1/exchange";
pub const TOOLS: &str = "/__musaic/agent/v1/tools";
pub const TOOL: &str = "/__musaic/agent/v1/tools/{tool_id}";
pub const TASK_CREATE: &str = "/__musaic/agent/v1/tasks/create";
pub const TASK_GET: &str = "/__musaic/agent/v1/tasks/get";
pub const TASK_UPDATE: &str = "/__musaic/agent/v1/tasks/update";
pub const TASK_CANCEL: &str = "/__musaic/agent/v1/tasks/cancel";

/// The largest request body.
pub const MAX_BODY: usize = 256 * 1024;

/// The agent routes as (method, path), for the route table.
pub const ROUTES: &[(&str, &str)] = &[
    ("POST", HELLO),
    ("POST", EXCHANGE),
    ("GET", TOOLS),
    ("POST", TOOL),
    ("POST", TASK_CREATE),
    ("POST", TASK_GET),
    ("POST", TASK_UPDATE),
    ("POST", TASK_CANCEL),
];

/// Who is on the other end of the connection and which port they reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnInfo {
    pub peer: SocketAddr,
    pub local: SocketAddr,
}

impl Connected<axum::serve::IncomingStream<'_, TcpListener>> for ConnInfo {
    fn connect_info(target: axum::serve::IncomingStream<'_, TcpListener>) -> Self {
        let local = target
            .io()
            .local_addr()
            .unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], 0)));
        ConnInfo {
            peer: *target.remote_addr(),
            local,
        }
    }
}

/// A request that passed the token check: whose it is.
#[derive(Debug, Clone)]
pub struct Authed(pub Principal);

/// `{"ok": false, "error": {"code", "message"}}` with the status for the code.
pub fn error_response(status: StatusCode, code: ToolErrorCode, message: &str) -> Response {
    let body = json!({"ok": false, "error": {"code": code.as_str(), "message": message}});
    let mut r = Response::new(Body::from(body.to_string()));
    *r.status_mut() = status;
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    r
}

fn refuse(status: StatusCode, code: ToolErrorCode, message: &str) -> Response {
    error_response(status, code, message)
}

fn is_json_type(value: &str) -> bool {
    value
        .split(';')
        .next()
        .is_some_and(|m| m.trim().eq_ignore_ascii_case("application/json"))
}

fn bearer(headers: &axum::http::HeaderMap) -> Option<String> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = v.split_once(' ')?;
    (scheme.eq_ignore_ascii_case("bearer") && !token.trim().is_empty())
        .then(|| token.trim().to_string())
}

/// The middleware: the checks above, then the principal into the request.
pub async fn guard(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let forbidden = |why: &str| refuse(StatusCode::FORBIDDEN, ToolErrorCode::Forbidden, why);
    // 1. loopback peer
    let Some(info) = req.extensions().get::<ConnectInfo<ConnInfo>>().map(|c| c.0) else {
        return forbidden("the connection cannot be verified");
    };
    if !info.peer.ip().is_loopback() {
        return forbidden("only the local computer may call this");
    }
    // 2. exactly one Host, the loopback address and the bound port
    let mut hosts = req.headers().get_all(header::HOST).iter();
    let want = format!("127.0.0.1:{}", info.local.port());
    match (hosts.next(), hosts.next()) {
        (Some(h), None) if h.as_bytes() == want.as_bytes() => {}
        _ => return forbidden("the Host header is not this server's address"),
    }
    // 3. no Origin; no cross-site fetch
    if req.headers().contains_key(header::ORIGIN) {
        return forbidden("browser requests are refused");
    }
    if req
        .headers()
        .get("sec-fetch-site")
        .is_some_and(|v| v.as_bytes() != b"none")
    {
        return forbidden("browser requests are refused");
    }
    // 4. body size (declared, then actual)
    let too_big = || {
        refuse(
            StatusCode::PAYLOAD_TOO_LARGE,
            ToolErrorCode::InvalidInput,
            "the request body is over 256 KiB",
        )
    };
    if req
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok())
        .is_some_and(|n| n > MAX_BODY)
    {
        return too_big();
    }
    let path = req.uri().path().to_string();
    let (mut parts, body) = req.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_BODY).await else {
        return too_big();
    };
    // 5. JSON type on a body
    if !bytes.is_empty()
        && !parts
            .headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(is_json_type)
    {
        return refuse(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            ToolErrorCode::InvalidInput,
            "the body must be application/json",
        );
    }
    // 6. bearer token (a launch cookie alone grants nothing)
    if path != HELLO && path != EXCHANGE {
        let Some(token) = bearer(&parts.headers) else {
            return refuse(
                StatusCode::UNAUTHORIZED,
                ToolErrorCode::NotPaired,
                "a bearer token is required",
            );
        };
        let ops_state = st.clone();
        let authed =
            tokio::task::spawn_blocking(move || ops_state.ops().agent().authenticate(&token)).await;
        match authed {
            Ok(Ok(principal)) => {
                parts.extensions.insert(Authed(principal));
            }
            Ok(Err(AgentError::Tool(e))) => {
                return refuse(StatusCode::UNAUTHORIZED, e.code, &e.message);
            }
            _ => {
                return refuse(
                    StatusCode::UNAUTHORIZED,
                    ToolErrorCode::NotPaired,
                    "the token is not valid",
                );
            }
        }
    }
    next.run(Request::from_parts(parts, Body::from(bytes)))
        .await
}

/// The agent routes behind their guard. A matched route that the guard
/// refuses still carries its [`crate::RouteTag`].
pub fn agent_router(state: AppState) -> Router {
    use axum::routing::{get, on};
    Router::new()
        .route(HELLO, on(MethodFilter::POST, agent::hello))
        .route(EXCHANGE, on(MethodFilter::POST, agent::exchange))
        .route(TOOLS, get(agent::list_tools))
        .route(TOOL, on(MethodFilter::POST, agent::call_tool))
        .route(TASK_CREATE, on(MethodFilter::POST, agent::task_create))
        .route(TASK_GET, on(MethodFilter::POST, agent::task_get))
        .route(TASK_UPDATE, on(MethodFilter::POST, agent::task_update))
        .route(TASK_CANCEL, on(MethodFilter::POST, agent::task_cancel))
        .route_layer(middleware::from_fn_with_state(state.clone(), guard))
        .route_layer(middleware::from_fn(crate::tag_route))
        .fallback(|| async {
            error_response(
                StatusCode::NOT_FOUND,
                ToolErrorCode::NotFound,
                "no such agent route",
            )
        })
        .with_state(state)
}

/// Write `<config dir>/agent-instance.json` for this running server: where
/// the agent CLI finds it. No credential is in it. A failure is logged and
/// leaves the app running without the agent bridge.
pub fn publish_descriptor(state: &AppState, addr: SocketAddr) {
    let d = musaic_commands::descriptor::Descriptor {
        format_version: 1,
        instance_id: state.0.agent.instance_id().to_string(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        protocol_profiles: musaic_commands::PROTOCOL_PROFILES
            .iter()
            .map(|p| p.to_string())
            .collect(),
        contract_version: musaic_commands::CONTRACT_VERSION.to_string(),
        host: "127.0.0.1".to_string(),
        port: u32::from(addr.port()),
        pid: std::process::id(),
    };
    if let Err(e) = musaic_commands::descriptor::write_atomic_owner_only(state.config_dir(), &d) {
        state.log().warn(
            "musaic.agent",
            &format!(
                "the agent descriptor could not be written: {e}; assistants cannot find the app"
            ),
        );
    }
}

/// Remove the descriptor if it still names this instance.
pub fn remove_descriptor(state: &AppState) {
    musaic_commands::descriptor::remove_if_own(state.config_dir(), state.0.agent.instance_id());
}
