//! The duplicate-tracks resolve dialog's "MB says: ..." line:
//! `GET /api/checks/duplicate-tracks/findings/{id}/canonical`
//! (`musaic/api/health.py:224-265`), as a plain operation the
//! HTTP handler (and any other host) calls from a blocking task.
//!
//! * The finding must exist and belong to `duplicate-tracks`, else 404
//!   `Finding not found`.
//! * Its stored detail's `recording_mbid` (falsy: none) is looked up with
//!   `mb_lookup.get_recording_canonical` ([`MusicBrainzClient::recording_canonical`]).
//! * The answer is `{title, artist, mbid, reason}` in that key order.
//!
//! The Python app's `lru_cache(maxsize=2048)` on `get_recording_canonical` is
//! [`RecordingCanonicalCache`]: one per server lifetime, owned by the host.
//! Least recently used entries go first once 2048 are held.
//!
//! Decided changes (2026-11-12):
//!
//! * Only definitive answers are kept
//!   ([`musaic_net::RecordingLookup::Done`]: the recording, a 404, a
//!   readable object without a title). A failed request, a non-200 other
//!   than 404 (5xx, 429, ...), a body that is not JSON and a malformed 200
//!   are not kept: the next request asks MusicBrainz again. The Python app's
//!   `lru_cache` kept every `None`, so a transient failure answered "no
//!   recording" until the server restarted.
//! * A malformed 200 (a shape the Python app's code raised on, its
//!   route answering 500) is "no answer": the route gives the
//!   [`NO_RECORDING_REASON`] body, and a WARNING is logged on
//!   `musaic.mb_lookup` by the client. Not kept (it is not definitive).
//!
//! The route reads no album files and writes nothing, so it takes no
//! restore gate (only writing routes are refused during a restore) and,
//! like the Python app,
//! answers for a finding whose album is Missing.

use std::collections::HashMap;
use std::sync::Mutex;

use musaic_db::findings::get_finding;
use musaic_net::{MusicBrainzClient, RecordingCanonical, RecordingLookup, Transport};
use rusqlite::Connection;
use serde_json::Value;

use crate::{HttpError, Json};

/// `@lru_cache(maxsize=2048)` (mb_lookup.py:537).
pub const CACHE_MAXSIZE: usize = 2048;

/// The check whose findings this route serves.
const CHECK_ID: &str = "duplicate-tracks";

#[derive(Debug, Default)]
struct Lru {
    tick: u64,
    entries: HashMap<String, (Option<RecordingCanonical>, u64)>,
}

/// `get_recording_canonical`'s per-process cache (see the module docs).
#[derive(Debug, Default)]
pub struct RecordingCanonicalCache {
    inner: Mutex<Lru>,
}

impl RecordingCanonicalCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Entries held.
    pub fn len(&self) -> usize {
        self.lock().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Lru> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The cached answer for `mbid`, or a lookup, kept only when it is
    /// definitive. The lock is not held during the request: like
    /// `lru_cache`, two callers missing at once both ask, and the later
    /// answer is kept.
    pub fn get_or_lookup<T: Transport>(
        &self,
        mb: &MusicBrainzClient<T>,
        mbid: &str,
    ) -> Option<RecordingCanonical> {
        {
            let mut lru = self.lock();
            lru.tick += 1;
            let tick = lru.tick;
            if let Some((hit, used)) = lru.entries.get_mut(mbid) {
                *used = tick;
                return hit.clone();
            }
        }
        let answer = match mb.recording_canonical_lookup(mbid) {
            RecordingLookup::Done(answer) => answer,
            RecordingLookup::Failed => return None,
        };
        let mut lru = self.lock();
        lru.tick += 1;
        let tick = lru.tick;
        lru.entries.insert(mbid.to_owned(), (answer.clone(), tick));
        if lru.entries.len() > CACHE_MAXSIZE {
            let oldest = lru
                .entries
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(k, _)| k.clone());
            if let Some(k) = oldest {
                lru.entries.remove(&k);
            }
        }
        answer
    }
}

fn body(
    title: Option<&str>,
    artist: Option<&str>,
    mbid: Option<&str>,
    reason: Option<&str>,
) -> Json {
    let s = |v: Option<&str>| v.map_or(Json::Null, |v| Json::Str(v.to_owned()));
    Json::obj([
        ("title", s(title)),
        ("artist", s(artist)),
        ("mbid", s(mbid)),
        ("reason", s(reason)),
    ])
}

/// health.py:248-252.
pub const NO_MBID_REASON: &str = "no recording MBID for this cluster";
/// health.py:254-260.
pub const NO_RECORDING_REASON: &str = "MusicBrainz didn't return a recording for that MBID";

fn internal(detail: impl Into<String>) -> HttpError {
    HttpError {
        status: 500,
        detail: detail.into(),
    }
}

/// `duplicate_finding_canonical(finding_id)` (health.py:224-265). Errors:
/// 404 `Finding not found`; 500 (the host answers FastAPI's plain
/// `Internal Server Error`, the detail is for the log) where the Python
/// app's handler raised: a database error, a stored detail or MBID of a
/// shape the Python app's code failed on. A malformed MusicBrainz answer is
/// not one of them: it answers [`NO_RECORDING_REASON`].
pub fn duplicate_finding_canonical<T: Transport>(
    conn: &Connection,
    finding_id: i64,
    mb: &MusicBrainzClient<T>,
    cache: &RecordingCanonicalCache,
) -> Result<Json, HttpError> {
    let finding = get_finding(conn, finding_id).map_err(|e| internal(e.to_string()))?;
    let Some(finding) = finding.filter(|f| f.check_id == CHECK_ID) else {
        return Err(HttpError {
            status: 404,
            detail: "Finding not found".into(),
        });
    };
    // `finding.get("detail") or {}`: unreadable JSON is None (runner.py:84-88).
    let detail = finding
        .detail_json
        .as_deref()
        .and_then(|d| serde_json::from_str::<Value>(d).ok());
    let mbid = match &detail {
        None | Some(Value::Null) => None,
        Some(Value::Object(o)) => match o.get("recording_mbid") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if s.is_empty() => None,
            Some(Value::String(s)) => Some(s.as_str()),
            // `if not mbid` passes falsy values; a truthy non-string is looked
            // up by its str() in the Python app (or fails as unhashable). Not
            // reproduced: no check writes one.
            Some(Value::Bool(false)) => None,
            Some(Value::Number(n)) if n.as_f64() == Some(0.0) => None,
            Some(Value::Array(a)) if a.is_empty() => None,
            Some(Value::Object(m)) if m.is_empty() => None,
            Some(other) => return Err(internal(format!("recording_mbid is {other}"))),
        },
        // A falsy non-object detail reads as {}; a truthy one has no .get.
        Some(Value::Bool(false)) => None,
        Some(Value::Number(n)) if n.as_f64() == Some(0.0) => None,
        Some(Value::String(s)) if s.is_empty() => None,
        Some(Value::Array(a)) if a.is_empty() => None,
        Some(other) => return Err(internal(format!("finding detail is {other}"))),
    };
    let Some(mbid) = mbid else {
        return Ok(body(None, None, None, Some(NO_MBID_REASON)));
    };
    match cache.get_or_lookup(mb, mbid) {
        Some(c) => Ok(body(Some(&c.title), Some(&c.artist), Some(mbid), None)),
        None => Ok(body(None, None, Some(mbid), Some(NO_RECORDING_REASON))),
    }
}
