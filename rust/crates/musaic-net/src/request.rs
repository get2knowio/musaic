//! Requests, and the key a recorded response is stored under.

use std::fmt;
use std::time::Duration;

use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Head,
    Post,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Head => "HEAD",
            Method::Post => "POST",
        }
    }
}

/// An outbound request. `Debug` is hand-written so the AcoustID key (a form
/// field) never reaches a log line.
#[derive(Clone, PartialEq, Eq)]
pub struct Request {
    pub method: Method,
    /// Scheme and host, e.g. `https://musicbrainz.org`.
    pub origin: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    /// `application/x-www-form-urlencoded` body fields, for POST.
    pub form: Option<Vec<(String, String)>>,
    pub headers: Vec<(String, String)>,
    /// Connect timeout and per-read timeout, the meaning of `requests`'
    /// `timeout=` (a live transport's default applies when `None`). Not part
    /// of the replay key.
    pub timeout: Option<Duration>,
    /// Largest response body accepted, in bytes (the transport's default
    /// applies when `None`). Not part of the replay key.
    pub max_body: Option<u64>,
    /// Follow 3xx redirects (the default). `false`: the transport returns
    /// the redirect itself, so the caller can check every hop (the cover
    /// preview, [`crate::preview`]). Not part of the replay key.
    pub follow_redirects: bool,
    /// Only public internet addresses: the live transport refuses (as
    /// [`crate::TransportError::Refused`]) any URL that is not HTTPS to a
    /// public host name or address, and any host name that resolves to a
    /// loopback, private, link-local or otherwise non-public address
    /// (checked when the connection's address is resolved, so a DNS answer
    /// cannot change between the check and the connect). Not part of the
    /// replay key.
    pub public_only: bool,
}

impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let redact = |pairs: &[(String, String)]| -> Vec<(String, String)> {
            pairs
                .iter()
                .map(|(k, v)| {
                    let v = if SECRET_PARAMS.contains(&k.as_str()) {
                        REDACTED.to_string()
                    } else {
                        v.clone()
                    };
                    (k.clone(), v)
                })
                .collect()
        };
        f.debug_struct("Request")
            .field("method", &self.method)
            .field("url", &self.redacted_url())
            .field("form", &self.form.as_deref().map(redact))
            .field("headers", &self.headers)
            .field("timeout", &self.timeout)
            .field("max_body", &self.max_body)
            .field("follow_redirects", &self.follow_redirects)
            .field("public_only", &self.public_only)
            .finish()
    }
}

/// Query or form parameters that hold a secret. They never take part in a
/// replay key and never appear in a recording (the AcoustID application key).
const SECRET_PARAMS: [&str; 1] = ["client"];
const REDACTED: &str = "<REDACTED>";

/// `urllib.parse.quote_plus`: unreserved characters stay, a space becomes
/// `+`, every other byte is `%XX` in upper case.
fn quote_plus(s: &str, out: &mut String) {
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
}

/// `urlencode(pairs)`.
pub(crate) fn urlencode<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
    let mut out = String::new();
    for (i, (k, v)) in pairs.into_iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        quote_plus(k, &mut out);
        out.push('=');
        quote_plus(v, &mut out);
    }
    out
}

/// Pairs with secrets replaced, sorted the way the recorder sorts them.
fn canonical_pairs(pairs: &[(String, String)]) -> String {
    let mut redacted: Vec<(&str, &str)> = pairs
        .iter()
        .map(|(k, v)| {
            (
                k.as_str(),
                if SECRET_PARAMS.contains(&k.as_str()) {
                    REDACTED
                } else {
                    v.as_str()
                },
            )
        })
        .collect();
    redacted.sort();
    urlencode(redacted)
}

/// `urllib.parse.unquote_plus`, bytes taken as UTF-8 (lossy).
fn unquote_plus(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                let hex = |c: u8| (c as char).to_digit(16);
                match (hex(b[i + 1]), hex(b[i + 2])) {
                    (Some(h), Some(l)) => {
                        out.push((h * 16 + l) as u8);
                        i += 2;
                    }
                    _ => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Replace each secret (raw and `quote_plus`-encoded) in `text`.
pub(crate) fn scrub_secrets(text: &str, secrets: &[&str]) -> String {
    let mut out = text.to_string();
    for s in secrets.iter().filter(|s| !s.is_empty()) {
        out = out.replace(s, REDACTED);
        let mut enc = String::new();
        quote_plus(s, &mut enc);
        if enc != *s {
            out = out.replace(&enc, REDACTED);
        }
    }
    out
}

fn sha256_hex(s: &str) -> String {
    Sha256::digest(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl Request {
    pub fn get(origin: &str, path: &str) -> Self {
        Self {
            method: Method::Get,
            origin: origin.into(),
            path: path.into(),
            query: vec![],
            form: None,
            headers: vec![],
            timeout: None,
            max_body: None,
            follow_redirects: true,
            public_only: false,
        }
    }

    pub fn head(origin: &str, path: &str) -> Self {
        Self {
            method: Method::Head,
            ..Self::get(origin, path)
        }
    }

    /// A request for an absolute URL as found in data (a cover URL): origin
    /// and path kept verbatim, the query split into decoded pairs so `url()`
    /// re-encodes it the way `requests` would. A fragment is dropped.
    pub fn from_url(method: Method, url: &str) -> Option<Self> {
        let url = url.split('#').next().unwrap_or("");
        let (scheme, rest) = url.split_once("://")?;
        let (authority_path, query) = match rest.split_once('?') {
            Some((a, q)) => (a, Some(q)),
            None => (rest, None),
        };
        let (host, path) = match authority_path.find('/') {
            Some(i) => (&authority_path[..i], &authority_path[i..]),
            None => (authority_path, ""),
        };
        if host.is_empty() || scheme.is_empty() {
            return None;
        }
        let mut req = Self::get(&format!("{scheme}://{host}"), path);
        req.method = method;
        if let Some(q) = query {
            req.query = q
                .split('&')
                .filter(|p| !p.is_empty())
                .map(|p| {
                    let (k, v) = p.split_once('=').unwrap_or((p, ""));
                    (unquote_plus(k), unquote_plus(v))
                })
                .collect();
        }
        Some(req)
    }

    pub fn timeout(mut self, d: Duration) -> Self {
        self.timeout = Some(d);
        self
    }

    pub fn max_body(mut self, bytes: u64) -> Self {
        self.max_body = Some(bytes);
        self
    }

    /// Return a 3xx as it is instead of following it ([`Self::follow_redirects`]).
    pub fn no_redirects(mut self) -> Self {
        self.follow_redirects = false;
        self
    }

    /// Public internet only ([`Self::public_only`]).
    pub fn public_only(mut self) -> Self {
        self.public_only = true;
        self
    }

    /// Values of secret parameters (query or form) this request carries.
    pub fn secret_values(&self) -> Vec<&str> {
        self.query
            .iter()
            .chain(self.form.iter().flatten())
            .filter(|(k, v)| SECRET_PARAMS.contains(&k.as_str()) && !v.is_empty())
            .map(|(_, v)| v.as_str())
            .collect()
    }

    /// `text` with every secret this request carries replaced, raw or
    /// URL-encoded. Used on any text that might echo the request.
    pub fn scrub(&self, text: &str) -> String {
        scrub_secrets(text, &self.secret_values())
    }

    /// The form body as sent: fields in the order given, `urlencode`d.
    pub fn form_body(&self) -> Option<String> {
        self.form
            .as_ref()
            .map(|f| urlencode(f.iter().map(|(k, v)| (k.as_str(), v.as_str()))))
    }

    pub fn post_form(origin: &str, path: &str, form: Vec<(String, String)>) -> Self {
        Self {
            method: Method::Post,
            form: Some(form),
            ..Self::get(origin, path)
        }
    }

    pub fn query(mut self, key: &str, value: &str) -> Self {
        self.query.push((key.into(), value.into()));
        self
    }

    pub fn header(mut self, key: &str, value: &str) -> Self {
        self.headers.push((key.into(), value.into()));
        self
    }

    pub fn host(&self) -> &str {
        self.origin
            .split_once("://")
            .map_or(self.origin.as_str(), |(_, h)| h)
    }

    /// The URL as sent: query in the order given, values as they are.
    pub fn url(&self) -> String {
        let q = urlencode(self.query.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        format!(
            "{}{}{}{}",
            self.origin,
            self.path,
            if q.is_empty() { "" } else { "?" },
            q
        )
    }

    /// The URL with secrets redacted, safe to log.
    pub fn redacted_url(&self) -> String {
        let q = urlencode(self.query.iter().map(|(k, v)| {
            (
                k.as_str(),
                if SECRET_PARAMS.contains(&k.as_str()) {
                    REDACTED
                } else {
                    v.as_str()
                },
            )
        }));
        format!(
            "{}{}{}{}",
            self.origin,
            self.path,
            if q.is_empty() { "" } else { "?" },
            q
        )
    }

    /// The key a recorded response is stored under in replay data:
    /// SHA-256 of `METHOD canonical-url`, plus a line with the SHA-256 of the
    /// canonical form body when there is one. Canonical means lower-cased
    /// origin, parameters sorted, secrets redacted.
    pub fn replay_key(&self) -> String {
        let q = canonical_pairs(&self.query);
        let path = if self.path.is_empty() {
            "/"
        } else {
            self.path.as_str()
        };
        let mut material = format!(
            "{} {}{}{}{}",
            self.method.as_str(),
            self.origin.to_lowercase(),
            path,
            if q.is_empty() { "" } else { "?" },
            q
        );
        if let Some(form) = &self.form {
            let body = canonical_pairs(form);
            if !body.is_empty() {
                material.push('\n');
                material.push_str(&sha256_hex(&body));
            }
        }
        sha256_hex(&material)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    /// Header value by case-insensitive name.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}
