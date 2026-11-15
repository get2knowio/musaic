//! The live transport: blocking HTTPS over `ureq` 3 with rustls (ring
//! provider, Mozilla roots from `webpki-roots`) and gzip.
//!
//! Policy, matching the Python app's `requests` calls unless noted:
//! - Only HTTPS leaves the machine; plain HTTP is refused before any I/O
//!   unless the host is loopback (tests). This is a deliberate change for
//!   AcoustID (the Python app used `http://api.acoustid.org`) and holds for every
//!   host, redirect targets included. A [`Request::public_only`] request
//!   (the cover preview, every cover download and every hop of the CAA
//!   front-cover HEAD) gets no loopback exemption: its
//!   URLs must pass [`crate::preview::check_public_url`] and its resolved
//!   addresses must be public. The app's own requests with the exemption go
//!   to fixed `https` origins (MusicBrainz, AcoustID); only their redirects
//!   or a test's `with_origin` can name a loopback `http` URL.
//! - Redirects (301, 302, 303, 307, 308) are followed for GET and HEAD, up to
//!   30 hops (`requests.models.DEFAULT_REDIRECT_LIMIT`), each hop checked
//!   against the HTTPS rule. A POST is never redirected (the Python app's only
//!   POST, the AcoustID lookup, never is).
//! - Timeouts: `Request::timeout` bounds connecting and each read, which is
//!   what `requests`' `timeout=` means; 30 s when the request sets none.
//! - Bodies above `Request::max_body` (default 32 MiB) are refused.
//! - The User-Agent is whatever the request carries; the clients set the
//!   Python app's. HTTP proxies come from the environment, as with `requests`.
//!
//! # Building the live clients in a host app
//!
//! ```no_run
//! use musaic_net::LiveClients;
//! // Once, at startup. Every client shares one connection pool and the
//! // process-wide limiter (MusicBrainz 1/s, AcoustID 3/s).
//! let clients = LiveClients::new(Some("user-acoustid-key".to_string()));
//! // The API is blocking: from tokio, move a clone into spawn_blocking.
//! let mb = clients.musicbrainz.clone();
//! let _ = std::thread::spawn(move || mb.search_release_groups("Artist", "Album"));
//! ```

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{DefaultConnector, NextTimeout};

use crate::pacing::{Paced, SystemClock, process_limiter};
use crate::{
    AcoustIdClient, CoverArtClient, Method, MusicBrainzClient, Request, Response, RetryPolicy,
    Transport, TransportError,
};

/// `requests.models.DEFAULT_REDIRECT_LIMIT`.
pub const MAX_REDIRECTS: u32 = 30;
/// When a request sets no timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// When a request sets no body limit.
pub const DEFAULT_MAX_BODY: u64 = 32 * 1024 * 1024;

/// Name resolution: the system's, or (tests) every name pinned to one
/// address with some names made to fail. Any failure is `HostNotFound`.
///
/// With `public_only` (the agent [`Request::public_only`] requests use), an
/// answer holding any non-public address ([`crate::preview::is_public_ip`])
/// is refused with [`NonPublic`], which becomes
/// [`TransportError::Refused`]. Pinned (test) resolution judges the address
/// `declared` for the name instead of the pinned one, so a test server on
/// 127.0.0.1 can stand in for a public host or for a private one.
#[derive(Debug)]
struct Resolve {
    pinned: Option<Pinned>,
    system: DefaultResolver,
    public_only: bool,
}

#[derive(Debug, Clone)]
struct Pinned {
    addr: SocketAddr,
    unresolvable: Vec<String>,
    declared: Vec<(String, IpAddr)>,
}

/// The error a `public_only` resolution answers with.
#[derive(Debug)]
struct NonPublic(IpAddr);

impl std::fmt::Display for NonPublic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "resolves to {}, not a public address", self.0)
    }
}

impl std::error::Error for NonPublic {}

fn refuse_non_public(ip: IpAddr) -> ureq::Error {
    ureq::Error::Io(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        NonPublic(ip),
    ))
}

impl Resolver for Resolve {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        config: &ureq::config::Config,
        timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        match &self.pinned {
            None => {
                let addrs = self
                    .system
                    .resolve(uri, config, timeout)
                    .map_err(|_| ureq::Error::HostNotFound)?;
                if self.public_only
                    && let Some(bad) = addrs
                        .iter()
                        .map(SocketAddr::ip)
                        .find(|ip| !crate::preview::is_public_ip(*ip))
                {
                    return Err(refuse_non_public(bad));
                }
                Ok(addrs)
            }
            Some(p) => {
                let host = uri.host().unwrap_or("").to_ascii_lowercase();
                let host = host.trim_start_matches('[').trim_end_matches(']');
                if p.unresolvable.iter().any(|u| u == host) {
                    return Err(ureq::Error::HostNotFound);
                }
                if self.public_only {
                    let judged = p
                        .declared
                        .iter()
                        .find(|(n, _)| n == host)
                        .map_or(p.addr.ip(), |(_, ip)| *ip);
                    if !crate::preview::is_public_ip(judged) {
                        return Err(refuse_non_public(judged));
                    }
                }
                let mut v = self.empty();
                v.push(p.addr);
                Ok(v)
            }
        }
    }
}

/// Blocking HTTPS. Cheap to clone; clones share the connection pool.
#[derive(Clone, Debug)]
pub struct LiveTransport {
    agent: ureq::Agent,
    /// For [`Request::public_only`] requests: resolution refuses
    /// non-public addresses, and no proxy is used (a proxy would resolve
    /// the target itself, out of reach of that check).
    public_agent: ureq::Agent,
}

impl Default for LiveTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveTransport {
    /// System DNS, proxies from the environment.
    pub fn new() -> Self {
        Self::build(None)
    }

    /// Tests: every host name resolves to `addr` (a local server) except
    /// those in `unresolvable`, which fail as DNS errors. No proxy. Nothing
    /// can reach another machine through this transport.
    pub fn with_static_dns(addr: SocketAddr, unresolvable: &[&str]) -> Self {
        Self::with_declared_dns(addr, unresolvable, &[])
    }

    /// [`Self::with_static_dns`], and for [`Request::public_only`]
    /// requests each name in `declared` is judged as if it resolved to the
    /// given address (any other name: as the pinned `addr`, a loopback
    /// address, so refused). The connection still goes to `addr`.
    pub fn with_declared_dns(
        addr: SocketAddr,
        unresolvable: &[&str],
        declared: &[(&str, IpAddr)],
    ) -> Self {
        Self::build(Some(Pinned {
            addr,
            unresolvable: unresolvable
                .iter()
                .map(|h| h.to_ascii_lowercase())
                .collect(),
            declared: declared
                .iter()
                .map(|(h, ip)| (h.to_ascii_lowercase(), *ip))
                .collect(),
        }))
    }

    fn build(pinned: Option<Pinned>) -> Self {
        Self {
            agent: Self::agent(pinned.clone(), false),
            public_agent: Self::agent(pinned, true),
        }
    }

    fn agent(pinned: Option<Pinned>, public_only: bool) -> ureq::Agent {
        let tests = pinned.is_some();
        let mut config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            // Redirects are followed here, hop by hop, so each is checked.
            .max_redirects(0)
            .max_redirects_will_error(false)
            .user_agent(crate::USER_AGENT)
            .timeout_connect(Some(DEFAULT_TIMEOUT))
            .timeout_per_read(Some(DEFAULT_TIMEOUT));
        if tests || public_only {
            config = config.proxy(None);
        }
        let resolver = Resolve {
            pinned,
            system: DefaultResolver::default(),
            public_only,
        };
        ureq::Agent::with_parts(config.build(), DefaultConnector::new(), resolver)
    }

    fn once(&self, method: Method, url: &str, request: &Request) -> Result<Response, Failure> {
        let timeout = request.timeout.unwrap_or(DEFAULT_TIMEOUT);
        let limit = request.max_body.unwrap_or(DEFAULT_MAX_BODY);
        macro_rules! configure {
            ($b:expr) => {{
                let mut b = $b
                    .config()
                    .timeout_connect(Some(timeout))
                    .timeout_per_read(Some(timeout))
                    .build();
                for (k, v) in &request.headers {
                    b = b.header(k.as_str(), v.as_str());
                }
                b
            }};
        }
        let agent = if request.public_only {
            &self.public_agent
        } else {
            &self.agent
        };
        let sent = match method {
            Method::Get => configure!(agent.get(url)).call(),
            Method::Head => configure!(agent.head(url)).call(),
            Method::Post => {
                let body = request.form_body().unwrap_or_default();
                configure!(agent.post(url))
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .send(body.as_bytes())
            }
        };
        let mut resp = sent.map_err(|e| Failure::Head(e, timeout))?;
        let status = resp.status().as_u16();
        let headers = resp
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_string(),
                    String::from_utf8_lossy(v.as_bytes()).into_owned(),
                )
            })
            .collect();
        let body = if method == Method::Head {
            Vec::new()
        } else {
            resp.body_mut()
                .with_config()
                .limit(limit)
                .read_to_vec()
                .map_err(|e| Failure::Body(e, timeout))?
        };
        Ok(Response {
            status,
            headers,
            body,
        })
    }
}

/// A ureq error and whether it came before or during the body.
enum Failure {
    Head(ureq::Error, Duration),
    Body(ureq::Error, Duration),
}

fn map_failure(f: Failure, url: &str, request: &Request) -> TransportError {
    use std::io::ErrorKind as K;
    let (e, timeout, in_body) = match f {
        Failure::Head(e, t) => (e, t, false),
        Failure::Body(e, t) => (e, t, true),
    };
    let url = redact_url(url, request);
    let detail = |e: &dyn std::fmt::Display| request.scrub(&e.to_string());
    let timed_out = || TransportError::Timeout {
        url: url.clone(),
        seconds: timeout.as_secs_f64(),
    };
    match e {
        ureq::Error::Timeout(_) => timed_out(),
        ureq::Error::HostNotFound => TransportError::Dns {
            host: host_of(&url).to_string(),
        },
        ureq::Error::Io(io)
            if io
                .get_ref()
                .is_some_and(|inner| inner.downcast_ref::<NonPublic>().is_some()) =>
        {
            TransportError::Refused {
                url,
                reason: io.get_ref().map(ToString::to_string).unwrap_or_default(),
            }
        }
        ureq::Error::BodyExceedsLimit(limit) => TransportError::BodyTooLarge {
            url,
            limit: request.max_body.unwrap_or(limit),
        },
        ureq::Error::Io(io)
            if io
                .get_ref()
                .is_some_and(|inner| inner.downcast_ref::<rustls::Error>().is_some()) =>
        {
            TransportError::Tls {
                url,
                detail: detail(&io),
            }
        }
        ureq::Error::Io(io) => match io.kind() {
            K::TimedOut | K::WouldBlock => timed_out(),
            K::ConnectionRefused
            | K::ConnectionReset
            | K::ConnectionAborted
            | K::NotConnected
            | K::BrokenPipe
            | K::AddrNotAvailable
                if !in_body =>
            {
                TransportError::Connect {
                    url,
                    detail: detail(&io),
                }
            }
            _ => TransportError::Protocol {
                url,
                detail: detail(&io),
            },
        },
        ureq::Error::ConnectionFailed => TransportError::Connect {
            url,
            detail: "connection failed".into(),
        },
        ureq::Error::Tls(m) => TransportError::Tls {
            url,
            detail: m.to_string(),
        },
        ureq::Error::Rustls(r) => TransportError::Tls {
            url,
            detail: detail(&r),
        },
        ureq::Error::Pem(p) => TransportError::Tls {
            url,
            detail: detail(&p),
        },
        ureq::Error::Protocol(p) => TransportError::Protocol {
            url,
            detail: detail(&p),
        },
        ureq::Error::TooManyRedirects => TransportError::TooManyRedirects {
            url,
            limit: MAX_REDIRECTS,
        },
        other => TransportError::Other(format!("{url}: {}", detail(&other))),
    }
}

fn scheme_and_rest(url: &str) -> (&str, &str) {
    url.split_once("://").unwrap_or(("", url))
}

/// Host without port or user info.
fn host_of(url: &str) -> &str {
    let rest = scheme_and_rest(url).1;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority.rsplit('@').next().unwrap_or("");
    if let Some(v6) = authority.strip_prefix('[') {
        return v6.split(']').next().unwrap_or("");
    }
    authority.split(':').next().unwrap_or("")
}

fn is_loopback(host: &str) -> bool {
    let h = host.to_ascii_lowercase();
    h == "localhost" || h == "::1" || h.starts_with("127.")
}

/// Only HTTPS, or HTTP to this machine.
pub(crate) fn check_scheme(url: &str) -> bool {
    match scheme_and_rest(url).0.to_ascii_lowercase().as_str() {
        "https" => true,
        "http" => is_loopback(host_of(url)),
        _ => false,
    }
}

/// A redirect target relative to the URL that answered.
pub(crate) fn resolve_location(base: &str, location: &str) -> String {
    if location.contains("://") {
        return location.to_string();
    }
    let (scheme, rest) = scheme_and_rest(base);
    if let Some(net) = location.strip_prefix("//") {
        return format!("{scheme}://{net}");
    }
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let origin = format!("{scheme}://{}", &rest[..authority_end]);
    if location.starts_with('/') {
        return format!("{origin}{location}");
    }
    let path = rest[authority_end..].split(['?', '#']).next().unwrap_or("");
    let dir = &path[..path.rfind('/').map_or(0, |i| i + 1)];
    let dir = if dir.is_empty() { "/" } else { dir };
    format!("{origin}{dir}{location}")
}

fn redact_url(url: &str, request: &Request) -> String {
    request.scrub(url)
}

impl Transport for LiveTransport {
    fn send(&self, request: &Request) -> Result<Response, TransportError> {
        let mut url = request.url();
        for hop in 0..=MAX_REDIRECTS {
            if request.public_only {
                // No plain-HTTP loopback exemption, no non-public host.
                if let Err(reason) = crate::preview::check_public_url(&url) {
                    return Err(TransportError::Refused {
                        url: redact_url(&url, request),
                        reason,
                    });
                }
            } else if !check_scheme(&url) {
                return Err(TransportError::InsecureScheme {
                    url: redact_url(&url, request),
                });
            }
            let resp = self
                .once(request.method, &url, request)
                .map_err(|f| map_failure(f, &url, request))?;
            let redirect = matches!(resp.status, 301 | 302 | 303 | 307 | 308)
                && request.method != Method::Post
                && request.follow_redirects;
            let location = resp.header("location").map(str::to_string);
            match (redirect, location) {
                (true, Some(loc)) => {
                    if hop == MAX_REDIRECTS {
                        return Err(TransportError::TooManyRedirects {
                            url: redact_url(&request.url(), request),
                            limit: MAX_REDIRECTS,
                        });
                    }
                    url = resolve_location(&url, &loc);
                }
                _ => return Ok(resp),
            }
        }
        unreachable!("the loop returns on its last hop")
    }
}

/// A live transport behind the process-wide limiter.
pub type LivePaced = Paced<LiveTransport, SystemClock>;

/// Every live client a host app needs, sharing one connection pool and the
/// process-wide limiter. Clone the clients freely; clones share both.
#[derive(Clone)]
pub struct LiveClients {
    pub musicbrainz: MusicBrainzClient<LivePaced>,
    pub cover_art: CoverArtClient<LivePaced>,
    /// `None` when the user has not set an AcoustID key.
    pub acoustid: Option<AcoustIdClient<LivePaced>>,
}

impl LiveClients {
    pub fn new(acoustid_key: Option<String>) -> Self {
        let paced = live_paced(LiveTransport::new());
        Self {
            musicbrainz: MusicBrainzClient::new(paced.clone()),
            cover_art: CoverArtClient::new(paced.clone()),
            acoustid: acoustid_key
                .filter(|k| !k.is_empty())
                .map(|k| AcoustIdClient::new(paced, k)),
        }
    }
}

/// A live transport behind the process-wide limiter ([`process_limiter`]).
pub fn live_paced(transport: LiveTransport) -> LivePaced {
    Paced::shared(transport, process_limiter(), RetryPolicy::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CAA's redirect Locations resolve against the answering URL.
    #[test]
    fn location_resolution() {
        let b = "https://coverartarchive.org/release-group/x/front?a=1";
        assert_eq!(
            resolve_location(b, "https://archive.org/d/y.jpg"),
            "https://archive.org/d/y.jpg"
        );
        assert_eq!(
            resolve_location(b, "//archive.org/z"),
            "https://archive.org/z"
        );
        assert_eq!(
            resolve_location(b, "/abs"),
            "https://coverartarchive.org/abs"
        );
        assert_eq!(
            resolve_location(b, "rel.jpg"),
            "https://coverartarchive.org/release-group/x/rel.jpg"
        );
    }

    /// HTTPS only, plain HTTP to loopback for tests.
    #[test]
    fn scheme_rule() {
        assert!(check_scheme("https://api.acoustid.org/v2/lookup"));
        assert!(!check_scheme("http://api.acoustid.org/v2/lookup"));
        assert!(check_scheme("http://127.0.0.1:8080/x"));
        assert!(check_scheme("http://localhost:8080/x"));
        assert!(check_scheme("http://[::1]:8080/x"));
        assert!(!check_scheme("ftp://example.org/x"));
    }
}
