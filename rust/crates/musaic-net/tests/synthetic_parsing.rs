//! What the MusicBrainz and AcoustID clients read from invented answers
//! (see `synthetic/mod.rs`), how each failure is classed, and that no
//! failure text carries the AcoustID key.

mod common;
mod synthetic;

use std::collections::BTreeSet;

use common::{FaultServer, closed_port};
use musaic_log::{Level, capture};
use musaic_net::{
    AcoustIdClient, ClientError, FailureClass, LiveTransport, MusicBrainzClient,
    RecordingCanonical, RecordingLookup, ReplayTransport, Request, Transport, TransportError,
    USER_AGENT,
};
use serde_json::json;
use synthetic::*;

fn search_req(artist: &str, title: &str) -> Request {
    MusicBrainzClient::<ReplayTransport>::search_request(artist, title)
}

/// Search hits in answer order. A numeric score is read as is, a string
/// score is trimmed and parsed, a missing or unreadable one reads 0; a hit
/// without an id is dropped (it cannot become an anchor).
#[test]
fn search_hits_are_read_in_order() {
    let body = search_body(vec![
        search_group(
            RG_HARBOUR,
            json!(100),
            "Lanterns Over Harbour Street",
            "Mira Quell",
        ),
        json!({"title": "No Id Here", "score": 99}),
        search_group(RG_COPPER, json!(" 87 "), "Copper Weather", "Mira Quell"),
        search_group(RG_NO_COVER, json!(null), "Quiet Ferries", "Mira Quell"),
        search_group(RG_BROKEN_COVER, json!("high"), "Salt Lamps", "Mira Quell"),
    ]);
    let t = Canned::default().answer(
        &search_req("Mira Quell", "Harbour"),
        json_response(200, &body),
    );
    let hits = MusicBrainzClient::new(&t)
        .search_release_groups("Mira Quell", "Harbour")
        .unwrap();
    let got: Vec<(&str, &str, u32)> = hits
        .iter()
        .map(|h| (h.release_group_id.as_str(), h.title.as_str(), h.score))
        .collect();
    assert_eq!(
        got,
        [
            (RG_HARBOUR, "Lanterns Over Harbour Street", 100),
            (RG_COPPER, "Copper Weather", 87),
            (RG_NO_COVER, "Quiet Ferries", 0),
            (RG_BROKEN_COVER, "Salt Lamps", 0),
        ]
    );
    // The request: the Lucene phrase query, JSON, five hits.
    let sent = &t.sent.lock().unwrap()[0];
    assert_eq!(
        sent.url(),
        "https://musicbrainz.org/ws/2/release-group/?query=artist%3A%22Mira+Quell%22+AND+release%3A%22Harbour%22&fmt=json&limit=5"
    );
    // No release-groups at all: no hits.
    let t = Canned::default().answer(&search_req("A", "B"), json_response(200, &json!({})));
    assert!(
        MusicBrainzClient::new(&t)
            .search_release_groups("A", "B")
            .unwrap()
            .is_empty()
    );
}

/// Each release's shape: a medium's track count is its `track-count`, else
/// the number of tracks listed; a medium's duration is the sum of its track
/// lengths (ms / 1000) only when every track has one; a release's track
/// count is the sum over its media, its duration known only when every
/// medium's is.
#[test]
fn release_listing_shapes() {
    let body = browse_body(vec![
        (
            REL_ONE,
            vec![
                medium(
                    1,
                    Some(2),
                    vec![
                        track(1, "Brinelight Hollow", Some(180_000)),
                        track(2, "Glass Buoys", Some(200_500)),
                    ],
                ),
                medium(2, None, vec![track(1, "Night Ferry Home", Some(99_500))]),
            ],
        ),
        (
            REL_TWO,
            vec![medium(
                1,
                Some(2),
                vec![
                    track(1, "Brinelight Hollow", Some(181_000)),
                    track(2, "Glass Buoys", None),
                ],
            )],
        ),
    ]);
    let t = Canned::default().answer(
        &MusicBrainzClient::<ReplayTransport>::browse_request(RG_HARBOUR),
        json_response(200, &body),
    );
    let releases = MusicBrainzClient::new(&t)
        .browse_releases(RG_HARBOUR)
        .unwrap();
    assert_eq!(releases.len(), 2);
    let (id, one) = &releases[0];
    assert_eq!(id, REL_ONE);
    assert_eq!(one.track_count, 3);
    assert_eq!(one.mediums.len(), 2);
    assert_eq!(one.mediums[0].track_count, 2);
    assert_eq!(one.mediums[0].duration_s, Some(380.5));
    assert_eq!(one.mediums[1].track_count, 1);
    assert_eq!(one.mediums[1].duration_s, Some(99.5));
    assert_eq!(one.duration_s, Some(480.0));
    let (id, two) = &releases[1];
    assert_eq!(id, REL_TWO);
    assert_eq!(two.track_count, 2);
    assert_eq!(two.mediums[0].duration_s, None);
    assert_eq!(two.duration_s, None);
}

/// The release-group lookup by id: the title stripped; the artist credit
/// joined with its joinphrases (a credit's own name, else its artist's) and
/// stripped; the first credit's sort-name; the year is the first four
/// characters of `first-release-date` when they are digits; the genre is
/// the group tag with the highest count (the first one on ties) and only
/// when that count is at least 2.
#[test]
fn release_group_fields() {
    let group = json!({
        "id": RG_COPPER,
        "title": "  Copper Weather ",
        "first-release-date": "1991-10",
        "artist-credit": [
            credit("Mira Quell", "Quell, Mira", " & "),
            {"artist": {"name": "The Tin Orchard", "sort-name": "Tin Orchard, The"}, "joinphrase": ""},
        ],
        "tags": [
            {"name": "chamber pop", "count": 3},
            {"name": "parlour jazz", "count": 3},
            {"name": "folk", "count": 1},
        ],
    });
    let below = json!({
        "id": RG_HARBOUR,
        "title": "Lanterns Over Harbour Street",
        "first-release-date": "199",
        "artist-credit": [credit("Mira Quell", "Quell, Mira", "")],
        "tags": [{"name": "folk", "count": 1}],
    });
    let stripped = json!({
        "id": RG_NO_COVER,
        "title": "Quiet Ferries",
        "first-release-date": "2004",
        "artist-credit": [credit("Mira Quell", "Quell, Mira", "")],
        "tags": [{"name": "folk", "count": 1}, {"name": "  sea shanty ", "count": 2}],
    });
    let req = MusicBrainzClient::<ReplayTransport>::release_group_request;
    let t = Canned::default()
        .answer(&req(RG_COPPER), json_response(200, &group))
        .answer(&req(RG_HARBOUR), json_response(200, &below))
        .answer(&req(RG_NO_COVER), json_response(200, &stripped))
        .answer(
            &req(RG_BROKEN_COVER),
            json_response(200, &json!(["not", "an object"])),
        )
        .answer(&req(REC_OTHER), json_response(503, &json!({})));
    let mb = MusicBrainzClient::new(&t);

    let f = mb.release_group(RG_COPPER).unwrap();
    assert_eq!(f.title.as_deref(), Some("Copper Weather"));
    assert_eq!(f.artist.as_deref(), Some("Mira Quell & The Tin Orchard"));
    assert_eq!(f.artist_sort.as_deref(), Some("Quell, Mira"));
    assert_eq!(f.year.as_deref(), Some("1991"));
    assert_eq!(f.primary_genre.as_deref(), Some("chamber pop"));

    let f = mb.release_group(RG_HARBOUR).unwrap();
    assert_eq!(f.year, None, "three characters are not a year");
    assert_eq!(f.primary_genre, None, "the top count is 1");

    let f = mb.release_group(RG_NO_COVER).unwrap();
    assert_eq!(f.year.as_deref(), Some("2004"));
    assert_eq!(f.primary_genre.as_deref(), Some("sea shanty"));

    let e = mb.release_group(RG_BROKEN_COVER).unwrap_err();
    assert!(matches!(e, ClientError::Parse { .. }), "{e}");
    assert_eq!(e.class(), FailureClass::Parse);
    let e = mb.release_group(REC_OTHER).unwrap_err();
    assert!(matches!(e, ClientError::Status { status: 503, .. }), "{e}");
    assert_eq!(e.class(), FailureClass::Http);
    assert_eq!(t.count(), 5, "one request per lookup");
}

/// The release-groups a recording appears on, de-duplicated in first-seen
/// order (the first title seen is kept); a release without a group id is
/// skipped. A 404 is an HTTP failure without a log line; another status
/// logs a warning.
#[test]
fn recording_release_groups_are_deduplicated() {
    let mut body = recording_groups_body(&[
        (RG_COPPER, "Copper Weather"),
        (RG_HARBOUR, "Lanterns Over Harbour Street"),
        (RG_COPPER, "Copper Weather (deluxe)"),
        ("", "Nameless"),
    ]);
    body["releases"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id": REL_TWO, "title": "No Group"}));
    let req = MusicBrainzClient::<ReplayTransport>::recording_request;
    let t = Canned::default()
        .answer(&req(REC_FOUND), json_response(200, &body))
        .answer(
            &req(REC_MISSING),
            json_response(404, &json!({"error": "Not Found"})),
        )
        .answer(&req(REC_OTHER), json_response(502, &json!({})));
    let mb = MusicBrainzClient::new(&t);
    assert_eq!(
        mb.recording_release_groups(REC_FOUND).unwrap(),
        [
            (RG_COPPER.to_owned(), "Copper Weather".to_owned()),
            (
                RG_HARBOUR.to_owned(),
                "Lanterns Over Harbour Street".to_owned()
            ),
        ]
    );
    let (got, lines) = capture(|| mb.recording_release_groups(REC_MISSING));
    assert!(matches!(got, Err(ClientError::Status { status: 404, .. })));
    assert!(lines.is_empty(), "{lines:?}");
    let (got, lines) = capture(|| mb.recording_release_groups(REC_OTHER));
    assert!(matches!(got, Err(ClientError::Status { status: 502, .. })));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].level, Level::Warning);
    assert_eq!(
        lines[0].message,
        format!("MB recording release-groups HTTP 502 for {REC_OTHER}")
    );
}

/// The recording lookup: the title stripped; the artist is every credit's
/// name (or its artist's name) followed by its joinphrase, a credit without
/// a name skipped together with its joinphrase, the whole stripped.
/// Definitive answers (`Done`): a readable 200 (`None` for a blank title),
/// a 404, an empty id (no request). Not definitive (`Failed`, asked again
/// next time, logged): a failed request, another status, a body that is
/// not JSON, a JSON shape the reading cannot accept.
#[test]
fn recording_lookup_outcomes() {
    let req = MusicBrainzClient::<ReplayTransport>::recording_canonical_request;
    let ids: Vec<String> = (0..7)
        .map(|i| format!("{}{i}", &REC_SECOND[..35]))
        .collect();
    let found = json!({
        "id": REC_FOUND,
        "title": "  Brinelight Hollow ",
        "artist-credit": [
            {"name": "", "joinphrase": " feat. "},
            {"name": "Mira Quell", "joinphrase": " & "},
            {"artist": {"name": "The Tin Orchard", "sort-name": "Tin Orchard, The"}, "joinphrase": " "},
        ],
    });
    let t = Canned::default()
        .answer(&req(REC_FOUND), json_response(200, &found))
        .answer(
            &req(REC_MISSING),
            json_response(404, &json!({"error": "Not Found"})),
        )
        .answer(
            &req(&ids[0]),
            json_response(200, &json!({"title": "   ", "artist-credit": []})),
        )
        .answer(&req(&ids[1]), json_response(503, &json!({})))
        .fail(&req(&ids[2]), "musicbrainz.org")
        .answer(&req(&ids[3]), response(200, &[], b"<html>not json</html>"))
        .answer(&req(&ids[4]), json_response(200, &json!(["a", "list"])))
        .answer(&req(&ids[5]), json_response(200, &json!({"title": 42})))
        .answer(
            &req(&ids[6]),
            json_response(200, &json!({"title": "Glass Buoys"})),
        );
    let mb = MusicBrainzClient::new(&t);

    let (got, lines) = capture(|| mb.recording_canonical_lookup(REC_FOUND));
    assert_eq!(
        got,
        RecordingLookup::Done(Some(RecordingCanonical {
            title: "Brinelight Hollow".into(),
            artist: "Mira Quell & The Tin Orchard".into(),
        }))
    );
    assert!(lines.is_empty());
    // No credit at all: an empty artist.
    assert_eq!(
        mb.recording_canonical_lookup(&ids[6])
            .answer()
            .unwrap()
            .artist,
        ""
    );
    let (got, lines) = capture(|| mb.recording_canonical_lookup(REC_MISSING));
    assert_eq!(got, RecordingLookup::Done(None));
    assert!(lines.is_empty());
    assert_eq!(
        mb.recording_canonical_lookup(&ids[0]),
        RecordingLookup::Done(None)
    );
    let before = t.count();
    assert_eq!(
        mb.recording_canonical_lookup(""),
        RecordingLookup::Done(None)
    );
    assert_eq!(t.count(), before, "an empty id asks nothing");

    let failed = |id: &str, head: String| {
        let (got, lines) = capture(|| mb.recording_canonical_lookup(id));
        assert_eq!(got, RecordingLookup::Failed, "{id}");
        assert_eq!(lines.len(), 1, "{id}: {lines:?}");
        assert_eq!(lines[0].level, Level::Warning);
        assert_eq!(lines[0].target, "musaic.mb_lookup");
        assert!(lines[0].message.starts_with(&head), "{}", lines[0].message);
    };
    failed(
        &ids[1],
        format!("MB recording lookup HTTP 503 for {}", ids[1]),
    );
    failed(
        &ids[2],
        format!(
            "MB recording lookup failed for {}: could not resolve host",
            ids[2]
        ),
    );
    failed(
        &ids[3],
        format!(
            "MB recording lookup for {} returned a body that is not JSON: ",
            ids[3]
        ),
    );
    for id in [&ids[4], &ids[5]] {
        failed(
            id,
            format!("MB recording lookup for {id} returned a malformed answer: "),
        );
    }

    // The plain reading of the same answers: `None` where the lookup
    // failed or found nothing, an error only for a malformed shape.
    assert_eq!(mb.recording_canonical(&ids[1]).unwrap(), None);
    assert_eq!(mb.recording_canonical(&ids[3]).unwrap(), None);
    assert!(mb.recording_canonical(&ids[4]).is_err());
    assert!(mb.recording_canonical(&ids[5]).is_err());
}

/// AcoustID answers: `status: ok` gives the results; `status: error` is the
/// service's own failure, its text without the key; a body that is not the
/// expected JSON is a parse failure; another status an HTTP failure; no
/// answer a network failure.
#[test]
fn acoustid_answers_and_failure_classes() {
    let lookup = |fp: &str| AcoustIdClient::<ReplayTransport>::lookup_request(KEY_ENCODED, fp, 201);
    let echo = format!(
        "{{\"status\": \"error\", \"error\": {{\"code\": 4, \"message\": \"invalid API key {KEY_ENCODED}\"}}}}"
    );
    let t = Canned::default()
        .answer(
            &lookup("AQAA-ok"),
            json_response(200, &acoustid_body(&[(0.93, REC_FOUND), (0.41, REC_OTHER)])),
        )
        .answer(&lookup("AQAA-error"), response(200, &[], echo.as_bytes()))
        .answer(
            &lookup("AQAA-garbled"),
            response(200, &[], b"{\"status\": \"ok\", \"results\": 7"),
        )
        .answer(&lookup("AQAA-busy"), response(500, &[], b"oops"))
        .fail(&lookup("AQAA-offline"), "api.acoustid.org");
    let client = AcoustIdClient::new(&t, KEY_ENCODED);

    let ok = client.lookup("AQAA-ok", 201).unwrap();
    assert!(ok.is_ok());
    assert_eq!(ok.results.len(), 2);
    assert_eq!(ok.results[0].score, Some(0.93));
    let rg = &ok.results[0].recordings[0].releasegroups[0];
    assert_eq!(rg.id.as_deref(), Some(RG_HARBOUR));
    assert_eq!(rg.releases[0].track_count, Some(10));
    assert_eq!(rg.releases[0].mediums[0].track_count, Some(10));

    let classes: Vec<FailureClass> = ["AQAA-error", "AQAA-garbled", "AQAA-busy", "AQAA-offline"]
        .iter()
        .map(|fp| {
            let e = client.lookup(fp, 201).unwrap_err();
            assert_key_absent(&format!("{e} {e:?}"));
            e.class()
        })
        .collect();
    assert_eq!(
        classes,
        [
            FailureClass::Service,
            FailureClass::Parse,
            FailureClass::Http,
            FailureClass::Network,
        ]
    );
    match client.lookup("AQAA-error", 201).unwrap_err() {
        ClientError::AcoustId(text) => {
            assert!(text.contains("invalid API key <REDACTED>"), "{text}")
        }
        other => panic!("{other:?}"),
    }
    // The key is on the wire (the service needs it), in the form body.
    let sent = t.sent.lock().unwrap();
    assert!(
        sent[0]
            .form_body()
            .unwrap()
            .contains("client=syn%2Bthetic%2Fkey%3D1")
    );
    assert!(
        sent[0]
            .headers
            .contains(&("User-Agent".into(), USER_AGENT.into()))
    );
}

fn assert_key_absent(text: &str) {
    for k in [KEY, KEY_ENCODED, "syn%2Bthetic%2Fkey%3D1"] {
        assert!(!text.contains(k), "the key leaks: {text}");
    }
}

/// The same classes over a real HTTP exchange with a scripted loopback
/// server: a 500 search is an HTTP failure, a 200 that is not JSON a parse
/// failure, a refused connection a network failure (and "offline"), an
/// AcoustID error answer the service's failure; every request carries the
/// User-Agent.
#[test]
fn failure_classes_over_a_loopback_server() {
    let server = |step: serde_json::Value| FaultServer::start(vec![step], true);
    let mb_with = |s: &FaultServer| {
        MusicBrainzClient::with_origin(LiveTransport::with_static_dns(s.addr, &[]), &s.origin())
    };

    let s = server(json!({"status": 500, "body": {"text": "busy"}}));
    let e = mb_with(&s)
        .search_release_groups("Mira Quell", "Copper Weather")
        .unwrap_err();
    assert!(matches!(e, ClientError::Status { status: 500, .. }), "{e}");
    assert_eq!(e.class(), FailureClass::Http);
    assert_eq!(s.seen()[0].user_agent, USER_AGENT);
    assert_eq!(s.seen()[0].path, "/ws/2/release-group/");

    let s = server(json!({"status": 200, "body": {"text": "<html>maintenance</html>"}}));
    let e = mb_with(&s).browse_releases(RG_COPPER).unwrap_err();
    assert!(matches!(e, ClientError::Parse { .. }), "{e}");
    assert_eq!(e.class(), FailureClass::Parse);

    let s = server(
        json!({"status": 200, "body": {"text": serde_json::to_string(&search_body(vec![
        search_group(RG_COPPER, json!(91), "Copper Weather", "Mira Quell"),
    ])).unwrap()}}),
    );
    let hits = mb_with(&s)
        .search_release_groups("Mira Quell", "Copper Weather")
        .unwrap();
    assert_eq!(hits[0].release_group_id, RG_COPPER);
    assert_eq!(hits[0].score, 91);

    let port = closed_port();
    let addr = format!("127.0.0.1:{port}").parse().unwrap();
    let mb = MusicBrainzClient::with_origin(
        LiveTransport::with_static_dns(addr, &[]),
        &format!("http://127.0.0.1:{port}"),
    );
    match mb
        .search_release_groups("Mira Quell", "Copper Weather")
        .unwrap_err()
    {
        ClientError::Transport(t) => assert!(t.is_offline(), "{t}"),
        other => panic!("{other:?}"),
    }

    let s = server(json!({"status": 200, "body": {"text": format!(
        "{{\"status\": \"error\", \"error\": {{\"message\": \"invalid API key {KEY}\"}}}}"
    )}}));
    let live = LiveTransport::with_static_dns(s.addr, &[]);
    let e = AcoustIdClient::with_origin(&live, KEY, &s.origin())
        .lookup("AQAA", 201)
        .unwrap_err();
    assert_eq!(e.class(), FailureClass::Service);
    assert_key_absent(&format!("{e} {e:?}"));
    let seen = &s.seen()[0];
    assert_eq!(seen.method, "POST");
    assert_eq!(seen.path, "/v2/lookup");
    assert_eq!(seen.user_agent, USER_AGENT);
    assert!(String::from_utf8_lossy(&seen.body).contains("client=synthetic-test-key"));
}

/// Every transport and client failure produced with the key on the request
/// (as a query parameter and a form field), formatted with Display and
/// Debug, and the requests and clients themselves: none shows the key, raw
/// or URL-encoded. The unobserved failure comes from a replay directory the
/// test writes.
#[test]
fn no_failure_text_contains_the_key() {
    let dir = ReplayDir::new("secrets");
    for key in [KEY, KEY_ENCODED] {
        let keyed = |origin: &str| {
            let mut r = Request::post_form(origin, "/x", vec![("client".into(), key.into())])
                .query("client", key)
                .timeout(std::time::Duration::from_millis(500));
            r.method = musaic_net::Method::Get;
            r
        };
        let mut errors: Vec<ClientError> = Vec::new();
        let via = |step: serde_json::Value, max_body: Option<u64>| {
            let s = FaultServer::start(vec![step], true);
            let mut r = keyed(&s.origin());
            r.max_body = max_body;
            LiveTransport::with_static_dns(s.addr, &[])
                .send(&r)
                .unwrap_err()
        };
        errors.push(ClientError::Transport(via(
            json!({"status": 200, "delay_s": 1.5, "body": {"text": "{}"}}),
            None,
        )));
        errors.push(ClientError::Transport(via(
            json!({"status": 302, "headers": {"Location": "{origin}/again"}}),
            None,
        )));
        errors.push(ClientError::Transport(via(
            json!({"status": 200, "body": {"fill": "x", "len": 64}}),
            Some(10),
        )));
        errors.push(ClientError::Transport(via(
            json!({"status": 200, "body": {"text": "{}"}, "action": "short_body"}),
            None,
        )));
        errors.push(ClientError::Transport(via(
            json!({"status": 302, "headers": {"Location": format!("http://elsewhere.example/?client={key}")}}),
            None,
        )));
        let port = closed_port();
        let refused = format!("127.0.0.1:{port}").parse().unwrap();
        // A longer timeout here: Windows retries a refused loopback connect
        // for about two seconds before reporting it, so 500 ms would end as
        // a timeout there instead of a refused connection.
        errors.push(ClientError::Transport(
            LiveTransport::with_static_dns(refused, &[])
                .send(
                    &keyed(&format!("http://127.0.0.1:{port}"))
                        .timeout(std::time::Duration::from_secs(10)),
                )
                .unwrap_err(),
        ));
        errors.push(ClientError::Transport(
            LiveTransport::with_static_dns(refused, &["unresolvable.example"])
                .send(&keyed("https://unresolvable.example"))
                .unwrap_err(),
        ));
        errors.push(ClientError::Transport(
            LiveTransport::new()
                .send(&keyed("http://plain.example"))
                .unwrap_err(),
        ));
        errors.push(ClientError::Transport(
            LiveTransport::with_static_dns(refused, &[])
                .send(&keyed("https://loopback.example").public_only())
                .unwrap_err(),
        ));
        errors.push(ClientError::Transport(
            ReplayTransport::new(dir.path())
                .send(&keyed("https://musicbrainz.org"))
                .unwrap_err(),
        ));
        errors.push(ClientError::Transport(TransportError::Other(
            keyed("https://x.example").scrub(&format!("echo {key}")),
        )));
        let lookup = AcoustIdClient::<ReplayTransport>::lookup_request(key, "AQAA", 10);
        let t = Canned::default().answer(
            &lookup,
            response(
                200,
                &[],
                format!("{{\"status\": \"error\", \"error\": {{\"message\": \"bad key {key}\"}}}}")
                    .as_bytes(),
            ),
        );
        errors.push(AcoustIdClient::new(&t, key).lookup("AQAA", 10).unwrap_err());
        let t = Canned::default().answer(
            &lookup,
            response(
                200,
                &[],
                format!("{{\"status\": \"ok\", \"results\": [\"{key}").as_bytes(),
            ),
        );
        errors.push(AcoustIdClient::new(&t, key).lookup("AQAA", 10).unwrap_err());
        let t = Canned::default().answer(&lookup, response(401, &[], b"no"));
        let c = AcoustIdClient::new(&t, key);
        errors.push(c.lookup("AQAA", 10).unwrap_err());

        let kinds: BTreeSet<String> = errors
            .iter()
            .map(|e| match e {
                ClientError::Transport(t) => format!("{t:?}")
                    .split([' ', '{', '('])
                    .next()
                    .unwrap()
                    .to_owned(),
                other => format!("{other:?}")
                    .split([' ', '{', '('])
                    .next()
                    .unwrap()
                    .to_owned(),
            })
            .collect();
        for want in [
            "Timeout",
            "TooManyRedirects",
            "BodyTooLarge",
            "Protocol",
            "InsecureScheme",
            "Connect",
            "Dns",
            "Refused",
            "Unobserved",
            "Other",
            "AcoustId",
            "Parse",
            "Status",
        ] {
            assert!(kinds.contains(want), "{want} not produced: {kinds:?}");
        }
        let encoded: String = key
            .bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect();
        let clean = |what: &str, text: &str| {
            assert!(
                !text.contains(key) && !text.contains(&encoded),
                "{what} leaks the key: {text}"
            );
        };
        for e in &errors {
            clean("Display", &format!("{e}"));
            clean("Debug", &format!("{e:?}"));
        }
        clean("Request Debug", &format!("{lookup:?}"));
        clean("redacted_url", &keyed("https://x.example").redacted_url());
        clean("AcoustIdClient Debug", &format!("{c:?}"));
    }
}
