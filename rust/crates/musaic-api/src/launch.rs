//! The per-launch token, in musaic-api so one layer
//! decides who may call the API (it was the desktop shell's own layer).
//! Off for the plain server binary
//! ([`crate::router`]); on for the desktop app
//! ([`crate::router_with`] with a [`LaunchGate`], which
//! [`crate::loopback::serve_loopback`] builds when asked).
//!
//! A random 256-bit token is minted per launch. The window's first
//! navigation is `GET /__musaic/launch?t=<token>`, which sets the session
//! cookie `musaic_launch=<token>; HttpOnly; SameSite=Strict; Path=/` and
//! moves the page to `/` with a `<meta http-equiv="refresh">` (no script;
//! the follow-up request is initiated by the 127.0.0.1 document itself, so
//! the Strict cookie is sent). The launch URL works once. Every other
//! request needs the cookie, else 403 with the Python app's `{"detail": ...}`
//! shape.
//!
//! Order, outermost first: Host / Origin check (the same
//! [`crate::host_check::check`] the router runs inside) → launch token →
//! Starlette-style routing (HEAD 405, decoded paths) → routes. The Host check
//! comes first so a DNS-rebinding page gets the Host refusal whatever
//! cookie it holds, and the launch URL itself is only reachable under an
//! allowed Host (and, stricter, only under the exact `127.0.0.1:<port>`
//! the window opens).
//!
//! What this adds over the Host / Origin check: another local user or
//! process that can reach 127.0.0.1:<port> but cannot read the window's
//! cookie gets nothing.

use std::sync::atomic::{AtomicBool, Ordering};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::http::ApiError;

pub const COOKIE: &str = "musaic_launch";
pub const LAUNCH_PATH: &str = "/__musaic/launch";

/// 403 detail for a request without the launch cookie.
pub const NO_TOKEN: &str = "Forbidden: this server only answers the Musaic window that launched it (launch cookie missing).";

/// 256 random bits as lowercase hex.
pub fn random_token() -> Result<String, String> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).map_err(|e| format!("no OS randomness: {e}"))?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// Constant-time equality.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The per-launch token for one bound port.
#[derive(Debug)]
pub struct LaunchGate {
    host: String,
    origin: String,
    token: String,
    launch_used: AtomicBool,
}

impl LaunchGate {
    /// `port` is the bound loopback port.
    pub fn new(port: u16, token: String) -> LaunchGate {
        LaunchGate {
            host: format!("127.0.0.1:{port}"),
            origin: format!("http://127.0.0.1:{port}"),
            token,
            launch_used: AtomicBool::new(false),
        }
    }

    /// A gate with a fresh random token.
    pub fn random(port: u16) -> Result<LaunchGate, String> {
        Ok(LaunchGate::new(port, random_token()?))
    }

    /// `http://127.0.0.1:<port>`.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// The URL the window opens first.
    pub fn launch_url(&self) -> String {
        format!("{}{LAUNCH_PATH}?t={}", self.origin, self.token)
    }

    /// Exactly one `Host` header, equal to `127.0.0.1:<port>`.
    pub fn host_is_self(&self, h: &HeaderMap) -> bool {
        let mut it = h.get_all(header::HOST).iter();
        matches!((it.next(), it.next()), (Some(v), None) if v.as_bytes() == self.host.as_bytes())
    }

    /// The `Origin` header is this server's own origin.
    pub fn origin_is_self(&self, h: &HeaderMap) -> bool {
        h.get(header::ORIGIN)
            .is_some_and(|o| o.as_bytes() == self.origin.as_bytes())
    }

    /// The one decision: does this request carry the launch cookie? (The
    /// desktop shell's own endpoints ask this too.)
    pub fn authorizes(&self, h: &HeaderMap) -> bool {
        h.get_all(header::COOKIE).iter().any(|v| {
            v.to_str().is_ok_and(|s| {
                s.split(';').any(|kv| {
                    kv.trim()
                        .strip_prefix(COOKIE)
                        .and_then(|r| r.strip_prefix('='))
                        .is_some_and(|t| ct_eq(t.as_bytes(), self.token.as_bytes()))
                })
            })
        })
    }

    fn launch(&self, req: &Request) -> Response {
        if req.method() != Method::GET || !self.host_is_self(req.headers()) {
            return ApiError::new(403, "Forbidden").into_response();
        }
        let given = req
            .uri()
            .query()
            .unwrap_or("")
            .split('&')
            .find_map(|kv| kv.strip_prefix("t="))
            .unwrap_or("");
        if !ct_eq(given.as_bytes(), self.token.as_bytes()) {
            return ApiError::new(403, "Forbidden").into_response();
        }
        if self.launch_used.swap(true, Ordering::SeqCst) {
            return ApiError::new(410, "This launch link was already used.").into_response();
        }
        let html = "<!doctype html><meta charset=\"utf-8\"><meta http-equiv=\"refresh\" content=\"0;url=/\"><title>Musaic</title>";
        let mut r = Response::new(Body::from(html));
        let h = r.headers_mut();
        h.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        h.insert(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        );
        if let Ok(v) = HeaderValue::from_str(&format!(
            "{COOKIE}={}; Path=/; HttpOnly; SameSite=Strict",
            self.token
        )) {
            h.insert(header::SET_COOKIE, v);
        }
        r
    }
}

/// The middleware (`from_fn_with_state(Arc<LaunchGate>, launch_guard)`).
pub async fn launch_guard(
    State(gate): State<std::sync::Arc<LaunchGate>>,
    req: Request,
    next: Next,
) -> Response {
    if req.uri().path() == LAUNCH_PATH {
        return gate.launch(&req);
    }
    if !gate.authorizes(req.headers()) {
        return ApiError::new(403, NO_TOKEN).into_response();
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    // The cookie parser takes the token among other cookies,
    // never a prefix or a near miss.
    #[test]
    fn cookie_forms() {
        let g = LaunchGate::new(1, "abc".into());
        let h = |v: &str| {
            let mut m = HeaderMap::new();
            m.insert(header::COOKIE, HeaderValue::from_str(v).unwrap());
            m
        };
        assert!(g.authorizes(&h("musaic_launch=abc")));
        assert!(g.authorizes(&h("x=1; musaic_launch=abc; y=2")));
        assert!(!g.authorizes(&h("musaic_launch=ab")));
        assert!(!g.authorizes(&h("musaic_launch=abcd")));
        assert!(!g.authorizes(&h("xmusaic_launch=abc")));
        assert!(!g.authorizes(&HeaderMap::new()));
    }

    #[test]
    fn random_tokens_are_256_bit_hex_and_differ() {
        let a = random_token().unwrap();
        let b = random_token().unwrap();
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }
}
