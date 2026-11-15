//! The resolver cascade, memoisation and invalidation.
//!
//! Port of `musaic/services/resolver.py::resolve_album`. Parity with the
//! Python app except where a deliberate change says otherwise:
//!
//! * Tier 0b persists (and anchors) only after the track-count and
//!   duration check passes ([`crate::name_search`]). A rejection is recorded in the trace and
//!   the cascade continues to Tier 0c.
//! * Tier 0c is the coverage rule of [`crate::vote`] over every AcoustID
//!   recording of every track, not the Python app's weighted
//!   vote over MusicBrainz recording lookups. Its trace line differs.
//! * Network failures are recorded in the trace. The Python app only
//!   logs them.
//! * Tier 1 works on the resolution's own anchor, by id. It never
//!   searches by name. For the stored anchor it is the Python app's cache path;
//!   for a STALE stored anchor it refetches the same release-group: when the
//!   release listing answers with at least one release the anchor is written
//!   back unchanged (same group, same score, best-fit release) so its
//!   `last_checked_at` is renewed, and the trace says so; when it does not,
//!   the anchor is kept as it is, freshness is not renewed, and the trace
//!   says why. The Python app instead searched by name with no blocklist and
//!   overwrote the anchor with whatever came back, even another group. A
//!   blocklisted stored anchor is handled as in the Python app: Tier 0a skips it
//!   and 0b / 0c (with the blocklist applied) look for another; Tier 1 never
//!   touches it.
//! * Canonical album fields (title, album artist, year, genre, and album
//!   artist sort) are filled for every anchor: from the search hit
//!   for a Tier 0b anchor, otherwise from a release-group lookup by id, with
//!   the release listing as fallback (no genre there). See [`Run::tier_1`].
//! * Tier 1 aligns against the best-fitting release of the group, not the
//!   earliest ([`find_release::best_fit_release`]).
//! * Without a tracklist every track counts as unresolved
//!   ([`crate::resolution::refine_from_anchor`]).
//! * When the 2 s pass leaves a track unaligned, the counts match and every
//!   length is within a measured bound, all tracks align by position
//!   ([`crate::resolution::positional_alignment`]); a "(disc N of
//!   M)" folder is compared with medium N of the chosen release.
//! * A resolve with a non-empty (after strip) hint does not read the memo
//!   (a hint is a fresh question); its result replaces the memo entry, so later un-hinted reads
//!   return what the user last asked for. `force` and `invalidate` are
//!   unchanged.
//!
//! Everything else — tier order, the 0a short-circuit, blocklist handling,
//! anchor-event emission, notes, memo key — follows the Python app;
//! `testdata/golden/resolver/cascade.json` is the reference, with the
//! deliberate deltas enumerated in `tests/cascade_python_parity.rs`.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, PoisonError};

use crate::find_release::{
    self, FindOptions, HIGH_CONFIDENCE_SCORE, MEDIUM_CONFIDENCE_SCORE, MbMatch,
    confidence_from_score,
};
use crate::musicbrainz::parse_release_browse;
use crate::name_search::{AlbumShape, NameSearchRejection, accept_name_search};
use crate::pyfmt::{json_dumps_sorted, repr_str};
use crate::resolution::{
    AlbumResolution, FieldFact, FingerprintMap, PositionalScope, refine_from_anchor_scoped, sources,
};
use crate::source::FlacTrack;
use crate::source::{
    AlbumRecord, AlbumSource, AnchorStore, FingerprintLookups, FingerprintSource,
    MusicBrainzSource, NetError, StoreError, StoredAnchor,
};
use crate::vote::{AlbumContext, Outcome, Rule, decide, matched_tracks};

/// Tier 0a's anchor: the stored release group, unless it is empty or on the
/// album's "Wrong match" blocklist (then the resolver searches instead).
pub fn cached_anchor(
    stored: Option<StoredAnchor>,
    blocked: &BTreeSet<String>,
) -> Option<StoredAnchor> {
    stored.filter(|a| !a.release_group_mbid.is_empty() && !blocked.contains(&a.release_group_mbid))
}

/// The cover the resolver proposes for an album that Tier 0a anchors: the
/// Cover Art Archive front of the anchored release group
/// ([`find_release::cover_front_url`], which Tier 1 offers when the archive
/// has one). `None` when the album has no usable anchor, or when the stored
/// id is not shaped like an MBID, so a URL is only ever built on
/// `coverartarchive.org` from 36 characters of hex and hyphens.
pub fn anchored_cover_url(
    store: &dyn AnchorStore,
    album_id: &str,
) -> Result<Option<String>, StoreError> {
    let blocked = store.blocked(album_id)?;
    Ok(cached_anchor(store.anchor(album_id)?, &blocked)
        .map(|a| a.release_group_mbid)
        .filter(|rg| is_mbid(rg))
        .map(|rg| find_release::cover_front_url(&rg)))
}

/// `8-4-4-4-12` hex digits, the shape of every MusicBrainz id.
pub fn is_mbid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

/// What a resolve needs from the outside world.
pub struct Env<'a> {
    pub albums: &'a dyn AlbumSource,
    pub store: &'a dyn AnchorStore,
    pub mb: &'a dyn MusicBrainzSource,
    pub fingerprints: &'a dyn FingerprintSource,
    /// Seconds since the Unix epoch, UTC: "now" for the anchor staleness test.
    pub now_epoch_s: i64,
}

/// `AlbumMbAnchorSet(album_id, source)`, for the caller to publish on the
/// events bus. In the Python app the bus's resolver-invalidate subscriber then
/// drops this album's memo; with this crate the caller does that by calling
/// [`Resolver::invalidate`] (the events layer's subscriber does).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchorSetEvent {
    pub album_id: String,
    pub source: String,
}

/// The outcome of one [`Resolver::resolve`] call.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// The same `Arc` for every call served from the memo (Python: `r1 is r2`).
    pub resolution: Arc<AlbumResolution>,
    /// Anchor writes this call made, in the order the Python app publishes them.
    pub events: Vec<AnchorSetEvent>,
    pub from_memo: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    /// Reading the anchor store failed (the Python app lets this raise, too).
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// `_content_hash(album, filenames)`: SHA-1 of
/// `json.dumps({track_count, total_seconds, dir_name, filenames: sorted}, sort_keys=True)`,
/// byte-identical to the Python app's (golden `content_hash.json`).
pub fn content_hash(album: &AlbumRecord, filenames: &[String]) -> String {
    use sha1::{Digest, Sha1};
    let mut sorted: Vec<&String> = filenames.iter().collect();
    sorted.sort();
    let payload = serde_json::json!({
        "track_count": album.track_count,
        "total_seconds": album.total_seconds,
        "dir_name": album.dir_name,
        "filenames": sorted,
    });
    Sha1::digest(json_dumps_sorted(&payload).as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `_is_stale(last_checked_at)`: missing, unparseable or timezone-naive
/// timestamps are stale (the Python app's `now(utc) - naive` raises and is caught);
/// otherwise stale when older than [`find_release::CACHE_REFRESH_DAYS`].
pub fn is_stale(last_checked_at: Option<&str>, now_epoch_s: i64) -> bool {
    let Some(ts) = last_checked_at.filter(|s| !s.is_empty()) else {
        return true;
    };
    match parse_iso_utc(&ts.replace('Z', "+00:00")) {
        Some(t) => now_epoch_s - t > find_release::CACHE_REFRESH_DAYS * 86_400,
        None => true,
    }
}

/// `datetime.fromisoformat` for `YYYY-MM-DD[T ]HH:MM[:SS[.ffffff]]±HH:MM`,
/// as whole seconds since the epoch (sub-second digits are ignored, which only
/// matters within a second of the 90-day boundary). `None` for anything else,
/// including timestamps without an offset.
fn parse_iso_utc(s: &str) -> Option<i64> {
    let num = |t: &str| -> Option<i64> {
        (!t.is_empty() && t.bytes().all(|b| b.is_ascii_digit())).then(|| t.parse().ok())?
    };
    let (date, rest) = (s.get(..10)?, s.get(10..)?);
    let mut d = date.split('-');
    let (y, mo, da) = (num(d.next()?)?, num(d.next()?)?, num(d.next()?)?);
    if d.next().is_some() || !(1..=12).contains(&mo) || !(1..=31).contains(&da) {
        return None;
    }
    let rest = rest.strip_prefix('T').or_else(|| rest.strip_prefix(' '))?;
    let split = rest.rfind(['+', '-'])?;
    let (time, offset) = rest.split_at(split);
    let sign = if offset.starts_with('-') { -1 } else { 1 };
    let (oh, om) = offset[1..].split_once(':')?;
    let offset_s = sign * (num(oh)? * 3600 + num(om)? * 60);
    let mut t = time.split(':');
    let h = num(t.next()?)?;
    let mi = num(t.next()?)?;
    let sec = match t.next() {
        Some(sec) => num(sec.split('.').next()?)?,
        None => 0,
    };
    if t.next().is_some() || h > 23 || mi > 59 || sec > 59 {
        return None;
    }
    Some(days_from_civil(y, mo, da) * 86_400 + h * 3600 + mi * 60 + sec - offset_s)
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

type MemoKey = (String, String);

/// The memo and the per-album locks. One per process; `Sync`.
#[derive(Default)]
pub struct Resolver {
    memo: Mutex<HashMap<MemoKey, Arc<AlbumResolution>>>,
    inflight: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    rule: Rule,
}

impl Resolver {
    pub fn new() -> Self {
        Self::default()
    }

    /// `invalidate(album_id)`; `None` clears everything.
    pub fn invalidate(&self, album_id: Option<&str>) {
        let mut memo = self.memo.lock().unwrap_or_else(PoisonError::into_inner);
        match album_id {
            None => memo.clear(),
            Some(id) => memo.retain(|(a, _), _| a != id),
        }
    }

    fn memo_get(&self, key: &MemoKey) -> Option<Arc<AlbumResolution>> {
        self.memo
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
            .cloned()
    }

    fn album_lock(&self, album_id: &str) -> Arc<Mutex<()>> {
        self.inflight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(album_id.to_owned())
            .or_default()
            .clone()
    }

    /// `resolve_album(album_id, user_hint=hint, force=force)`.
    pub fn resolve(
        &self,
        env: &Env<'_>,
        album_id: &str,
        hint: &str,
        force: bool,
    ) -> Result<Resolved, ResolveError> {
        let Some(album) = env.albums.album(album_id) else {
            let mut r = AlbumResolution::new(album_id);
            r.notes.push("Album not in cache; rescan first".to_owned());
            return Ok(Resolved {
                resolution: Arc::new(r),
                events: Vec::new(),
                from_memo: false,
            });
        };
        let filenames = env.albums.filenames(&album);
        let key = (album_id.to_owned(), content_hash(&album, &filenames));
        // A hint is a fresh question; never answer it from the memo.
        // The answer is still stored below (replacing the entry).
        let skip_memo = force || !musaic_core::pycompat::strip(hint).is_empty();
        if !skip_memo && let Some(hit) = self.memo_get(&key) {
            return Ok(Resolved {
                resolution: hit,
                events: Vec::new(),
                from_memo: true,
            });
        }
        let prev_anchor = album.mb_release_group_mbid.clone();
        let lock = self.album_lock(album_id);
        let (resolution, mut events) = {
            let _guard = lock.lock().unwrap_or_else(PoisonError::into_inner);
            if !skip_memo && let Some(hit) = self.memo_get(&key) {
                return Ok(Resolved {
                    resolution: hit,
                    events: Vec::new(),
                    from_memo: true,
                });
            }
            let (res, events) = self.resolve_uncached(env, &album, hint)?;
            let res = Arc::new(res);
            self.memo
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(key, res.clone());
            (res, events)
        };
        if prev_anchor.is_none()
            && let Some(a) = &resolution.anchor
        {
            events.push(AnchorSetEvent {
                album_id: album_id.to_owned(),
                source: a.source.clone(),
            });
        }
        Ok(Resolved {
            resolution,
            events,
            from_memo: false,
        })
    }

    fn resolve_uncached(
        &self,
        env: &Env<'_>,
        album: &AlbumRecord,
        hint: &str,
    ) -> Result<(AlbumResolution, Vec<AnchorSetEvent>), ResolveError> {
        let mut run = Run {
            env,
            mb: BrowseOnce::new(env.mb),
            album,
            events: Vec::new(),
        };
        let mut res = AlbumResolution::new(&album.id);
        let flac = env.albums.tracks(album);
        let mut fp = fingerprint_map(env.store, &album.id)?;
        let blocked = env.store.blocked(&album.id)?;

        let stored = env.store.anchor(&album.id)?;
        let mut search_hit = None;
        match cached_anchor(stored, &blocked) {
            Some(a) => {
                let score = a.score.unwrap_or(0);
                res.anchor = Some(FieldFact::new(
                    a.release_group_mbid,
                    sources::CACHE,
                    score as f64 / 100.0,
                ));
                res.anchor_strategy = "cache".to_owned();
                res.notes
                    .push(format!("Tier 0a: anchored from cache (score {score})"));
            }
            None => {
                if !blocked.is_empty() {
                    res.notes.push(format!(
                        "Blocklist active ({} release-group(s) rejected via Wrong match)",
                        blocked.len()
                    ));
                }
                if let Some(c) = run.tier_0b(hint, &blocked, &mut res.notes) {
                    res.anchor = Some(FieldFact::new(
                        c.release_group,
                        sources::MUSICBRAINZ,
                        c.score as f64 / 100.0,
                    ));
                    res.anchor_strategy = "dir-search".to_owned();
                    res.notes.push(format!("Tier 0b: {}", c.notes));
                    search_hit = c.hit;
                } else if let Some(c) = run.tier_0c(&self.rule, &blocked, &mut res.notes) {
                    res.anchor = Some(FieldFact::new(
                        c.release_group.clone(),
                        sources::FINGERPRINT_MB,
                        c.score as f64 / 100.0,
                    ));
                    res.anchor_strategy = "fingerprint-vote".to_owned();
                    res.notes.push(format!("Tier 0c: {}", c.notes));
                    // The Python app logs and carries on when this write fails
                    // (services/resolver.py:579, `_log.exception`: ERROR;
                    // the error text stands in for the traceback).
                    if let Err(e) = env
                        .store
                        .set_anchor(&album.id, &c.release_group, None, c.score)
                    {
                        musaic_log::error(
                            "musaic.services.resolver",
                            &format!(
                                "Couldn't persist fingerprint-derived anchor for {}: {e}",
                                album.id
                            ),
                        );
                    }
                }
            }
        }

        if fp.is_empty() {
            fp = fingerprint_map(env.store, &album.id)?;
        }

        if let Some(anchor) = res.anchor.as_ref().map(|a| a.value.clone()) {
            let (m, medium_sizes) = run.tier_1(&anchor, search_hit, &flac, &mut res.notes)?;
            let scope = if album.is_disc_directory() {
                PositionalScope::Disc {
                    disc: album.disc_number.unwrap_or(0),
                    medium_sizes,
                }
            } else {
                PositionalScope::WholeRelease
            };
            refine_from_anchor_scoped(&mut res, &m, &flac, &fp, &scope);
        } else {
            res.attach_unanchored_tracks(&flac, &fp);
        }
        if !hint.is_empty() {
            res.notes.push(format!(
                "Hint applied to dir-search reranking: {}",
                repr_str(hint)
            ));
        }
        Ok((res, run.events))
    }
}

fn fingerprint_map(store: &dyn AnchorStore, album_id: &str) -> Result<FingerprintMap, StoreError> {
    Ok(store
        .fingerprints(album_id)?
        .into_iter()
        .map(|r| (r.filename, (r.recording_mbid, r.score)))
        .collect())
}

/// One release browse per release-group per resolve: the Tier 0b acceptance check and
/// the Tier 1 tracklist ask the same question.
struct BrowseOnce<'a> {
    inner: &'a dyn MusicBrainzSource,
    browsed: RefCell<HashMap<String, Result<Vec<u8>, NetError>>>,
}

impl<'a> BrowseOnce<'a> {
    fn new(inner: &'a dyn MusicBrainzSource) -> Self {
        Self {
            inner,
            browsed: RefCell::new(HashMap::new()),
        }
    }
}

impl MusicBrainzSource for BrowseOnce<'_> {
    fn search_release_groups(&self, artist: &str, title: &str) -> Result<Vec<u8>, NetError> {
        self.inner.search_release_groups(artist, title)
    }
    fn browse_releases(&self, release_group: &str) -> Result<Vec<u8>, NetError> {
        if let Some(hit) = self.browsed.borrow().get(release_group) {
            return hit.clone();
        }
        let r = self.inner.browse_releases(release_group);
        self.browsed
            .borrow_mut()
            .insert(release_group.to_owned(), r.clone());
        r
    }
    fn browse_release_dates(&self, release_group: &str) -> Result<Vec<u8>, NetError> {
        self.inner.browse_release_dates(release_group)
    }
    fn cover_front_status(&self, release_group: &str) -> Result<u16, NetError> {
        self.inner.cover_front_status(release_group)
    }
    fn lookup_release_group(&self, release_group: &str) -> Result<Vec<u8>, NetError> {
        self.inner.lookup_release_group(release_group)
    }
}

struct Candidate {
    release_group: String,
    score: i64,
    notes: String,
    /// The Tier 0b search hit, whose release-group fields Tier 1 reuses.
    hit: Option<MbMatch>,
}

/// The Tier 0b acceptance check of a name-search hit `m` for release-group
/// `rg`: some release (or, for a "(disc N of M)" directory, some disc)
/// of the group's release listing must agree with `album` on track count
/// (within 2) and total duration (within 3%). The listing is `m.browse` when
/// the search already fetched it, else one `browse_releases(rg)`. `Err` is
/// the one-line reason for the trace. Used by Tier 0b and by the check-fix
/// name search ([`crate::find_release::find_release_for_fix`]).
pub fn check_name_search_hit(
    album: &AlbumRecord,
    m: &MbMatch,
    rg: &str,
    mb: &dyn MusicBrainzSource,
) -> Result<(), String> {
    let body = match &m.browse {
        Some(b) => b.clone(),
        None => mb.browse_releases(rg),
    };
    let body = body.map_err(|e| format!("release listing unavailable ({e})"))?;
    let releases = parse_release_browse(&body)
        .map_err(|e| format!("release listing unavailable (parse error: {e})"))?;
    let shapes: Vec<_> = releases.into_iter().map(|(_, s)| s).collect();
    accept_name_search(
        u32::try_from(m.score.max(0)).unwrap_or(u32::MAX),
        &album_shape(album),
        &shapes,
    )
    .map(|_| ())
    .map_err(|r: NameSearchRejection| r.message())
}

/// The album on disk as the acceptance check compares it.
fn album_shape(album: &AlbumRecord) -> AlbumShape {
    AlbumShape {
        track_count: album.track_count.unwrap_or(0).max(0) as u64,
        total_duration_s: album.total_seconds.unwrap_or(0.0),
        multi_disc: album.is_disc_directory(),
    }
}

struct Run<'e, 'a> {
    env: &'e Env<'a>,
    mb: BrowseOnce<'e>,
    album: &'e AlbumRecord,
    events: Vec<AnchorSetEvent>,
}

/// The Python app's `musaic.mb_lookup` logger: the anchor writes and tracklist
/// fetches the cascade does were `mb_lookup` calls there.
const MB_LOOKUP_LOG: &str = "musaic.mb_lookup";
/// mb_lookup.py:302 (`find_release`'s confident-match write).
const CACHE_ANCHOR_FAILED: &str = "Failed to cache MB anchor for album";
/// mb_lookup.py:432 (`find_release_cached`'s release backfill).
const BACKFILL_FAILED: &str = "Failed to backfill release_mbid for album";

impl Run<'_, '_> {
    /// `failure` is the Python app's log message for this write, without the
    /// album id (see [`CACHE_ANCHOR_FAILED`], [`BACKFILL_FAILED`]).
    fn persist(&mut self, rg: &str, release: Option<&str>, score: i64, failure: &str) {
        // Like the Python app: a failed write is logged, not raised, and publishes nothing.
        match self
            .env
            .store
            .set_anchor(&self.album.id, rg, release, score)
        {
            Ok(()) => self.events.push(AnchorSetEvent {
                album_id: self.album.id.clone(),
                source: sources::MUSICBRAINZ.to_owned(),
            }),
            // `log.exception`: ERROR; the error text stands in for the traceback.
            Err(e) => {
                musaic_log::error(MB_LOOKUP_LOG, &format!("{failure} {}: {e}", self.album.id))
            }
        }
    }

    /// The acceptance check against the candidate's release browse.
    fn name_search_check(&self, m: &MbMatch, rg: &str) -> Result<(), String> {
        check_name_search_hit(self.album, m, rg, &self.mb)
    }

    /// Tier 0b: `find_release` by directory name, accepted at score >= 70 AND
    /// the acceptance check; persisted (with an event) when also >= 88.
    fn tier_0b(
        &mut self,
        hint: &str,
        blocked: &BTreeSet<String>,
        notes: &mut Vec<String>,
    ) -> Option<Candidate> {
        let m = find_release::find_release(
            self.album,
            FindOptions {
                hint,
                blocked,
                fetch_cover: false,
                fetch_tracklist: false,
            },
            &self.mb,
        );
        if let Some(e) = &m.failure {
            notes.push(format!("Tier 0b: MusicBrainz search failed ({e})"));
        }
        let rg = m.mbid.clone()?;
        if m.score < MEDIUM_CONFIDENCE_SCORE {
            return None;
        }
        if let Err(why) = self.name_search_check(&m, &rg) {
            notes.push(format!(
                "Tier 0b: rejected {}: {why}",
                m.notes.trim_start_matches("Matched ")
            ));
            return None;
        }
        if m.confident {
            self.persist(&rg, m.release_mbid.as_deref(), m.score, CACHE_ANCHOR_FAILED);
        }
        Some(Candidate {
            release_group: rg,
            score: m.score,
            notes: m.notes.clone(),
            hit: Some(m),
        })
    }

    /// Tier 0c: the coverage rule over the AcoustID lookups.
    fn tier_0c(
        &mut self,
        rule: &Rule,
        blocked: &BTreeSet<String>,
        notes: &mut Vec<String>,
    ) -> Option<Candidate> {
        let lookups = match self.env.fingerprints.lookups(self.album) {
            FingerprintLookups::Unavailable(_) => return None,
            FingerprintLookups::Tracks(t) if t.is_empty() => return None,
            FingerprintLookups::Tracks(t) => t,
        };
        let n = lookups.len();
        let failed: Vec<&NetError> = lookups
            .iter()
            .filter_map(|l| l.results.as_ref().err())
            .collect();
        if let Some(first) = failed.first() {
            notes.push(format!(
                "Tier 0c: AcoustID lookup failed for {} of {n} tracks ({first})",
                failed.len()
            ));
        }
        let results: Vec<Vec<_>> = lookups
            .into_iter()
            .map(|l| l.results.unwrap_or_default())
            .collect();
        let title = self.album.own_title().to_owned();
        let blocked_refs: Vec<&str> = blocked.iter().map(String::as_str).collect();
        let d = decide(
            &results,
            &AlbumContext {
                track_count: n,
                title: &title,
                multi_disc: self.album.is_disc_directory(),
            },
            &blocked_refs,
            rule,
        );
        let matched = matched_tracks(&results, rule.min_result_score);
        let describe = |w: &crate::vote::Winner| {
            format!(
                "{}/{n} tracks on \"{}\" ({}); {} of {n} matched by AcoustID; track-count gap {}; {} tied",
                w.on_tracks,
                w.title.as_deref().unwrap_or(""),
                w.release_group_id,
                matched,
                w.count_gap.map_or("unknown".to_owned(), |g| g.to_string()),
                w.tied_candidates,
            )
        };
        match (&d.outcome, &d.winner) {
            (Outcome::Accepted(a), Some(w)) => Some(Candidate {
                release_group: a.release_group_id.clone(),
                score: i64::from(a.score),
                notes: describe(w),
                hit: None,
            }),
            (outcome, winner) => {
                notes.push(match winner {
                    Some(w) => format!(
                        "Tier 0c: no anchor: {}: {}",
                        describe(w),
                        outcome.reason_text()
                    ),
                    None => format!("Tier 0c: no anchor: {}", outcome.reason_text()),
                });
                None
            }
        }
    }

    /// Tier 1: the match `refine_from_anchor` consumes, for the resolution's
    /// own anchor `rg`, by id only. Requests, in order:
    ///
    /// 1. the Cover Art Archive check (as in the Python app);
    /// 2. the release browse (as in the Python app; shared with the acceptance
    ///    check), from
    ///    which [`find_release::best_fit_release`] picks the release to align
    ///    against;
    /// 3. only when the anchor did not come from a name search: the
    ///    release-group lookup for the album fields. A Tier 0b
    ///    anchor already has them from its search hit, as the Python app's re-search
    ///    had. When the lookup fails, title / artist / sort come from the
    ///    chosen release and the year from the earliest listed release; genre
    ///    stays empty (MusicBrainz lists no tags on releases); the trace says so.
    ///
    /// Store writes, only when `rg` is the stored anchor:
    /// * fresh: the Python app's release backfill (when the chosen release differs
    ///   from the stored one);
    /// * stale: renew it — same group, same stored score, the chosen release
    ///   (or the stored one) — if the listing named at least one release;
    ///   otherwise keep it untouched and stale, with the reason in the trace.
    fn tier_1(
        &mut self,
        rg: &str,
        search_hit: Option<MbMatch>,
        flac: &[FlacTrack],
        notes: &mut Vec<String>,
    ) -> Result<(MbMatch, Option<Vec<usize>>), ResolveError> {
        let own = self
            .env
            .store
            .anchor(&self.album.id)?
            .filter(|a| a.release_group_mbid == rg);
        let score = match (&own, &search_hit) {
            (Some(a), _) => a.score.unwrap_or(0),
            (None, Some(h)) => h.score,
            (None, None) => 0,
        };
        let stored_release = own.as_ref().and_then(|a| a.release_mbid.clone());
        let mut m = MbMatch {
            confident: score >= HIGH_CONFIDENCE_SCORE,
            score,
            confidence: confidence_from_score(score),
            mbid: Some(rg.to_owned()),
            release_mbid: stored_release.clone(),
            notes: format!("Using MB anchor by id (release-group {rg})"),
            source: format!("MusicBrainz (release-group {rg})"),
            ..MbMatch::default()
        };
        let from_search = search_hit.is_some();
        if let Some(h) = search_hit {
            m.title = h.title;
            m.artist = h.artist;
            m.artist_sort = h.artist_sort;
            m.year = h.year;
            m.primary_genre = h.primary_genre;
        }
        find_release::check_cover(&mut m, rg, &self.mb);

        let browse = self.mb.browse_releases(rg);
        let listing = browse
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|b| find_release::parse_listing(b));
        let chosen = match &listing {
            Ok(l) => find_release::best_fit_release(l, flac).cloned(),
            Err(e) => {
                // The Python app's Tier 1 is `find_release_cached`: a fresh
                // stored anchor takes the cached path (mb_lookup.py:454),
                // anything else `find_release` (mb_lookup.py:283).
                let cached = own
                    .as_ref()
                    .is_some_and(|a| !is_stale(a.last_checked_at.as_deref(), self.env.now_epoch_s));
                musaic_log::warning(
                    MB_LOOKUP_LOG,
                    &if cached {
                        format!("MB tracklist fetch (cached path) failed for {rg}: {e}")
                    } else {
                        format!("MB tracklist fetch failed for {rg}: {e}")
                    },
                );
                m.notes.push_str(&format!("\nTracklist fetch failed: {e}"));
                None
            }
        };
        // The chosen release's media, for a "(disc N of M)" folder.
        let medium_sizes = match (&browse, &chosen) {
            (Ok(b), Some(c)) => find_release::release_medium_sizes(b, &c.id),
            _ => None,
        };
        if let Some(c) = &chosen {
            if !c.id.is_empty() {
                m.release_mbid = Some(c.id.clone());
            }
            m.tracklist = c.tracks.clone();
        }

        if !from_search {
            match self
                .mb
                .lookup_release_group(rg)
                .and_then(|b| find_release::parse_release_group(&b))
            {
                Ok(g) => {
                    m.title = g.title;
                    m.artist = g.artist;
                    m.artist_sort = g.artist_sort;
                    m.year = g.year;
                    m.primary_genre = g.primary_genre;
                }
                Err(e) => {
                    if let Ok(l) = &listing {
                        m.year = l.earliest_year.clone();
                        if let Some(c) = &chosen {
                            m.title = c.title.clone();
                            m.artist = c.artist.clone();
                            m.artist_sort = c.artist_sort.clone();
                        }
                    }
                    let filled = m.title.is_some()
                        || m.artist.is_some()
                        || m.artist_sort.is_some()
                        || m.year.is_some();
                    notes.push(format!(
                        "Tier 1: release-group lookup failed ({e}); {}",
                        if filled {
                            "album fields taken from the release listing"
                        } else {
                            "no album fields"
                        }
                    ));
                }
            }
        }

        if let Some(a) = own {
            if is_stale(a.last_checked_at.as_deref(), self.env.now_epoch_s) {
                match &listing {
                    Ok(l) if l.listed > 0 => {
                        let rel = m.release_mbid.clone().filter(|r| !r.is_empty());
                        // The Python app refreshes a stale anchor by a new
                        // search (`find_release`), whose write logs this.
                        self.persist(rg, rel.as_deref(), score, CACHE_ANCHOR_FAILED);
                        notes.push(format!(
                            "Tier 1: stale anchor refreshed by release-group id ({rg})"
                        ));
                    }
                    Ok(_) => notes.push(format!(
                        "Tier 1: stale anchor kept, not refreshed: MusicBrainz lists no release for release-group {rg}"
                    )),
                    Err(e) => notes.push(format!(
                        "Tier 1: stale anchor kept, not refreshed: release listing unavailable ({e})"
                    )),
                }
            } else if let Some(rel) = m.release_mbid.clone()
                && !rel.is_empty()
                && Some(&rel) != stored_release.as_ref()
            {
                self.persist(rg, Some(&rel), score, BACKFILL_FAILED);
            }
        }
        Ok((m, medium_sizes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The Python app's `_is_stale`: missing, naive or unparseable -> stale.
    #[test]
    fn staleness() {
        let now = days_from_civil(2026, 11, 10) * 86_400;
        assert!(is_stale(None, now));
        assert!(is_stale(Some(""), now));
        assert!(is_stale(Some("garbage"), now));
        assert!(is_stale(Some("2026-11-01T00:00:00"), now)); // naive
        assert!(!is_stale(Some("2026-11-01T00:00:00Z"), now));
        assert!(!is_stale(Some("2026-11-01T00:00:00.123456+00:00"), now));
        assert!(!is_stale(Some("2026-08-12T00:00:00Z"), now)); // exactly 90 days: not older
        assert!(is_stale(Some("2026-08-11T23:59:59Z"), now));
        assert!(!is_stale(Some("2999-01-01T00:00:00Z"), now));
        assert!(is_stale(Some("2000-01-01T00:00:00Z"), now));
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(parse_iso_utc("1970-01-02T01:00:00+01:00"), Some(86_400));
    }

    struct OneAnchor(Option<&'static str>, &'static [&'static str]);

    impl AnchorStore for OneAnchor {
        fn anchor(&self, _: &str) -> Result<Option<StoredAnchor>, StoreError> {
            Ok(self.0.map(|rg| StoredAnchor {
                release_group_mbid: rg.to_owned(),
                release_mbid: None,
                score: Some(95),
                last_checked_at: None,
            }))
        }
        fn set_anchor(&self, _: &str, _: &str, _: Option<&str>, _: i64) -> Result<(), StoreError> {
            unreachable!("read only")
        }
        fn blocked(&self, _: &str) -> Result<BTreeSet<String>, StoreError> {
            Ok(self.1.iter().map(|s| (*s).to_owned()).collect())
        }
        fn fingerprints(&self, _: &str) -> Result<Vec<crate::source::FingerprintRow>, StoreError> {
            Ok(Vec::new())
        }
    }

    // The cover follows Tier 0a's anchor choice, and only an MBID-shaped
    // id is ever put into a URL.
    #[test]
    fn anchored_cover_url_follows_tier_0a() {
        const RG: &str = "0d9e7b1c-2f4a-4b6e-9c3d-1a2b3c4d5e6f";
        let url = |s: OneAnchor| anchored_cover_url(&s, "a1").unwrap();
        assert_eq!(
            url(OneAnchor(Some(RG), &[])).as_deref(),
            Some(
                "https://coverartarchive.org/release-group/0d9e7b1c-2f4a-4b6e-9c3d-1a2b3c4d5e6f/front"
            )
        );
        assert_eq!(url(OneAnchor(None, &[])), None);
        assert_eq!(url(OneAnchor(Some(""), &[])), None);
        assert_eq!(
            url(OneAnchor(Some(RG), &[RG])),
            None,
            "blocked: Wrong match"
        );
        for odd in [
            "0d9e7b1c-2f4a-4b6e-9c3d-1a2b3c4d5e6",  // short
            "0d9e7b1c-2f4a-4b6e-9c3d-1a2b3c4d5e6/", // path
            "x@evil.example/0000-0000-0000-000000000",
            "../../../../../../../../../../../../.",
            "0d9e7b1c 2f4a-4b6e-9c3d-1a2b3c4d5e6f",
        ] {
            assert_eq!(url(OneAnchor(Some(odd), &[])), None, "{odd:?}");
        }
        assert!(is_mbid(&RG.to_ascii_uppercase()));
    }
}
