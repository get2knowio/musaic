//! Response helpers: FastAPI's JSON body rendering (Starlette `JSONResponse`:
//! `json.dumps(ensure_ascii=False, separators=(",", ":"))`), `HTTPException`
//! bodies (`{"detail": ...}`), and the blocking-task bridge.

use axum::body::Body;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use musaic_services::Json;
use serde_json::Value;

/// An `HTTPException(status_code, detail)`.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiError {
    pub status: u16,
    pub detail: Value,
}

impl ApiError {
    pub fn new(status: u16, detail: impl Into<String>) -> Self {
        ApiError {
            status,
            detail: Value::String(detail.into()),
        }
    }

    /// Starlette's default for an unmatched route.
    pub fn not_found() -> Self {
        ApiError::new(404, "Not Found")
    }

    pub fn method_not_allowed() -> Self {
        ApiError::new(405, "Method Not Allowed")
    }

    /// An unexpected server-side failure. FastAPI answers these with a plain
    /// text `Internal Server Error`; the detail is logged, never sent.
    pub fn internal(e: impl std::fmt::Display) -> Self {
        crate::logging::global(
            crate::logging::Level::Error,
            "musaic.api",
            &format!("internal error: {e}"),
        );
        ApiError {
            status: 500,
            detail: Value::Null,
        }
    }
}

impl From<musaic_services::HttpError> for ApiError {
    fn from(e: musaic_services::HttpError) -> Self {
        ApiError::new(e.status, e.detail)
    }
}

impl From<musaic_db::Error> for ApiError {
    fn from(e: musaic_db::Error) -> Self {
        ApiError::internal(e)
    }
}

impl From<rusqlite::Error> for ApiError {
    fn from(e: rusqlite::Error) -> Self {
        ApiError::internal(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        if self.status == 500 && self.detail.is_null() {
            let mut r = Response::new(Body::from("Internal Server Error"));
            *r.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            r.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            );
            return r;
        }
        if self.status == 422
            && let Some(detail) = crate::lax::validation_detail(&self.detail)
        {
            return json_ordered(422, &Json::obj([("detail", detail)]));
        }
        let mut body = serde_json::Map::new();
        body.insert("detail".into(), self.detail);
        json_value(self.status, &Value::Object(body))
    }
}

pub type ApiResult = Result<Response, ApiError>;

fn status(code: u16) -> StatusCode {
    StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
}

fn with_json_type(code: u16, bytes: Vec<u8>) -> Response {
    let mut r = Response::new(Body::from(bytes));
    *r.status_mut() = status(code);
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    r
}

/// A `serde_json::Value` body.
pub fn json_value(code: u16, v: &Value) -> Response {
    with_json_type(code, serde_json::to_vec(v).unwrap_or_default())
}

pub fn ok_value(v: Value) -> ApiResult {
    Ok(json_value(200, &v))
}

/// An ordered (Python-dict) body.
pub fn json_ordered(code: u16, v: &Json) -> Response {
    let mut out = String::new();
    write_compact(v, &mut out);
    with_json_type(code, out.into_bytes())
}

pub fn ok_ordered(v: &Json) -> ApiResult {
    Ok(json_ordered(200, v))
}

/// `json.dumps(v, ensure_ascii=False, separators=(",", ":"))`.
pub fn write_compact(v: &Json, out: &mut String) {
    match v {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Int(n) => out.push_str(&n.to_string()),
        Json::Float(f) => out.push_str(&musaic_services::json::py_float_repr(*f)),
        Json::Str(s) => write_str(s, out),
        Json::Arr(items) => {
            out.push('[');
            for (i, x) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_compact(x, out);
            }
            out.push(']');
        }
        Json::Obj(pairs) => {
            out.push('{');
            for (i, (k, x)) in pairs.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_str(k, out);
                out.push(':');
                write_compact(x, out);
            }
            out.push('}');
        }
    }
}

fn write_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Run blocking crate code off the async runtime, carrying the
/// `in_post_scan_rerun` / `in_bulk_run` flags of the caller into the worker
/// thread (musaic-events `context` docs: `spawn_blocking` does not copy
/// them, `asyncio.to_thread(ctx.run, ...)` did).
pub async fn blocking<T, F>(f: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let ctx = musaic_events::context::current();
    tokio::task::spawn_blocking(move || musaic_events::context::run_in(ctx, f))
        .await
        .map_err(ApiError::internal)
}
