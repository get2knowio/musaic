//! The live transport against a scripted server on 127.0.0.1, replaying the
//! faults the Python app was run through (testdata/golden/net/faults.json).
//! Expected values are the Python app's outcome from that file, or, where the
//! Rust client deliberately differs (retry on 503/429, a timeout on AcoustID,
//! no year-fallback browse), the difference is named.
//!
//! Every `LiveTransport` here is built with `with_static_dns`, which pins
//! every host name to the local server: nothing in this file can reach
//! another machine.

mod common;

use std::time::Duration;

use common::{FaultServer, Seen, Shorten, ThreadClock, closed_port, golden, rec, replay_dir};
use musaic_net::preview::PREVIEW_MAX_REDIRECTS;
use musaic_net::{
    ACOUSTID_TIMEOUT, AcoustIdClient, ClientError, CoverArtClient, CoverError, FailureClass,
    FrontCover, LiveTransport, MusicBrainzClient, Paced, PacingPolicy, ReplayTransport,
    RetryPolicy, Transport, TransportError, USER_AGENT,
};
use serde_json::Value;

const SECS: fn(u64) -> Duration = Duration::from_secs;

struct Golden {
    doc: Value,
}

impl Golden {
    fn load() -> Self {
        Self {
            doc: golden("net/faults.json"),
        }
    }

    fn scenario(&self, id: &str) -> &Value {
        self.doc["scenarios"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == id)
            .unwrap_or_else(|| panic!("no scenario {id}"))
    }

    fn ids(&self, target: &str) -> Vec<String> {
        self.doc["scenarios"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["target"] == target)
            .map(|s| s["id"].as_str().unwrap().to_string())
            .collect()
    }

    fn key(&self) -> &str {
        self.doc["fake_key"].as_str().unwrap()
    }

    fn timeout(&self) -> Duration {
        SECS(self.doc["short_timeout_s"].as_u64().unwrap())
    }

    fn dns_host(&self) -> &str {
        self.doc["dns_fault_host"].as_str().unwrap()
    }
}

/// One scenario's server (if any), origin, and a paced live transport with
/// the golden's short timeout on virtual retry time.
struct Rig {
    server: Option<FaultServer>,
    origin: String,
    transport: Paced<Shorten<LiveTransport>, ThreadClock>,
}

impl Rig {
    fn new(g: &Golden, sc: &Value) -> Self {
        ThreadClock::reset();
        let (server, origin, live) = if sc["closed_port"] == true {
            let port = closed_port();
            let addr = format!("127.0.0.1:{port}").parse().unwrap();
            (
                None,
                format!("http://127.0.0.1:{port}"),
                LiveTransport::with_static_dns(addr, &[]),
            )
        } else if sc["dns_fault"] == true {
            // A listening server exists but the name never resolves to it.
            let s = FaultServer::start(vec![], false);
            let live = LiveTransport::with_static_dns(s.addr, &[g.dns_host()]);
            (Some(s), format!("https://{}", g.dns_host()), live)
        } else {
            let s = FaultServer::start(
                sc["script"].as_array().unwrap().clone(),
                sc["repeat_last"] == true,
            );
            let live = LiveTransport::with_static_dns(s.addr, &[]);
            let origin = s.origin();
            (Some(s), origin, live)
        };
        Self {
            server,
            origin,
            transport: Paced::with_policies(
                Shorten(live, g.timeout()),
                ThreadClock,
                PacingPolicy::default(),
                RetryPolicy::default(),
            ),
        }
    }

    fn seen(&self) -> Vec<Seen> {
        self.server.as_ref().map(|s| s.seen()).unwrap_or_default()
    }

    fn paths(&self) -> Vec<(String, String)> {
        self.seen()
            .into_iter()
            .map(|s| (s.method, s.path))
            .collect()
    }
}

fn python_paths(o: &Value) -> Vec<(String, String)> {
    o["requests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["method"].as_str().unwrap().into(),
                r["path"].as_str().unwrap().into(),
            )
        })
        .collect()
}

/// `MusicBrainzMatch.source` (mb_lookup.py:169-187) as a failure class.
fn python_class(source: &str) -> Option<FailureClass> {
    if source.ends_with("(network error)") {
        Some(FailureClass::Network)
    } else if source.ends_with("(HTTP error)") {
        Some(FailureClass::Http)
    } else if source.ends_with("(parse error)") {
        Some(FailureClass::Parse)
    } else {
        None
    }
}

/// Every request sends [`USER_AGENT`]. The
/// Python app's record keeps what the Python app sent; these are the only targets
/// where that differs: (target, the Python app's recorded User-Agent, why).
/// Every other target must match the Python app's record exactly.
const USER_AGENT_DELTAS: &[(&str, &str, &str)] = &[
    // tags.py:392 / ai.py:219: a bare product token on cover GETs.
    ("cover_get", "musaic/0.1", "User-Agent"),
    // pyacoustid: python-requests' default.
    ("acoustid", "python-requests/2.34.2", "User-Agent"),
];

/// Every `user_agents` list anywhere in a scenario's Python app record.
fn python_user_agents(o: &Value, out: &mut Vec<String>) {
    match o {
        Value::Object(m) => {
            for (k, v) in m {
                if k == "user_agents" {
                    out.extend(
                        v.as_array()
                            .unwrap()
                            .iter()
                            .map(|u| u.as_str().unwrap().to_string()),
                    );
                } else {
                    python_user_agents(v, out);
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|v| python_user_agents(v, out)),
        _ => {}
    }
}

/// The User-Agents the Rust client must send in this scenario: the Python app's
/// record with the enumerated delta applied. Fails if a delta's old value
/// is not what the Python app recorded.
fn want_user_agents(sc: &Value) -> Vec<String> {
    let target = sc["target"].as_str().unwrap();
    let mut recorded = Vec::new();
    python_user_agents(&sc["python_app"], &mut recorded);
    recorded.sort();
    recorded.dedup();
    match USER_AGENT_DELTAS.iter().find(|d| d.0 == target) {
        Some((_, old, unk)) => {
            assert_eq!(
                recorded,
                [*old],
                "{}: the Python app's record before {unk}",
                sc["id"]
            );
            vec![USER_AGENT.to_string()]
        }
        None => recorded,
    }
}

/// The User-Agents seen by the server in this scenario
/// equal the Python app's record plus the enumerated delta.
fn assert_user_agents(sc: &Value, seen: &[Seen]) {
    let mut got: Vec<String> = seen.iter().map(|s| s.user_agent.clone()).collect();
    got.sort();
    got.dedup();
    assert_eq!(got, want_user_agents(sc), "{}: User-Agent", sc["id"]);
}

/// The search recorded as replay key `35b87b24...` was recorded for (the golden
/// serves that recording's body); the album title and its release-group are
/// in the private data.
const ARTIST: &str = "Ray Conniff";
fn title() -> &'static str {
    rec("conniff_album_title")
}
fn rg() -> &'static str {
    rec("conniff_rg")
}
/// Any title, for scenarios that do not depend on the recorded search.
const INVENTED_TITLE: &str = "An Invented Title";
const REC: &str = "11111111-2222-4333-8444-555555555555";

/// The name search under every recorded fault. Failure
/// classes match the Python app's labels; 503/429 are retried where
/// the Python app gave up; nothing panics.
#[test]
fn musicbrainz_search_faults_match_the_python() {
    musaic_testdata::require_private!("replay", "golden/net/faults.json");
    let g = Golden::load();
    let ok = &g.scenario("mb_search_ok")["python_app"];
    for id in g.ids("mb_search") {
        let sc = g.scenario(&id);
        let o = &sc["python_app"];
        let rig = Rig::new(&g, sc);
        let mb = MusicBrainzClient::with_origin(&rig.transport, &rig.origin);
        let got = mb.search_release_groups(ARTIST, title());
        // Same User-Agent as the Python app (no delta for MusicBrainz).
        assert_user_agents(sc, &rig.seen());
        match id.as_str() {
            // Deliberate: retried, then answered like the plain 200.
            "mb_search_503_then_200" | "mb_search_429_retry_after" => {
                assert_eq!(o["source"], "MusicBrainz (HTTP error)", "{id}");
                assert_eq!(python_paths(o).len(), 1, "the Python app never retries");
                let top = &got.unwrap_or_else(|e| panic!("{id}: {e}"))[0];
                assert_eq!(Some(top.release_group_id.as_str()), ok["mbid"].as_str());
                assert_eq!(rig.paths().len(), 2, "{id}");
                let wait = if id.ends_with("429_retry_after") {
                    SECS(2) // Retry-After: 2 beats the 1 s backoff
                } else {
                    SECS(1) // Retry-After: 0 does not mean "now"
                };
                assert_eq!(ThreadClock::sleeps(), [wait], "{id}");
            }
            // Deliberate: the Rust client makes no year-fallback browse; the
            // Python app's raises out of find_release (mb_lookup.py:776).
            "mb_search_year_fallback_malformed" => {
                assert_eq!(o["raised"], "JSONDecodeError");
                assert_eq!(python_paths(o).len(), 2);
                assert_eq!(got.unwrap()[0].release_group_id, rg());
                assert_eq!(rig.paths().len(), 1);
            }
            "mb_search_ok" => {
                let top = &got.unwrap()[0];
                assert_eq!(Some(top.release_group_id.as_str()), o["mbid"].as_str());
                assert_eq!(Some(top.score as u64), o["score"].as_u64());
                assert_eq!(rig.paths(), python_paths(o));
            }
            _ => {
                let want = python_class(o["source"].as_str().unwrap())
                    .unwrap_or_else(|| panic!("{id}: the Python app succeeded"));
                let err = got.err().unwrap_or_else(|| panic!("{id}: Rust succeeded"));
                assert_eq!(err.class(), want, "{id}: {err}");
                assert_eq!(rig.paths(), python_paths(o), "{id}: same requests");
            }
        }
    }
}

/// The delta table is complete and not vacuous. Every scenario that
/// recorded a User-Agent either matches the Rust client's (MusicBrainz, CAA
/// HEAD) or is a delta target whose record holds exactly the stated old
/// value; each delta target has recorded scenarios; and the new value really
/// differs from every old one.
#[test]
fn user_agent_deltas_from_the_python_app_record_are_enumerated() {
    musaic_testdata::require_private!("replay", "golden/net/faults.json");
    let g = Golden::load();
    let mut per_target: std::collections::BTreeMap<String, usize> = Default::default();
    for sc in g.doc["scenarios"].as_array().unwrap() {
        let mut recorded = Vec::new();
        python_user_agents(&sc["python_app"], &mut recorded);
        if recorded.is_empty() {
            continue; // nothing reached a server (refused, DNS)
        }
        let want = want_user_agents(sc);
        assert_eq!(want, [USER_AGENT], "{}", sc["id"]);
        *per_target
            .entry(sc["target"].as_str().unwrap().into())
            .or_default() += 1;
    }
    for (target, old, _) in USER_AGENT_DELTAS {
        assert!(
            per_target.get(*target).copied().unwrap_or(0) > 0,
            "{target}"
        );
        assert_ne!(*old, USER_AGENT);
    }
    let targets: Vec<&str> = per_target.keys().map(String::as_str).collect();
    assert_eq!(
        targets,
        [
            "acoustid",
            "caa_head",
            "cover_get",
            "mb_recording",
            "mb_search"
        ]
    );
}

/// The specific transport failures behind the "network error"
/// class, so offline can be told from a broken server.
#[test]
fn network_failures_are_typed() {
    musaic_testdata::require_private!("replay", "golden/net/faults.json");
    let g = Golden::load();
    type Is = fn(&TransportError) -> bool;
    let cases: [(&str, Is); 5] = [
        ("mb_search_slow", |e| {
            matches!(e, TransportError::Timeout { .. })
        }),
        ("mb_search_reset", |e| {
            matches!(e, TransportError::Connect { .. })
        }),
        ("mb_search_short_body", |e| {
            matches!(e, TransportError::Protocol { .. })
        }),
        ("mb_search_refused", |e| {
            matches!(e, TransportError::Connect { .. })
        }),
        ("mb_search_dns", |e| matches!(e, TransportError::Dns { .. })),
    ];
    for (id, is) in cases {
        let rig = Rig::new(&g, g.scenario(id));
        let mb = MusicBrainzClient::with_origin(&rig.transport, &rig.origin);
        match mb.search_release_groups(ARTIST, title()) {
            Err(ClientError::Transport(e)) => {
                assert!(is(&e), "{id}: {e:?}");
                if id != "mb_search_short_body" {
                    assert!(e.is_offline(), "{id}: {e:?}");
                }
            }
            other => panic!("{id}: {other:?}"),
        }
    }
}

/// The slow server is cut off by the request's timeout, in about
/// that time, not by the server finishing (2.5 s).
#[test]
fn the_timeout_is_enforced() {
    let g = Golden::load();
    let rig = Rig::new(&g, g.scenario("mb_search_slow"));
    let mb = MusicBrainzClient::with_origin(&rig.transport, &rig.origin);
    let t0 = std::time::Instant::now();
    let err = mb
        .search_release_groups(ARTIST, INVENTED_TITLE)
        .unwrap_err();
    let took = t0.elapsed();
    assert!(
        matches!(err, ClientError::Transport(TransportError::Timeout { .. })),
        "{err:?}"
    );
    assert!(took < Duration::from_millis(2300), "{took:?}");
}

/// The recording lookup (resolver Tier 0c). The Python app turns every
/// failure into `None`; the Rust client reports it, classified, and retries
/// a busy answer.
#[test]
fn musicbrainz_recording_faults_match_the_python() {
    musaic_testdata::require_private!("replay", "golden/net/faults.json");
    let g = Golden::load();
    let want: Vec<(String, String)> = g.scenario("mb_recording_ok")["python_app"]["release_groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["release_group_mbid"].as_str().unwrap().into(),
                r["title"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert!(!want.is_empty());
    for id in g.ids("mb_recording") {
        let sc = g.scenario(&id);
        let rig = Rig::new(&g, sc);
        let mb = MusicBrainzClient::with_origin(&rig.transport, &rig.origin);
        let got = mb.recording_release_groups(REC);
        assert_eq!(rig.seen()[0].path, format!("/ws/2/recording/{REC}"));
        assert_user_agents(sc, &rig.seen());
        match id.as_str() {
            "mb_recording_ok" => assert_eq!(got.unwrap(), want),
            "mb_recording_404" => {
                assert!(sc["python_app"]["release_groups"].is_null());
                assert_eq!(got.unwrap_err().class(), FailureClass::Http);
            }
            "mb_recording_503_then_200" => {
                assert!(sc["python_app"]["release_groups"].is_null());
                assert_eq!(got.unwrap(), want, "retried (pacing)");
            }
            other => panic!("unhandled {other}"),
        }
    }
}

/// The CAA front-cover HEAD, under the cover download's public rule on
/// every hop. Over the Python app's fault
/// scenarios:
///
/// - the scenario's own origin (`http://127.0.0.1:{port}`, a test's
///   `with_origin`) is refused before any request: nothing reaches the
///   server, the check fails with the refusal and offers no cover;
/// - behind a public name (the test's stand-in below), redirects are
///   followed hop by hop (same hops as the Python app), 404 and other statuses
///   give the Python app's reason, and only a 200 offers the CAA URL. Where the
///   Python app followed 30 redirects (31 requests) the check stops after 5
///   (6 requests, public-only cover downloads); the 503 is tried four times.
#[test]
fn caa_front_cover_check_matches_the_python() {
    musaic_testdata::require_private!("replay", "golden/net/faults.json");
    let g = Golden::load();
    for id in g.ids("caa_head") {
        let sc = g.scenario(&id);
        let o = &sc["python_app"];
        let rig = Rig::new(&g, sc);
        assert!(rig.origin.starts_with("http://127.0.0.1:"), "{id}");
        let got = CoverArtClient::with_origin(&rig.transport, &rig.origin).check_front(rg());
        assert!(
            matches!(&got, FrontCover::Failed(TransportError::Refused { .. })),
            "{id}: {got:?}"
        );
        assert!(got.cover_url().is_none(), "{id}");
        assert!(rig.paths().is_empty(), "{id}: nothing was sent");

        let rig = Rig::new(&g, sc);
        let port = rig.server.as_ref().unwrap().addr.port();
        let public = PublicNames {
            inner: &rig.transport,
            port,
        };
        let caa = CoverArtClient::with_origin(&public, "https://covers.example.org");
        let got = caa.check_front(rg());
        assert!(rig.seen().iter().all(|s| s.method == "HEAD"), "{id}");
        assert_user_agents(sc, &rig.seen());
        assert_eq!(got.cover_url().is_some(), o["ok"] == true, "{id}");
        match id.as_str() {
            "caa_head_redirect_chain" => {
                assert_eq!(
                    got.cover_url(),
                    Some(
                        format!("https://covers.example.org/release-group/{}/front", rg()).as_str()
                    )
                );
                assert_eq!(got.reason(), o["reason"].as_str().unwrap());
                assert_eq!(rig.paths(), python_paths(o));
            }
            "caa_head_404" => {
                assert!(matches!(got, FrontCover::Missing));
                assert_eq!(got.reason(), o["reason"].as_str().unwrap());
                assert_eq!(rig.paths(), python_paths(o));
            }
            "caa_head_503" => {
                assert_eq!(got.reason(), o["reason"].as_str().unwrap());
                // Deliberate: four tries where the Python app made one.
                assert_eq!(python_paths(o).len(), 1);
                assert_eq!(rig.paths().len(), 4);
            }
            "caa_head_redirect_loop" => {
                assert!(
                    matches!(
                        got,
                        FrontCover::Failed(TransportError::TooManyRedirects { limit, .. })
                            if limit == PREVIEW_MAX_REDIRECTS
                    ),
                    "{got:?}"
                );
                // Deliberate: the Python app's first 6 requests, then
                // stop, where the Python app made 31.
                let want = python_paths(o);
                assert_eq!(want.len(), 31);
                assert_eq!(rig.paths(), want[..PREVIEW_MAX_REDIRECTS as usize + 1]);
            }
            "caa_head_reset" => {
                assert!(
                    matches!(got, FrontCover::Failed(TransportError::Connect { .. })),
                    "{got:?}"
                );
                assert_eq!(rig.paths(), python_paths(o));
            }
            other => panic!("unhandled {other}"),
        }
    }
}

/// Test only, never in the app: stands in for the public internet. A
/// request for `https://covers.example.org` or `https://alt.example.org`
/// goes to the scenario's 127.0.0.1 server over plain HTTP (as
/// `http://127.0.0.1:{port}` / `http://localhost:{port}`, the origins the
/// fault script's `{origin}` / `{alt_origin}` stand for), and a Location
/// pointing at the server is mapped back, so the public-image rule sees
/// public `https` URLs on every hop while the bytes come from the script.
struct PublicNames<'a, T> {
    inner: &'a T,
    port: u16,
}

impl<T> PublicNames<'_, T> {
    fn pairs(&self) -> [(String, String); 2] {
        [
            (
                "https://covers.example.org".into(),
                format!("http://127.0.0.1:{}", self.port),
            ),
            (
                "https://alt.example.org".into(),
                format!("http://localhost:{}", self.port),
            ),
        ]
    }
}

impl<T: Transport> Transport for PublicNames<'_, T> {
    fn send(&self, r: &musaic_net::Request) -> Result<musaic_net::Response, TransportError> {
        assert!(r.public_only && !r.follow_redirects, "{r:?}");
        let mut req = r.clone();
        let (_, local) = self
            .pairs()
            .into_iter()
            .find(|(public, _)| *public == r.origin)
            .unwrap_or_else(|| panic!("not a stand-in name: {}", r.origin));
        req.origin = local;
        req.public_only = false;
        let mut resp = self.inner.send(&req)?;
        for (k, v) in &mut resp.headers {
            if k.eq_ignore_ascii_case("location") {
                for (public, local) in self.pairs() {
                    if let Some(rest) = v.strip_prefix(&local) {
                        *v = format!("{public}{rest}");
                    }
                }
            }
        }
        Ok(resp)
    }
}

/// The cover download is the public-image
/// fetch (the cover preview's rule), where the Python app's verification GET
/// (`ai._verify_cover_url`) and apply download (`tags.download_cover`)
/// fetched any URL. Over the Python app's fault scenarios:
///
/// - the scenario's own URL (`http://127.0.0.1:{port}/...`, what the Python app
///   fetched) is refused before any request: nothing reaches the server;
/// - behind a public name (the test's stand-in above), every scenario makes
///   the Python app's requests; where the Python app failed it fails with the
///   Python app's reason (wrong type, too large, 404); where it succeeded on a
///   body that is not an image (the redirect, the 206: the script's filler
///   bytes) or on an absent `Content-Type`, this build refuses.
#[test]
fn cover_download_applies_the_public_image_rule() {
    let g = Golden::load();
    let ids = g.ids("cover_get");
    assert_eq!(ids.len(), 6, "{ids:?}");
    for id in ids {
        let sc = g.scenario(&id);
        let o = &sc["python_app"]["verify"];
        let python_ok = o["ok"].as_bool().unwrap();

        let rig = Rig::new(&g, sc);
        let caa = CoverArtClient::with_origin(&rig.transport, &rig.origin);
        let url = format!("{}/cover/front.jpg", rig.origin);
        assert!(url.starts_with("http://127.0.0.1:"), "{url}");
        let (got, lines) = musaic_log::capture(|| caa.download(&url));
        assert!(matches!(got, Err(CoverError::Refused(_))), "{id}: {got:?}");
        assert!(rig.paths().is_empty(), "{id}: nothing was sent");
        assert!(
            !python_ok || !python_paths(o).is_empty(),
            "{id}: the Python app fetched it"
        );
        // tags.download_cover's WARNING for a failed download.
        assert_eq!(lines.len(), 1, "{id}: {lines:?}");
        assert_eq!(lines[0].target, "musaic.tags");
        assert!(
            lines[0]
                .message
                .starts_with("cover download failed: refused: ")
        );

        let rig = Rig::new(&g, sc);
        let port = rig.server.as_ref().unwrap().addr.port();
        let public = PublicNames {
            inner: &rig.transport,
            port,
        };
        let got =
            CoverArtClient::new(&public).download("https://covers.example.org/cover/front.jpg");
        assert_eq!(
            rig.paths(),
            python_paths(o),
            "{id}: the Python app's requests"
        );
        // The full User-Agent where the Python app sent "musaic/0.1".
        assert_user_agents(sc, &rig.seen());
        match id.as_str() {
            "cover_wrong_content_type" | "cover_oversized" | "cover_404" => {
                assert!(!python_ok, "{id}");
                let e = got.unwrap_err();
                assert_eq!(e.to_string(), o["reason"].as_str().unwrap(), "{id}");
            }
            "cover_redirect_then_image" | "cover_206_partial" => {
                assert!(python_ok, "{id}");
                assert!(
                    matches!(&got, Err(CoverError::NotAnImage(ct)) if ct == "image/jpeg"),
                    "{id}: {got:?}"
                );
            }
            "cover_no_content_type" => {
                assert!(python_ok, "{id}");
                assert!(
                    matches!(got, Err(CoverError::ContentType(_))),
                    "{id}: {got:?}"
                );
            }
            other => panic!("unhandled {other}"),
        }
    }
    // The apply path (tags.download_cover) took the HTML page and the 8 MiB+1
    // body; the Rust download does not.
    let lenient = &g.scenario("cover_wrong_content_type")["python_app"]["download_cover"]["result"];
    assert_eq!(lenient["mime"], "text/html");
}

/// The AcoustID lookup. Failure classes follow pyacoustid's
/// outcomes; an error answer is an error here (the Python app stored NULLs
/// without a word, fingerprint.py:233); there is a timeout (the Python app has
/// none, fingerprint.py:206).
#[test]
fn acoustid_faults_match_the_python() {
    musaic_testdata::require_private!("replay", "golden/net/faults.json");
    let g = Golden::load();
    for id in g.ids("acoustid") {
        let sc = g.scenario(&id);
        let o = &sc["python_app"];
        let rig = Rig::new(&g, sc);
        let client = AcoustIdClient::with_origin(&rig.transport, g.key(), &rig.origin);
        let got = client.lookup("AQAAtest", 200);
        let seen = rig.seen();
        assert_eq!(seen.len(), 1, "{id}");
        assert_eq!(
            (seen[0].method.as_str(), seen[0].path.as_str()),
            ("POST", "/v2/lookup")
        );
        // The full User-Agent where the Python app sent python-requests'.
        assert_user_agents(sc, &seen);
        match id.as_str() {
            "acoustid_ok" => {
                let r = got.unwrap();
                let best = &r.results[0];
                assert_eq!(
                    best.recordings[0].id.as_deref(),
                    o["stored"]["acoustid_recording_mbid"].as_str()
                );
                assert_eq!(best.score, o["stored"]["acoustid_score"].as_f64());
            }
            "acoustid_error_answer" | "acoustid_error_echoes_key" => {
                assert_eq!(o["status"], "error");
                assert!(o["stored"]["acoustid_recording_mbid"].is_null());
                let e = got.unwrap_err();
                assert_eq!(e.class(), FailureClass::Service);
                assert!(!format!("{e} {e:?}").contains(g.key()), "{id}: key leaked");
            }
            "acoustid_500_html" => {
                assert_eq!(o["message"], "response is not valid JSON");
                assert_eq!(got.unwrap_err().class(), FailureClass::Http);
            }
            "acoustid_truncated_json" => {
                assert_eq!(o["message"], "response is not valid JSON");
                assert_eq!(got.unwrap_err().class(), FailureClass::Parse);
            }
            "acoustid_reset" => {
                assert_eq!(o["raised"], "WebServiceError");
                assert_eq!(got.unwrap_err().class(), FailureClass::Network);
            }
            "acoustid_slow" => {
                assert_eq!(
                    o["waited_for_slow_server"], true,
                    "the Python app has no timeout"
                );
                assert!(matches!(
                    got,
                    Err(ClientError::Transport(TransportError::Timeout { .. }))
                ));
            }
            other => panic!("unhandled {other}"),
        }
        assert!(
            o["stored"]["acoustid_recording_mbid"].is_null()
                == (id != "acoustid_ok" && id != "acoustid_slow"),
            "{id}"
        );
    }
}

/// The form body the live transport puts on the wire carries the
/// key (the service needs it) and the pinned metadata, and the request has
/// a timeout where the Python app had none.
#[test]
fn acoustid_wire_body() {
    musaic_testdata::require_private!("replay", "golden/net/faults.json");
    let g = Golden::load();
    let rig = Rig::new(&g, g.scenario("acoustid_ok"));
    let client = AcoustIdClient::with_origin(&rig.transport, g.key(), &rig.origin);
    client.lookup("AQAAtest", 200).unwrap();
    let body = String::from_utf8(rig.seen()[0].body.clone()).unwrap();
    assert_eq!(
        body,
        format!(
            "client={}&format=json&duration=200&fingerprint=AQAAtest&meta=recordings+releasegroups+releases+tracks+compress",
            g.key()
        )
    );
    let req = AcoustIdClient::<ReplayTransport>::lookup_request("k", "AQAA", 1);
    assert_eq!(req.timeout, Some(ACOUSTID_TIMEOUT));
}

/// AcoustID only over HTTPS. A plain-HTTP origin is
/// refused before any I/O, even though the name resolves to a live server
/// that would answer.
#[test]
fn acoustid_over_plain_http_is_refused() {
    musaic_testdata::require_private!("replay", "golden/net/faults.json");
    let g = Golden::load();
    let server = FaultServer::start(
        g.scenario("acoustid_ok")["script"]
            .as_array()
            .unwrap()
            .clone(),
        true,
    );
    let live = LiveTransport::with_static_dns(server.addr, &[]);
    for origin in ["http://api.acoustid.org", "HTTP://api.acoustid.org"] {
        let client = AcoustIdClient::with_origin(&live, g.key(), origin);
        match client.lookup("AQAAtest", 200) {
            Err(ClientError::Transport(TransportError::InsecureScheme { url })) => {
                assert!(!url.contains(g.key()));
            }
            other => panic!("{origin}: {other:?}"),
        }
    }
    // A redirect to plain HTTP elsewhere is refused at that hop as well.
    let s = FaultServer::start(
        vec![
            serde_json::json!({"status": 302, "headers": {"Location": "http://cdn.example.org/x.jpg"}}),
        ],
        false,
    );
    let live2 = LiveTransport::with_static_dns(s.addr, &[]);
    let get = musaic_net::Request::from_url(
        musaic_net::Method::Get,
        &format!("{}/cover.jpg", s.origin()),
    )
    .unwrap();
    match live2.send(&get) {
        Err(TransportError::InsecureScheme { url }) => {
            assert_eq!(url, "http://cdn.example.org/x.jpg")
        }
        other => panic!("{other:?}"),
    }
    assert!(server.seen().is_empty(), "nothing reached the server");
    assert_eq!(s.seen().len(), 1);
    // A cover download gets no loopback exemption at all; the same
    // URL is refused before any request.
    match CoverArtClient::new(&live2).download(&format!("{}/cover.jpg", s.origin())) {
        Err(CoverError::Refused(_)) => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(s.seen().len(), 1);
}

/// The live and replay transports are interchangeable: one generic call
/// gives the same answer from a recording and from a server serving that
/// recording's body.
#[test]
fn live_and_replay_are_interchangeable() {
    musaic_testdata::require_private!("replay", "golden/net/faults.json");
    fn lookup<T: Transport>(t: T, origin: Option<&str>) -> Vec<(String, u32)> {
        let mb = match origin {
            Some(o) => MusicBrainzClient::with_origin(t, o),
            None => MusicBrainzClient::new(t),
        };
        mb.search_release_groups(ARTIST, title())
            .unwrap()
            .into_iter()
            .map(|c| (c.release_group_id, c.score))
            .collect()
    }
    let g = Golden::load();
    let rig = Rig::new(&g, g.scenario("mb_search_ok"));
    let live = lookup(&rig.transport, Some(&rig.origin));
    let replay = ReplayTransport::new(replay_dir());
    // The server's body is the recording this very request is keyed to.
    let key = MusicBrainzClient::<ReplayTransport>::search_request(ARTIST, title()).replay_key();
    assert_eq!(
        g.scenario("mb_search_ok")["script"][0]["body"]["replay"],
        key.as_str()
    );
    assert_eq!(lookup(&replay, None), live);
    assert!(!live.is_empty());
    // Both also fit behind one trait object.
    let boxed: Vec<Box<dyn Transport>> = vec![Box::new(replay), Box::new(rig.transport)];
    assert_eq!(boxed.len(), 2);
}

/// A real-network smoke test. Never run by default, and
/// refuses to run unless a person sets MUSAIC_ALLOW_LIVE_TESTS=1. Searches
/// for the synthetic test album's name, which real releases share.
#[test]
#[ignore = "real network: needs MUSAIC_ALLOW_LIVE_TESTS=1"]
fn live_smoke_against_the_real_services() {
    if std::env::var("MUSAIC_ALLOW_LIVE_TESTS").as_deref() != Ok("1") {
        panic!("refusing: set MUSAIC_ALLOW_LIVE_TESTS=1 to allow outbound HTTP");
    }
    let clients = musaic_net::LiveClients::new(None);
    let found = clients
        .musicbrainz
        .search_release_groups("Various Artists", "Night Mix")
        .unwrap();
    assert!(!found.is_empty());
    let front = clients.cover_art.check_front(&found[0].release_group_id);
    eprintln!("front cover: {}", front.reason());
    // One cover fetched under the public-image rule over
    // real TLS (https only, public addresses at every hop, signature check).
    if let FrontCover::Verified { url } = &front {
        let transport = musaic_net::live::live_paced(LiveTransport::new());
        let p = musaic_net::preview::fetch_preview(&transport, url).unwrap();
        eprintln!("cover fetched: {} bytes, {}", p.bytes.len(), p.mime);
        assert!(p.mime.starts_with("image/"));
        assert!(!p.bytes.is_empty());
    }
}
