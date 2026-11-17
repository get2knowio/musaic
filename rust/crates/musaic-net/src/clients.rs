//! The requests the resolver makes, in the exact shape that was recorded.

use std::fmt;
use std::time::Duration;

use musaic_core::Album;
use musaic_resolve::LookupResponse;
use musaic_resolve::find_release::{GroupFields, parse_release_group};
use musaic_resolve::musicbrainz::{SearchCandidate, parse_release_browse, parse_search};
use musaic_resolve::name_search::ReleaseShape;

use crate::{Request, Response, Transport, TransportError};

/// Sent on every request: MusicBrainz and the CAA HEAD as the Python app
/// does; AcoustID and cover downloads too (a deliberate change).
pub const USER_AGENT: &str = "musaic/0.1 (https://github.com/get2knowio/musaic)";
/// AcoustID metadata the anchoring rule needs. Part of the replay key.
pub const ACOUSTID_META: &str = "recordings releasegroups releases tracks compress";

/// `find_release(timeout=15)`: search, release browse and the CAA HEAD
/// (mb_lookup.py:133, 266, 279).
pub const MB_TIMEOUT: Duration = Duration::from_secs(15);
/// `get_recording_release_groups(timeout=10)` (mb_lookup.py:491).
pub const MB_RECORDING_TIMEOUT: Duration = Duration::from_secs(10);
/// The Python app sets none (fingerprint.py:206 calls `acoustid.lookup` without
/// `timeout`, so `requests` waits forever). A deliberate change: the same
/// figure as MusicBrainz.
pub const ACOUSTID_TIMEOUT: Duration = Duration::from_secs(15);

pub(crate) const MUSICBRAINZ: &str = "https://musicbrainz.org";
pub(crate) const ACOUSTID: &str = "https://api.acoustid.org";

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("HTTP {status} from {url}")]
    Status { status: u16, url: String },
    #[error("unreadable response from {url}: {detail}")]
    Parse { url: String, detail: String },
    #[error("AcoustID answered with an error: {0}")]
    AcoustId(String),
}

/// The three ways the Python app labels a failed MusicBrainz lookup
/// (`MusicBrainzMatch.source`, mb_lookup.py:169-187), plus AcoustID's own
/// error answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    /// "MusicBrainz (network error)": no answer at all.
    Network,
    /// "MusicBrainz (HTTP error)": an answer other than 200.
    Http,
    /// "MusicBrainz (parse error)": a 200 whose body is not what was asked for.
    Parse,
    /// AcoustID answered `{"status": "error"}`.
    Service,
    /// The caller cancelled the request; nothing failed.
    Cancelled,
}

impl ClientError {
    /// The request was cancelled by its caller (see
    /// [`TransportError::is_cancelled`]), as opposed to failing.
    pub fn is_cancelled(&self) -> bool {
        matches!(self, ClientError::Transport(t) if t.is_cancelled())
    }

    pub fn class(&self) -> FailureClass {
        match self {
            ClientError::Transport(t) if t.is_cancelled() => FailureClass::Cancelled,
            ClientError::Transport(_) => FailureClass::Network,
            ClientError::Status { .. } => FailureClass::Http,
            ClientError::Parse { .. } => FailureClass::Parse,
            ClientError::AcoustId(_) => FailureClass::Service,
        }
    }
}

pub(crate) fn rebase(mut req: Request, origin: &str) -> Request {
    req.origin = origin.to_string();
    req
}

fn expect_200(req: &Request, resp: Response) -> Result<Response, ClientError> {
    if resp.status == 200 {
        Ok(resp)
    } else {
        Err(ClientError::Status {
            status: resp.status,
            url: req.redacted_url(),
        })
    }
}

/// Artist and title for a name search: the directory name first
/// (`dir_artist -> album_artist -> artist`, `dir_album -> title`), with
/// "Various Artists" for a compilation. `None` when either is empty, in
/// which case no search is made.
pub fn name_search_terms(album: &Album) -> Option<(String, String)> {
    let pick = |opts: [&Option<String>; 3]| {
        opts.into_iter()
            .flatten()
            .find(|s| !s.is_empty())
            .map(|s| musaic_core::pycompat::strip(s).to_string())
    };
    let artist = if album.is_compilation {
        "Various Artists".to_string()
    } else {
        pick([&album.dir_artist, &album.album_artist, &album.artist]).unwrap_or_default()
    };
    let title = pick([&album.dir_album, &album.title, &None]).unwrap_or_default();
    (!artist.is_empty() && !title.is_empty()).then_some((artist, title))
}

/// Lucene query: two phrase-matched fields joined by AND.
fn mb_query(artist: &str, title: &str) -> String {
    let q = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    format!("artist:{} AND release:{}", q(artist), q(title))
}

#[derive(Clone)]
pub struct MusicBrainzClient<T> {
    transport: T,
    origin: String,
}

impl<T> fmt::Debug for MusicBrainzClient<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MusicBrainzClient")
            .field("origin", &self.origin)
            .finish_non_exhaustive()
    }
}

impl<T: Transport> MusicBrainzClient<T> {
    pub fn new(transport: T) -> Self {
        Self::with_origin(transport, MUSICBRAINZ)
    }

    /// Same requests against another origin (a test server).
    pub fn with_origin(transport: T, origin: &str) -> Self {
        Self {
            transport,
            origin: origin.trim_end_matches('/').to_string(),
        }
    }

    fn send(&self, req: Request) -> Result<(Request, Response), ClientError> {
        let req = rebase(req, &self.origin);
        let resp = expect_200(&req, self.transport.send(&req)?)?;
        Ok((req, resp))
    }

    pub fn search_request(artist: &str, title: &str) -> Request {
        Request::get(MUSICBRAINZ, "/ws/2/release-group/")
            .query("query", &mb_query(artist, title))
            .query("fmt", "json")
            .query("limit", "5")
            .header("User-Agent", USER_AGENT)
            .timeout(MB_TIMEOUT)
    }

    pub fn browse_request(release_group: &str) -> Request {
        Request::get(MUSICBRAINZ, "/ws/2/release")
            .query("release-group", release_group)
            .query("fmt", "json")
            .query("inc", "media+recordings+artist-credits")
            .query("limit", "25")
            .header("User-Agent", USER_AGENT)
            .timeout(MB_TIMEOUT)
    }

    /// `_earliest_release_year` (mb_lookup.py:758-768): the release listing
    /// without `inc`, asked when a search hit has no usable
    /// `first-release-date`. Same timeout as the search it follows.
    pub fn release_dates_request(release_group: &str) -> Request {
        Request::get(MUSICBRAINZ, "/ws/2/release")
            .query("release-group", release_group)
            .query("fmt", "json")
            .query("limit", "25")
            .header("User-Agent", USER_AGENT)
            .timeout(MB_TIMEOUT)
    }

    /// `GET /ws/2/release-group/{mbid}?fmt=json&inc=artist-credits+tags`:
    /// the group's own title, artist credit (with sort-names),
    /// first-release-date and tags. Not a request the Python app made: Tier 1
    /// asks it for every anchor that did not come from a name search, and a
    /// stale anchor is refreshed by id. The shape follows the MusicBrainz web
    /// service documentation for a release-group lookup; a live run
    /// recorded it for three release-groups under exactly this replay key.
    /// `inc` is one parameter whose value holds a literal `+` (sent as
    /// `%2B`, as `requests` would encode it). Same timeout as a search.
    pub fn release_group_request(release_group: &str) -> Request {
        Request::get(MUSICBRAINZ, &format!("/ws/2/release-group/{release_group}"))
            .query("fmt", "json")
            .query("inc", "artist-credits+tags")
            .header("User-Agent", USER_AGENT)
            .timeout(MB_TIMEOUT)
    }

    /// The body of a 200 answer to `req` (sent to this client's origin).
    /// Any other status is [`ClientError::Status`]. What the resolver's
    /// `MusicBrainzSource` needs: it parses the bodies itself.
    pub fn body(&self, req: Request) -> Result<Vec<u8>, ClientError> {
        self.send(req).map(|(_, resp)| resp.body)
    }

    /// The album-level fields of a release-group, by id.
    pub fn release_group(&self, release_group: &str) -> Result<GroupFields, ClientError> {
        let (req, resp) = self.send(Self::release_group_request(release_group))?;
        parse_release_group(&resp.body).map_err(|e| ClientError::Parse {
            url: req.redacted_url(),
            detail: e.to_string(),
        })
    }

    /// `get_recording_release_groups` (mb_lookup.py:505-511): the release
    /// groups a recording appears on.
    pub fn recording_request(recording: &str) -> Request {
        Request::get(MUSICBRAINZ, &format!("/ws/2/recording/{recording}"))
            .query("fmt", "json")
            .query("inc", "releases+release-groups")
            .header("User-Agent", USER_AGENT)
            .timeout(MB_RECORDING_TIMEOUT)
    }

    /// `(release_group_id, title)` for every release-group holding the
    /// recording, de-duplicated in first-seen order (mb_lookup.py:524-535).
    pub fn recording_release_groups(
        &self,
        recording: &str,
    ) -> Result<Vec<(String, String)>, ClientError> {
        let (req, resp) = match self.send(Self::recording_request(recording)) {
            Ok(r) => r,
            Err(e) => {
                // The Python app logs a failed request (mb_lookup.py:513) and a
                // status other than 200 / 404 (mb_lookup.py:518); a 404 and
                // an unreadable body return None silently. URLs in `e` are
                // redacted.
                match &e {
                    ClientError::Transport(t) => musaic_log::warning(
                        "musaic.mb_lookup",
                        &format!("MB recording release-groups lookup failed for {recording}: {t}"),
                    ),
                    ClientError::Status { status, .. } if *status != 404 => musaic_log::warning(
                        "musaic.mb_lookup",
                        &format!("MB recording release-groups HTTP {status} for {recording}"),
                    ),
                    _ => {}
                }
                return Err(e);
            }
        };
        let v: serde_json::Value =
            serde_json::from_slice(&resp.body).map_err(|e| ClientError::Parse {
                url: req.redacted_url(),
                detail: e.to_string(),
            })?;
        let mut out: Vec<(String, String)> = Vec::new();
        for release in v["releases"].as_array().into_iter().flatten() {
            let rg = &release["release-group"];
            let Some(id) = rg["id"].as_str().filter(|s| !s.is_empty()) else {
                continue;
            };
            if out.iter().any(|(seen, _)| seen == id) {
                continue;
            }
            out.push((id.to_string(), rg["title"].as_str().unwrap_or("").into()));
        }
        Ok(out)
    }

    /// `get_recording_canonical(mbid, timeout=10)` (mb_lookup.py:538-560):
    /// `GET /ws/2/recording/{mbid}?fmt=json&inc=artist-credits`, the
    /// MusicBrainz User-Agent, a 10 s timeout. Recorded on 2026-11-12 for one
    /// recording that exists and one that does not.
    pub fn recording_canonical_request(recording: &str) -> Request {
        Request::get(MUSICBRAINZ, &format!("/ws/2/recording/{recording}"))
            .query("fmt", "json")
            .query("inc", "artist-credits")
            .header("User-Agent", USER_AGENT)
            .timeout(MB_RECORDING_TIMEOUT)
    }

    /// `get_recording_canonical` without its `lru_cache`, as the Python app's
    /// function answers (the parity reference; the app calls
    /// [`Self::recording_canonical_lookup`]):
    ///
    /// * `Ok(Some(..))`: a 200 whose JSON object has a non-blank title; the
    ///   title stripped, the artist the credits joined with their
    ///   joinphrases ([`join_artist_credits`]).
    /// * `Ok(None)`, the Python app's `None`: an empty MBID (no request), a
    ///   failed request (WARNING `MB recording lookup failed for {mbid}: {e}`,
    ///   mb_lookup.py:556), a 404 (no line), any other status (WARNING
    ///   `MB recording lookup HTTP {status} for {mbid}`, mb_lookup.py:561),
    ///   a body that is not JSON (no line), a blank or missing title.
    /// * `Err(..)`: a 200 whose JSON has a shape the Python app's code raises on
    ///   (not an object; a title, credit list, credit, name or joinphrase of
    ///   the wrong type). The Python app's route then answers 500 and, the call
    ///   having raised, `lru_cache` keeps nothing.
    pub fn recording_canonical(
        &self,
        recording: &str,
    ) -> Result<Option<RecordingCanonical>, MalformedRecording> {
        self.recording_canonical_outcome(recording).0
    }

    /// The recording lookup with its outcome (a deliberate change; named after
    /// the fingerprint lookup's `done` / `failed` states):
    ///
    /// * [`RecordingLookup::Done`]: a definitive answer, to be remembered:
    ///   a 200 whose JSON object reads (the recording; `None` for a blank
    ///   or missing title, which asking again would not change), a 404, an
    ///   empty MBID (no request).
    /// * [`RecordingLookup::Failed`]: no answer, not to be remembered: a
    ///   failed request, any other status (5xx, 429, ...), a 200 whose body
    ///   is not JSON, logged as a WARNING
    ///   `MB recording lookup for {mbid} returned a body that is not JSON: {why}`
    ///   (the Python app logged nothing), and a malformed 200 (a shape the
    ///   Python app raised on), logged as a WARNING
    ///   `MB recording lookup for {mbid} returned a malformed answer: {why}`
    ///   (the Python app raised and its route answered 500).
    ///
    /// Requests and the other log lines are [`Self::recording_canonical`]'s.
    pub fn recording_canonical_lookup(&self, recording: &str) -> RecordingLookup {
        match self.recording_canonical_outcome(recording) {
            (Ok(answer), Outcome::Definitive) => RecordingLookup::Done(answer),
            (Ok(_), Outcome::Retry(failure)) => RecordingLookup::Failed(failure),
            (Ok(_), Outcome::NotJson(why)) => {
                // Logged like a malformed answer (the Python app's
                // function returned None for it without a line).
                musaic_log::warning(
                    "musaic.mb_lookup",
                    &format!(
                        "MB recording lookup for {recording} returned a body that is not JSON: {}",
                        cut_detail(why)
                    ),
                );
                RecordingLookup::Failed(LookupFailure::UnreadableAnswer)
            }
            (Err(MalformedRecording(why)), _) => {
                musaic_log::warning(
                    "musaic.mb_lookup",
                    &format!(
                        "MB recording lookup for {recording} returned a malformed answer: {}",
                        cut_detail(why)
                    ),
                );
                RecordingLookup::Failed(LookupFailure::UnreadableAnswer)
            }
        }
    }

    /// The Python app's answer, and whether it is definitive (found, a readable
    /// object, 404, an empty MBID).
    fn recording_canonical_outcome(
        &self,
        recording: &str,
    ) -> (
        Result<Option<RecordingCanonical>, MalformedRecording>,
        Outcome,
    ) {
        if recording.is_empty() {
            return (Ok(None), Outcome::Definitive);
        }
        let resp = match self.transport.send(&rebase(
            Self::recording_canonical_request(recording),
            &self.origin,
        )) {
            Ok(r) => r,
            Err(e) => {
                musaic_log::warning(
                    "musaic.mb_lookup",
                    &format!("MB recording lookup failed for {recording}: {e}"),
                );
                return (Ok(None), Outcome::Retry(LookupFailure::Unreachable));
            }
        };
        if resp.status == 404 {
            return (Ok(None), Outcome::Definitive);
        }
        if resp.status != 200 {
            musaic_log::warning(
                "musaic.mb_lookup",
                &format!("MB recording lookup HTTP {} for {recording}", resp.status),
            );
            let failure = match resp.status {
                429 | 503 => LookupFailure::RateLimited,
                status => LookupFailure::ServiceError(status),
            };
            return (Ok(None), Outcome::Retry(failure));
        }
        let data = match serde_json::from_slice::<serde_json::Value>(&resp.body) {
            Ok(d) => d,
            Err(e) => return (Ok(None), Outcome::NotJson(e.to_string())),
        };
        let parsed = parse_recording_canonical(&data);
        let outcome = if parsed.is_ok() {
            Outcome::Definitive
        } else {
            Outcome::Retry(LookupFailure::UnreadableAnswer)
        };
        (parsed, outcome)
    }

    /// Release-group candidates for a name search, best first.
    pub fn search_release_groups(
        &self,
        artist: &str,
        title: &str,
    ) -> Result<Vec<SearchCandidate>, ClientError> {
        let (req, resp) = self.send(Self::search_request(artist, title))?;
        parse_search(&resp.body).map_err(|e| ClientError::Parse {
            url: req.redacted_url(),
            detail: e.to_string(),
        })
    }

    /// Every release in a release-group with its track counts and durations.
    pub fn browse_releases(
        &self,
        release_group: &str,
    ) -> Result<Vec<(String, ReleaseShape)>, ClientError> {
        let (req, resp) = self.send(Self::browse_request(release_group))?;
        parse_release_browse(&resp.body).map_err(|e| ClientError::Parse {
            url: req.redacted_url(),
            detail: e.to_string(),
        })
    }
}

/// A recording's canonical title and joined artist credit
/// (`get_recording_canonical`'s `{"title", "artist"}`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingCanonical {
    pub title: String,
    pub artist: String,
}

/// How long the malformed-answer warning's detail may be (characters).
const MALFORMED_DETAIL_CHARS: usize = 200;

/// A log detail cut to [`MALFORMED_DETAIL_CHARS`] characters.
fn cut_detail(why: String) -> String {
    if why.chars().count() > MALFORMED_DETAIL_CHARS {
        why.chars()
            .take(MALFORMED_DETAIL_CHARS)
            .chain("...".chars())
            .collect()
    } else {
        why
    }
}

/// Whether a recording answer is definitive (to be remembered) and, when
/// it is not, whether the reason still needs a log line.
enum Outcome {
    /// Found, a readable object, 404, an empty MBID.
    Definitive,
    /// A failed request or another status (already logged), or a malformed
    /// shape (the `Err` side carries why).
    Retry(LookupFailure),
    /// A 200 whose body is not JSON (the parse error); logged by
    /// [`MusicBrainzClient::recording_canonical_lookup`] only.
    NotJson(String),
}

/// The outcome of one recording lookup ([`MusicBrainzClient::recording_canonical_lookup`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordingLookup {
    /// The lookup completed: the recording, or `None` when MusicBrainz has
    /// no (titled) recording for the MBID. Definitive; may be remembered.
    Done(Option<RecordingCanonical>),
    /// The lookup did not complete (network, status, unreadable or
    /// malformed answer). Not remembered; the next request asks again.
    Failed(LookupFailure),
}

/// Why a recording lookup did not complete, for the answer's reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LookupFailure {
    /// The request could not be made or got no answer: no connection,
    /// offline mode, a timeout, a certificate or TLS failure.
    Unreachable,
    /// MusicBrainz answered 429 or 503, which it uses to limit request rates.
    RateLimited,
    /// MusicBrainz answered with another unexpected status.
    ServiceError(u16),
    /// A 200 whose body was not the expected JSON.
    UnreadableAnswer,
}

impl RecordingLookup {
    /// The recording, if the lookup found one.
    pub fn answer(&self) -> Option<&RecordingCanonical> {
        match self {
            RecordingLookup::Done(a) => a.as_ref(),
            RecordingLookup::Failed(_) => None,
        }
    }
}

/// A 200 answer to the recording lookup whose JSON the Python app's code raises
/// on (`AttributeError` / `TypeError`, an uncaught 500 at the route).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unexpected recording lookup shape: {0}")]
pub struct MalformedRecording(pub String);

/// Python truthiness of a JSON value (`x or default`).
fn truthy(v: Option<&serde_json::Value>) -> Option<&serde_json::Value> {
    use serde_json::Value;
    v.filter(|v| match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    })
}

/// `(x or "").strip()` where a truthy `x` must be a string.
fn py_str_or_empty<'a>(
    v: Option<&'a serde_json::Value>,
    what: &str,
) -> Result<&'a str, MalformedRecording> {
    match truthy(v) {
        None => Ok(""),
        Some(serde_json::Value::String(s)) => Ok(musaic_core::pycompat::strip(s)),
        Some(other) => Err(MalformedRecording(format!("{what} is {other}"))),
    }
}

/// The body half of `get_recording_canonical` (mb_lookup.py:568-579): the
/// title stripped (`None` when blank), the artist by [`join_artist_credits`]
/// over `artist-credit or []`.
pub fn parse_recording_canonical(
    data: &serde_json::Value,
) -> Result<Option<RecordingCanonical>, MalformedRecording> {
    let obj = data
        .as_object()
        .ok_or_else(|| MalformedRecording(format!("not an object: {data}")))?;
    let title = py_str_or_empty(obj.get("title"), "title")?;
    if title.is_empty() {
        return Ok(None);
    }
    let credits = match truthy(obj.get("artist-credit")) {
        None => &[][..],
        Some(serde_json::Value::Array(a)) => a.as_slice(),
        Some(other) => {
            return Err(MalformedRecording(format!("artist-credit is {other}")));
        }
    };
    Ok(Some(RecordingCanonical {
        title: title.to_owned(),
        artist: join_artist_credits(credits)?,
    }))
}

/// `_join_artist_credits` (mb_lookup.py:582-594): for each credit, its
/// `name` (or its artist's `name`) stripped; a credit without one is
/// skipped together with its joinphrase; each kept name is followed by its
/// joinphrase (not stripped); the whole is stripped at the end
/// ("Simon & Garfunkel", "Beyoncé feat. Jay-Z"). A credit's sort-name is
/// never used.
pub fn join_artist_credits(credits: &[serde_json::Value]) -> Result<String, MalformedRecording> {
    let mut out = String::new();
    for c in credits {
        let c = c
            .as_object()
            .ok_or_else(|| MalformedRecording(format!("artist credit is {c}")))?;
        let name = match truthy(c.get("name")) {
            Some(v) => py_str_or_empty(Some(v), "credit name")?,
            None => match c.get("artist") {
                // `c.get("artist", {})`: absent -> {}; present -> must be a dict.
                None => "",
                Some(serde_json::Value::Object(a)) => {
                    py_str_or_empty(a.get("name"), "artist name")?
                }
                Some(other) => return Err(MalformedRecording(format!("credit artist is {other}"))),
            },
        };
        if name.is_empty() {
            continue;
        }
        out.push_str(name);
        match truthy(c.get("joinphrase")) {
            None => {}
            Some(serde_json::Value::String(j)) => out.push_str(j),
            Some(other) => {
                return Err(MalformedRecording(format!("joinphrase is {other}")));
            }
        }
    }
    Ok(musaic_core::pycompat::strip(&out).to_owned())
}

/// `Debug` never shows the key.
#[derive(Clone)]
pub struct AcoustIdClient<T> {
    transport: T,
    api_key: String,
    origin: String,
}

impl<T> fmt::Debug for AcoustIdClient<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AcoustIdClient")
            .field("origin", &self.origin)
            .field("api_key", &"<REDACTED>")
            .finish_non_exhaustive()
    }
}

impl<T: Transport> AcoustIdClient<T> {
    pub fn new(transport: T, api_key: impl Into<String>) -> Self {
        Self::with_origin(transport, api_key, ACOUSTID)
    }

    /// Same requests against another origin (a test server).
    pub fn with_origin(transport: T, api_key: impl Into<String>, origin: &str) -> Self {
        Self {
            transport,
            api_key: api_key.into(),
            origin: origin.trim_end_matches('/').to_string(),
        }
    }

    pub fn lookup_request(api_key: &str, fingerprint: &str, duration_s: u64) -> Request {
        Request::post_form(
            ACOUSTID,
            "/v2/lookup",
            vec![
                ("client".into(), api_key.into()),
                ("format".into(), "json".into()),
                ("duration".into(), duration_s.to_string()),
                ("fingerprint".into(), fingerprint.into()),
                ("meta".into(), ACOUSTID_META.into()),
            ],
        )
        // pyacoustid sends requests' default User-Agent; this one
        // names us, like every other request.
        .header("User-Agent", USER_AGENT)
        .timeout(ACOUSTID_TIMEOUT)
    }

    /// Everything AcoustID knows for one fingerprint. One request per track:
    /// the documented lookup has no batch form.
    pub fn lookup(
        &self,
        fingerprint: &str,
        duration_s: u64,
    ) -> Result<LookupResponse, ClientError> {
        let req = rebase(
            Self::lookup_request(&self.api_key, fingerprint, duration_s),
            &self.origin,
        );
        let resp = expect_200(&req, self.transport.send(&req)?)?;
        let parsed = LookupResponse::from_json(&resp.body).map_err(|e| ClientError::Parse {
            url: req.redacted_url(),
            detail: req.scrub(&e.to_string()),
        })?;
        if !parsed.is_ok() {
            // The service's own words, minus anything that echoes the key.
            let text: String = String::from_utf8_lossy(&resp.body)
                .chars()
                .take(200)
                .collect();
            return Err(ClientError::AcoustId(req.scrub(&text)));
        }
        Ok(parsed)
    }
}
