//! The agent bridge's routes.
//!
//! Two families, both thin adapters over `ops().agent()`:
//!
//! * `/__musaic/agent/v1/*`, called by an agent's CLI with its own guard
//!   ([`crate::agent_guard`]): `hello` and `exchange` (the handshake), the one
//!   tool route, which parses the body with `musaic_commands::parse_call` and
//!   renders the typed output, and the catalog.
//! * `/api/agent/*`, called by the webview behind the launch gate: pairings,
//!   policy, activity, the approval cards and the Undo of a recorded change.
//!   Only the person at the window reaches these; no agent token opens them.
//!
//! The guard, the validation order, the events and the notifications are the
//! operations' (`docs/operations.md`).

use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::response::Response;
use musaic_app::ops::agent::{
    ActivityRecord, AgentError, ApprovalFilter, ApprovalRecord, ChangeRecord, PairingRecord,
};
use musaic_commands::auth::ExchangeRequest;
use musaic_commands::{Decision, PermissionKind, ToolError, ToolErrorCode, parse_call};
use serde_json::{Value, json};

use crate::agent_guard::{Authed, error_response};
use crate::http::{ApiError, ApiResult, blocking, json_value, ok_value};
use crate::routes::library::{Q, qp};
use crate::state::AppState;

fn status_of(code: ToolErrorCode) -> StatusCode {
    match code {
        ToolErrorCode::NotFound | ToolErrorCode::MissingAlbum => StatusCode::NOT_FOUND,
        ToolErrorCode::InvalidInput => StatusCode::BAD_REQUEST,
        ToolErrorCode::NotPaired
        | ToolErrorCode::PairingRevoked
        | ToolErrorCode::InstanceMismatch => StatusCode::UNAUTHORIZED,
        ToolErrorCode::LockedOut => StatusCode::TOO_MANY_REQUESTS,
        ToolErrorCode::Forbidden => StatusCode::FORBIDDEN,
        ToolErrorCode::Busy | ToolErrorCode::Restoring => StatusCode::CONFLICT,
        ToolErrorCode::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        ToolErrorCode::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        _ => StatusCode::BAD_REQUEST,
    }
}

fn tool_error_body(e: &ToolError) -> Value {
    let mut err = json!({"code": e.code.as_str(), "message": e.message});
    if let Some(d) = &e.details {
        err["details"] = json!(d);
        if let Some(secs) = d
            .get("retryAfterSeconds")
            .and_then(|s| s.parse::<u64>().ok())
        {
            err["retryAfterSeconds"] = json!(secs);
        }
    }
    json!({"ok": false, "error": err})
}

fn agent_family_error(e: AgentError) -> Response {
    match e {
        AgentError::Tool(t) => json_value(status_of(t.code).as_u16(), &tool_error_body(&t)),
        AgentError::Conflict { message } | AgentError::ChangedSince { message, .. } => {
            error_response(StatusCode::CONFLICT, ToolErrorCode::Busy, &message)
        }
        AgentError::Op(_) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            ToolErrorCode::Internal,
            "the request failed",
        ),
    }
}

// ------------------------------------------------- /__musaic/agent/v1/*

pub async fn hello(State(st): State<AppState>) -> Response {
    match blocking(move || st.ops().agent().hello()).await {
        Ok(h) => json_value(200, &serde_json::to_value(h).unwrap_or(Value::Null)),
        Err(_) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            ToolErrorCode::Internal,
            "the request failed",
        ),
    }
}

pub async fn exchange(State(st): State<AppState>, body: Bytes) -> Response {
    let Ok(req) = serde_json::from_slice::<ExchangeRequest>(&body) else {
        return error_response(
            StatusCode::BAD_REQUEST,
            ToolErrorCode::InvalidInput,
            "the exchange request is malformed",
        );
    };
    match blocking(move || st.ops().agent().exchange(&req)).await {
        Ok(Ok(r)) => json_value(200, &serde_json::to_value(r).unwrap_or(Value::Null)),
        Ok(Err(e)) => agent_family_error(e),
        Err(_) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            ToolErrorCode::Internal,
            "the request failed",
        ),
    }
}

pub async fn list_tools(
    State(st): State<AppState>,
    Extension(Authed(p)): Extension<Authed>,
) -> Response {
    match blocking(move || st.ops().agent().list_tools(&p)).await {
        Ok(info) => json_value(
            200,
            &musaic_commands::catalog::with_digest(&info.catalog, &info.registry_digest),
        ),
        Err(_) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            ToolErrorCode::Internal,
            "the request failed",
        ),
    }
}

pub async fn call_tool(
    State(st): State<AppState>,
    Extension(Authed(p)): Extension<Authed>,
    UrlPath(tool_id): UrlPath<String>,
    body: Bytes,
) -> Response {
    let call = match parse_call(&tool_id, &body) {
        Ok(c) => c,
        Err(e) => {
            let status = if e.code == ToolErrorCode::NotFound {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::BAD_REQUEST
            };
            return json_value(status.as_u16(), &tool_error_body(&e));
        }
    };
    match blocking(move || st.ops().agent().call_tool(&p, call)).await {
        Ok(Ok(out)) => json_value(200, &json!({"ok": true, "result": out.to_json()})),
        // A domain failure is an answer, not an HTTP failure.
        Ok(Err(e)) => json_value(200, &tool_error_body(&e)),
        Err(_) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            ToolErrorCode::Internal,
            "the request failed",
        ),
    }
}

// ------------------------------------------------------- /api/agent/*

fn api_error(e: AgentError) -> ApiError {
    match e {
        AgentError::Tool(t) => ApiError::new(status_of(t.code).as_u16(), t.message),
        AgentError::Conflict { message } => ApiError::new(409, message),
        AgentError::ChangedSince { message, changed } => ApiError {
            status: 409,
            detail: json!({"message": message, "changed": changed}),
        },
        AgentError::Op(e) => ApiError::internal(format!("{e:?}")),
    }
}

fn pairing_json(p: &PairingRecord) -> Value {
    let policy: serde_json::Map<String, Value> = p
        .policy
        .iter()
        .map(|(k, d)| (k.as_str().to_string(), json!(d.as_str())))
        .collect();
    json!({
        "id": p.id,
        "name": p.name,
        "scopes": p.scopes.iter().map(|k| k.as_str()).collect::<Vec<_>>(),
        "policy": policy,
        "createdAt": p.created_at,
        "lastUsedAt": p.last_used_at,
        "revokedAt": p.revoked_at,
    })
}

fn card_json(c: &ApprovalRecord) -> Value {
    json!({
        "id": c.id,
        "pairingId": c.pairing_id,
        "pairingName": c.pairing_name,
        "action": c.action,
        "albumId": c.album_id,
        "albumLabel": c.album_label,
        "kinds": c.kinds.iter().map(|k| k.as_str()).collect::<Vec<_>>(),
        "state": c.state,
        "reason": c.reason,
        "createdAt": c.created_at,
        "expiresAt": c.expires_at,
        "decidedAt": c.decided_at,
        "diff": c.diff,
    })
}

fn kinds_of(v: &Value) -> Result<Vec<PermissionKind>, ApiError> {
    v.as_array()
        .ok_or_else(|| ApiError::new(400, "scopes must be a list"))?
        .iter()
        .map(|k| {
            k.as_str()
                .and_then(PermissionKind::parse)
                .ok_or_else(|| ApiError::new(400, "scopes holds an unknown permission"))
        })
        .collect()
}

pub async fn list_pairings(State(st): State<AppState>) -> ApiResult {
    blocking(move || {
        let list = st.ops().agent().list_pairings().map_err(api_error)?;
        ok_value(Value::Array(list.iter().map(pairing_json).collect()))
    })
    .await?
}

pub async fn create_pairing(State(st): State<AppState>, body: Bytes) -> ApiResult {
    let doc: Value = serde_json::from_slice(&body)
        .map_err(|_| ApiError::new(400, "the body is not valid JSON"))?;
    let obj = doc
        .as_object()
        .ok_or_else(|| ApiError::new(400, "the body must be an object"))?;
    let name = obj
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::new(400, "name is required"))?
        .to_string();
    let scopes = kinds_of(
        obj.get("scopes")
            .ok_or_else(|| ApiError::new(400, "scopes is required"))?,
    )?;
    blocking(move || {
        let made = st
            .ops()
            .agent()
            .create_pairing(&name, &scopes)
            .map_err(api_error)?;
        let mut v = pairing_json(&made.pairing);
        v["connect"] = json!({"command": made.connect.command, "args": made.connect.args});
        ok_value(v)
    })
    .await?
}

pub async fn revoke_pairing(
    State(st): State<AppState>,
    UrlPath(pairing_id): UrlPath<String>,
) -> ApiResult {
    blocking(move || {
        let p = st
            .ops()
            .agent()
            .revoke_pairing(&pairing_id)
            .map_err(api_error)?;
        ok_value(pairing_json(&p))
    })
    .await?
}

pub async fn set_policy(
    State(st): State<AppState>,
    UrlPath(pairing_id): UrlPath<String>,
    body: Bytes,
) -> ApiResult {
    let doc: Value = serde_json::from_slice(&body)
        .map_err(|_| ApiError::new(400, "the body is not valid JSON"))?;
    let obj = doc
        .as_object()
        .ok_or_else(|| ApiError::new(400, "the body must be an object"))?;
    let mut changes = Vec::new();
    for (k, v) in obj {
        let kind =
            PermissionKind::parse(k).ok_or_else(|| ApiError::new(400, "unknown permission"))?;
        let decision = v
            .as_str()
            .and_then(Decision::parse)
            .ok_or_else(|| ApiError::new(400, "a decision is allow, ask or refuse"))?;
        changes.push((kind, decision));
    }
    blocking(move || {
        let p = st
            .ops()
            .agent()
            .set_policy(&pairing_id, &changes)
            .map_err(api_error)?;
        ok_value(pairing_json(&p))
    })
    .await?
}

fn activity_json(a: &[ActivityRecord]) -> Value {
    Value::Array(
        a.iter()
            .map(|r| json!({"at": r.at, "tool": r.tool, "outcome": r.outcome}))
            .collect(),
    )
}

pub async fn pairing_activity(
    State(st): State<AppState>,
    UrlPath(pairing_id): UrlPath<String>,
) -> ApiResult {
    blocking(move || {
        let a = st.ops().agent().activity(&pairing_id).map_err(api_error)?;
        ok_value(activity_json(&a))
    })
    .await?
}

pub async fn list_approvals(State(st): State<AppState>, q: Q) -> ApiResult {
    let q = qp(q);
    let filter = match q.get("state") {
        None | Some("pending") => ApprovalFilter::Pending,
        Some("all") => ApprovalFilter::All,
        Some(_) => return Err(ApiError::new(400, "state is pending or all")),
    };
    blocking(move || {
        let cards = st.ops().agent().list_approvals(filter).map_err(api_error)?;
        ok_value(Value::Array(cards.iter().map(card_json).collect()))
    })
    .await?
}

pub async fn approve(
    State(st): State<AppState>,
    UrlPath(approval_id): UrlPath<String>,
) -> ApiResult {
    decide(st, approval_id, true).await
}

pub async fn deny(State(st): State<AppState>, UrlPath(approval_id): UrlPath<String>) -> ApiResult {
    decide(st, approval_id, false).await
}

fn change_json(c: &ChangeRecord) -> Value {
    json!({
        "id": c.id,
        "pairingId": c.pairing_id,
        "pairingName": c.pairing_name,
        "albumId": c.album_id,
        "albumLabel": c.album_label,
        "createdAt": c.created_at,
        "state": c.state,
        "summary": c.summary,
        "undoes": c.undoes,
        "undoable": c.undoable,
        "reason": c.reason,
    })
}

pub async fn list_changes(State(st): State<AppState>, q: Q) -> ApiResult {
    let q = qp(q);
    let pairing = q.get("pairing").map(str::to_string);
    blocking(move || {
        let list = st
            .ops()
            .agent()
            .list_changes(pairing.as_deref())
            .map_err(api_error)?;
        ok_value(Value::Array(list.iter().map(change_json).collect()))
    })
    .await?
}

pub async fn undo_change(
    State(st): State<AppState>,
    UrlPath(change_id): UrlPath<String>,
) -> ApiResult {
    blocking(move || {
        let out = st
            .ops()
            .agent()
            .undo_change(&change_id)
            .map_err(api_error)?;
        ok_value(json!({
            "change": change_json(&out.change),
            "undoId": out.undo_id,
            "complete": out.complete,
            "message": out.message,
        }))
    })
    .await?
}

async fn decide(st: AppState, approval_id: String, approve: bool) -> ApiResult {
    blocking(move || {
        let card = st
            .ops()
            .agent()
            .decide_approval(&approval_id, approve)
            .map_err(api_error)?;
        ok_value(card_json(&card))
    })
    .await?
}
