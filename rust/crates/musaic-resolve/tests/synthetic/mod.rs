//! A self-contained, synthetic world for driving the resolver cascade.
//!
//! Every answer is built here: MusicBrainz, AcoustID and Cover Art Archive
//! bodies in the shape the real clients parse, for an invented artist and
//! album, with random UUIDs generated for these tests. The answers are
//! served by an in-memory transport keyed the way the replay transport keys
//! recorded traffic (`Request::replay_key`), and the requests are built by
//! the real `musaic-net` clients, so the cascade sees exactly what it would
//! see from a replay of the real services.
#![allow(dead_code)]

use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;

use musaic_net::{
    AcoustIdClient, ClientError, CoverArtClient, FrontCover, MusicBrainzClient, ReplayTransport,
    Request, Response, Transport, TransportError,
};
use musaic_resolve::source::{
    AlbumRecord, AlbumSource, AnchorStore, FingerprintLookups, FingerprintRow, FingerprintSource,
    FlacTrack, MusicBrainzSource, NetError, StoreError, StoredAnchor, TrackLookup, TrackNumber,
};
use musaic_resolve::{Env, Resolved, Resolver};
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// Invented identifiers (random UUIDs generated for these tests).

/// The studio album's release-group, and its two releases.
pub const RG_ALBUM: &str = "fb1af71e-7058-4a34-a78c-066fc63fbc24";
pub const REL_ALBUM: &str = "10914cc3-2e96-4c56-af1b-70c05af8bf3b";
pub const REL_ALBUM_DELUXE: &str = "d65897e0-6de8-4b48-ad9a-a02bcf09d4bb";
/// A live album of the same name.
pub const RG_LIVE: &str = "af32bbff-0143-41b7-affa-e6f6f29b380e";
pub const REL_LIVE: &str = "9dc0093c-52f0-42dd-97f2-242275af41fb";
/// A compilation that carries the same recordings.
pub const RG_COMPILATION: &str = "37105227-46ef-42f9-8e25-0e7fb3de810b";
pub const REL_COMPILATION: &str = "4c0d55bf-9f52-4322-8357-d8a4b401471a";
/// Another release-group, unrelated by title.
pub const RG_OTHER: &str = "00d74279-ab1a-4788-ac0f-7bd1cd14fc53";
pub const REL_OTHER: &str = "729e3626-4e5f-4155-b2a0-ab08ee3ff414";
/// A two-disc release-group (disc 1: 4 tracks, disc 2: 3 tracks).
pub const RG_DOUBLE: &str = "59116e8c-8f6e-4d86-9b91-e5eac6e95044";
pub const REL_DOUBLE: &str = "1750e1b6-e790-4f54-a475-291d90db0aae";
/// A fifth release-group for tie-break cases.
pub const RG_FIFTH: &str = "bfd794f2-d71e-4e78-a836-0fc5cbed0ce1";
pub const REL_FIFTH: &str = "2cbbc94b-867f-4082-9085-f395f9bf1bb0";

pub const ARTIST_ID: &str = "cb2cc503-dea0-4cde-b48c-cda7329b5a12";
pub const GUEST_ARTIST_ID: &str = "c78844b4-5662-4d22-a725-4bcea4180c1b";

/// The album's five recordings.
pub const RECORDINGS: [&str; 5] = [
    "b7dbafd4-5f3d-4a54-a04f-159f9158484c",
    "1138e5e6-7d61-46f6-8712-d919340ece97",
    "abc8cd89-4424-4c47-9144-9aa01190149d",
    "b7c1f1d1-8ec4-4dce-804b-bafe1dea0d74",
    "d82067f4-48ec-49ed-9a19-b0d8b499df98",
];
/// Two bonus recordings of the deluxe release.
pub const BONUS_RECORDINGS: [&str; 2] = [
    "4358f7ec-29d2-40a1-bddc-82d2c508ee49",
    "463b5a2c-5eac-4f5f-a3f8-52d8045a89f1",
];
/// AcoustID track ids, one per file.
pub const ACOUSTID_IDS: [&str; 5] = [
    "65c8116c-5ea9-4346-aecf-bfa9da87fead",
    "7f543459-969f-4c22-bf6d-73e1279c497c",
    "119f8222-7802-49a1-b71a-7067545702cc",
    "72abb4e6-67f6-498e-a74b-eb74d069b80a",
    "d92c3bbe-68c1-4f3d-878d-33c60e1d5c01",
];
/// The two-disc release's seven recordings.
pub const DOUBLE_RECORDINGS: [&str; 7] = [
    "31d03601-daa5-4247-9fd1-075337114856",
    "3400bca5-c2ca-409a-9127-f1a79661b66d",
    "343d5b94-40eb-41b7-99af-7c919f8f46b0",
    "7f21bbd6-0b23-4069-a578-5a7a8c54655e",
    "eb2969d5-a4a9-471c-be0e-76e270976c7f",
    "006f6da4-7a73-48ba-a7a6-fa569e6cab96",
    "3c627a8a-1f0c-4e21-8b89-55f014442feb",
];

// ---------------------------------------------------------------------------
// The invented album on disk.

pub const ARTIST: &str = "Lantern Coast";
pub const ARTIST_SORT: &str = "Coast, Lantern";
pub const ALBUM: &str = "Paper Harbours";
pub const ALBUM_ID: &str = "synthetic-album-paper-harbours";
pub const DIR_NAME: &str = "Lantern Coast - Paper Harbours [FLAC]";

/// (file name, TRACKNUMBER, FLAC duration in s, MusicBrainz title, MusicBrainz length in ms).
/// FLAC and MusicBrainz lengths differ by at most 0.9 s per track; the FLAC
/// durations add up to exactly 1000.0 s, the MusicBrainz lengths to 999.7 s.
pub const TRACKS: [(&str, &str, f64, &str, i64); 5] = [
    ("01 - Tide Glass.flac", "1", 181.0, "Tide Glass", 181_400),
    ("02 - Salt Ladder.flac", "2", 205.5, "Salt Ladder", 204_900),
    (
        "03 - Low Lanterns.flac",
        "3",
        167.2,
        "Low Lanterns",
        168_100,
    ),
    ("04 - Gull Static.flac", "4", 243.8, "Gull Static", 243_300),
    (
        "05 - Harbour Paper.flac",
        "5",
        202.5,
        "Harbour Paper",
        202_000,
    ),
];
pub const ALBUM_SECONDS: f64 = 1000.0;

/// "Now" for staleness: 2026-11-10T00:00:00Z.
pub const NOW_EPOCH_S: i64 = 1_794_268_800;
/// A `last_checked_at` that is never stale.
pub const FRESH: &str = "2999-01-01T00:00:00Z";
/// A `last_checked_at` far older than the 90-day refresh period.
pub const STALE: &str = "2000-01-01T00:00:00Z";

pub fn album_record() -> AlbumRecord {
    AlbumRecord {
        id: ALBUM_ID.to_owned(),
        dir_name: Some(DIR_NAME.to_owned()),
        dir_artist: Some(ARTIST.to_owned()),
        dir_album: Some(ALBUM.to_owned()),
        is_compilation: false,
        track_count: Some(TRACKS.len() as i64),
        total_seconds: Some(ALBUM_SECONDS),
        title: Some(ALBUM.to_owned()),
        album_artist: Some(ARTIST.to_owned()),
        artist: Some(ARTIST.to_owned()),
        disc_number: None,
        disc_total: None,
        mb_release_group_mbid: None,
    }
}

pub fn flac(filename: &str, tracknumber: &str, duration: f64) -> FlacTrack {
    FlacTrack {
        filename: filename.to_owned(),
        tracknumber: if tracknumber.is_empty() {
            TrackNumber::Missing
        } else {
            TrackNumber::Text(tracknumber.to_owned())
        },
        duration: Some(duration),
    }
}

pub fn album_tracks() -> Vec<FlacTrack> {
    TRACKS
        .iter()
        .map(|(f, n, d, _, _)| flac(f, n, *d))
        .collect()
}

// ---------------------------------------------------------------------------
// Response bodies, in the shapes the parsers read.

/// One `artist-credit` entry: the credited name, the artist's own name and
/// sort-name, and the join phrase that follows it.
pub fn credit(name: &str, artist_id: &str, sort_name: &str, joinphrase: &str) -> Value {
    json!({
        "name": name,
        "joinphrase": joinphrase,
        "artist": {"id": artist_id, "name": name, "sort-name": sort_name, "type": "Group"},
    })
}

pub fn main_credit() -> Vec<Value> {
    vec![credit(ARTIST, ARTIST_ID, ARTIST_SORT, "")]
}

/// One hit of `GET /ws/2/release-group/?query=...`.
pub fn search_hit(
    id: &str,
    score: Value,
    title: &str,
    credits: Vec<Value>,
    first_release_date: Option<&str>,
    tags: &[(&str, i64)],
) -> Value {
    let mut hit = json!({
        "id": id,
        "score": score,
        "primary-type": "Album",
        "title": title,
        "artist-credit": credits,
        "count": 1,
        "tags": tags.iter().map(|(n, c)| json!({"name": n, "count": c})).collect::<Vec<_>>(),
    });
    if let Some(d) = first_release_date {
        hit["first-release-date"] = json!(d);
    }
    hit
}

pub fn search_body(hits: Vec<Value>) -> Value {
    json!({"created": "2026-11-10T00:00:00.000Z", "count": hits.len(), "offset": 0, "release-groups": hits})
}

/// One track of a release browse: title, length in ms (`None`: unknown),
/// recording id.
pub type TrackSpec<'a> = (&'a str, Option<i64>, &'a str);

/// One release of `GET /ws/2/release?release-group=...&inc=media+recordings+artist-credits`.
pub fn release(
    id: &str,
    title: &str,
    date: Option<&str>,
    credits: Vec<Value>,
    media: &[&[TrackSpec<'_>]],
) -> Value {
    let media: Vec<Value> = media
        .iter()
        .enumerate()
        .map(|(mi, tracks)| {
            json!({
                "position": mi + 1,
                "format": "CD",
                "track-count": tracks.len(),
                "track-offset": 0,
                "tracks": tracks.iter().enumerate().map(|(ti, (t, len, rec))| json!({
                    "position": ti + 1,
                    "number": (ti + 1).to_string(),
                    "title": t,
                    "length": len,
                    "recording": {
                        "id": rec,
                        "title": t,
                        "length": len,
                        "artist-credit": credits.clone(),
                    },
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    let mut r = json!({
        "id": id,
        "title": title,
        "status": "Official",
        "artist-credit": credits,
        "media": media,
    });
    if let Some(d) = date {
        r["date"] = json!(d);
    }
    r
}

pub fn browse_body(releases: Vec<Value>) -> Value {
    json!({"release-count": releases.len(), "release-offset": 0, "releases": releases})
}

/// `GET /ws/2/release-group/{id}?fmt=json&inc=artist-credits+tags`.
pub fn group_body(
    id: &str,
    title: &str,
    credits: Vec<Value>,
    first_release_date: &str,
    tags: &[(&str, i64)],
) -> Value {
    json!({
        "id": id,
        "title": title,
        "primary-type": "Album",
        "secondary-types": [],
        "first-release-date": first_release_date,
        "artist-credit": credits,
        "tags": tags.iter().map(|(n, c)| json!({"name": n, "count": c})).collect::<Vec<_>>(),
    })
}

/// The studio album's tracks as a release-browse medium.
pub fn album_medium() -> Vec<TrackSpec<'static>> {
    TRACKS
        .iter()
        .zip(RECORDINGS)
        .map(|((_, _, _, t, ms), rec)| (*t, Some(*ms), rec))
        .collect()
}

/// The studio album's release listing: one release, dated, five tracks.
pub fn album_listing() -> Value {
    browse_body(vec![release(
        REL_ALBUM,
        ALBUM,
        Some("1987-03-02"),
        main_credit(),
        &[&album_medium()],
    )])
}

/// An AcoustID release-group: (id, title, releases as (release id, whole
/// track count, per-medium track counts)).
pub fn ac_group(id: &str, title: &str, releases: &[(&str, u32, &[u32])]) -> Value {
    json!({
        "id": id,
        "title": title,
        "type": "Album",
        "releases": releases.iter().map(|(rid, whole, discs)| json!({
            "id": rid,
            "track_count": whole,
            "medium_count": discs.len(),
            "mediums": discs.iter().enumerate().map(|(i, c)| json!({
                "position": i + 1,
                "track_count": c,
                "tracks": [{"position": 1}],
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

/// One AcoustID result: a track id, a score and recordings, each on the
/// listed release-groups.
pub fn ac_result(id: &str, score: f64, recordings: &[(&str, Vec<Value>)]) -> Value {
    json!({
        "id": id,
        "score": score,
        "recordings": recordings.iter().map(|(rid, groups)| json!({
            "id": rid,
            "duration": 200,
            "releasegroups": groups,
        })).collect::<Vec<_>>(),
    })
}

pub fn ac_body(results: Vec<Value>) -> Value {
    json!({"status": "ok", "results": results})
}

// ---------------------------------------------------------------------------
// The in-memory transport.

/// Answers requests by replay key; an unknown request fails as unobserved,
/// exactly like the replay transport. Every request is logged.
#[derive(Default)]
pub struct Net {
    answers: Mutex<HashMap<String, Response>>,
    log: Mutex<Vec<String>>,
}

fn response(status: u16, headers: &[(&str, &str)], body: Vec<u8>) -> Response {
    Response {
        status,
        headers: headers
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        body,
    }
}

type Mb = MusicBrainzClient<ReplayTransport>;
type Caa = CoverArtClient<ReplayTransport>;

impl Net {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn answer(&self, req: &Request, resp: Response) {
        self.answers.lock().unwrap().insert(req.replay_key(), resp);
    }

    pub fn json(&self, req: &Request, body: &Value) {
        self.answer(
            req,
            response(
                200,
                &[("Content-Type", "application/json")],
                body.to_string().into_bytes(),
            ),
        );
    }

    pub fn status(&self, req: &Request, status: u16) {
        self.answer(req, response(status, &[], b"{}".to_vec()));
    }

    pub fn search(&self, artist: &str, title: &str, body: &Value) {
        self.json(&Mb::search_request(artist, title), body);
    }

    pub fn browse(&self, rg: &str, body: &Value) {
        self.json(&Mb::browse_request(rg), body);
    }

    pub fn release_dates(&self, rg: &str, body: &Value) {
        self.json(&Mb::release_dates_request(rg), body);
    }

    pub fn group(&self, rg: &str, body: &Value) {
        self.json(&Mb::release_group_request(rg), body);
    }

    /// The front cover answered 200 directly.
    pub fn cover_ok(&self, rg: &str) {
        self.answer(
            &Caa::front_request(rg),
            response(200, &[("Content-Type", "image/jpeg")], vec![]),
        );
    }

    /// The front cover answered 307 to an archive URL, 302 to a storage
    /// node, then 200 (the Cover Art Archive's redirect chain).
    pub fn cover_via_redirects(&self, rg: &str, release: &str) {
        let archive = format!("https://archive.org/download/mbid-{release}/mbid-{release}-1.jpg");
        let node =
            format!("https://node7.us.archive.org/0/items/mbid-{release}/mbid-{release}-1.jpg");
        self.answer(
            &Caa::front_request(rg),
            response(307, &[("Location", &archive)], vec![]),
        );
        self.answer(
            &Caa::front_hop_request(&archive).unwrap(),
            response(302, &[("Location", &node)], vec![]),
        );
        self.answer(
            &Caa::front_hop_request(&node).unwrap(),
            response(200, &[("Content-Type", "image/jpeg")], vec![]),
        );
    }

    pub fn cover_missing(&self, rg: &str) {
        self.status(&Caa::front_request(rg), 404);
    }

    /// The AcoustID answer for one fingerprint.
    pub fn acoustid(&self, fingerprint: &str, duration_s: u64, body: &Value) {
        self.json(
            &AcoustIdClient::<ReplayTransport>::lookup_request("any", fingerprint, duration_s),
            body,
        );
    }

    /// Requests sent, as `METHOD url`, in order.
    pub fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    pub fn take_log(&self) -> Vec<String> {
        std::mem::take(&mut *self.log.lock().unwrap())
    }

    /// Whether any logged request's URL contains `needle`.
    pub fn asked(&self, needle: &str) -> bool {
        self.log().iter().any(|l| l.contains(needle))
    }
}

impl Transport for Net {
    fn send(&self, request: &Request) -> Result<Response, TransportError> {
        self.log.lock().unwrap().push(format!(
            "{} {}",
            request.method.as_str(),
            request.redacted_url()
        ));
        let key = request.replay_key();
        self.answers
            .lock()
            .unwrap()
            .get(&key)
            .cloned()
            .ok_or_else(|| TransportError::Unobserved {
                key,
                method: request.method.as_str(),
                url: request.redacted_url(),
            })
    }
}

fn net_error(e: ClientError) -> NetError {
    match e {
        ClientError::Transport(t) => NetError::Network(t.to_string()),
        ClientError::Status { status, .. } => NetError::Http(status),
        other => NetError::Parse(other.to_string()),
    }
}

/// `MusicBrainzSource` over the in-memory transport, through the real
/// clients (as the app's own source does).
pub struct SynthMb<'a>(pub &'a Net);

impl MusicBrainzSource for SynthMb<'_> {
    fn search_release_groups(&self, artist: &str, title: &str) -> Result<Vec<u8>, NetError> {
        MusicBrainzClient::new(self.0)
            .body(Mb::search_request(artist, title))
            .map_err(net_error)
    }
    fn browse_releases(&self, release_group: &str) -> Result<Vec<u8>, NetError> {
        MusicBrainzClient::new(self.0)
            .body(Mb::browse_request(release_group))
            .map_err(net_error)
    }
    fn browse_release_dates(&self, release_group: &str) -> Result<Vec<u8>, NetError> {
        MusicBrainzClient::new(self.0)
            .body(Mb::release_dates_request(release_group))
            .map_err(net_error)
    }
    fn lookup_release_group(&self, release_group: &str) -> Result<Vec<u8>, NetError> {
        MusicBrainzClient::new(self.0)
            .body(Mb::release_group_request(release_group))
            .map_err(net_error)
    }
    fn cover_front_status(&self, release_group: &str) -> Result<u16, NetError> {
        match CoverArtClient::new(self.0).check_front(release_group) {
            FrontCover::Verified { .. } => Ok(200),
            FrontCover::Missing => Ok(404),
            FrontCover::Status(s) => Ok(s),
            FrontCover::Failed(e) => Err(NetError::Network(e.to_string())),
        }
    }
}

/// AcoustID lookups of invented fingerprints through `AcoustIdClient` over
/// the in-memory transport: `(filename, fingerprint, duration)` per file.
pub struct SynthPrints<'a> {
    pub net: &'a Net,
    pub files: Vec<(String, String, u64)>,
    pub calls: Mutex<usize>,
}

impl<'a> SynthPrints<'a> {
    pub fn new(net: &'a Net, files: &[(&str, &str, u64)]) -> Self {
        Self {
            net,
            files: files
                .iter()
                .map(|(n, f, d)| ((*n).to_owned(), (*f).to_owned(), *d))
                .collect(),
            calls: Mutex::new(0),
        }
    }

    pub fn calls(&self) -> usize {
        *self.calls.lock().unwrap()
    }
}

impl FingerprintSource for SynthPrints<'_> {
    fn lookups(&self, _album: &AlbumRecord) -> FingerprintLookups {
        *self.calls.lock().unwrap() += 1;
        let client = AcoustIdClient::new(self.net, "synthetic-test-key");
        FingerprintLookups::Tracks(
            self.files
                .iter()
                .map(|(name, fp, dur)| TrackLookup {
                    filename: name.clone(),
                    results: client
                        .lookup(fp, *dur)
                        .map(|r| r.results)
                        .map_err(net_error),
                })
                .collect(),
        )
    }
}

/// Fingerprinting unavailable (no AcoustID key).
pub struct NoPrints;

impl FingerprintSource for NoPrints {
    fn lookups(&self, _album: &AlbumRecord) -> FingerprintLookups {
        FingerprintLookups::Unavailable("no AcoustID key".into())
    }
}

/// The album files of the five-track album with an invented fingerprint and
/// a whole-second duration each (the lookup key).
pub fn album_prints() -> Vec<(&'static str, &'static str, u64)> {
    vec![
        (TRACKS[0].0, "SYNTHETIC-PRINT-01", 181),
        (TRACKS[1].0, "SYNTHETIC-PRINT-02", 205),
        (TRACKS[2].0, "SYNTHETIC-PRINT-03", 167),
        (TRACKS[3].0, "SYNTHETIC-PRINT-04", 243),
        (TRACKS[4].0, "SYNTHETIC-PRINT-05", 202),
    ]
}

// ---------------------------------------------------------------------------
// Album row, files, tags and the anchor store.

/// One anchor write: (release-group, release, score).
pub type Write = (String, Option<String>, i64);

pub struct World {
    pub album: Mutex<Option<AlbumRecord>>,
    pub tracks: Mutex<Vec<FlacTrack>>,
    pub fingerprints: Vec<FingerprintRow>,
    pub store: Mutex<Option<StoredAnchor>>,
    pub blocked: BTreeSet<String>,
    pub writes: Mutex<Vec<Write>>,
}

impl World {
    /// The five-track album, nothing stored.
    pub fn new() -> Self {
        Self::with(album_record(), album_tracks())
    }

    pub fn with(album: AlbumRecord, tracks: Vec<FlacTrack>) -> Self {
        Self {
            album: Mutex::new(Some(album)),
            tracks: Mutex::new(tracks),
            fingerprints: Vec::new(),
            store: Mutex::new(None),
            blocked: BTreeSet::new(),
            writes: Mutex::new(Vec::new()),
        }
    }

    pub fn stored(self, rg: &str, release: Option<&str>, score: i64, checked: &str) -> Self {
        *self.store.lock().unwrap() = Some(StoredAnchor {
            release_group_mbid: rg.to_owned(),
            release_mbid: release.map(str::to_owned),
            score: Some(score),
            last_checked_at: Some(checked.to_owned()),
        });
        self
    }

    pub fn block(mut self, rg: &str) -> Self {
        self.blocked.insert(rg.to_owned());
        self
    }

    pub fn writes(&self) -> Vec<Write> {
        self.writes.lock().unwrap().clone()
    }

    pub fn stored_anchor(&self) -> Option<StoredAnchor> {
        self.store.lock().unwrap().clone()
    }
}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

impl AlbumSource for World {
    fn album(&self, _album_id: &str) -> Option<AlbumRecord> {
        let store = self.store.lock().unwrap();
        self.album.lock().unwrap().clone().map(|mut a| {
            a.mb_release_group_mbid = store.as_ref().map(|s| s.release_group_mbid.clone());
            a
        })
    }
    fn filenames(&self, _album: &AlbumRecord) -> Vec<String> {
        let mut f: Vec<String> = self
            .tracks
            .lock()
            .unwrap()
            .iter()
            .map(|t| t.filename.clone())
            .collect();
        f.sort();
        f
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
        _album_id: &str,
        release_group_mbid: &str,
        release_mbid: Option<&str>,
        score: i64,
    ) -> Result<(), StoreError> {
        self.writes.lock().unwrap().push((
            release_group_mbid.to_owned(),
            release_mbid.map(str::to_owned),
            score,
        ));
        // The store stamps the write with "now", which is never stale.
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

/// One resolve of the album.
pub fn resolve(
    resolver: &Resolver,
    world: &World,
    net: &Net,
    prints: &dyn FingerprintSource,
    hint: &str,
) -> Resolved {
    let mb = SynthMb(net);
    let env = Env {
        albums: world,
        store: world,
        mb: &mb,
        fingerprints: prints,
        now_epoch_s: NOW_EPOCH_S,
    };
    resolver.resolve(&env, ALBUM_ID, hint, false).unwrap()
}

/// A fresh resolver, one resolve.
pub fn resolve_once(world: &World, net: &Net, prints: &dyn FingerprintSource) -> Resolved {
    resolve(&Resolver::new(), world, net, prints, "")
}

/// The note `find_release` writes for an accepted hit (the em dash is
/// U+2014).
pub fn matched_note(artist: &str, title: &str, score: i64, rg: &str) -> String {
    format!("Matched \"{artist} \u{2014} {title}\" (score {score}/100, MBID {rg})")
}
