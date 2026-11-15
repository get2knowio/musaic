//! Self-authored MusicBrainz / AcoustID / Cover Art Archive answers for the
//! public tests: invented artists and titles, freshly generated ids. The
//! shapes follow what the clients and the resolver's parsers read
//! (`musaic_resolve::musicbrainz`, `musaic_resolve::find_release`,
//! `musaic_resolve::acoustid`).
#![allow(dead_code)]

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use base64::Engine as _;
use musaic_net::{Clock, Method, Request, Response, Transport, TransportError};
use serde_json::{Value, json};

// Release-groups, releases and recordings of the invented catalogue.
pub const RG_HARBOUR: &str = "e2b22f13-628e-42a1-92c5-097d14b85b3e";
pub const RG_COPPER: &str = "35141ffb-77b7-4ba8-873c-e56df845e521";
pub const RG_NO_COVER: &str = "5efa0781-a4fb-4b16-855d-b64b0c4c3598";
pub const RG_BROKEN_COVER: &str = "4cdb3451-6158-4bea-963a-512d85305ded";
pub const REL_ONE: &str = "12b3a9b3-1839-457e-802c-5a9bafd37200";
pub const REL_TWO: &str = "471b38bc-5b73-4312-96bf-8bd6d02aa504";
pub const REC_FOUND: &str = "ae2e5904-8b24-4d6e-a470-b92cd511c088";
pub const REC_MISSING: &str = "831e7f5c-dd5f-4de8-af6b-15954a062a18";
pub const REC_OTHER: &str = "48433336-6807-42d3-b0ab-9d8b04e7fb9f";
pub const COVER_IMAGE: &str = "c13da821-98d3-47ca-a792-fbc618979426";
pub const ACOUSTID_TRACK: &str = "faab7240-c1f3-4c0c-afea-1e5efaace1cf";
pub const REC_SECOND: &str = "970243bf-8421-43aa-a8dd-710f63c2392c";

/// An AcoustID key used only by these tests; two spellings, one that
/// changes under URL encoding.
pub const KEY: &str = "synthetic-test-key";
pub const KEY_ENCODED: &str = "syn+thetic/key=1";

pub fn response(status: u16, headers: &[(&str, &str)], body: &[u8]) -> Response {
    Response {
        status,
        headers: headers
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        body: body.to_vec(),
    }
}

pub fn json_response(status: u16, body: &Value) -> Response {
    response(
        status,
        &[("Content-Type", "application/json")],
        &serde_json::to_vec(body).unwrap(),
    )
}

/// A directory under the system temp dir, removed on drop.
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "musaic-net-synthetic-{}-{}-{tag}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A replay directory written by the tests, one record per request, in
/// the format `ReplayTransport` reads (`status`, `headers`, `content_b64`).
pub struct ReplayDir {
    pub dir: TempDir,
}

impl ReplayDir {
    pub fn new(tag: &str) -> Self {
        Self {
            dir: TempDir::new(tag),
        }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Store `resp` as the answer to `req`; returns the replay key.
    pub fn record(&self, req: &Request, resp: &Response) -> String {
        let key = req.replay_key();
        let headers: serde_json::Map<String, Value> = resp
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), Value::from(v.as_str())))
            .collect();
        let rec = json!({
            "key": key,
            "method": req.method.as_str(),
            "url": req.redacted_url(),
            "status": resp.status,
            "headers": headers,
            "content_b64": base64::engine::general_purpose::STANDARD.encode(&resp.body),
        });
        std::fs::write(
            self.path().join(format!("{key}.json")),
            serde_json::to_vec_pretty(&rec).unwrap(),
        )
        .unwrap();
        key
    }

    /// Store the answer to a HEAD of an absolute URL.
    pub fn record_head(&self, url: &str, resp: &Response) -> String {
        self.record(&Request::from_url(Method::Head, url).unwrap(), resp)
    }
}

/// Virtual time: sleeping advances the clock and is logged.
#[derive(Default)]
pub struct ManualClock {
    pub now: Cell<Duration>,
    pub sleeps: RefCell<Vec<Duration>>,
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

/// Wraps a transport and notes every request (with the clock's time).
pub struct Noting<'c, T> {
    pub inner: T,
    pub clock: Option<&'c ManualClock>,
    pub sent: RefCell<Vec<(Duration, Request)>>,
}

impl<'c, T> Noting<'c, T> {
    pub fn new(inner: T, clock: Option<&'c ManualClock>) -> Self {
        Self {
            inner,
            clock,
            sent: RefCell::new(Vec::new()),
        }
    }

    pub fn times(&self, host: &str) -> Vec<Duration> {
        self.sent
            .borrow()
            .iter()
            .filter(|(_, r)| r.host() == host)
            .map(|(t, _)| *t)
            .collect()
    }

    pub fn requests(&self) -> Vec<Request> {
        self.sent.borrow().iter().map(|(_, r)| r.clone()).collect()
    }
}

impl<T: Transport> Transport for Noting<'_, T> {
    fn send(&self, request: &Request) -> Result<Response, TransportError> {
        let now = self.clock.map_or(Duration::ZERO, Clock::now);
        self.sent.borrow_mut().push((now, request.clone()));
        self.inner.send(request)
    }
}

/// One canned answer per replay key: a response, or a failure to connect.
pub enum Answer {
    Respond(Response),
    Dns(String),
}

/// Answers by replay key (headers do not count); an unknown key is
/// unobserved. Every request is noted.
#[derive(Default)]
pub struct Canned {
    answers: HashMap<String, Answer>,
    pub sent: Mutex<Vec<Request>>,
}

impl Canned {
    pub fn answer(mut self, req: &Request, resp: Response) -> Self {
        self.answers.insert(req.replay_key(), Answer::Respond(resp));
        self
    }

    pub fn fail(mut self, req: &Request, host: &str) -> Self {
        self.answers
            .insert(req.replay_key(), Answer::Dns(host.to_owned()));
        self
    }

    pub fn count(&self) -> usize {
        self.sent.lock().unwrap().len()
    }
}

impl Transport for Canned {
    fn send(&self, req: &Request) -> Result<Response, TransportError> {
        self.sent.lock().unwrap().push(req.clone());
        match self.answers.get(&req.replay_key()) {
            Some(Answer::Respond(r)) => Ok(r.clone()),
            Some(Answer::Dns(host)) => Err(TransportError::Dns { host: host.clone() }),
            None => Err(TransportError::Unobserved {
                key: req.replay_key(),
                method: req.method.as_str(),
                url: req.redacted_url(),
            }),
        }
    }
}

/// One artist credit: `(name, sort-name, joinphrase)`.
pub fn credit(name: &str, sort_name: &str, joinphrase: &str) -> Value {
    json!({
        "name": name,
        "joinphrase": joinphrase,
        "artist": {"id": REC_OTHER, "name": name, "sort-name": sort_name},
    })
}

/// A release-group search answer (`/ws/2/release-group/?query=…`).
pub fn search_body(groups: Vec<Value>) -> Value {
    json!({"created": "2026-01-01T00:00:00.000Z", "count": groups.len(), "offset": 0, "release-groups": groups})
}

/// One hit of a search answer.
pub fn search_group(id: &str, score: Value, title: &str, artist: &str) -> Value {
    json!({
        "id": id,
        "score": score,
        "title": title,
        "primary-type": "Album",
        "first-release-date": "1987-05-04",
        "artist-credit": [credit(artist, artist, "")],
    })
}

/// One release track: `(title, length in ms or null)`.
pub fn track(position: u64, title: &str, length_ms: Option<u64>) -> Value {
    json!({
        "position": position,
        "number": position.to_string(),
        "title": title,
        "length": length_ms,
        "recording": {"id": REC_OTHER, "title": title, "length": length_ms},
    })
}

/// One medium of a release. `track_count` is written as MusicBrainz does
/// unless `None`.
pub fn medium(position: u64, track_count: Option<u64>, tracks: Vec<Value>) -> Value {
    let mut m = json!({"position": position, "format": "CD", "tracks": tracks});
    if let Some(tc) = track_count {
        m["track-count"] = json!(tc);
    }
    m
}

/// A release browse answer (`/ws/2/release?release-group=…`).
pub fn browse_body(releases: Vec<(&str, Vec<Value>)>) -> Value {
    let releases: Vec<Value> = releases
        .into_iter()
        .map(|(id, media)| {
            json!({
                "id": id,
                "title": "Lanterns Over Harbour Street",
                "date": "1987-05-04",
                "status": "Official",
                "media": media,
            })
        })
        .collect();
    json!({"release-count": releases.len(), "release-offset": 0, "releases": releases})
}

/// A recording lookup with releases and their release-groups.
pub fn recording_groups_body(groups: &[(&str, &str)]) -> Value {
    let releases: Vec<Value> = groups
        .iter()
        .map(|(id, title)| {
            json!({
                "id": REL_ONE,
                "title": title,
                "release-group": {"id": id, "title": title, "primary-type": "Album"},
            })
        })
        .collect();
    json!({"id": REC_FOUND, "title": "Brinelight Hollow", "length": 201_000, "releases": releases})
}

/// An AcoustID `/v2/lookup` answer with one result per `(score, recording id)`.
pub fn acoustid_body(results: &[(f64, &str)]) -> Value {
    let results: Vec<Value> = results
        .iter()
        .map(|(score, rec)| {
            json!({
                "id": ACOUSTID_TRACK,
                "score": score,
                "recordings": [{
                    "id": rec,
                    "title": "Brinelight Hollow",
                    "duration": 201,
                    "releasegroups": [{
                        "id": RG_HARBOUR,
                        "title": "Lanterns Over Harbour Street",
                        "type": "Album",
                        "releases": [{"id": REL_ONE, "track_count": 10, "medium_count": 1,
                                      "mediums": [{"position": 1, "track_count": 10}]}],
                    }],
                }],
            })
        })
        .collect();
    json!({"status": "ok", "results": results})
}

/// The Cover Art Archive front URL of a release-group.
pub fn caa_front(rg: &str) -> String {
    format!("https://coverartarchive.org/release-group/{rg}/front")
}

/// The archive.org download URL the CAA redirects to, and the storage node
/// archive.org redirects to (both invented).
pub fn archive_hops() -> (String, String) {
    (
        format!("https://archive.org/download/mbid-{COVER_IMAGE}/mbid-{COVER_IMAGE}-1001.jpg"),
        format!(
            "https://node-07.archive-storage.example.org/0/items/mbid-{COVER_IMAGE}/mbid-{COVER_IMAGE}-1001.jpg"
        ),
    )
}
