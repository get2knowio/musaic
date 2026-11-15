//! The replay transport, pacing and retry, redirect following and the Cover
//! Art Archive front-cover check, over replay directories the tests write
//! themselves from invented answers (see `synthetic/mod.rs`).

mod synthetic;

use std::time::Duration;

use musaic_net::{
    AcoustIdClient, CAA_TIMEOUT, ClientError, CoverArtClient, FollowRedirects, FrontCover, Method,
    MusicBrainzClient, Paced, ReplayTransport, Request, Transport, TransportError, USER_AGENT,
};
use serde_json::json;
use synthetic::*;

const SECS: fn(u64) -> Duration = Duration::from_secs;
const MS: fn(u64) -> Duration = Duration::from_millis;

/// A recorded answer is found by the request the client builds (same key),
/// whatever headers the request carries; hits are listed in the order
/// served; a request nobody recorded is unobserved and names its key and
/// its URL, never the AcoustID key.
#[test]
fn replay_serves_recorded_answers_by_key_in_order() {
    let dir = ReplayDir::new("replay-order");
    let search = MusicBrainzClient::<ReplayTransport>::search_request(
        "Mira Quell",
        "Lanterns Over Harbour Street",
    );
    let browse = MusicBrainzClient::<ReplayTransport>::browse_request(RG_HARBOUR);
    let k_search = dir.record(
        &search,
        &json_response(
            200,
            &search_body(vec![search_group(
                RG_HARBOUR,
                json!(100),
                "Lanterns Over Harbour Street",
                "Mira Quell",
            )]),
        ),
    );
    let k_browse = dir.record(
        &browse,
        &json_response(
            200,
            &browse_body(vec![(
                REL_ONE,
                vec![medium(
                    1,
                    Some(1),
                    vec![track(1, "Brinelight Hollow", Some(201_000))],
                )],
            )]),
        ),
    );
    let replay = ReplayTransport::new(dir.path());
    let mb = MusicBrainzClient::new(&replay);
    let hits = mb
        .search_release_groups("Mira Quell", "Lanterns Over Harbour Street")
        .unwrap();
    assert_eq!(hits[0].release_group_id, RG_HARBOUR);
    assert_eq!(mb.browse_releases(RG_HARBOUR).unwrap().len(), 1);
    // The same search again, with an extra header: headers do not key replay.
    let mut again = search.clone();
    again
        .headers
        .push(("Accept".into(), "application/json".into()));
    assert_eq!(again.replay_key(), k_search);
    assert_eq!(replay.send(&again).unwrap().status, 200);
    assert_eq!(replay.hits(), [k_search.clone(), k_browse, k_search]);

    // Not recorded: an unobserved failure with the key and the URL.
    let other = MusicBrainzClient::<ReplayTransport>::browse_request(RG_COPPER);
    match mb.browse_releases(RG_COPPER) {
        Err(ClientError::Transport(TransportError::Unobserved { key, method, url })) => {
            assert_eq!(key, other.replay_key());
            assert_eq!(method, "GET");
            assert_eq!(url, other.redacted_url());
            assert!(url.starts_with("https://musicbrainz.org/ws/2/release?"));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(replay.hits().len(), 3, "an unobserved request is not a hit");

    // An unrecorded AcoustID lookup: the key is in the form, never in the
    // error.
    let acoustid = AcoustIdClient::new(&replay, KEY_ENCODED);
    let err = acoustid.lookup("AQADtNRSnsZT", 201).unwrap_err();
    let text = format!("{err} {err:?}");
    assert!(matches!(
        err,
        ClientError::Transport(TransportError::Unobserved { method: "POST", .. })
    ));
    assert!(
        !text.contains(KEY_ENCODED) && !text.contains("syn%2Bthetic"),
        "{text}"
    );
}

/// The AcoustID key never takes part in a replay key: an answer recorded
/// under one key replays for any other; the fingerprint and duration do.
#[test]
fn acoustid_answers_replay_whatever_the_key() {
    let dir = ReplayDir::new("acoustid-key");
    let req =
        AcoustIdClient::<ReplayTransport>::lookup_request("recorded-with", "AQADtNRSnsZT", 201);
    dir.record(
        &req,
        &json_response(200, &acoustid_body(&[(0.97, REC_FOUND)])),
    );
    let replay = ReplayTransport::new(dir.path());
    let got = AcoustIdClient::new(&replay, KEY)
        .lookup("AQADtNRSnsZT", 201)
        .unwrap();
    assert!(got.is_ok());
    assert_eq!(got.results[0].recordings[0].id.as_deref(), Some(REC_FOUND));
    let other_duration = AcoustIdClient::new(&replay, KEY).lookup("AQADtNRSnsZT", 202);
    assert!(other_duration.is_err());
}

/// A recorded 503 keeps answering 503: the paced client retries with its own
/// doubling backoff (MusicBrainz's "Retry-After: 0" is not "immediately"),
/// four tries in all, then reports the HTTP failure.
#[test]
fn a_replayed_503_is_retried_then_reported() {
    let dir = ReplayDir::new("replay-503");
    dir.record(
        &MusicBrainzClient::<ReplayTransport>::search_request("Mira Quell", "Copper Weather"),
        &response(503, &[("Retry-After", "0")], b"<html>busy</html>"),
    );
    let clock = ManualClock::default();
    let replay = ReplayTransport::new(dir.path());
    let mb = MusicBrainzClient::new(Paced::new(&replay, &clock));
    let err = mb
        .search_release_groups("Mira Quell", "Copper Weather")
        .unwrap_err();
    assert!(
        matches!(err, ClientError::Status { status: 503, .. }),
        "{err}"
    );
    assert_eq!(replay.hits().len(), 4);
    // Waits of 1 s, 2 s, 4 s (base 1 s, doubling; the 1 s pacing gap is
    // inside each).
    assert_eq!(*clock.sleeps.borrow(), [SECS(1), SECS(2), SECS(4)]);
}

/// A server figure longer than the backoff is honoured on every retry; one
/// above 30 s waits 30 s (it does not abort).
#[test]
fn a_replayed_retry_after_is_honoured_and_capped() {
    for (retry_after, wait) in [("5", SECS(5)), ("120", SECS(30))] {
        let dir = ReplayDir::new("replay-retry-after");
        dir.record(
            &MusicBrainzClient::<ReplayTransport>::browse_request(RG_COPPER),
            &response(429, &[("Retry-After", retry_after)], b""),
        );
        let clock = ManualClock::default();
        let replay = ReplayTransport::new(dir.path());
        let err = MusicBrainzClient::new(Paced::new(&replay, &clock))
            .browse_releases(RG_COPPER)
            .unwrap_err();
        assert!(matches!(err, ClientError::Status { status: 429, .. }));
        // Backoffs 1, 2, 4 s are each below 5 s (and 30 s).
        assert_eq!(*clock.sleeps.borrow(), [wait, wait, wait], "{retry_after}");
    }
}

/// Over replay, MusicBrainz requests are one second apart and AcoustID
/// lookups 334 ms apart, each host on its own schedule: the first
/// MusicBrainz request after four AcoustID lookups does not wait.
#[test]
fn replayed_requests_are_paced_per_host() {
    let dir = ReplayDir::new("replay-pacing");
    let fp = |i: u64| format!("AQADtNRSnsZT{i}");
    for i in 0..4 {
        dir.record(
            &AcoustIdClient::<ReplayTransport>::lookup_request(KEY, &fp(i), 200 + i),
            &json_response(200, &acoustid_body(&[])),
        );
    }
    let titles = ["Copper Weather", "Lanterns Over Harbour Street"];
    for t in titles {
        dir.record(
            &MusicBrainzClient::<ReplayTransport>::search_request("Mira Quell", t),
            &json_response(200, &search_body(vec![])),
        );
    }
    let clock = ManualClock::default();
    let noting = Noting::new(ReplayTransport::new(dir.path()), Some(&clock));
    let paced = Paced::new(&noting, &clock);
    let acoustid = AcoustIdClient::new(&paced, KEY);
    for i in 0..4 {
        assert!(acoustid.lookup(&fp(i), 200 + i).unwrap().results.is_empty());
    }
    let mb = MusicBrainzClient::new(&paced);
    for t in titles {
        assert!(
            mb.search_release_groups("Mira Quell", t)
                .unwrap()
                .is_empty()
        );
    }
    assert_eq!(
        noting.times("api.acoustid.org"),
        [MS(0), MS(334), MS(668), MS(1002)]
    );
    assert_eq!(noting.times("musicbrainz.org"), [MS(1002), MS(2002)]);
    for r in noting.requests() {
        assert!(
            r.headers
                .contains(&("User-Agent".to_owned(), USER_AGENT.to_owned())),
            "{r:?}"
        );
    }
}

/// `FollowRedirects` over replay asks for each hop's own recording: the CAA
/// 307 to archive.org, archive.org's 302 to a storage node on another
/// host, the node's 200; and a relative Location resolved against the hop
/// that sent it.
#[test]
fn follow_redirects_over_replay_walks_a_constructed_chain() {
    let dir = ReplayDir::new("follow-chain");
    let (archive, node) = archive_hops();
    let caa = caa_front(RG_HARBOUR);
    let k1 = dir.record_head(&caa, &response(307, &[("Location", &archive)], b""));
    let k2 = dir.record_head(&archive, &response(302, &[("Location", &node)], b""));
    let k3 = dir.record_head(
        &node,
        &response(200, &[("Content-Type", "image/jpeg")], b""),
    );
    let replay = ReplayTransport::new(dir.path());
    let req = Request::from_url(Method::Head, &caa)
        .unwrap()
        .header("User-Agent", USER_AGENT);
    let resp = FollowRedirects(&replay).send(&req).unwrap();
    assert_eq!(resp.status, 200);
    assert_eq!(resp.header("content-type"), Some("image/jpeg"));
    assert_eq!(replay.hits(), [k1, k2, k3]);

    // A relative Location resolves against the hop that sent it.
    let dir = ReplayDir::new("follow-relative");
    dir.record_head(
        &caa,
        &response(302, &[("Location", "/elsewhere/front")], b""),
    );
    let k = dir.record_head(
        "https://coverartarchive.org/elsewhere/front",
        &response(404, &[], b""),
    );
    let replay = ReplayTransport::new(dir.path());
    assert_eq!(FollowRedirects(&replay).send(&req).unwrap().status, 404);
    assert_eq!(replay.hits().last(), Some(&k));

    // Asked not to follow: the 302 itself comes back.
    let resp = FollowRedirects(&replay)
        .send(&req.clone().no_redirects())
        .unwrap();
    assert_eq!(resp.status, 302);
}

/// A redirect loop ends after the live transport's limit (30 redirects, so
/// 31 requests) as too many redirects; a hop to plain HTTP on a public host
/// is refused as insecure before it is asked.
#[test]
fn follow_redirects_over_replay_stops_loops_and_plain_http() {
    let dir = ReplayDir::new("follow-loop");
    let caa = caa_front(RG_COPPER);
    dir.record_head(&caa, &response(307, &[("Location", &caa)], b""));
    let replay = ReplayTransport::new(dir.path());
    let req = Request::from_url(Method::Head, &caa).unwrap();
    match FollowRedirects(&replay).send(&req) {
        Err(TransportError::TooManyRedirects { limit, url }) => {
            assert_eq!(limit, musaic_net::live::MAX_REDIRECTS);
            assert_eq!(url, caa);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        replay.hits().len() as u32,
        musaic_net::live::MAX_REDIRECTS + 1
    );

    let dir = ReplayDir::new("follow-http");
    let plain = "http://archive.org/download/x/x.jpg";
    dir.record_head(&caa, &response(302, &[("Location", plain)], b""));
    dir.record_head(plain, &response(200, &[], b""));
    let replay = ReplayTransport::new(dir.path());
    assert!(matches!(
        FollowRedirects(&replay).send(&req),
        Err(TransportError::InsecureScheme { .. })
    ));
    assert_eq!(replay.hits().len(), 1);
}

/// Every hop of the front-cover check is a HEAD under the public rule:
/// public addresses only, redirects returned to the check, the CAA timeout,
/// the full User-Agent.
fn assert_head_hops(sent: &[Request]) {
    assert!(!sent.is_empty());
    for r in sent {
        assert_eq!(r.method, Method::Head, "{r:?}");
        assert!(r.public_only && !r.follow_redirects, "{r:?}");
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

/// The front-cover check over replay: a 200 at the end of the CAA ->
/// archive.org -> storage node chain is verified and offers the CAA URL (not
/// the storage URL); a 404 is "Missing" with its reason and no URL; any
/// other final status is reported as that status; a hop nobody recorded is a
/// failed check. Each answer is walked one hop at a time by the check.
#[test]
fn front_cover_check_over_replay() {
    let dir = ReplayDir::new("front-check");
    let (archive, node) = archive_hops();
    dir.record_head(
        &caa_front(RG_HARBOUR),
        &response(307, &[("Location", &archive)], b""),
    );
    dir.record_head(&archive, &response(302, &[("Location", &node)], b""));
    dir.record_head(
        &node,
        &response(200, &[("Content-Type", "image/jpeg")], b""),
    );
    dir.record_head(&caa_front(RG_NO_COVER), &response(404, &[], b""));
    dir.record_head(
        &caa_front(RG_BROKEN_COVER),
        &response(307, &[("Location", "/release-group/x/elsewhere")], b""),
    );
    dir.record_head(
        "https://coverartarchive.org/release-group/x/elsewhere",
        &response(500, &[], b""),
    );

    let noting = Noting::new(ReplayTransport::new(dir.path()), None);
    let caa = CoverArtClient::new(&noting);

    let front = caa.check_front(RG_HARBOUR);
    let offered = caa_front(RG_HARBOUR);
    assert!(
        matches!(&front, FrontCover::Verified { url } if *url == offered),
        "{front:?}"
    );
    assert_eq!(front.cover_url(), Some(offered.as_str()));
    assert_eq!(front.reason(), "ok");
    assert_eq!(caa.front_url(RG_HARBOUR), offered);
    let urls: Vec<String> = noting.requests().iter().map(Request::url).collect();
    assert_eq!(urls, [offered.clone(), archive, node]);
    assert_head_hops(&noting.requests());

    let front = caa.check_front(RG_NO_COVER);
    assert!(matches!(front, FrontCover::Missing), "{front:?}");
    assert_eq!(front.reason(), "no front-cover for this release");
    assert_eq!(front.cover_url(), None);

    let front = caa.check_front(RG_BROKEN_COVER);
    assert!(matches!(front, FrontCover::Status(500)), "{front:?}");
    assert_eq!(front.reason(), "HTTP 500");
    assert_eq!(front.cover_url(), None);

    let front = caa.check_front(RG_COPPER);
    assert!(
        matches!(
            &front,
            FrontCover::Failed(TransportError::Unobserved { .. })
        ),
        "{front:?}"
    );
    assert_eq!(front.cover_url(), None);
    assert!(front.reason().starts_with("unobserved in replay set"));
    assert_head_hops(&noting.requests());
    // 3 + 1 + 2 + 1 hops.
    assert_eq!(noting.requests().len(), 7);
}
