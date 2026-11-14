//! Where the window may go.
//!
//! * The backend origin (`http://127.0.0.1:<port>`) and `blob:` URLs minted
//!   by it: navigate in the window.
//! * The shell's own boot page (`tauri://localhost` on macOS/Linux,
//!   `http(s)://tauri.localhost` on Windows): navigate.
//! * `about:blank` / `about:srcdoc`: allowed (they load nothing).
//! * An `https` URL on any public host, default port, no user info
//!   ([`external_allowed`]), `target="_blank"` or not: opened in the system
//!   browser (`tauri-plugin-opener`, called from Rust), never in the window.
//!   That covers the hosts the UI links to (MusicBrainz pages in the album
//!   sheet, the AcoustID / FireCrawl sign-up links in Settings, a cover
//!   URL from any host) without a list to keep in step with the frontend:
//!   the shell only hands the URL to the browser, it fetches nothing.
//!   Electron opened `target="_blank"` links in a new Electron window
//!   instead; this is a deliberate change (docs/desktop.md).
//! * Everything else: refused, so the click does nothing. That is plain
//!   `http:` off the backend (any host), `https:` to a non-public host
//!   (`localhost`, `*.localhost`, `*.local`, `*.internal`, `*.home.arpa`,
//!   `*.lan`, single-label names, loopback / private / link-local / other
//!   non-public IP literals, so neither the backend nor another local
//!   service is reached over `https` either), a non-default port, user info,
//!   `file:`, `javascript:`, `mailto:` and custom schemes.

use url::Url;

/// Whether `url` may be opened in the system browser: `https`, a public
/// host (the cover-preview rule, [`musaic_net::preview::check_public_url`]:
/// a public DNS name or public IP literal), the default port, and no user
/// info. Only the URL text is judged; a public name that resolves to a
/// local address is the browser's business, as for any link.
pub fn external_allowed(url: &Url) -> bool {
    url.scheme() == "https"
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && !is_shell(url)
        && musaic_net::preview::check_public_url(url.as_str()).is_ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    OpenExternally,
    Deny,
}

/// `backend` is the backend origin (`http://127.0.0.1:<port>`) once the
/// server is bound, `None` before.
pub fn decide(url: &Url, backend: Option<&str>) -> Decision {
    if is_shell(url) {
        return Decision::Allow;
    }
    if url.scheme() == "about" && matches!(url.path(), "blank" | "srcdoc") {
        return Decision::Allow;
    }
    if let Some(b) = backend
        && matches!(url.scheme(), "http" | "blob")
        && url.origin().ascii_serialization() == b
    {
        return Decision::Allow;
    }
    if external_allowed(url) {
        return Decision::OpenExternally;
    }
    Decision::Deny
}

/// A request for a new window (`target="_blank"`, `window.open`): never a
/// window. Returns the URL to open in the system browser, if any.
pub fn new_window_target<'u>(url: &'u Url, backend: Option<&str>) -> Option<&'u Url> {
    (decide(url, backend) == Decision::OpenExternally).then_some(url)
}

/// The boot page served by Tauri's own asset protocol.
pub fn is_shell(url: &Url) -> bool {
    match url.scheme() {
        "tauri" => url.host_str() == Some("localhost"),
        "http" | "https" => url.host_str() == Some("tauri.localhost"),
        _ => false,
    }
}

/// A download is allowed only from the backend (the DB backup is fetched and
/// handed to an `<a download>` as a `blob:` URL of the backend origin).
pub fn download_allowed(url: &str, backend: Option<&str>) -> bool {
    Url::parse(url).is_ok_and(|u| {
        backend.is_some_and(|b| {
            matches!(u.scheme(), "http" | "blob") && u.origin().ascii_serialization() == b
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(u: &str) -> Decision {
        decide(&Url::parse(u).unwrap(), Some("http://127.0.0.1:41234"))
    }

    // The window stays on its own origin; MusicBrainz
    // links (PropTable, TrackList, AISuggestions) go to the system browser.
    // Any public https host, default port, no user info.
    #[test]
    fn navigation_policy() {
        assert_eq!(d("http://127.0.0.1:41234/library"), Decision::Allow);
        assert_eq!(
            d("blob:http://127.0.0.1:41234/7b1c4a0e-1111-2222-3333-444455556666"),
            Decision::Allow
        );
        assert_eq!(d("tauri://localhost/index.html#error=x"), Decision::Allow);
        assert_eq!(d("http://tauri.localhost/index.html"), Decision::Allow);
        assert_eq!(d("https://tauri.localhost/index.html"), Decision::Allow);
        assert_eq!(d("about:blank"), Decision::Allow);
        // The hosts the UI links to (the Python app's frontend at 6902c23).
        for s in [
            "https://musicbrainz.org/release-group/0d6f8a59-0000-0000-0000-000000000000",
            "https://musicbrainz.org/recording/0d6f8a59-0000-0000-0000-000000000001",
            "https://acoustid.org/new-application",
            "https://www.firecrawl.dev/",
            "https://coverartarchive.org/release-group/0d6f8a59-0000-0000-0000-000000000000/front",
        ] {
            assert_eq!(d(s), Decision::OpenExternally, "{s}");
        }
        // Any other public https host, subdomains, an explicit :443 (the
        // default port) and a public IP literal: also the browser.
        for s in [
            "https://example.com/",
            "https://i.ebayimg.com/images/g/x.jpg",
            "https://beta.musicbrainz.org/",
            "https://musicbrainz.org:443/",
            "https://MusicBrainz.ORG/",
            "https://93.184.215.14/",
            "https://[2606:4700::1111]/",
        ] {
            assert_eq!(d(s), Decision::OpenExternally, "{s}");
        }
        // Plain http, on any host: refused.
        assert_eq!(d("http://acoustid.org/new-application"), Decision::Deny);
        assert_eq!(d("http://musicbrainz.org/"), Decision::Deny);
        assert_eq!(d("http://example.com/"), Decision::Deny);
        // Other ports, user info.
        assert_eq!(d("https://musicbrainz.org:8443/"), Decision::Deny);
        assert_eq!(d("https://user@musicbrainz.org/"), Decision::Deny);
        assert_eq!(d("https://user:pw@musicbrainz.org/"), Decision::Deny);
        // Loopback, the backend's own address over https, local names,
        // private / link-local / CGNAT / unspecified literals: refused.
        for s in [
            "https://127.0.0.1:41234/",
            "https://127.0.0.1/",
            "https://127.1/",
            "https://[::1]/",
            "https://[::ffff:127.0.0.1]/",
            "https://0.0.0.0/",
            "https://localhost/",
            "https://localhost:41234/",
            "https://app.localhost/",
            "https://nas.local/",
            "https://router.lan/",
            "https://svc.internal/",
            "https://printer.home.arpa/",
            "https://intranet/",
            "https://10.0.0.1/",
            "https://192.168.1.1/",
            "https://172.16.0.1/",
            "https://169.254.169.254/",
            "https://100.64.0.1/",
            "https://[fd00::1]/",
            "https://[fe80::1]/",
        ] {
            assert_eq!(d(s), Decision::Deny, "{s}");
        }
        assert_eq!(d("mailto:someone@example.com"), Decision::Deny);
        // Another port on loopback is a different origin: refused.
        assert_eq!(d("http://127.0.0.1:8765/"), Decision::Deny);
        assert_eq!(d("http://localhost:41234/"), Decision::Deny);
        assert_eq!(d("file:///etc/passwd"), Decision::Deny);
        assert_eq!(d("javascript:alert(1)"), Decision::Deny);
        assert_eq!(d("musaic://open"), Decision::Deny);
        // Before the server is bound only the boot page is allowed.
        assert_eq!(
            decide(&Url::parse("http://127.0.0.1:41234/").unwrap(), None),
            Decision::Deny
        );
    }

    // `target="_blank"` / `window.open` never makes a window; an
    // allowed external link goes to the system browser, anything else
    // (the backend itself included) is dropped.
    #[test]
    fn new_window_requests() {
        let b = Some("http://127.0.0.1:41234");
        let u = |s: &str| Url::parse(s).unwrap();
        for s in [
            "https://musicbrainz.org/release-group/0d6f8a59-0000-0000-0000-000000000000",
            "https://example.com/",
        ] {
            let url = u(s);
            assert_eq!(new_window_target(&url, b), Some(&url), "{s}");
            assert_eq!(new_window_target(&url, None), Some(&url), "{s}");
        }
        for s in [
            "http://127.0.0.1:41234/library",
            "https://127.0.0.1:41234/library",
            "blob:http://127.0.0.1:41234/7b1c4a0e-1111-2222-3333-444455556666",
            "tauri://localhost/index.html",
            "https://tauri.localhost/index.html",
            "about:blank",
            "http://musicbrainz.org/",
            "https://localhost/",
            "https://192.168.1.1/",
            "file:///etc/passwd",
        ] {
            assert_eq!(new_window_target(&u(s), b), None, "{s}");
        }
    }

    #[test]
    fn downloads_only_from_backend() {
        let b = Some("http://127.0.0.1:41234");
        assert!(download_allowed(
            "blob:http://127.0.0.1:41234/7b1c4a0e-1111-2222-3333-444455556666",
            b
        ));
        assert!(download_allowed("http://127.0.0.1:41234/api/db/backup", b));
        assert!(!download_allowed("https://example.com/x.db", b));
        assert!(!download_allowed(
            "http://127.0.0.1:41234/api/db/backup",
            None
        ));
    }
}
