//! The release-group lookup and redirect following over
//! replay.
//!
//! RECORDED: a live run recorded the lookup
//! `GET /ws/2/release-group/{mbid}?fmt=json&inc=artist-credits+tags` and the
//! Cover Art Archive front HEAD for the three release-groups only the Rust
//! cascade anchors on the real-audio set (real-album-4, real-album-5 and the
//! Conniff album; ids in the private data). The `recorded_*` tests read those. The
//! Python app never makes the lookup, so its fields are compared with the rules
//! the Python app applies to the same release-group object in a search answer
//! (mb_lookup.py:221-236), and, for Conniff, with the Python app's own reading
//! of that group in its find_release golden.
//!
//! CONSTRUCTED BODIES (`release_group_fields_match_the_python_app_reading_of_the_same_object`):
//! a release-group object the Python app itself parsed, the top group of a
//! search answer in the Python app's find_release golden
//! (testdata/golden/resolver/find_release.json, recorded or constructed
//! there), served as the lookup's answer without its search `score`.

mod common;

use std::sync::Mutex;
use std::time::Duration;

use base64::Engine as _;
use common::{golden, rec, replay_dir, replay_record};
use musaic_net::{
    ClientError, CoverArtClient, FollowRedirects, FrontCover, MB_TIMEOUT, Method,
    MusicBrainzClient, ReplayTransport, Request, Response, Transport, TransportError, USER_AGENT,
};
use serde_json::Value;

struct Stub {
    status: u16,
    body: Vec<u8>,
    seen: Mutex<Vec<Request>>,
}

impl Transport for Stub {
    fn send(&self, request: &Request) -> Result<Response, TransportError> {
        self.seen.lock().unwrap().push(request.clone());
        Ok(Response {
            status: self.status,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: self.body.clone(),
        })
    }
}

fn search_body(case: &Value) -> Option<Value> {
    let req = case["requests"].as_array()?.iter().find(|r| {
        r["url"]
            .as_str()
            .is_some_and(|u| u.contains("/ws/2/release-group/?"))
    })?;
    let key = req["key"].as_str()?;
    let b64 = match req["served"].as_str()? {
        "constructed" => case["overlay"]
            .as_array()?
            .iter()
            .find(|o| o["key"] == key)?["content_b64"]
            .as_str()?
            .to_owned(),
        "recorded" => {
            let rec: Value = serde_json::from_str(
                &std::fs::read_to_string(musaic_testdata::private_required(format!(
                    "replay/{key}.json"
                )))
                .ok()?,
            )
            .ok()?;
            rec["content_b64"].as_str()?.to_owned()
        }
        _ => return None,
    };
    serde_json::from_slice(&base64::engine::general_purpose::STANDARD.decode(b64).ok()?).ok()
}

// One GET to the group by id, JSON, with
// the artist credits and tags, our User-Agent, the search's timeout.
#[test]
fn release_group_request_shape() {
    let mbid = "4c1f7e0a-9b2d-4e61-8a35-2f7d9c0b6e14";
    let req = MusicBrainzClient::<ReplayTransport>::release_group_request(mbid);
    assert_eq!(req.method, Method::Get);
    assert_eq!(
        req.url(),
        format!(
            "https://musicbrainz.org/ws/2/release-group/{mbid}?fmt=json&inc=artist-credits%2Btags"
        )
    );
    assert!(
        req.headers
            .iter()
            .any(|(k, v)| k == "User-Agent" && v == USER_AGENT)
    );
    assert_eq!(req.timeout, Some(MB_TIMEOUT));
}

/// The three release-groups whose lookup and CAA front HEAD were recorded
/// (private).
fn disc_album() -> &'static str {
    rec("disc_album_rg")
}
fn compilation() -> &'static str {
    rec("compilation_rg")
}
fn conniff() -> &'static str {
    rec("conniff_rg")
}
fn recorded_groups() -> [&'static str; 3] {
    [disc_album(), compilation(), conniff()]
}

fn record(key: &str) -> Value {
    replay_record(key)
}

fn recorded_body(key: &str) -> Value {
    serde_json::from_slice(
        &base64::engine::general_purpose::STANDARD
            .decode(record(key)["content_b64"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap()
}

fn replay() -> ReplayTransport {
    ReplayTransport::new(replay_dir())
}

// The request this
// crate builds has the replay key, URL and method of the recorded lookup,
// and the recording answered 200.
#[test]
fn recorded_lookups_have_the_built_request_shape() {
    musaic_testdata::require_private!("replay");
    for rg in recorded_groups() {
        let req = MusicBrainzClient::<ReplayTransport>::release_group_request(rg);
        let rec = record(&req.replay_key());
        assert_eq!(rec["method"], "GET", "{rg}");
        assert_eq!(rec["url"], req.url(), "{rg}");
        assert_eq!(rec["status"], 200, "{rg}");
        assert_eq!(recorded_body(&req.replay_key())["id"], rg, "{rg}");
    }
}

/// The Python app's reading of a release-group object (mb_lookup.py:221-236):
/// title stripped, credit names joined by their joinphrases, the first
/// credit's sort-name, the first four digits of `first-release-date`, and
/// `_pick_primary_genre` (mb_lookup.py:638-648) over the GROUP's tags: the
/// highest count, the first one on ties, only at count >= 2.
fn python_rules(g: &Value) -> [Option<String>; 5] {
    let credit = g["artist-credit"].as_array().unwrap();
    let artist: String = credit
        .iter()
        .map(|c| {
            format!(
                "{}{}",
                c["name"].as_str().unwrap(),
                c["joinphrase"].as_str().unwrap_or("")
            )
        })
        .collect();
    let mut best: Option<&Value> = None;
    for t in g["tags"].as_array().unwrap() {
        if best.is_none_or(|b| t["count"].as_i64() > b["count"].as_i64()) {
            best = Some(t);
        }
    }
    let genre = best
        .filter(|t| t["count"].as_i64().unwrap_or(0) >= 2)
        .and_then(|t| t["name"].as_str())
        .map(|n| n.trim().to_owned())
        .filter(|n| !n.is_empty());
    [
        g["title"].as_str().map(|t| t.trim().to_owned()),
        Some(artist.trim().to_owned()),
        credit[0]["artist"]["sort-name"].as_str().map(str::to_owned),
        g["first-release-date"].as_str().map(|d| d[..4].to_owned()),
        genre,
    ]
}

// album fields (parse, artist credit, tags -> genre) on the three recordings:
// every field equals the Python app's rules applied to the recorded body, and
// each recording exercises one side of the genre rule.
#[test]
fn recorded_lookups_parse_by_the_python_app_rules() {
    musaic_testdata::require_private!("replay");
    let replay = replay();
    let client = MusicBrainzClient::new(&replay);
    for rg in recorded_groups() {
        let key = MusicBrainzClient::<ReplayTransport>::release_group_request(rg).replay_key();
        let body = recorded_body(&key);
        let f = client.release_group(rg).unwrap();
        let [title, artist, sort, year, genre] = python_rules(&body);
        assert_eq!(f.title, title, "{rg}");
        assert_eq!(f.artist, artist, "{rg}");
        assert_eq!(f.artist_sort, sort, "{rg}");
        assert_eq!(f.year, year, "{rg}");
        assert_eq!(f.primary_genre, genre, "{rg}");
        assert!(f.title.is_some() && f.artist.is_some() && f.year.is_some());
    }
    assert_eq!(replay.hits().len(), 3, "one request per lookup");

    let tags = |rg: &str| {
        let key = MusicBrainzClient::<ReplayTransport>::release_group_request(rg).replay_key();
        recorded_body(&key)["tags"].as_array().unwrap().clone()
    };
    // real-album-5: six group tags, exactly one at the top count, which is 2:
    // it is the genre.
    let t = tags(compilation());
    let top = t
        .iter()
        .map(|x| x["count"].as_i64().unwrap())
        .max()
        .unwrap();
    assert_eq!(top, 2);
    let at_top: Vec<_> = t.iter().filter(|x| x["count"] == top).collect();
    assert_eq!(at_top.len(), 1);
    assert_eq!(
        client
            .release_group(compilation())
            .unwrap()
            .primary_genre
            .as_deref(),
        at_top[0]["name"].as_str()
    );
    // Conniff: one group tag at count 1: below the threshold, no genre.
    let t = tags(conniff());
    assert_eq!(t.len(), 1);
    assert_eq!(t[0]["count"], 1);
    assert_eq!(client.release_group(conniff()).unwrap().primary_genre, None);
    // real-album-4: no group tags; the credited ARTIST carries tags at count
    // >= 2 ("french" 3), which must not leak into the album genre.
    assert!(tags(disc_album()).is_empty());
    let key =
        MusicBrainzClient::<ReplayTransport>::release_group_request(disc_album()).replay_key();
    let artist_tags = recorded_body(&key)["artist-credit"][0]["artist"]["tags"].clone();
    assert!(
        artist_tags
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["count"].as_i64().unwrap() >= 2)
    );
    assert_eq!(
        client.release_group(disc_album()).unwrap().primary_genre,
        None
    );
}

// The Conniff lookup reads the same title, artist, year and genre
// as the Python app's find_release read from the Conniff group in its recorded
// search answer (golden case "recorded conniff").
#[test]
fn recorded_conniff_lookup_reads_like_the_python_app_search_hit() {
    musaic_testdata::require_private!("replay");
    let g = golden("resolver/find_release.json");
    let case = g["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "recorded conniff")
        .unwrap();
    let m = &case["match"];
    assert_eq!(m["mbid"], conniff());
    let replay = replay();
    let f = MusicBrainzClient::new(&replay)
        .release_group(conniff())
        .unwrap();
    assert_eq!(f.title.as_deref(), m["title"].as_str());
    assert_eq!(f.artist.as_deref(), m["artist"].as_str());
    assert_eq!(f.year.as_deref(), m["year"].as_str());
    assert_eq!(f.primary_genre.as_deref(), m["primary_genre"].as_str());
}

// The recorded front HEAD of real-album-4's group
// answered 404, which is "Missing" with the Python app's reason
// (mb_lookup.py:661-662) and no cover URL, after one request.
#[test]
fn recorded_front_cover_404_is_missing() {
    musaic_testdata::require_private!("replay");
    let replay = replay();
    let caa = CoverArtClient::new(FollowRedirects(&replay));
    let key = CoverArtClient::<ReplayTransport>::front_request(disc_album()).replay_key();
    assert_eq!(record(&key)["status"], 404);
    let front = caa.check_front(disc_album());
    assert!(matches!(front, FrontCover::Missing), "{front:?}");
    assert_eq!(front.reason(), "no front-cover for this release");
    assert_eq!(front.cover_url(), None);
    assert_eq!(replay.hits(), vec![key]);
}

// real-album-5 and Conniff answered 200 after
// archive.org redirects. The recording stores the final answer under the CAA
// request (requests followed the redirects; the recording tool records the outer
// send), so the check sees 200 at once and offers the CAA URL, not the
// archive.org one (mb_lookup.py:267-268). The recorded hops chain up: the
// archive.org 302 points at the storage node, whose 200 is the very answer
// stored for the CAA request (same ETag and length), and FollowRedirects
// walks that chain over replay.
#[test]
fn recorded_front_cover_200_after_redirects_is_verified() {
    musaic_testdata::require_private!("replay");
    for (rg, hop) in [
        (compilation(), rec("compilation_cover_hop")),
        (conniff(), rec("conniff_cover_hop")),
    ] {
        let direct = replay();
        let caa = CoverArtClient::new(FollowRedirects(&direct));
        let front = caa.check_front(rg);
        let url = format!("https://coverartarchive.org/release-group/{rg}/front");
        assert!(
            matches!(&front, FrontCover::Verified { url: u } if *u == url),
            "{front:?}"
        );
        assert_eq!(front.cover_url(), Some(url.as_str()));
        assert_eq!(direct.hits().len(), 1, "{rg}");
        let front_rec = record(&CoverArtClient::<ReplayTransport>::front_request(rg).replay_key());

        let hops = replay();
        let req = Request::from_url(Method::Head, hop)
            .unwrap()
            .header("User-Agent", USER_AGENT)
            .timeout(Duration::from_secs(15));
        let first = record(&req.replay_key());
        assert_eq!(first["status"], 302, "{rg}");
        let resp = FollowRedirects(&hops).send(&req).unwrap();
        assert_eq!(resp.status, 200, "{rg}");
        assert_eq!(
            hops.hits().len(),
            2,
            "{rg}: the 302 and its recorded target"
        );
        let last = record(&hops.hits()[1]);
        assert_eq!(last["url"], first["headers"]["Location"], "{rg}");
        for h in ["ETag", "Content-Length"] {
            assert_eq!(last["headers"][h], front_rec["headers"][h], "{rg}: {h}");
        }
    }
}

// Title, year
// and genre read from the group object as the Python app read them from the
// same object in a search answer (title when the group has one; the
// Python app's fallback to the query title does not apply to a lookup by id);
// artist where the credit has one name
// (several names: the lookup joins with MusicBrainz's joinphrases, the
// release-listing rule, where a search hit is joined with ", ").
#[test]
fn release_group_fields_match_the_python_app_reading_of_the_same_object() {
    let g = golden("resolver/find_release.json");
    let mut compared = 0;
    for c in g["cases"].as_array().unwrap() {
        let m = &c["match"];
        let Some(mbid) = m["mbid"].as_str() else {
            continue;
        };
        if !c["hint"].as_str().unwrap_or("").trim().is_empty() {
            continue; // reranked: the top group is not the first
        }
        let Some(search) = search_body(c) else {
            continue;
        };
        let Some(group) = search["release-groups"]
            .as_array()
            .and_then(|gs| gs.iter().find(|g| g["id"] == mbid))
        else {
            continue;
        };
        let date = group["first-release-date"].as_str().unwrap_or("");
        if !(date.len() >= 4 && date[..4].bytes().all(|b| b.is_ascii_digit())) {
            continue; // the Python app's year came from the release browse
        }
        let mut body = group.clone();
        body.as_object_mut().unwrap().remove("score");
        let stub = Stub {
            status: 200,
            body: serde_json::to_vec(&body).unwrap(),
            seen: Mutex::default(),
        };
        let client = MusicBrainzClient::new(&stub);
        let f = client.release_group(mbid).unwrap();
        let ctx = c["name"].as_str().unwrap();
        // A group without a title: the Python app fell back to the query's
        // title, which a lookup by id does not have.
        if group["title"].as_str().is_some_and(|t| !t.is_empty()) {
            assert_eq!(f.title.as_deref(), m["title"].as_str(), "{ctx}");
        }
        assert_eq!(f.year.as_deref(), m["year"].as_str(), "{ctx}");
        assert_eq!(
            f.primary_genre.as_deref(),
            m["primary_genre"].as_str(),
            "{ctx}"
        );
        let credits = group["artist-credit"].as_array().map_or(0, Vec::len);
        if credits == 1 {
            assert_eq!(f.artist.as_deref(), m["artist"].as_str(), "{ctx}");
        }
        let seen = stub.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(
            seen[0].replay_key(),
            MusicBrainzClient::<ReplayTransport>::release_group_request(mbid).replay_key()
        );
        compared += 1;
    }
    assert!(compared >= 5, "only {compared} cases compared");
}

// network failures class: a non-200 answer is an HTTP failure, as the search's.
#[test]
fn release_group_http_error_is_status() {
    musaic_testdata::require_private!("replay");
    // The 503 body recorded for a MusicBrainz search (the Python app saw it).
    let g = golden("resolver/find_release.json");
    let case = g["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "recorded 503 (aurora first light)")
        .unwrap();
    let key = case["requests"][0]["key"].as_str().unwrap();
    let rec: Value = serde_json::from_str(
        &std::fs::read_to_string(musaic_testdata::private_required(format!(
            "replay/{key}.json"
        )))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(rec["status"], 503);
    let stub = Stub {
        status: 503,
        body: base64::engine::general_purpose::STANDARD
            .decode(rec["content_b64"].as_str().unwrap())
            .unwrap(),
        seen: Mutex::default(),
    };
    let err = MusicBrainzClient::new(&stub)
        .release_group(common::rec("night_mix_rg"))
        .unwrap_err();
    assert!(
        matches!(err, ClientError::Status { status: 503, .. }),
        "{err}"
    );
    assert_eq!(case["match"]["source"], "MusicBrainz (HTTP error)");
}

// Over replay, `FollowRedirects` asks for exactly the hops the
// Python app's `requests` followed when the traffic was recorded (archive.org
// 302 -> a storage mirror, 200), and the CAA front check sees the
// recorded final answers (200 verified, 404 missing).
#[test]
fn follow_redirects_over_replay_walks_the_recorded_chain() {
    musaic_testdata::require_private!("replay");
    let replay = ReplayTransport::new(replay_dir());
    let follow = FollowRedirects(&replay);
    let hop = rec("night_mix_cover_hop");
    let req = Request::from_url(Method::Head, hop)
        .unwrap()
        .header("User-Agent", USER_AGENT)
        .timeout(Duration::from_secs(15));
    let resp = follow.send(&req).unwrap();
    assert_eq!(resp.status, 200);
    assert_eq!(replay.hits().len(), 2, "the 302 and its recorded target");

    let caa = CoverArtClient::new(FollowRedirects(&replay));
    assert!(matches!(
        caa.check_front(rec("night_mix_rg")),
        FrontCover::Verified { .. }
    ));
    assert!(matches!(
        caa.check_front(rec("rip_0003_rg")),
        FrontCover::Missing
    ));
}
