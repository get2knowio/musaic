//! Every cover download
//! (`CoverArtClient::download`: apply with a `cover_url`, a check fix's
//! cover, the AI cover verification) follows the cover preview's rule
//! (`musaic_net::preview::fetch_public_image`): https only, public hosts
//! and addresses only, at most 5 redirects each re-checked, a body cap, an
//! allowed image type whose signature the body starts with. The Python app
//! fetched any URL (ai.py:1483-1531, tags.py:392). The Cover Art Archive
//! front-cover HEAD (`CoverArtClient::check_front`, CAA front cover) follows the
//! same hop rule: https only, every hop public, at most 5 redirects (the
//! Python app followed 30, mb_lookup.py:651-664). Scripted transports and the
//! recorded Cover Art Archive redirect hops (the recorded replay set (private)) only.

mod common;

use std::collections::HashMap;
use std::sync::Mutex;

use common::replay_dir;
use musaic_net::preview::{PREVIEW_MAX_BYTES, PREVIEW_MAX_REDIRECTS, check_public_url};
use musaic_net::{
    CAA_TIMEOUT, COVER_DOWNLOAD_TIMEOUT, CoverArtClient, CoverError, FrontCover, Method, Request,
    Response, Transport, TransportError, USER_AGENT,
};

const JPEG: &[u8] = &[
    0xff, 0xd8, 0xff, 0xe0, 0, 0x10, b'J', b'F', b'I', b'F', 0, 1,
];

/// Answers by URL; records every request it was sent.
#[derive(Default)]
struct Scripted {
    answers: HashMap<String, Response>,
    sent: Mutex<Vec<Request>>,
}

impl Scripted {
    fn ok(mut self, url: &str, status: u16, headers: &[(&str, &str)], body: &[u8]) -> Self {
        self.answers.insert(
            url.into(),
            Response {
                status,
                headers: headers
                    .iter()
                    .map(|(k, v)| ((*k).into(), (*v).into()))
                    .collect(),
                body: body.to_vec(),
            },
        );
        self
    }
    fn urls(&self) -> Vec<String> {
        self.sent.lock().unwrap().iter().map(Request::url).collect()
    }
}

impl Transport for Scripted {
    fn send(&self, r: &Request) -> Result<Response, TransportError> {
        self.sent.lock().unwrap().push(r.clone());
        self.answers
            .get(&r.url())
            .cloned()
            .ok_or_else(|| TransportError::Unobserved {
                key: String::new(),
                method: "GET",
                url: r.url(),
            })
    }
}

/// A model-suggested or user-supplied cover URL on this machine, the LAN,
/// a cloud metadata address or plain HTTP is refused before any request
/// (the Python app fetched every one; a loopback URL made the server call its
/// own API). Each refusal logs tags.download_cover's WARNING.
#[test]
fn private_and_plain_http_cover_urls_are_refused_before_any_request() {
    for url in [
        "http://127.0.0.1:8765/api/health",
        "http://localhost:8765/api/albums",
        "https://127.0.0.1/x.jpg",
        "https://[::1]/x.jpg",
        "https://192.168.1.20/cover.jpg",
        "https://nas.local/cover.jpg",
        "https://169.254.169.254/latest/meta-data/",
        "http://coverartarchive.org/release-group/x/front",
        "https://coverartarchive.org:8443/release-group/x/front",
    ] {
        let t = Scripted::default().ok(url, 200, &[("Content-Type", "image/jpeg")], JPEG);
        let (got, lines) = musaic_log::capture(|| CoverArtClient::new(&t).download(url));
        assert!(matches!(got, Err(CoverError::Refused(_))), "{url}: {got:?}");
        assert!(t.urls().is_empty(), "{url}: nothing was sent");
        assert_eq!(lines.len(), 1, "{url}");
        assert_eq!(lines[0].level, musaic_log::Level::Warning);
        assert_eq!(lines[0].target, "musaic.tags");
        assert!(
            lines[0]
                .message
                .starts_with("cover download failed: refused: "),
            "{}",
            lines[0].message
        );
    }
    // Not a URL at all: the Python app's "not a cover URL".
    let t = Scripted::default();
    assert!(matches!(
        CoverArtClient::new(&t).download("cover.jpg"),
        Err(CoverError::BadUrl(_))
    ));
}

/// Redirects are followed by the download, one checked hop at a time: a
/// hop to a private or plain-HTTP target is refused before it is sent, and
/// a sixth redirect is refused. Every request is public-only, does not
/// follow redirects itself, and carries the apply path's 30 s timeout and
/// the 8 MiB (+1) body cap.
#[test]
fn redirects_are_checked_hop_by_hop() {
    const A: &str = "https://cdn.example.org/a.jpg";
    for target in [
        "http://127.0.0.1:8765/api/health",
        "https://10.0.0.7/x.jpg",
        "http://cdn.example.org/b.jpg",
    ] {
        let t = Scripted::default()
            .ok(A, 302, &[("Location", target)], b"")
            .ok(target, 200, &[("Content-Type", "image/jpeg")], JPEG);
        let got = CoverArtClient::new(&t).download(A);
        assert!(
            matches!(got, Err(CoverError::Refused(_))),
            "{target}: {got:?}"
        );
        assert_eq!(t.urls(), [A], "{target}: the hop was never sent");
    }
    let mut t = Scripted::default();
    for i in 0..=PREVIEW_MAX_REDIRECTS + 1 {
        t = t.ok(
            &format!("https://a{i}.example.org/x"),
            302,
            &[("Location", &format!("https://a{}.example.org/x", i + 1))],
            b"",
        );
    }
    let got = CoverArtClient::new(&t).download("https://a0.example.org/x");
    assert!(
        matches!(got, Err(CoverError::TooManyRedirects(n)) if n == PREVIEW_MAX_REDIRECTS),
        "{got:?}"
    );
    assert_eq!(t.urls().len() as u32, PREVIEW_MAX_REDIRECTS + 1);
    for r in t.sent.lock().unwrap().iter() {
        assert!(r.public_only && !r.follow_redirects, "{r:?}");
        assert_eq!(r.timeout, Some(COVER_DOWNLOAD_TIMEOUT));
        assert_eq!(r.max_body, Some(PREVIEW_MAX_BYTES + 1));
        assert_eq!(r.method, Method::Get);
    }
}

/// The body must be an allowed image type and start with its signature:
/// an absent Content-Type (the Python app took it as JPEG), SVG, and a page
/// declared as an image are refused; a real JPEG is taken with its type.
#[test]
fn only_real_images_are_taken() {
    const A: &str = "https://cdn.example.org/a.jpg";
    for (headers, body) in [
        (vec![], JPEG),
        (
            vec![("Content-Type", "image/svg+xml")],
            b"<svg/>".as_slice(),
        ),
        (
            vec![("Content-Type", "image/jpeg")],
            b"<html>landing</html>".as_slice(),
        ),
    ] {
        let t = Scripted::default().ok(A, 200, &headers, body);
        let got = CoverArtClient::new(&t).download(A);
        assert!(
            matches!(
                got,
                Err(CoverError::ContentType(_) | CoverError::NotAnImage(_))
            ),
            "{headers:?}: {got:?}"
        );
    }
    let t = Scripted::default().ok(A, 200, &[("Content-Type", "image/jpeg; q=1")], JPEG);
    let img = CoverArtClient::new(&t).download(A).unwrap();
    assert_eq!(img.bytes, JPEG);
    assert_eq!(img.mime, "image/jpeg");
}

/// The recorded Cover Art Archive chains (see the test below):
/// for each recorded `archive.org/download/...` hop, (that URL, the storage
/// node its 302 points at, the node's recorded Content-Type).
fn recorded_caa_chains() -> Vec<(String, String, String)> {
    let dir = replay_dir();
    let mut recs = Vec::new();
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let r = musaic_testdata::read_json(&p);
        let url = r["url"].as_str().unwrap_or("").to_owned();
        if url.starts_with("https://archive.org/download/") || url.contains(".archive.org/") {
            recs.push(r);
        }
    }
    let by_url: HashMap<String, &serde_json::Value> = recs
        .iter()
        .map(|r| (r["url"].as_str().unwrap().to_owned(), r))
        .collect();
    let header = |r: &serde_json::Value, name: &str| -> Option<String> {
        r["headers"]
            .as_object()
            .unwrap()
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str().unwrap().to_owned())
    };
    let mut chains = Vec::new();
    for first in recs.iter().filter(|r| {
        r["url"]
            .as_str()
            .unwrap()
            .starts_with("https://archive.org/download/")
    }) {
        let ia = first["url"].as_str().unwrap();
        assert_eq!(first["status"], 302, "{ia}");
        let node_url = header(first, "location").unwrap();
        let node = by_url[node_url.as_str()];
        assert_eq!(node["status"], 200, "{node_url}");
        let ct = header(node, "content-type").unwrap();
        chains.push((ia.to_owned(), node_url, ct));
    }
    // 6 recorded archive.org/download hops: 5 from front-cover HEADs (3
    // from one live run, 2 for the hinted Night Mix groups) and 1 from a
    // recorded front-cover GET.
    // tests/replay.rs counts each chain's two archive.org hops.
    assert_eq!(chains.len(), 6);
    chains
}

/// CAA front cover under public-only cover downloads: Cover Art Archive covers still download. The
/// recorded CAA chains (the private replay set):
/// coverartarchive.org answers 307 to `archive.org/download/...` (requests
/// folded that hop into the CAA record, so its 307 is reconstructed here
/// from the archive.org hop the recording starts at), which answers 302 to
/// a storage node (`dn*.ca.archive.org`), which answers 200 `image/jpeg`.
/// Two redirects, within the limit of five; every hop's URL passes the
/// public rule. The recorded statuses, Locations and Content-Types are
/// served as GET answers (the recordings are HEADs and their bodies are
/// placeholders), with a JPEG body on the last hop.
#[test]
fn cover_art_archive_chains_from_the_recordings_still_pass() {
    musaic_testdata::require_private!("replay");
    for (i, (ia, node_url, ct)) in recorded_caa_chains().into_iter().enumerate() {
        let caa = format!("https://coverartarchive.org/release-group/rg-{i}/front");
        let t = Scripted::default()
            .ok(&caa, 307, &[("Location", &ia)], b"")
            .ok(&ia, 302, &[("Location", &node_url)], b"")
            .ok(&node_url, 200, &[("Content-Type", &ct)], JPEG);
        let img = CoverArtClient::new(&t).download(&caa).unwrap();
        assert_eq!(img.bytes, JPEG);
        assert_eq!(img.mime, ct);
        let urls = t.urls();
        assert_eq!(urls, [caa.clone(), ia.clone(), node_url.clone()]);
        let redirects = urls.len() as u32 - 1;
        assert_eq!(redirects, 2);
        assert!(redirects <= PREVIEW_MAX_REDIRECTS);
        for u in &urls {
            check_public_url(u).unwrap_or_else(|e| panic!("{u}: {e}"));
        }
    }
}

// ---------- the front-cover HEAD under the same rule ----------

/// An invented release-group id (the scripted transports answer any).
const RG: &str = "5e0d2c71-3a84-4f9b-b6e2-81c4d7a09f35";

fn front(origin: &str) -> String {
    format!("{origin}/release-group/{RG}/front")
}

/// Every request the check sent is a HEAD under the public rule: public
/// only, redirects returned to the check, the CAA timeout, the full
/// User-Agent.
fn assert_head_hops(t: &Scripted) {
    for r in t.sent.lock().unwrap().iter() {
        assert!(r.public_only && !r.follow_redirects, "{r:?}");
        assert_eq!(r.method, Method::Head);
        assert_eq!(r.timeout, Some(CAA_TIMEOUT));
        let ua: Vec<&str> = r
            .headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("user-agent"))
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(ua, [USER_AGENT]);
    }
}

/// The HEAD's own URL must be public `https` (the old rule let a
/// loopback `http` origin through for tests): refused before any request,
/// a failed check (no cover offered, the refusal as the reason).
#[test]
fn front_check_refuses_a_non_public_origin_before_any_request() {
    for origin in [
        "http://127.0.0.1:8765",
        "http://localhost:8765",
        "https://127.0.0.1",
        "https://192.168.1.20",
        "https://nas.local",
        "http://coverartarchive.org",
    ] {
        let t = Scripted::default().ok(&front(origin), 200, &[], b"");
        let got = CoverArtClient::with_origin(&t, origin).check_front(RG);
        assert!(
            matches!(&got, FrontCover::Failed(TransportError::Refused { .. })),
            "{origin}: {got:?}"
        );
        assert_eq!(got.cover_url(), None, "{origin}");
        assert!(
            got.reason().starts_with("refused to fetch "),
            "{}",
            got.reason()
        );
        assert!(t.urls().is_empty(), "{origin}: nothing was sent");
    }
}

/// Redirects are followed by the check one hop at a time; a hop to
/// a private, loopback or plain-HTTP target is refused before it is sent,
/// and a sixth redirect is refused (the Python app followed 30, anywhere). Both
/// are failed checks: no cover offered.
#[test]
fn front_check_redirects_are_checked_hop_by_hop() {
    let caa = front("https://coverartarchive.org");
    for target in [
        "http://127.0.0.1:8765/api/health",
        "https://10.0.0.7/x.jpg",
        "https://[::1]/x.jpg",
        "http://archive.org/download/x/x.jpg",
    ] {
        let t = Scripted::default()
            .ok(&caa, 307, &[("Location", target)], b"")
            .ok(target, 200, &[], b"");
        let got = CoverArtClient::new(&t).check_front(RG);
        assert!(
            matches!(&got, FrontCover::Failed(TransportError::Refused { .. })),
            "{target}: {got:?}"
        );
        assert_eq!(got.cover_url(), None);
        assert_eq!(
            t.urls(),
            std::slice::from_ref(&caa),
            "{target}: the hop was never sent"
        );
        assert_head_hops(&t);
    }

    // Five redirects then 200: verified, the CAA URL offered.
    let chain = |n: u32| {
        let mut t =
            Scripted::default().ok(&caa, 307, &[("Location", "https://a1.example.org/x")], b"");
        for i in 1..=n {
            t = t.ok(
                &format!("https://a{i}.example.org/x"),
                302,
                &[("Location", &format!("https://a{}.example.org/x", i + 1))],
                b"",
            );
        }
        t
    };
    let t = chain(PREVIEW_MAX_REDIRECTS - 1).ok(
        &format!("https://a{}.example.org/x", PREVIEW_MAX_REDIRECTS),
        200,
        &[],
        b"",
    );
    let got = CoverArtClient::new(&t).check_front(RG);
    assert_eq!(got.cover_url(), Some(caa.as_str()), "{got:?}");
    assert_eq!(t.urls().len() as u32, PREVIEW_MAX_REDIRECTS + 1);
    assert_head_hops(&t);

    // A sixth redirect: refused after the sixth request.
    let t = chain(PREVIEW_MAX_REDIRECTS + 1);
    let got = CoverArtClient::new(&t).check_front(RG);
    assert!(
        matches!(
            &got,
            FrontCover::Failed(TransportError::TooManyRedirects { limit, url })
                if *limit == PREVIEW_MAX_REDIRECTS && *url == caa
        ),
        "{got:?}"
    );
    assert_eq!(got.cover_url(), None);
    assert_eq!(t.urls().len() as u32, PREVIEW_MAX_REDIRECTS + 1);
    assert_head_hops(&t);

    // A 404 at the end of a public chain is "no front-cover", as before.
    let t = Scripted::default()
        .ok(&caa, 307, &[("Location", "/elsewhere")], b"")
        .ok("https://coverartarchive.org/elsewhere", 404, &[], b"");
    let got = CoverArtClient::new(&t).check_front(RG);
    assert!(matches!(got, FrontCover::Missing), "{got:?}");
}

/// CAA front cover under public-only cover downloads: the recorded Cover Art Archive chains (CAA 307 to
/// archive.org, 302 to a storage node, 200) still verify as HEADs: two
/// redirects, every hop public, the CAA URL offered.
#[test]
fn front_check_passes_the_recorded_cover_art_archive_chains() {
    musaic_testdata::require_private!("replay");
    for (i, (ia, node_url, ct)) in recorded_caa_chains().into_iter().enumerate() {
        let rg = format!("rg-{i}");
        let caa = format!("https://coverartarchive.org/release-group/{rg}/front");
        let t = Scripted::default()
            .ok(&caa, 307, &[("Location", &ia)], b"")
            .ok(&ia, 302, &[("Location", &node_url)], b"")
            .ok(&node_url, 200, &[("Content-Type", &ct)], b"");
        let got = CoverArtClient::new(&t).check_front(&rg);
        assert_eq!(got.cover_url(), Some(caa.as_str()), "{got:?}");
        assert_eq!(t.urls(), [caa.clone(), ia.clone(), node_url.clone()]);
        assert_head_hops(&t);
    }
}

/// tags.py:400 `log.warning("cover download failed: %s", e)`:
/// the Python app logged it when `requests.get` raised, i.e. no answer at all or
/// too many redirects, and returned None without a line for any status other
/// than 200. Here: a failed request and a sixth redirect log the line (with
/// this build's error text, not requests'); a 404 and the public rule's type
/// refusals do not.
#[test]
fn failed_cover_downloads_log_like_the_python() {
    const A: &str = "https://cdn.example.org/a.jpg";
    let one_line = |lines: &[musaic_log::Record], what: &str| -> String {
        assert_eq!(lines.len(), 1, "{what}: {lines:?}");
        assert_eq!(lines[0].level, musaic_log::Level::Warning, "{what}");
        assert_eq!(lines[0].target, "musaic.tags", "{what}");
        lines[0].message.clone()
    };

    // No answer: the scripted transport has nothing for the URL.
    let t = Scripted::default();
    let (got, lines) = musaic_log::capture(|| CoverArtClient::new(&t).download(A));
    let Err(e @ CoverError::Transport(_)) = got else {
        panic!("{got:?}");
    };
    assert_eq!(
        one_line(&lines, "transport"),
        format!("cover download failed: {e}")
    );

    // A sixth redirect.
    let mut t = Scripted::default();
    for i in 0..=PREVIEW_MAX_REDIRECTS + 1 {
        t = t.ok(
            &format!("https://a{i}.example.org/x"),
            302,
            &[("Location", &format!("https://a{}.example.org/x", i + 1))],
            b"",
        );
    }
    let (got, lines) =
        musaic_log::capture(|| CoverArtClient::new(&t).download("https://a0.example.org/x"));
    assert!(
        matches!(got, Err(CoverError::TooManyRedirects(_))),
        "{got:?}"
    );
    assert_eq!(
        one_line(&lines, "redirects"),
        format!("cover download failed: more than {PREVIEW_MAX_REDIRECTS} redirects")
    );

    // The AI cover verification's fetch (`ai._verify_cover_url` logged
    // nothing): no line for the same failure.
    let t = Scripted::default();
    let (got, lines) = musaic_log::capture(|| CoverArtClient::new(&t).fetch(A));
    assert!(matches!(got, Err(CoverError::Transport(_))), "{got:?}");
    assert!(lines.is_empty(), "{lines:?}");

    // A status other than 200, and a body that is not an image: no line.
    for t in [
        Scripted::default().ok(A, 404, &[], b""),
        Scripted::default().ok(A, 200, &[("Content-Type", "image/jpeg")], b"<html/>"),
    ] {
        let (got, lines) = musaic_log::capture(|| CoverArtClient::new(&t).download(A));
        assert!(got.is_err());
        assert!(lines.is_empty(), "{lines:?}");
    }
}
