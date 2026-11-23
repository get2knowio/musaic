//! Events over MCP (extension `io.modelcontextprotocol/events`, revision
//! 2026-07-28). rmcp has no Events support, so the methods arrive as custom
//! requests and are served to the documented wire contract
//! (`specs/005-skills-events-agent-hosts/contracts/events.md`).
//!
//! * `events/list` and `events/poll` are one blocking call to the app each.
//! * `events/stream` keeps the request open: it sends
//!   `notifications/events/*` through the peer while a loop of waiting polls
//!   runs, and stops on `notifications/cancelled` (rmcp cancels the request's
//!   token), on a failed write or on a revoked pairing. No per-subscriber
//!   buffer exists: a parked poll is the whole cost of waiting.
//! * `events/subscribe` and `events/unsubscribe` are webhook delivery, which
//!   is not offered (`Unsupported`).
//!
//! Every method answers only on the 2026-07-28 profile and only when the
//! request declares the Events extension; anything else is `-32601`.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use musaic_commands::events::{
    EVENTS_EXTENSION_ID, HEARTBEAT_DEFAULT_MS, HEARTBEAT_MAX_MS, HEARTBEAT_MIN_MS, MAX_STREAMS,
    MAX_WAIT_MS, PollParams, PollResult, rpc, rpc_code,
};
use musaic_commands::{ToolError, ToolErrorCode};
use rmcp::model::{CustomNotification, CustomRequest, CustomResult, ErrorCode, ServerNotification};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer};
use serde_json::{Value, json};

use crate::{Backend, Server, blocking, modern};

const SUBSCRIPTION_ID: &str = "io.modelcontextprotocol/subscriptionId";
/// How long a stream waits before asking again after the app cannot be
/// reached (it may be restarting); a stream survives that.
const RETRY_AFTER: Duration = Duration::from_secs(5);

fn declares_events(context: &RequestContext<RoleServer>) -> bool {
    context
        .client_capabilities()
        .and_then(|c| c.extensions)
        .is_some_and(|e| e.contains_key(EVENTS_EXTENSION_ID))
}

fn method_not_found(method: &str) -> McpError {
    McpError::new(ErrorCode::METHOD_NOT_FOUND, method.to_string(), None)
}

/// The JSON-RPC error of a bridge error.
fn rpc_error(e: &ToolError) -> McpError {
    match rpc_code(e.code) {
        Some(code) => McpError::new(ErrorCode(code), e.message.clone(), None),
        None => McpError::internal_error(e.message.clone(), None),
    }
}

fn error_object(e: &ToolError) -> Value {
    let code = rpc_code(e.code).unwrap_or(-32603);
    json!({"code": code, "message": e.message})
}

/// Parameters of `events/poll` and `events/stream`, as the wire has them.
/// Returns the poll parameters and the requested `heartbeatMs`.
fn parse_params(request: &CustomRequest) -> Result<(PollParams, Option<u32>), McpError> {
    let mut value = request.params.clone().unwrap_or_else(|| json!({}));
    let Some(obj) = value.as_object_mut() else {
        return Err(McpError::invalid_params("params must be an object", None));
    };
    obj.remove("_meta");
    // `waitMs` is the bridge's own; a client never sends it.
    if obj.remove("waitMs").is_some() {
        return Err(McpError::invalid_params("unknown parameter", None));
    }
    let heartbeat = match obj.remove("heartbeatMs") {
        None => None,
        Some(v) => Some(
            v.as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| McpError::invalid_params("heartbeatMs must be a number", None))?,
        ),
    };
    let params: PollParams = serde_json::from_value(value).map_err(|_| {
        McpError::invalid_params("the parameters do not match the event schema", None)
    })?;
    params.validate().map_err(|e| rpc_error(&e))?;
    Ok((params, heartbeat))
}

pub(crate) async fn handle(
    server: &Server,
    request: CustomRequest,
    context: RequestContext<RoleServer>,
) -> Result<CustomResult, McpError> {
    let method = request.method.as_str();
    if !matches!(
        method,
        "events/list" | "events/poll" | "events/stream" | "events/subscribe" | "events/unsubscribe"
    ) || !modern(&context)
        || !declares_events(&context)
    {
        return Err(method_not_found(method));
    }
    server.observe(&context);
    match method {
        "events/list" => {
            let backend = Arc::clone(&server.backend);
            let list = blocking(move || backend.events_list())
                .await?
                .map_err(|e| rpc_error(&e))?;
            Ok(CustomResult::new(json!({
                "resultType": "complete",
                "events": list.events,
            })))
        }
        "events/poll" => {
            let (mut params, _) = parse_params(&request)?;
            params.wait_ms = None;
            let backend = Arc::clone(&server.backend);
            let result = blocking(move || backend.events_poll(params))
                .await?
                .map_err(|e| rpc_error(&e))?;
            let mut value = serde_json::to_value(result).expect("a poll result is JSON");
            value["resultType"] = json!("complete");
            Ok(CustomResult::new(value))
        }
        "events/stream" => stream(server, &request, context).await,
        // Webhook delivery is not offered.
        _ => Err(McpError::new(
            ErrorCode(rpc::UNSUPPORTED),
            "webhook delivery is not offered; use events/stream or events/poll",
            None,
        )),
    }
}

/// Counts an open stream and gives the slot back when it ends.
struct Slot(Arc<AtomicUsize>);

impl Slot {
    fn take(counter: &Arc<AtomicUsize>) -> Option<Slot> {
        let mut current = counter.load(Ordering::SeqCst);
        loop {
            if current >= MAX_STREAMS {
                return None;
            }
            match counter.compare_exchange(current, current + 1, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return Some(Slot(Arc::clone(counter))),
                Err(seen) => current = seen,
            }
        }
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

struct Notifier {
    context: RequestContext<RoleServer>,
    subscription: Value,
}

impl Notifier {
    /// Send one `notifications/events/<name>`. `false` when the write failed
    /// (the host is gone).
    async fn send(&self, name: &str, mut params: Value) -> bool {
        params["_meta"] = json!({ SUBSCRIPTION_ID: self.subscription });
        let note = CustomNotification::new(format!("notifications/events/{name}"), Some(params));
        self.context
            .peer
            .send_notification(ServerNotification::CustomNotification(note))
            .await
            .is_ok()
    }
}

async fn stream(
    server: &Server,
    request: &CustomRequest,
    context: RequestContext<RoleServer>,
) -> Result<CustomResult, McpError> {
    let (mut params, heartbeat) = parse_params(request)?;
    let Some(_slot) = Slot::take(&server.streams) else {
        return Err(McpError::new(
            ErrorCode(rpc::RESOURCE_EXHAUSTED),
            "too many open event streams in this process",
            None,
        ));
    };
    let heartbeat_ms = heartbeat
        .unwrap_or(HEARTBEAT_DEFAULT_MS)
        .clamp(HEARTBEAT_MIN_MS, HEARTBEAT_MAX_MS);
    let heartbeat = Duration::from_millis(u64::from(heartbeat_ms));
    let ct = context.ct.clone();
    let notifier = Notifier {
        subscription: serde_json::to_value(&context.id).unwrap_or(Value::Null),
        context,
    };
    let done = || Ok(CustomResult::new(json!({"resultType": "complete"})));

    // The first poll establishes the cursor (the head for none) and says
    // whether the one given was unusable.
    params.wait_ms = None;
    let first = match poll(&server.backend, params.clone(), ct.cancelled()).await {
        Poll::Cancelled => return done(),
        Poll::Done(Ok(r)) => r,
        Poll::Done(Err(e)) => return Err(rpc_error(&e)),
    };
    params.cursor = Some(first.cursor.clone());
    let mut last_sent = Instant::now();
    let active = json!({
        "cursor": first.cursor, "truncated": first.truncated, "heartbeatMs": heartbeat_ms,
    });
    if !notifier.send("active", active).await {
        return done();
    }
    // A first poll can already hold events (a cursor from before).
    if !deliver(&notifier, &first, &mut last_sent).await {
        return done();
    }

    loop {
        let remaining = heartbeat.saturating_sub(last_sent.elapsed());
        let wait = u32::try_from(remaining.as_millis())
            .unwrap_or(MAX_WAIT_MS)
            .clamp(1_000, MAX_WAIT_MS);
        params.wait_ms = Some(wait);
        match poll(&server.backend, params.clone(), ct.cancelled()).await {
            Poll::Cancelled => return done(),
            Poll::Done(Ok(r)) => {
                params.cursor = Some(r.cursor.clone());
                if r.truncated {
                    let active = json!({
                        "cursor": r.cursor, "truncated": true, "heartbeatMs": heartbeat_ms,
                    });
                    if !notifier.send("active", active).await {
                        return done();
                    }
                    last_sent = Instant::now();
                }
                if !deliver(&notifier, &r, &mut last_sent).await {
                    return done();
                }
                if r.events.is_empty() && last_sent.elapsed() >= heartbeat {
                    if !notifier
                        .send("heartbeat", json!({"cursor": r.cursor}))
                        .await
                    {
                        return done();
                    }
                    last_sent = Instant::now();
                }
            }
            Poll::Done(Err(e)) => match e.code {
                // Revoked, or the event type is gone: tell the host, end.
                ToolErrorCode::EventForbidden
                | ToolErrorCode::EventNotFound
                | ToolErrorCode::Forbidden
                | ToolErrorCode::PairingRevoked
                | ToolErrorCode::NotPaired => {
                    let _ = notifier
                        .send("terminated", json!({"error": error_object(&e)}))
                        .await;
                    return done();
                }
                // The app is unreachable (it may be restarting) or briefly
                // unable, or this pairing's waiting polls are all taken (more
                // streams than the app parks per pairing, or the poll of a
                // stream just cancelled still waiting out its time): keep the
                // stream and ask again.
                _ if e.code.is_transport()
                    || e.code == ToolErrorCode::Unavailable
                    || e.code == ToolErrorCode::EventLimit =>
                {
                    tokio::select! {
                        () = ct.cancelled() => return done(),
                        () = tokio::time::sleep(RETRY_AFTER) => {}
                    }
                    if last_sent.elapsed() >= heartbeat {
                        let beat = json!({"cursor": params.cursor});
                        if !notifier.send("heartbeat", beat).await {
                            return done();
                        }
                        last_sent = Instant::now();
                    }
                }
                _ => return Err(rpc_error(&e)),
            },
        }
    }
}

/// Send each event of a page. `false` when a write failed.
async fn deliver(notifier: &Notifier, page: &PollResult, last_sent: &mut Instant) -> bool {
    for event in &page.events {
        let mut params = serde_json::to_value(event).expect("an event is JSON");
        if event.cursor.is_none() {
            params["cursor"] = json!(page.cursor);
        }
        if !notifier.send("event", params).await {
            return false;
        }
        *last_sent = Instant::now();
    }
    true
}

enum Poll {
    Cancelled,
    Done(Result<PollResult, ToolError>),
}

/// One blocking poll, abandoned when `cancelled` completes. A call already
/// parked in the app ends on its own wait (at most 25 seconds).
async fn poll(
    backend: &Arc<dyn Backend>,
    params: PollParams,
    cancelled: impl Future<Output = ()>,
) -> Poll {
    let backend = Arc::clone(backend);
    let work = tokio::task::spawn_blocking(move || backend.events_poll(params));
    tokio::select! {
        () = cancelled => Poll::Cancelled,
        r = work => match r {
            Ok(r) => Poll::Done(r),
            Err(_) => Poll::Done(Err(ToolError::new(
                ToolErrorCode::Internal,
                "the poll could not run",
            ))),
        },
    }
}
