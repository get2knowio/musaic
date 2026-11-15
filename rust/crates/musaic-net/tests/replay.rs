//! The network layer against traffic recorded from the real services
//! (the recorded replay set, private test data). Nothing here can reach the
//! network: the only transport is the replay one.

mod common;

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use musaic_net::{
    ACOUSTID_META, AcoustIdClient, ClientError, Clock, MusicBrainzClient, Paced, PacingPolicy,
    ReplayTransport, Request, Response, RetryPolicy, Transport, TransportError, USER_AGENT,
    name_search_terms,
};
use musaic_resolve::{AlbumContext, LookupResult, Rule, decide};
use serde_json::Value;

fn golden(rel: &str) -> Value {
    musaic_testdata::golden(rel)
}

fn read_private_json(rel: &str) -> Value {
    musaic_testdata::read_json(musaic_testdata::private_required(rel))
}

/// The recorded traffic (private test data: every test that replays it is
/// Tier 2 and starts with `require_private!("replay")`).
fn replay_dir() -> PathBuf {
    musaic_testdata::private_required("replay")
}

fn replay() -> ReplayTransport {
    ReplayTransport::new(replay_dir())
}

/// Virtual time: sleeping advances the clock and is logged.
#[derive(Default)]
struct ManualClock {
    now: Cell<Duration>,
    sleeps: RefCell<Vec<Duration>>,
}

impl Clock for ManualClock {
    fn now(&self) -> Duration {
        self.now.get()
    }

    fn sleep(&self, d: Duration) {
        self.now.set(self.now.get() + d);
        self.sleeps.borrow_mut().push(d);
    }
}

/// When a request was sent, to which host, with which headers.
type Sent = (Duration, String, Vec<(String, String)>);

/// Answers from a script and notes when each request was sent.
struct Scripted<'c> {
    clock: &'c ManualClock,
    answers: RefCell<Vec<Response>>,
    sent: RefCell<Vec<Sent>>,
}

impl<'c> Scripted<'c> {
    fn new(clock: &'c ManualClock, mut answers: Vec<Response>) -> Self {
        answers.reverse();
        Self {
            clock,
            answers: RefCell::new(answers),
            sent: RefCell::new(Vec::new()),
        }
    }

    fn times(&self) -> Vec<Duration> {
        self.sent.borrow().iter().map(|s| s.0).collect()
    }
}

impl Transport for Scripted<'_> {
    fn send(&self, request: &Request) -> Result<Response, TransportError> {
        self.sent.borrow_mut().push((
            self.clock.now(),
            request.host().to_string(),
            request.headers.clone(),
        ));
        Ok(self
            .answers
            .borrow_mut()
            .pop()
            .unwrap_or_else(|| resp(200, &[], b"{}")))
    }
}

fn resp(status: u16, headers: &[(&str, &str)], body: &[u8]) -> Response {
    Response {
        status,
        headers: headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        body: body.to_vec(),
    }
}

const SECS: fn(u64) -> Duration = Duration::from_secs;

/// Every one of the 94 recorded HTTPS AcoustID lookups is found by
/// the request the Rust client builds (so the key derivation, parameter set
/// and redaction of the `client` key agree with the recorder), whatever the
/// real key is; and the answers reproduce the expected anchors end to end.
#[test]
fn acoustid_lookups_replay_and_reproduce_the_expected_anchors() {
    musaic_testdata::require_private!("replay");
    let transport = replay();
    let client = AcoustIdClient::new(&transport, "any-key-the-recording-never-saw");
    let mut albums: BTreeMap<String, Vec<Vec<LookupResult>>> = BTreeMap::new();
    let mut lookups = 0;
    for golden in [
        "captures/dsp-synthetic/dsp/goldens.json",
        "captures/dsp-real/dsp/goldens.json",
    ] {
        // The synthetic DSP goldens are public, the real-audio set's private.
        let g = match musaic_testdata::private(golden) {
            Some(p) => musaic_testdata::read_json(p),
            None => musaic_testdata::public_json(golden),
        };
        // serde_json's map is sorted, so tracks arrive in file name order.
        for (rel, f) in g["files"].as_object().unwrap() {
            let fp = f["fingerprint"]["fingerprint"].as_str().unwrap();
            let duration = f["fingerprint"]["duration"].as_f64().unwrap().round() as u64;
            let answer = client
                .lookup(fp, duration)
                .unwrap_or_else(|e| panic!("{rel}: {e}"));
            albums
                .entry(rel.split('/').next().unwrap().to_string())
                .or_default()
                .push(answer.results);
            lookups += 1;
        }
    }
    assert_eq!(lookups, 94);
    assert_eq!(transport.hits().len(), 94);

    let expected = read_private_json("captures/expected-anchors/expected-anchors.json");
    let mut anchored = Vec::new();
    for e in expected["albums"].as_array().unwrap() {
        let name = e["album"].as_str().unwrap();
        let tracks = &albums[name];
        let ctx = AlbumContext {
            track_count: tracks.len(),
            title: e["title_used_for_ties"].as_str().unwrap(),
            multi_disc: e["multi_disc"].as_bool().unwrap(),
        };
        let d = decide(tracks, &ctx, &[], &Rule::default());
        assert_eq!(
            d.outcome.anchor().map(|a| a.release_group_id.as_str()),
            e["anchor"].as_str(),
            "{name}"
        );
        if let Some(a) = d.outcome.anchor() {
            assert_eq!(Some(a.score as u64), e["score"].as_u64(), "{name}");
            anchored.push(name.to_string());
        }
    }
    assert_eq!(anchored.len(), 3, "{anchored:?}");
}

/// The request is HTTPS, a form POST, with the pinned metadata set.
#[test]
fn acoustid_request_shape() {
    let r = AcoustIdClient::<ReplayTransport>::lookup_request("SECRET", "AQAA", 291);
    assert_eq!(r.origin, "https://api.acoustid.org");
    assert_eq!(r.path, "/v2/lookup");
    let form: BTreeMap<_, _> = r.form.clone().unwrap().into_iter().collect();
    assert_eq!(form["meta"], ACOUSTID_META);
    assert_eq!(form["duration"], "291");
    assert_eq!(form["format"], "json");
    // The key never reaches a URL, a log line or a replay key.
    assert!(!r.url().contains("SECRET") && !r.redacted_url().contains("SECRET"));
    assert_eq!(
        r.replay_key(),
        AcoustIdClient::<ReplayTransport>::lookup_request("OTHER", "AQAA", 291).replay_key()
    );
    assert_ne!(
        r.replay_key(),
        AcoustIdClient::<ReplayTransport>::lookup_request("SECRET", "AQAB", 291).replay_key()
    );
}

/// The name search and release browse for the
/// five real albums hit the recordings made through the Python app's own request
/// code, and return what that run saw.
#[test]
fn musicbrainz_real_album_requests_replay() {
    musaic_testdata::require_private!("replay");
    let transport = replay();
    let mb = MusicBrainzClient::new(&transport);
    let g = read_private_json("captures/mb-name-search/mb/name-search.json");
    for a in g["albums"].as_array().unwrap() {
        let (artist, title) = (
            a["album"]["artist"].as_str().unwrap(),
            a["album"]["title"].as_str().unwrap(),
        );
        let found = mb
            .search_release_groups(artist, title)
            .unwrap_or_else(|e| panic!("{artist} / {title}: {e}"));
        let want: Vec<(String, u64)> = a["search_candidates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                (
                    c["id"].as_str().unwrap().to_string(),
                    c["score"].as_u64().unwrap(),
                )
            })
            .collect();
        let have: Vec<(String, u64)> = found
            .iter()
            .map(|c| (c.release_group_id.clone(), c.score as u64))
            .collect();
        assert_eq!(have, want, "{title}");
        for b in a["browsed"].as_array().unwrap() {
            let releases = mb
                .browse_releases(b["release_group"].as_str().unwrap())
                .unwrap();
            assert_eq!(releases.len() as u64, b["release_count"].as_u64().unwrap());
        }
    }
    assert_eq!(transport.hits().len(), 10);
}

/// For the synthetic library, the terms derived from
/// the scanned album rows produce exactly the searches the Python app made in
/// the live capture. Three of those were answered 503 at the time; replay
/// returns the 503 and the client reports it.
#[test]
fn musicbrainz_synthetic_album_searches_replay() {
    musaic_testdata::require_private!("replay");
    let scan = golden("scan-synthetic.json");
    let transport = replay();
    let mb = MusicBrainzClient::new(&transport);
    let (mut ok, mut busy) = (0, Vec::new());
    for a in scan["albums"].as_array().unwrap() {
        let s = |k: &str| a[k].as_str().map(str::to_string);
        let album = musaic_core::Album {
            id: String::new(),
            path: String::new(),
            dir_name: s("dir_name").unwrap(),
            title: s("title"),
            artist: s("artist"),
            album_artist: s("album_artist"),
            year: None,
            genre: None,
            track_count: 0,
            total_seconds: 0.0,
            has_embedded_cover: false,
            cover_file: None,
            sample_track: String::new(),
            dir_mtime: 0.0,
            dir_artist: s("dir_artist"),
            dir_album: s("dir_album"),
            is_compilation: a["is_compilation"].as_bool().unwrap(),
            disc_number: None,
            disc_total: None,
            sample_rate: None,
            bits_per_sample: None,
            channels: None,
            track_numbers: vec![],
            album_artist_sort: None,
            artist_sort: None,
            album_sort: None,
            track_artists: vec![],
        };
        let (artist, title) =
            name_search_terms(&album).unwrap_or_else(|| panic!("no terms for {}", album.dir_name));
        match mb.search_release_groups(&artist, &title) {
            Ok(_) => ok += 1,
            Err(ClientError::Status { status: 503, .. }) => busy.push(album.dir_name),
            Err(e) => panic!("{}: {e}", album.dir_name),
        }
    }
    busy.sort();
    assert_eq!(ok, 10);
    assert_eq!(
        busy,
        [
            "Aurora Fields - First Light [FLAC]",
            "Aurora Fields - Folder Art [FLAC]",
            "The Lanterns - Porchlight [FLAC]"
        ]
    );
}

/// A request nobody recorded fails as unobserved; it is never sent anywhere.
#[test]
fn unrecorded_requests_fail() {
    musaic_testdata::require_private!("replay");
    let mb = MusicBrainzClient::new(replay());
    match mb.search_release_groups("Nobody", "Nothing Recorded") {
        Err(ClientError::Transport(TransportError::Unobserved {
            method: "GET", url, ..
        })) => {
            assert!(
                url.starts_with("https://musicbrainz.org/ws/2/release-group/?query="),
                "{url}"
            )
        }
        other => panic!("{other:?}"),
    }
}

/// Every MusicBrainz request carries the User-Agent, and no two are
/// sent less than a second apart, across different kinds of request.
#[test]
fn musicbrainz_requests_are_paced_globally_and_identified() {
    let clock = ManualClock::default();
    let spy = Scripted::new(&clock, vec![]);
    let paced = Paced::new(&spy, &clock);
    for i in 0..5 {
        let req = if i % 2 == 0 {
            MusicBrainzClient::<ReplayTransport>::search_request("A", "B")
        } else {
            MusicBrainzClient::<ReplayTransport>::browse_request(
                "11111111-1111-4111-8111-111111111111",
            )
        };
        paced.send(&req).unwrap();
        clock.sleep(Duration::from_millis(100)); // the caller does a little work in between
    }
    let times = spy.times();
    assert!(
        times.windows(2).all(|w| w[1] - w[0] >= SECS(1)),
        "{times:?}"
    );
    assert_eq!(times[4], SECS(4));
    for (_, host, headers) in spy.sent.borrow().iter() {
        assert_eq!(host, "musicbrainz.org");
        assert!(headers.contains(&("User-Agent".to_string(), USER_AGENT.to_string())));
    }
}

/// AcoustID stays at or below three requests per second, and its
/// pacing is independent of MusicBrainz's.
#[test]
fn acoustid_requests_stay_under_three_per_second() {
    let clock = ManualClock::default();
    let spy = Scripted::new(&clock, vec![]);
    let paced = Paced::new(&spy, &clock);
    paced
        .send(&MusicBrainzClient::<ReplayTransport>::search_request(
            "A", "B",
        ))
        .unwrap();
    for _ in 0..7 {
        paced
            .send(&AcoustIdClient::<ReplayTransport>::lookup_request(
                "k", "AQAA", 10,
            ))
            .unwrap();
    }
    let acoustid: Vec<Duration> = spy
        .sent
        .borrow()
        .iter()
        .filter(|s| s.1 == "api.acoustid.org")
        .map(|s| s.0)
        .collect();
    assert_eq!(
        acoustid[0],
        Duration::ZERO,
        "the first AcoustID request does not wait for MusicBrainz's slot"
    );
    for w in acoustid.windows(4) {
        assert!(
            w[3] - w[0] >= SECS(1),
            "four requests inside one second: {acoustid:?}"
        );
    }
}

/// A 503 is retried. MusicBrainz's "Retry-After: 0" does not mean
/// "immediately"; a longer server figure is honoured; the wait doubles.
#[test]
fn busy_answers_are_retried_with_backoff() {
    let busy0 = || resp(503, &[("Retry-After", "0")], b"busy");
    let clock = ManualClock::default();
    let spy = Scripted::new(
        &clock,
        vec![
            busy0(),
            busy0(),
            resp(200, &[], b"{\"release-groups\": []}"),
        ],
    );
    let mb = MusicBrainzClient::new(Paced::new(&spy, &clock));
    assert!(mb.search_release_groups("A", "B").unwrap().is_empty());
    assert_eq!(spy.times(), [SECS(0), SECS(1), SECS(3)]); // waits of 1 s, then 2 s

    let clock = ManualClock::default();
    let spy = Scripted::new(
        &clock,
        vec![
            resp(429, &[("retry-after", "7")], b""),
            resp(200, &[], b"{}"),
        ],
    );
    Paced::new(&spy, &clock)
        .send(&MusicBrainzClient::<ReplayTransport>::search_request(
            "A", "B",
        ))
        .unwrap();
    assert_eq!(spy.times(), [SECS(0), SECS(7)]);
}

/// Retries are bounded, and the last busy answer is reported as failure.
#[test]
fn retries_are_bounded() {
    let clock = ManualClock::default();
    let spy = Scripted::new(&clock, (0..10).map(|_| resp(503, &[], b"busy")).collect());
    let policy = RetryPolicy {
        max_attempts: 4,
        base_backoff: SECS(1),
        max_backoff: SECS(3),
    };
    let mb = MusicBrainzClient::new(Paced::with_policies(
        &spy,
        &clock,
        PacingPolicy::default(),
        policy,
    ));
    assert!(matches!(
        mb.search_release_groups("A", "B"),
        Err(ClientError::Status { status: 503, .. })
    ));
    assert_eq!(spy.times(), [SECS(0), SECS(1), SECS(3), SECS(6)]); // 1 s, 2 s, then capped at 3 s
}

/// MusicBrainz pacing on recorded data: a recorded 503 goes through the same retry path
/// (replay keeps answering 503, so it ends as a failure after four tries).
#[test]
fn a_recorded_503_is_retried_then_reported() {
    musaic_testdata::require_private!("replay");
    let clock = ManualClock::default();
    let transport = replay();
    let mb = MusicBrainzClient::new(Paced::new(&transport, &clock));
    let err = mb
        .search_release_groups("Aurora Fields", "First Light")
        .unwrap_err();
    assert!(
        matches!(err, ClientError::Status { status: 503, .. }),
        "{err}"
    );
    assert_eq!(transport.hits().len(), 4);
    assert_eq!(*clock.sleeps.borrow(), [SECS(1), SECS(2), SECS(4)]);
}

/// `urllib.parse.unquote_plus` for the recorded URLs and form bodies.
fn unquote_plus(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
                i += 2;
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8(out).unwrap()
}

fn pairs(q: &str) -> BTreeMap<String, String> {
    q.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (unquote_plus(k), unquote_plus(v))
        })
        .collect()
}

/// The two phrases of `artist:"…" AND release:"…"` (mb_lookup.py:622-628).
fn lucene_terms(q: &str) -> (String, String) {
    let mut terms = Vec::new();
    let mut chars = q.chars();
    while let Some(c) = chars.next() {
        if c == '"' {
            let mut t = String::new();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => t.push(chars.next().unwrap()),
                    '"' => break,
                    c => t.push(c),
                }
            }
            terms.push(t);
        }
    }
    assert_eq!(terms.len(), 2, "{q}");
    (terms[0].clone(), terms[1].clone())
}

/// For every recording under
/// the recorded replay set (private), the request the Rust client builds for the same
/// inputs hashes to the recorded replay key. Two kinds are not built by the
/// client and are accounted for instead: the plain-HTTP AcoustID lookups in
/// the Python app's old shape (retired by HTTPS only / pacing; the client's own
/// HTTPS shape is recorded beside them), and archive.org hops that are only
/// ever reached by following CAA's redirects.
#[test]
fn every_recording_is_reproduced_by_the_rust_request_builders() {
    musaic_testdata::require_private!("replay");
    let dir = replay_dir();
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    let mut total = 0;
    let mut redirect_targets = Vec::new();
    let mut hops = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        total += 1;
        let rec: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let key = rec["key"].as_str().unwrap();
        assert_eq!(path.file_stem().unwrap().to_str().unwrap(), key);
        let url = rec["url"].as_str().unwrap();
        let method = rec["method"].as_str().unwrap();
        if let Some(loc) = rec["headers"]["Location"].as_str() {
            redirect_targets.push(loc.to_string());
        }
        let (scheme_host, rest) = url.split_at(url.find("://").unwrap() + 3);
        let (host, path_q) = rest.split_at(rest.find('/').unwrap());
        let (p, q) = path_q.split_once('?').unwrap_or((path_q, ""));
        let q = pairs(q);
        let (built, kind): (Option<Request>, &str) = match (method, scheme_host, host) {
            ("GET", "https://", "musicbrainz.org") if p == "/ws/2/release-group/" => {
                let (artist, title) = lucene_terms(&q["query"]);
                (
                    Some(MusicBrainzClient::<ReplayTransport>::search_request(
                        &artist, &title,
                    )),
                    "mb-search",
                )
            }
            ("GET", "https://", "musicbrainz.org") if p == "/ws/2/release" => (
                Some(MusicBrainzClient::<ReplayTransport>::browse_request(
                    &q["release-group"],
                )),
                "mb-browse",
            ),
            ("GET", "https://", "musicbrainz.org")
                if p.starts_with("/ws/2/release-group/") && q.contains_key("inc") =>
            {
                (
                    Some(MusicBrainzClient::<ReplayTransport>::release_group_request(
                        p.trim_start_matches("/ws/2/release-group/"),
                    )),
                    "mb-release-group",
                )
            }
            ("GET", "https://", "musicbrainz.org")
                if p.starts_with("/ws/2/recording/") && q["inc"] == "artist-credits" =>
            {
                // mb_lookup.get_recording_canonical's shape, for the
                // duplicate-tracks canonical route.
                (
                    Some(
                        MusicBrainzClient::<ReplayTransport>::recording_canonical_request(
                            p.trim_start_matches("/ws/2/recording/"),
                        ),
                    ),
                    "mb-recording-canonical",
                )
            }
            ("GET", "https://", "musicbrainz.org") if p.starts_with("/ws/2/recording/") => (
                Some(MusicBrainzClient::<ReplayTransport>::recording_request(
                    p.trim_start_matches("/ws/2/recording/"),
                )),
                "mb-recording",
            ),
            ("POST", "https://", "api.acoustid.org") => {
                let form = pairs(rec["request_body"].as_str().unwrap());
                assert_eq!(form["client"], "<REDACTED>");
                assert_eq!(form["meta"], ACOUSTID_META);
                (
                    Some(AcoustIdClient::<ReplayTransport>::lookup_request(
                        "test-key-not-real",
                        &form["fingerprint"],
                        form["duration"].parse().unwrap(),
                    )),
                    "acoustid-https",
                )
            }
            ("POST", "http://", "api.acoustid.org") => {
                // The Python app's shape: plain HTTP, meta=recordings. Retired.
                let form = pairs(rec["request_body"].as_str().unwrap());
                assert_eq!(form["meta"], "recordings");
                let ours = AcoustIdClient::<ReplayTransport>::lookup_request(
                    "k",
                    &form["fingerprint"],
                    form["duration"].parse().unwrap(),
                );
                assert!(ours.origin.starts_with("https://"));
                assert_ne!(ours.replay_key(), key);
                (None, "acoustid-http-retired")
            }
            ("HEAD", "https://", "coverartarchive.org") => {
                let rg = p
                    .strip_prefix("/release-group/")
                    .and_then(|r| r.strip_suffix("/front"))
                    .unwrap();
                (
                    Some(musaic_net::CoverArtClient::<ReplayTransport>::front_request(rg)),
                    "caa-front",
                )
            }
            ("HEAD", "https://", h) if h == "archive.org" || h.ends_with(".archive.org") => {
                hops.push(url.to_string());
                (None, "archive-redirect-hop")
            }
            // The front-cover GET recorded for the cover preview route: the
            // request `fetch_preview` builds, hop by hop.
            ("GET", "https://", "coverartarchive.org") => (
                Some(
                    musaic_net::preview::public_image_request(
                        url,
                        musaic_net::preview::PREVIEW_TIMEOUT,
                    )
                    .unwrap(),
                ),
                "caa-front-get",
            ),
            ("GET", "https://", h) if h == "archive.org" || h.ends_with(".archive.org") => {
                hops.push(url.to_string());
                (
                    Some(
                        musaic_net::preview::public_image_request(
                            url,
                            musaic_net::preview::PREVIEW_TIMEOUT,
                        )
                        .unwrap(),
                    ),
                    "archive-image-hop",
                )
            }
            other => panic!("unclassified recording {key}: {other:?} {url}"),
        };
        if let Some(req) = built {
            assert_eq!(req.replay_key(), key, "{kind}: {url}");
            assert_eq!(req.method.as_str(), method);
        }
        *counts.entry(kind).or_default() += 1;
    }
    // Every archive.org hop is the target of a recorded redirect, or the
    // first hop after CAA's (whose own 307 is folded into the CAA record:
    // requests records the final answer under the first URL).
    for h in &hops {
        let reached =
            redirect_targets.contains(h) || h.starts_with("https://archive.org/download/");
        assert!(reached, "{h}");
    }
    let expected: BTreeMap<&str, usize> = [
        ("acoustid-http-retired", 93),
        // 93 for the Python app's fpcalc fingerprints plus 52 recorded later
        // for this build's own fingerprints of the real-audio copy.
        ("acoustid-https", 145),
        // One live run: 3 release-group
        // details and 3 CAA HEADs (4 archive.org hops) for the albums only this
        // build anchors, and 2 recording details (one found, one 404).
        // A later run: one front-cover GET (its two archive.org
        // hops), and for the hinted Night Mix groups 3 listings, 4
        // release-group details and 3 CAA HEADs (one 404; 4 archive.org hops).
        ("archive-image-hop", 2),
        ("archive-redirect-hop", 10),
        ("caa-front", 8),
        ("caa-front-get", 1),
        ("mb-browse", 9),
        ("mb-recording", 38),
        ("mb-recording-canonical", 2),
        ("mb-release-group", 7),
        ("mb-search", 17),
    ]
    .into();
    assert_eq!(counts, expected);
    assert_eq!(total, counts.values().sum::<usize>());
}

/// CAA front cover on recorded data: the CAA HEAD for the Night Mix anchor was
/// answered 200 after its redirects (requests folds the chain into the first
/// URL's recording), so the cover is offered; the other was a 404, so it is
/// not, with the Python app's reason.
#[test]
fn caa_front_cover_replays() {
    musaic_testdata::require_private!("replay");
    let transport = replay();
    let caa = musaic_net::CoverArtClient::new(&transport);
    let night_mix = common::rec("night_mix_rg");
    let ok = caa.check_front(night_mix);
    assert_eq!(
        ok.cover_url(),
        Some(format!("https://coverartarchive.org/release-group/{night_mix}/front").as_str())
    );
    let missing = caa.check_front(common::rec("rip_0003_rg"));
    assert_eq!(missing.cover_url(), None);
    assert_eq!(missing.reason(), "no front-cover for this release");
    assert_eq!(transport.hits().len(), 2);
}

/// Every request the crate builds carries the
/// full User-Agent with the contact URL, exactly once (the Python app sent
/// python-requests' default to AcoustID and "musaic/0.1" on cover GETs).
/// Headers are not part of the replay key (the recording tool's
/// `replay_key`: method, canonical URL, form body), so the change leaves
/// every recorded key as it was.
#[test]
fn every_request_carries_the_full_user_agent_and_headers_do_not_key_replay() {
    let rg = "11111111-1111-4111-8111-111111111111";
    let requests = [
        MusicBrainzClient::<ReplayTransport>::search_request("A", "B"),
        MusicBrainzClient::<ReplayTransport>::browse_request(rg),
        MusicBrainzClient::<ReplayTransport>::recording_request(rg),
        AcoustIdClient::<ReplayTransport>::lookup_request("k", "AQAA", 10),
        musaic_net::CoverArtClient::<ReplayTransport>::front_request(rg),
        musaic_net::CoverArtClient::<ReplayTransport>::download_request(
            "https://coverartarchive.org/release/x/front.jpg",
        )
        .unwrap(),
    ];
    for req in &requests {
        let uas: Vec<&str> = req
            .headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("user-agent"))
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(uas, [USER_AGENT], "{}", req.redacted_url());
        let mut other = req.clone();
        for (k, v) in &mut other.headers {
            if k.eq_ignore_ascii_case("user-agent") {
                *v = "musaic/0.1".into();
            }
        }
        assert_eq!(other.replay_key(), req.replay_key());
    }
}
