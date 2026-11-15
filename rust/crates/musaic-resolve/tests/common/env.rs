//! Test doubles for the resolver's traits, wired to the real replay transport.
//!
//! * [`ReplayMb`]: `MusicBrainzSource` over `musaic_net::ReplayTransport`
//!   (the recorded replay set (private)), with an optional overlay of CONSTRUCTED responses that
//!   the golden's generator stored next to its output, keyed by the
//!   same replay key. Every request is logged with how it was served.
//! * [`World`]: album row + tracks + an in-memory anchor store (the Python app
//!   golden stubs the same functions).
//! * [`ReplayFingerprints`]: AcoustID lookups of the DSP-golden fingerprints
//!   through `musaic_net::AcoustIdClient` over the replay transport.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Mutex;

use base64::Engine as _;
use musaic_net::{
    AcoustIdClient, ClientError, Method, MusicBrainzClient, ReplayTransport, Request, Response,
    Transport, USER_AGENT,
};
use musaic_resolve::source::{
    AlbumRecord, AlbumSource, AnchorStore, FingerprintLookups, FingerprintRow, FingerprintSource,
    FlacTrack, MusicBrainzSource, NetError, StoreError, StoredAnchor, TrackLookup, TrackNumber,
};
use serde_json::Value;

use super::DSP_GOLDENS;

/// The golden generator's FRESH / STALE, and "now" for the tests.
pub const FRESH: &str = "2999-01-01T00:00:00Z";
/// 2026-11-10T00:00:00Z.
pub const NOW_EPOCH_S: i64 = 1_794_268_800;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Logged {
    pub method: String,
    pub key: String,
    pub served: &'static str,
}

pub struct ReplayMb {
    overlay: HashMap<String, Response>,
    replay: ReplayTransport,
    pub log: Mutex<Vec<Logged>>,
}

fn overlay_response(o: &Value) -> Response {
    Response {
        status: o["status"].as_u64().unwrap() as u16,
        headers: o["headers"]
            .as_object()
            .map(|h| {
                h.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_owned()))
                    .collect()
            })
            .unwrap_or_default(),
        body: base64::engine::general_purpose::STANDARD
            .decode(o["content_b64"].as_str().unwrap())
            .unwrap(),
    }
}

impl ReplayMb {
    pub fn new(overlay: &Value) -> Self {
        let overlay = overlay
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|o| (o["key"].as_str().unwrap().to_owned(), overlay_response(o)))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            overlay,
            replay: ReplayTransport::new(musaic_testdata::private_required("replay")),
            log: Mutex::new(Vec::new()),
        }
    }

    pub fn take_log(&self) -> Vec<Logged> {
        std::mem::take(&mut *self.log.lock().unwrap())
    }

    pub fn send(&self, req: &Request) -> Result<Response, NetError> {
        let key = req.replay_key();
        let log = |served| {
            self.log.lock().unwrap().push(Logged {
                method: req.method.as_str().to_owned(),
                key: key.clone(),
                served,
            })
        };
        if let Some(r) = self.overlay.get(&key) {
            log("constructed");
            return Ok(r.clone());
        }
        match self.replay.send(req) {
            Ok(r) => {
                log("recorded");
                Ok(r)
            }
            Err(e) => {
                log("unobserved");
                Err(NetError::Network(e.to_string()))
            }
        }
    }

    fn body_200(&self, req: &Request) -> Result<Vec<u8>, NetError> {
        let r = self.send(req)?;
        if r.status == 200 {
            Ok(r.body)
        } else {
            Err(NetError::Http(r.status))
        }
    }
}

/// The release-group lookup Tier 1 makes for an anchor that did not come
/// from a name search. Not a request the Python app made. Recorded for a few
/// release-groups by live runs (`common::deltas::recorded_group_lookups`); any other is a replay miss.
/// Built by `musaic-net` (the shape the recordings use).
pub fn release_group_lookup_request(release_group: &str) -> Request {
    MusicBrainzClient::<ReplayTransport>::release_group_request(release_group)
}

/// The Cover Art Archive HEAD the resolver's cover check starts with.
pub fn cover_head_request(release_group: &str) -> Request {
    request_for(
        Method::Head,
        &format!("https://coverartarchive.org/release-group/{release_group}/front"),
    )
}

/// Split an absolute URL into a `Request` (redirect targets).
fn request_for(method: Method, url: &str) -> Request {
    let (scheme, rest) = url.split_once("://").unwrap();
    let (host, path_q) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let (path, query) = path_q.split_once('?').unwrap_or((path_q, ""));
    let mut req = Request::get(&format!("{scheme}://{host}"), path);
    req.method = method;
    for (k, v) in form_urlencoded::parse(query.as_bytes()) {
        req = req.query(&k, &v);
    }
    req.header("User-Agent", USER_AGENT)
}

impl MusicBrainzSource for ReplayMb {
    fn search_release_groups(&self, artist: &str, title: &str) -> Result<Vec<u8>, NetError> {
        self.body_200(&MusicBrainzClient::<ReplayTransport>::search_request(
            artist, title,
        ))
    }

    fn browse_releases(&self, release_group: &str) -> Result<Vec<u8>, NetError> {
        self.body_200(&MusicBrainzClient::<ReplayTransport>::browse_request(
            release_group,
        ))
    }

    fn browse_release_dates(&self, release_group: &str) -> Result<Vec<u8>, NetError> {
        self.body_200(&MusicBrainzClient::<ReplayTransport>::release_dates_request(release_group))
    }

    fn lookup_release_group(&self, release_group: &str) -> Result<Vec<u8>, NetError> {
        self.body_200(&release_group_lookup_request(release_group))
    }

    fn cover_front_status(&self, release_group: &str) -> Result<u16, NetError> {
        let mut req = cover_head_request(release_group);
        for _ in 0..10 {
            let r = self.send(&req)?;
            match (r.status, r.header("Location")) {
                (301 | 302 | 303 | 307 | 308, Some(loc)) => req = request_for(Method::Head, loc),
                (s, _) => return Ok(s),
            }
        }
        Err(NetError::Network("too many redirects".into()))
    }
}

/// Album row, tracks and anchor store for one golden case.
pub struct World {
    pub album: Mutex<Option<Value>>,
    pub filenames: Mutex<Vec<String>>,
    pub tracks: Mutex<Vec<FlacTrack>>,
    pub fingerprints: Vec<FingerprintRow>,
    pub store: Mutex<Option<StoredAnchor>>,
    pub blocked: BTreeSet<String>,
    pub writes: Mutex<Vec<Value>>,
}

fn opt_str(v: &Value) -> Option<String> {
    v.as_str().map(str::to_owned)
}

pub fn flac_track(t: &Value) -> FlacTrack {
    FlacTrack {
        filename: t["filename"].as_str().unwrap_or("").to_owned(),
        tracknumber: match &t["tracknumber"] {
            Value::String(s) => TrackNumber::Text(s.clone()),
            Value::Number(n) => TrackNumber::Int(n.as_i64().unwrap()),
            _ => TrackNumber::Missing,
        },
        duration: t["duration"].as_f64(),
    }
}

pub fn stored_anchor(v: &Value) -> Option<StoredAnchor> {
    v.is_object().then(|| StoredAnchor {
        release_group_mbid: v["release_group_mbid"].as_str().unwrap().to_owned(),
        release_mbid: opt_str(&v["release_mbid"]),
        score: v["score"].as_i64(),
        last_checked_at: opt_str(&v["last_checked_at"]),
    })
}

impl World {
    /// From a `cascade.json` case (or anything with the same input keys).
    pub fn from_case(case: &Value) -> Self {
        Self {
            album: Mutex::new((!case["album"].is_null()).then(|| case["album"].clone())),
            filenames: Mutex::new(
                case["filenames"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|f| f.as_str().unwrap().to_owned())
                    .collect(),
            ),
            tracks: Mutex::new(
                case["tracks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(flac_track)
                    .collect(),
            ),
            fingerprints: case["fingerprints"]
                .as_array()
                .unwrap()
                .iter()
                .map(|f| FingerprintRow {
                    filename: f["filename"].as_str().unwrap().to_owned(),
                    recording_mbid: opt_str(&f["acoustid_recording_mbid"]),
                    score: f["acoustid_score"].as_f64(),
                })
                .collect(),
            store: Mutex::new(stored_anchor(&case["stored_anchor"])),
            blocked: case["blocked"]
                .as_array()
                .map(|a| a.iter().map(|b| b.as_str().unwrap().to_owned()).collect())
                .unwrap_or_default(),
            writes: Mutex::new(Vec::new()),
        }
    }

    pub fn take_writes(&self) -> Vec<Value> {
        std::mem::take(&mut *self.writes.lock().unwrap())
    }

    pub fn store_json(&self) -> Value {
        match &*self.store.lock().unwrap() {
            None => Value::Null,
            Some(a) => serde_json::json!({
                "release_group_mbid": a.release_group_mbid,
                "release_mbid": a.release_mbid,
                "score": a.score,
                "last_checked_at": a.last_checked_at,
            }),
        }
    }

    pub fn rename_file(&self, from: &str, to: &str) {
        let mut f = self.filenames.lock().unwrap();
        for name in f.iter_mut() {
            if name == from {
                *name = to.to_owned();
            }
        }
        f.sort();
        for t in self.tracks.lock().unwrap().iter_mut() {
            if t.filename == from {
                t.filename = to.to_owned();
            }
        }
    }
}

pub fn album_record(a: &Value, stored: Option<&StoredAnchor>) -> AlbumRecord {
    AlbumRecord {
        id: a["id"].as_str().unwrap().to_owned(),
        dir_name: opt_str(&a["dir_name"]),
        dir_artist: opt_str(&a["dir_artist"]),
        dir_album: opt_str(&a["dir_album"]),
        is_compilation: a["is_compilation"].as_bool().unwrap_or(false),
        track_count: a["track_count"].as_i64(),
        total_seconds: a["total_seconds"].as_f64(),
        title: opt_str(&a["title"]),
        album_artist: opt_str(&a["album_artist"]),
        artist: opt_str(&a["artist"]),
        disc_number: a["disc_number"].as_i64(),
        disc_total: a["disc_total"].as_i64(),
        mb_release_group_mbid: stored.map(|s| s.release_group_mbid.clone()),
    }
}

impl AlbumSource for World {
    fn album(&self, _album_id: &str) -> Option<AlbumRecord> {
        let a = self.album.lock().unwrap();
        let store = self.store.lock().unwrap();
        a.as_ref().map(|a| album_record(a, store.as_ref()))
    }
    fn filenames(&self, _album: &AlbumRecord) -> Vec<String> {
        self.filenames.lock().unwrap().clone()
    }
    fn tracks(&self, _album: &AlbumRecord) -> Vec<FlacTrack> {
        self.tracks.lock().unwrap().clone()
    }
}

impl AnchorStore for World {
    fn anchor(&self, _album_id: &str) -> Result<Option<StoredAnchor>, StoreError> {
        Ok(self.store.lock().unwrap().clone())
    }
    fn set_anchor(
        &self,
        album_id: &str,
        release_group_mbid: &str,
        release_mbid: Option<&str>,
        score: i64,
    ) -> Result<(), StoreError> {
        self.writes.lock().unwrap().push(serde_json::json!({
            "album_id": album_id,
            "release_group_mbid": release_group_mbid,
            "release_mbid": release_mbid,
            "score": score,
        }));
        *self.store.lock().unwrap() = Some(StoredAnchor {
            release_group_mbid: release_group_mbid.to_owned(),
            release_mbid: release_mbid.map(str::to_owned),
            score: Some(score),
            last_checked_at: Some(FRESH.to_owned()),
        });
        Ok(())
    }
    fn blocked(&self, _album_id: &str) -> Result<BTreeSet<String>, StoreError> {
        Ok(self.blocked.clone())
    }
    fn fingerprints(&self, _album_id: &str) -> Result<Vec<FingerprintRow>, StoreError> {
        Ok(self.fingerprints.clone())
    }
}

/// The Python app golden's stub: fingerprinting unavailable.
pub struct NoFingerprints;

impl FingerprintSource for NoFingerprints {
    fn lookups(&self, _album: &AlbumRecord) -> FingerprintLookups {
        FingerprintLookups::Unavailable("no AcoustID key".into())
    }
}

/// `(filename, fingerprint, duration)` per file of each fixture album
/// directory, in sorted order, from the DSP goldens.
pub fn dsp_files() -> BTreeMap<String, Vec<(String, String, u64)>> {
    let mut files: BTreeMap<String, (String, u64)> = BTreeMap::new();
    for g in DSP_GOLDENS {
        let v = super::dsp_golden(g);
        for (rel, f) in v["files"].as_object().unwrap() {
            let fp = &f["fingerprint"];
            if fp["status"].as_str() != Some("ok") {
                continue;
            }
            let dur = fp["duration"].as_f64().unwrap().round_ties_even() as u64;
            files.insert(
                rel.clone(),
                (fp["fingerprint"].as_str().unwrap().to_owned(), dur),
            );
        }
    }
    let mut out: BTreeMap<String, Vec<(String, String, u64)>> = BTreeMap::new();
    for (rel, (fp, dur)) in files {
        let (dir, name) = rel.split_once('/').unwrap();
        out.entry(dir.to_owned())
            .or_default()
            .push((name.to_owned(), fp, dur));
    }
    out
}

/// AcoustID lookups through `AcoustIdClient` over the replay transport.
/// `dir` is the album directory in the fixture library.
pub struct ReplayFingerprints {
    pub dir: String,
    pub files: Vec<(String, String, u64)>,
    /// Filenames whose lookup is made to fail (network-failure tests).
    pub fail: BTreeSet<String>,
    pub transport: ReplayTransport,
    pub calls: RefCell<usize>,
}

impl ReplayFingerprints {
    pub fn new(dir: &str) -> Self {
        let files = dsp_files()
            .remove(dir)
            .unwrap_or_else(|| panic!("no DSP golden for {dir}"));
        Self {
            dir: dir.to_owned(),
            files,
            fail: BTreeSet::new(),
            transport: ReplayTransport::new(musaic_testdata::private_required("replay")),
            calls: RefCell::new(0),
        }
    }
}

fn net(e: ClientError) -> NetError {
    match e {
        ClientError::Transport(t) => NetError::Network(t.to_string()),
        ClientError::Status { status, .. } => NetError::Http(status),
        other => NetError::Parse(other.to_string()),
    }
}

impl FingerprintSource for ReplayFingerprints {
    fn lookups(&self, _album: &AlbumRecord) -> FingerprintLookups {
        *self.calls.borrow_mut() += 1;
        let client = AcoustIdClient::new(&self.transport, "test-key");
        FingerprintLookups::Tracks(
            self.files
                .iter()
                .map(|(name, fp, dur)| TrackLookup {
                    filename: name.clone(),
                    results: if self.fail.contains(name) {
                        Err(NetError::Network("lookup failed (test)".into()))
                    } else {
                        client.lookup(fp, *dur).map(|r| r.results).map_err(net)
                    },
                })
                .collect(),
        )
    }
}
