//! The public-image fetch: every remote picture the server fetches on the
//! user's or a model's say-so goes through [`fetch_public_image`].
//!
//! Two callers share it:
//!
//! - the cover preview (the desktop CSP allows `img-src 'self'` only, so the
//!   backend fetches the picture and serves it same-origin; not a code path
//!   of the Python app, the Python app's browser loaded the remote URL
//!   itself), [`fetch_preview`];
//! - every cover download (a deliberate change): apply with a
//!   `cover_url`, a check fix's cover, the AI cover verification, all
//!   through [`crate::CoverArtClient::download`]. The Python app downloaded any
//!   URL, plain HTTP and private hosts included, following 30 redirects
//!   (ai.py:1483-1531, tags.py:392); this build applies the preview rule.
//!
//! The rule, checked on every hop:
//!
//! - `https` only (no plain-HTTP loopback exemption, unlike the resolver's
//!   transport rule), default port only, no user info;
//! - the host is a public DNS name (not `localhost`, `*.localhost`,
//!   `*.local`, `*.internal`, `*.home.arpa`, `*.lan`, a single label, or a
//!   numeric last label that a resolver could read as an IPv4 address in
//!   short or hex form) or a public IP literal ([`is_public_ip`]);
//! - at most [`PREVIEW_MAX_REDIRECTS`] redirects, each target checked again;
//! - the request is [`Request::public_only`], so the live transport also
//!   refuses a name that *resolves* to a non-public address, at connect
//!   time;
//! - the caller's timeout for connecting and each read, at most
//!   [`PREVIEW_MAX_BYTES`] of body;
//! - a 2xx whose `Content-Type` is one of [`PREVIEW_TYPES`] (no SVG, no
//!   absent type) and whose first bytes are that format's signature.
//!
//! The rule is checked on the URL text before the transport is asked, so
//! it holds whatever transport the app was built with (live, replay, or a
//! test's own). Nothing relaxes it: there is no loopback, private-host or
//! plain-HTTP exception for any caller. Tests that need an answer use a
//! [`Transport`] of their own that answers public `https` URLs.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use crate::{
    COVER_ACCEPT, COVER_MAX_BYTES, Method, Request, Transport, TransportError, USER_AGENT,
};

/// Largest preview served (the cover download's cap).
pub const PREVIEW_MAX_BYTES: u64 = COVER_MAX_BYTES;
/// Connect and per-read timeout of each hop.
pub const PREVIEW_TIMEOUT: Duration = Duration::from_secs(10);
/// Redirects followed (Cover Art Archive answers with two: CAA to
/// archive.org to a storage node).
pub const PREVIEW_MAX_REDIRECTS: u32 = 5;
/// The image types served, each with its file signature. AVIF is not one
/// (deliberately): the JPEG normalization cannot decode it, so an AVIF cover
/// would be saved unconverted under a `.jpg` name.
pub const PREVIEW_TYPES: [&str; 4] = ["image/jpeg", "image/png", "image/gif", "image/webp"];

/// Why no preview was served. `Display` never carries the body.
#[derive(Debug, thiserror::Error)]
pub enum PreviewError {
    #[error("refused: {0}")]
    Refused(String),
    #[error("more than {0} redirects")]
    TooManyRedirects(u32),
    #[error("HTTP {0}")]
    Status(u16),
    #[error("not an image (content-type {0:?})")]
    ContentType(String),
    #[error("the body is not a {0} image")]
    NotAnImage(String),
    #[error("image larger than {0} bytes")]
    TooLarge(u64),
    #[error(transparent)]
    Transport(TransportError),
}

/// A fetched preview: the bytes and their (allow-listed) type.
#[derive(Clone, PartialEq, Eq)]
pub struct Preview {
    pub bytes: Vec<u8>,
    pub mime: String,
}

impl std::fmt::Debug for Preview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Preview")
            .field("bytes", &self.bytes.len())
            .field("mime", &self.mime)
            .finish()
    }
}

/// An address on the public internet: not unspecified, loopback, private
/// (RFC 1918, unique local), link-local, shared (CGNAT 100.64/10),
/// benchmarking, documentation, reserved, broadcast or multicast; IPv6
/// forms that embed an IPv4 address (mapped, compatible, 6to4, NAT64) are
/// judged by that address.
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || o[0] == 0
        || (o[0] == 100 && (o[1] & 0xc0) == 64)
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)
        || (o[0] == 198 && (o[1] & 0xfe) == 18)
        || o[0] >= 240)
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_v4(v4);
    }
    // 6to4 (2002::/16) and NAT64 (64:ff9b::/96) carry an IPv4 address.
    if s[0] == 0x2002 {
        return is_public_v4(Ipv4Addr::new(
            (s[1] >> 8) as u8,
            s[1] as u8,
            (s[2] >> 8) as u8,
            s[2] as u8,
        ));
    }
    if s[0] == 0x64 && s[1] == 0xff9b && s[2..6] == [0, 0, 0, 0] {
        return is_public_v4(Ipv4Addr::new(
            (s[6] >> 8) as u8,
            s[6] as u8,
            (s[7] >> 8) as u8,
            s[7] as u8,
        ));
    }
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        // IPv4-compatible (::a.b.c.d, deprecated) and the rest of ::/96.
        || s[..6] == [0, 0, 0, 0, 0, 0]
        || (s[0] & 0xfe00) == 0xfc00 // unique local fc00::/7
        || (s[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
        || (s[0] & 0xffc0) == 0xfec0 // site-local fec0::/10
        || (s[0] == 0x2001 && s[1] == 0x0db8) // documentation
        || s[0] == 0x0100 && s[1..4] == [0, 0, 0]) // discard 100::/64
}

/// Names that never leave the local network.
const LOCAL_SUFFIXES: [&str; 5] = [".localhost", ".local", ".internal", ".home.arpa", ".lan"];

/// `Ok(host)` (lower case, IPv6 without brackets) when `url` may be fetched
/// for a preview; `Err(reason)` otherwise.
pub fn check_public_url(url: &str) -> Result<String, String> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| "not an absolute URL".to_string())?;
    if !scheme.eq_ignore_ascii_case("https") {
        return Err(format!("scheme {scheme:?} is not https"));
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return Err("user info in the URL".into());
    }
    if authority.contains('\\') || authority.contains('%') {
        return Err("unusual characters in the host".into());
    }
    let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
        let (h, after) = v6
            .split_once(']')
            .ok_or_else(|| "unterminated IPv6 literal".to_string())?;
        (h, after.strip_prefix(':'))
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (authority, None),
        }
    };
    if let Some(p) = port
        && !(p.is_empty() || p == "443")
    {
        return Err(format!("port {p} is not 443"));
    }
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() {
        return Err("no host".into());
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return if is_public_ip(ip) {
            Ok(host)
        } else {
            Err(format!("{ip} is not a public address"))
        };
    }
    if host.contains(':') {
        return Err("bad host".into());
    }
    if host == "localhost" || LOCAL_SUFFIXES.iter().any(|s| host.ends_with(s)) {
        return Err(format!("{host} is a local name"));
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 || labels.iter().any(|l| l.is_empty()) {
        return Err(format!("{host} is not a public host name"));
    }
    let last = labels[labels.len() - 1];
    // `127.1`, `0x7f.0.0.1`, `2130706433`: a resolver may read these as
    // IPv4 addresses. A real top-level domain is never numeric.
    if last.bytes().all(|b| b.is_ascii_digit())
        || (last.starts_with("0x") && last[2..].bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(format!("{host} looks like a numeric address"));
    }
    if !host
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
    {
        return Err(format!("{host} is not a plain host name"));
    }
    Ok(host)
}

/// Whether `body` starts with the signature of `mime`.
fn signature_matches(mime: &str, body: &[u8]) -> bool {
    match mime {
        "image/jpeg" => body.starts_with(&[0xff, 0xd8, 0xff]),
        "image/png" => body.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/gif" => body.starts_with(b"GIF87a") || body.starts_with(b"GIF89a"),
        "image/webp" => body.len() >= 12 && &body[..4] == b"RIFF" && &body[8..12] == b"WEBP",
        _ => false,
    }
}

/// The request for one hop of a preview.
pub fn preview_request(url: &str) -> Option<Request> {
    public_image_request(url, PREVIEW_TIMEOUT)
}

/// The request for one hop of a public-image fetch: no redirects followed
/// by the transport (the fetch follows them, checking each), public
/// addresses only, `timeout` for connecting and each read.
pub fn public_image_request(url: &str, timeout: Duration) -> Option<Request> {
    Some(
        Request::from_url(Method::Get, url)?
            .header("User-Agent", USER_AGENT)
            .header("Accept", COVER_ACCEPT)
            .timeout(timeout)
            // One byte over the cap: "too large" without an unbounded read.
            .max_body(PREVIEW_MAX_BYTES + 1)
            .no_redirects()
            .public_only(),
    )
}

/// Fetch `url` for a preview, following redirects hop by hop (module docs).
pub fn fetch_preview<T: Transport>(transport: &T, url: &str) -> Result<Preview, PreviewError> {
    fetch_public_image(transport, url, PREVIEW_TIMEOUT)
}

/// Fetch the image at `url` under the rule in the module docs, following
/// redirects hop by hop, each hop's request made with `timeout`.
pub fn fetch_public_image<T: Transport>(
    transport: &T,
    url: &str,
    timeout: Duration,
) -> Result<Preview, PreviewError> {
    let mut url = url.to_string();
    for hop in 0..=PREVIEW_MAX_REDIRECTS {
        check_public_url(&url).map_err(PreviewError::Refused)?;
        let req = public_image_request(&url, timeout)
            .ok_or_else(|| PreviewError::Refused("not an absolute URL".into()))?;
        let resp = match transport.send(&req) {
            Ok(r) => r,
            Err(TransportError::BodyTooLarge { .. }) => {
                return Err(PreviewError::TooLarge(PREVIEW_MAX_BYTES));
            }
            Err(TransportError::Refused { reason, .. }) => {
                return Err(PreviewError::Refused(reason));
            }
            Err(e) => return Err(PreviewError::Transport(e)),
        };
        if matches!(resp.status, 301 | 302 | 303 | 307 | 308) {
            let Some(loc) = resp.header("location") else {
                return Err(PreviewError::Status(resp.status));
            };
            if hop == PREVIEW_MAX_REDIRECTS {
                return Err(PreviewError::TooManyRedirects(PREVIEW_MAX_REDIRECTS));
            }
            url = crate::live::resolve_location(&url, loc);
            continue;
        }
        if !(200..300).contains(&resp.status) {
            return Err(PreviewError::Status(resp.status));
        }
        let ct = resp
            .header("content-type")
            .map(|c| {
                c.split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase()
            })
            .unwrap_or_default();
        if !PREVIEW_TYPES.contains(&ct.as_str()) {
            return Err(PreviewError::ContentType(ct));
        }
        if resp.body.len() as u64 > PREVIEW_MAX_BYTES {
            return Err(PreviewError::TooLarge(PREVIEW_MAX_BYTES));
        }
        if !signature_matches(&ct, &resp.body) {
            return Err(PreviewError::NotAnImage(ct));
        }
        return Ok(Preview {
            bytes: resp.body,
            mime: ct,
        });
    }
    unreachable!("the loop returns on its last hop")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_and_private_addresses() {
        for ip in [
            "93.184.216.34",
            "207.241.224.2",
            "2606:4700::1111",
            "1.1.1.1",
        ] {
            assert!(is_public_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "0.1.2.3",
            "255.255.255.255",
            "224.0.0.1",
            "192.0.2.1",
            "198.18.0.1",
            "240.0.0.1",
            "::",
            "::1",
            "fe80::1",
            "fc00::1",
            "fd12:3456::1",
            "fec0::1",
            "ff02::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::127.0.0.1",
            "2002:7f00:1::",
            "2002:c0a8:101::",
            "64:ff9b::a00:1",
            "100::1",
        ] {
            assert!(!is_public_ip(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn url_rule() {
        for u in [
            "https://coverartarchive.org/release-group/x/front",
            "https://ia800.us.archive.org:443/x.jpg",
            "https://i.ebayimg.com/images/g/a.jpg?x=1",
            "https://93.184.216.34/x.jpg",
            "https://[2606:4700::1111]/x.jpg",
            "HTTPS://Example.COM./a",
        ] {
            assert!(
                check_public_url(u).is_ok(),
                "{u}: {:?}",
                check_public_url(u)
            );
        }
        for u in [
            "http://coverartarchive.org/x",
            "ftp://example.com/x",
            "https://localhost/x",
            "https://LOCALHOST./x",
            "https://foo.localhost/x",
            "https://printer.local/x",
            "https://nas.home.arpa/x",
            "https://metadata.google.internal/x",
            "https://router.lan/x",
            "https://intranet/x",
            "https://127.0.0.1/x",
            "https://127.1/x",
            "https://2130706433/x",
            "https://0x7f.0.0.1/x",
            "https://10.0.0.1/x",
            "https://169.254.169.254/latest/meta-data",
            "https://[::1]/x",
            "https://[fe80::1]/x",
            "https://[::ffff:192.168.0.1]/x",
            "https://user@example.com/x",
            "https://example.com@127.0.0.1/x",
            "https://example.com:8443/x",
            "https://example.com:22/x",
            "https://exa%6dple.com/x",
            "https:///x",
            "not a url",
        ] {
            assert!(check_public_url(u).is_err(), "{u}");
        }
    }

    #[test]
    fn signatures() {
        assert!(signature_matches("image/jpeg", &[0xff, 0xd8, 0xff, 0xe0]));
        assert!(signature_matches("image/png", b"\x89PNG\r\n\x1a\n...."));
        assert!(signature_matches("image/gif", b"GIF89a..."));
        assert!(signature_matches("image/webp", b"RIFF\0\0\0\0WEBPVP8 "));
        // AVIF is not accepted.
        assert!(!PREVIEW_TYPES.contains(&"image/avif"));
        assert!(!signature_matches("image/avif", b"\0\0\0\x1cftypavif"));
        assert!(!signature_matches("image/jpeg", b"<html>"));
        assert!(!signature_matches("image/png", &[0xff, 0xd8, 0xff]));
        assert!(!signature_matches("image/svg+xml", b"<svg"));
    }
}
