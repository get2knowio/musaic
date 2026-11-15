//! Deliberate change from the Python version: a Host and Origin check over
//! every route (API, SSE streams, the SPA and the unmatched-route answers).
//!
//! The Python app has no such check: any web page the user visits can fire
//! body-less POSTs at it, and a DNS-rebinding page can read its answers.
//!
//! Rules, in order (the first that fails answers 403 with the Python app's
//! `{"detail": ...}` error shape):
//!
//! 1. Exactly one `Host` header, or none when the request target carries an
//!    authority (absolute form). No `Host` and no authority is refused: a
//!    browser always sends `Host`, so this only turns away hand-made
//!    HTTP/1.0 requests, and on a non-loopback bind it closes the gap a
//!    Host-less request would otherwise open. A deliberate choice of this
//!    build.
//! 2. Every host name the request names (the `Host` header and, in absolute
//!    form, the target's authority) is allowed: `localhost`, `127.0.0.1`,
//!    `[::1]` (any port, ASCII case-insensitive), plus the names in
//!    [`AllowedHosts`] (built from a deliberate non-loopback bind address and
//!    `MUSAIC_ALLOWED_HOSTS`, see `main.rs`).
//! 3. When an `Origin` header is present it must be a single
//!    `http://` / `https://` origin whose host is allowed by rule 2, any
//!    port. `Origin: null` (sandboxed frames, `file:` pages, some redirects)
//!    is refused. The port is not compared with the server's own port so
//!    that the Vite dev server (`http://localhost:5173`, which proxies `/api`
//!    with `changeOrigin: true`: Host becomes `127.0.0.1:8765`, Origin stays
//!    `http://localhost:5173`) keeps working.

use axum::extract::{Request, State};
use axum::http::{HeaderMap, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::http::ApiError;
use crate::state::AppState;

/// The names every build accepts.
pub const LOOPBACK_HOSTS: [&str; 3] = ["localhost", "127.0.0.1", "[::1]"];

pub const HOST_REFUSED: &str = "Forbidden: this server only answers requests addressed to its own host name (Host header refused).";
pub const HOST_MISSING: &str = "Forbidden: a Host header is required.";
pub const ORIGIN_REFUSED: &str =
    "Forbidden: cross-origin requests are not accepted (Origin header refused).";

/// Extra host names a deliberately non-loopback bind accepts, normalised
/// (lower case, IPv6 literals in brackets, no port).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AllowedHosts(Vec<String>);

impl AllowedHosts {
    pub fn none() -> Self {
        AllowedHosts(Vec::new())
    }

    /// From names as a user writes them (`musaic.lan`, `192.168.1.5`,
    /// `fe80::1`, `[fe80::1]`, optionally with a port, which is ignored).
    /// Empty entries and unspecified addresses (`0.0.0.0`, `::`) are
    /// dropped: a browser sends `Host: 0.0.0.0` for `http://0.0.0.0:8765/`,
    /// which some systems route to loopback ("0.0.0.0 day"), so it is never
    /// a valid name for this server.
    pub fn from_names<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut out: Vec<String> = Vec::new();
        for n in names {
            let n = n.as_ref().trim();
            if n.is_empty() {
                continue;
            }
            let name = if n.matches(':').count() > 1 && !n.starts_with('[') {
                // A bare IPv6 literal.
                format!("[{}]", n.to_ascii_lowercase())
            } else {
                match split_host(n) {
                    Some(h) => h,
                    None => continue,
                }
            };
            if is_unspecified(&name) || out.contains(&name) {
                continue;
            }
            out.push(name);
        }
        AllowedHosts(out)
    }

    /// The names a bind address adds: none for loopback or unspecified
    /// addresses, the address itself otherwise.
    pub fn for_bind(host: &str) -> Self {
        let n = host.trim();
        let probe = if n.matches(':').count() > 1 && !n.starts_with('[') {
            format!("[{}]", n.to_ascii_lowercase())
        } else {
            n.to_ascii_lowercase()
        };
        if LOOPBACK_HOSTS.contains(&probe.as_str()) {
            return AllowedHosts::none();
        }
        AllowedHosts::from_names([n])
    }

    pub fn extend(&mut self, other: AllowedHosts) {
        for n in other.0 {
            if !self.0.contains(&n) {
                self.0.push(n);
            }
        }
    }

    pub fn names(&self) -> &[String] {
        &self.0
    }

    /// Whether a normalised host name is accepted.
    pub fn allows(&self, host: &str) -> bool {
        LOOPBACK_HOSTS.contains(&host) || self.0.iter().any(|h| h == host)
    }
}

fn is_unspecified(name: &str) -> bool {
    matches!(name, "0.0.0.0" | "[::]" | "[0:0:0:0:0:0:0:0]")
}

/// `host[:port]` or `[v6][:port]` -> the host, lower case; `None` when it
/// is not of that form (bad port, junk after the bracket, empty).
pub fn split_host(authority: &str) -> Option<String> {
    let a = authority.trim();
    if a.is_empty() || a.contains(['/', '@', '?', '#', ' ', '\\']) {
        return None;
    }
    let (host, port) = if let Some(rest) = a.strip_prefix('[') {
        let end = rest.find(']')?;
        let host = &a[..end + 2];
        let after = &rest[end + 1..];
        let port = match after {
            "" => None,
            p => Some(p.strip_prefix(':')?),
        };
        (host, port)
    } else {
        match a.rsplit_once(':') {
            Some((h, p)) => {
                if h.contains(':') {
                    return None;
                }
                (h, Some(p))
            }
            None => (a, None),
        }
    };
    if let Some(p) = port
        && (p.is_empty() || p.len() > 5 || !p.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    if host.is_empty() {
        return None;
    }
    Some(host.to_ascii_lowercase())
}

/// The host of an `Origin` value, or `None` when it is not a plain
/// `http(s)://authority` origin (this includes `null`).
pub fn origin_host(origin: &str) -> Option<String> {
    let lower = origin.trim().to_ascii_lowercase();
    let rest = lower
        .strip_prefix("http://")
        .or_else(|| lower.strip_prefix("https://"))?;
    split_host(rest)
}

/// The decision for one request: `Ok` or the 403 detail.
pub fn check(
    headers: &HeaderMap,
    target_authority: Option<&str>,
    allowed: &AllowedHosts,
) -> Result<(), &'static str> {
    let mut hosts = headers.get_all(header::HOST).iter();
    let host_header = hosts.next();
    if hosts.next().is_some() {
        return Err(HOST_REFUSED);
    }
    if host_header.is_none() && target_authority.is_none() {
        return Err(HOST_MISSING);
    }
    if let Some(h) = host_header {
        let h = h.to_str().map_err(|_| HOST_REFUSED)?;
        match split_host(h) {
            Some(name) if allowed.allows(&name) => {}
            _ => return Err(HOST_REFUSED),
        }
    }
    if let Some(a) = target_authority {
        match split_host(a) {
            Some(name) if allowed.allows(&name) => {}
            _ => return Err(HOST_REFUSED),
        }
    }
    let mut origins = headers.get_all(header::ORIGIN).iter();
    if let Some(o) = origins.next() {
        if origins.next().is_some() {
            return Err(ORIGIN_REFUSED);
        }
        let o = o.to_str().map_err(|_| ORIGIN_REFUSED)?;
        match origin_host(o) {
            Some(name) if allowed.allows(&name) => {}
            _ => return Err(ORIGIN_REFUSED),
        }
    }
    Ok(())
}

/// The middleware (`axum::middleware::from_fn_with_state`).
pub async fn host_origin_guard(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let authority = req.uri().authority().map(|a| a.as_str().to_string());
    match check(req.headers(), authority.as_deref(), &st.0.allowed_hosts) {
        Ok(()) => next.run(req).await,
        Err(detail) => ApiError::new(403, detail).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Host parsing.
    #[test]
    fn split_host_forms() {
        assert_eq!(split_host("LocalHost:8765").as_deref(), Some("localhost"));
        assert_eq!(split_host("127.0.0.1").as_deref(), Some("127.0.0.1"));
        assert_eq!(split_host("[::1]:8765").as_deref(), Some("[::1]"));
        assert_eq!(split_host("[::1]").as_deref(), Some("[::1]"));
        assert_eq!(split_host("::1"), None);
        assert_eq!(split_host("localhost:"), None);
        assert_eq!(split_host("localhost:abc"), None);
        assert_eq!(split_host("[::1]x"), None);
        assert_eq!(split_host("a@localhost"), None);
        assert_eq!(split_host(""), None);
    }

    // What a bind address and MUSAIC_ALLOWED_HOSTS add.
    #[test]
    fn allowed_hosts_from_bind_and_names() {
        assert_eq!(AllowedHosts::for_bind("127.0.0.1"), AllowedHosts::none());
        assert_eq!(AllowedHosts::for_bind("::1"), AllowedHosts::none());
        assert_eq!(AllowedHosts::for_bind("0.0.0.0"), AllowedHosts::none());
        assert_eq!(AllowedHosts::for_bind("::"), AllowedHosts::none());
        assert_eq!(
            AllowedHosts::for_bind("192.168.1.5").names(),
            ["192.168.1.5"]
        );
        assert_eq!(AllowedHosts::for_bind("fe80::1").names(), ["[fe80::1]"]);
        let a =
            AllowedHosts::from_names(["Musaic.LAN:8765", " ", "0.0.0.0", "fe80::2", "musaic.lan"]);
        assert_eq!(a.names(), ["musaic.lan", "[fe80::2]"]);
    }

    // Origin parsing; `null` and non-http schemes have no host.
    #[test]
    fn origin_forms() {
        assert_eq!(
            origin_host("http://localhost:5173").as_deref(),
            Some("localhost")
        );
        assert_eq!(origin_host("HTTPS://[::1]").as_deref(), Some("[::1]"));
        assert_eq!(origin_host("null"), None);
        assert_eq!(origin_host("file://"), None);
        assert_eq!(origin_host("http://localhost/path"), None);
        assert_eq!(origin_host("chrome-extension://abc"), None);
    }
}
