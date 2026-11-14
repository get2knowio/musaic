//! The listening address from `--host` / `--port` (uvicorn's `host`,
//! `port`). An IP literal is used as is: IPv6 with or without brackets
//! (`::1`, `[::1]`), so `--host ::1` binds `[::1]:8765` (the previous
//! `format!("{host}:{port}").parse()` read `::1:8765` as an address without
//! a port and refused it). Any other host name is resolved by the system
//! resolver, as uvicorn does.

use std::net::{IpAddr, SocketAddr, ToSocketAddrs};

/// The IP literal in `host`, brackets stripped.
pub fn ip_literal(host: &str) -> Option<IpAddr> {
    let h = host.trim();
    let h = h
        .strip_prefix('[')
        .and_then(|x| x.strip_suffix(']'))
        .unwrap_or(h);
    h.parse().ok()
}

/// Every address to try binding, in resolver order.
pub fn bind_addrs(host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
    if let Some(ip) = ip_literal(host) {
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    let h = host.trim();
    if h.is_empty() {
        return Err("empty host".into());
    }
    let addrs: Vec<SocketAddr> = (h, port)
        .to_socket_addrs()
        .map_err(|e| format!("cannot resolve {h}: {e}"))?
        .collect();
    if addrs.is_empty() {
        return Err(format!("{h} resolves to no address"));
    }
    Ok(addrs)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Startup: `--host ::1` and its bracketed form bind the IPv6
    // loopback; IPv4 literals unchanged; never a resolver call for a literal.
    #[test]
    fn ip_literals_including_bare_ipv6() {
        for (host, want) in [
            ("::1", "[::1]:8765"),
            ("[::1]", "[::1]:8765"),
            (" ::1 ", "[::1]:8765"),
            ("::", "[::]:8765"),
            ("127.0.0.1", "127.0.0.1:8765"),
            ("0.0.0.0", "0.0.0.0:8765"),
            ("fe80::1", "[fe80::1]:8765"),
        ] {
            let got = bind_addrs(host, 8765).unwrap();
            assert_eq!(got.len(), 1, "{host}");
            assert_eq!(got[0].to_string(), want, "{host}");
        }
        // The old construction refused exactly this.
        assert!(format!("{}:{}", "::1", 8765).parse::<SocketAddr>().is_err());
    }

    #[test]
    fn empty_host_is_an_error() {
        assert!(bind_addrs("  ", 1).is_err());
    }
}
