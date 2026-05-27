"""Tiered metadata resolver — the single source of "what does the
authoritative world say about this album."

The resolver runs the cascade documented in `docs/IMPROVEMENTS.md`'s
metadata-tier discussion:

  Tier 0 — Bootstrap (find the MBID anchor + fingerprints)
    0a. Cache hit (mb_release_group_mbid already stored on the album row)
    0b. Directory + duration MB search
    0c. Per-track AcoustID fingerprint vote
        Strategies are tried cheapest-first, stop at the first confident
        match. Disagreements between 0b and 0c are surfaced rather than
        silently picking one.

  Tier 1 — Refine from anchor (canonical per-track titles + credits)
    Once the anchor is known, fetch the release-group's canonical
    tracklist and align FLACs to MB recordings by (tracknumber,
    duration±2s).

  Tier 2 — User hint (caller supplies; biases candidate selection)

  Tier 3 — AI (NOT invoked here; caller decides whether to escalate
    based on `unresolved_tracks` + `anchor is None`)

Lazy semantics: nothing here runs unless something asks for an
`AlbumResolution`. Results are memoized in-process; invalidation key is
the album's `(track_count, total_seconds, dir_name, sorted_filenames)`
content hash, so a rescan that changes any of those re-derives.

Callers in async contexts should `await asyncio.to_thread(resolve_album,
album_id, ...)` — the resolver is sync internally because every
underlying call (mb_lookup, fingerprint, cache) is sync.
"""
from __future__ import annotations

import hashlib
import json
import logging
import threading
from collections import Counter
from dataclasses import dataclass, field
from typing import Any, Literal

from .. import cache as cache_module
from .. import fingerprint as fp_module
from .. import mb_lookup
from .albums import cached_album_dict, list_album_filenames, read_tracks

_log = logging.getLogger(__name__)


# ---------- Result types ----------


# Where a fact came from. Higher-trust sources should win when multiple
# tiers produce the same field.
Source = Literal[
    "cache",            # already-anchored MB data
    "musicbrainz",      # fresh MB lookup (post-anchor)
    "fingerprint",      # AcoustID-resolved per-track recording MBID
    "fingerprint+mb",   # fingerprint vote that produced an album anchor
    "current_tag",      # what's already in the FLAC
    "ai",               # Claude-supplied (when caller chose to escalate)
    "user",             # explicit user override
]


@dataclass
class FieldFact:
    value: Any
    source: Source
    confidence: float  # 0.0–1.0


@dataclass
class TrackResolution:
    filename: str
    recording_mbid: str | None = None
    fingerprint_score: float | None = None
    canonical_title: FieldFact | None = None
    canonical_artist: FieldFact | None = None
    canonical_tracknumber: FieldFact | None = None
    canonical_duration_s: float | None = None


@dataclass
class AlbumResolution:
    album_id: str
    anchor: FieldFact | None = None
    """MB release-group MBID + how confident we are."""
    anchor_strategy: str = "none"
    """One of: cache | dir-search | fingerprint-vote | ai | user | none."""
    canonical_title: FieldFact | None = None
    canonical_album_artist: FieldFact | None = None
    canonical_year: FieldFact | None = None
    canonical_album_artist_sort: FieldFact | None = None
    canonical_genre: FieldFact | None = None
    cover_url: FieldFact | None = None
    tracks: list[TrackResolution] = field(default_factory=list)
    unresolved_track_count: int = 0
    """Tracks for which we couldn't produce a canonical title — caller may
    want to escalate to AI (Tier 3)."""
    notes: list[str] = field(default_factory=list)
    """Human-readable trace of which strategies ran and what they produced.
    Surfaced in the UI's 'View source' affordance."""

    def to_dict(self) -> dict[str, Any]:
        def fact(f: FieldFact | None) -> dict[str, Any] | None:
            if f is None:
                return None
            return {"value": f.value, "source": f.source, "confidence": f.confidence}

        return {
            "album_id": self.album_id,
            "anchor": fact(self.anchor),
            "anchor_strategy": self.anchor_strategy,
            "canonical_title": fact(self.canonical_title),
            "canonical_album_artist": fact(self.canonical_album_artist),
            "canonical_year": fact(self.canonical_year),
            "canonical_album_artist_sort": fact(self.canonical_album_artist_sort),
            "canonical_genre": fact(self.canonical_genre),
            "cover_url": fact(self.cover_url),
            "tracks": [
                {
                    "filename": t.filename,
                    "recording_mbid": t.recording_mbid,
                    "fingerprint_score": t.fingerprint_score,
                    "canonical_title": fact(t.canonical_title),
                    "canonical_artist": fact(t.canonical_artist),
                    "canonical_tracknumber": fact(t.canonical_tracknumber),
                    "canonical_duration_s": t.canonical_duration_s,
                }
                for t in self.tracks
            ],
            "unresolved_track_count": self.unresolved_track_count,
            "notes": self.notes,
        }


# ---------- Cache ----------


_cache: dict[tuple[str, str], AlbumResolution] = {}
_cache_lock = threading.Lock()
# Per-album lock so concurrent callers for the same album coalesce into
# one resolution rather than racing into duplicate MB / fpcalc work.
_inflight: dict[str, threading.Lock] = {}
_inflight_lock = threading.Lock()


def _content_hash(album: dict[str, Any], filenames: list[str]) -> str:
    """Stable digest of the album's identity-relevant inputs. A change here
    means our previous resolution is stale (a rescan changed something
    that could affect MB / fingerprint anchoring)."""
    payload = {
        "track_count": album.get("track_count"),
        "total_seconds": album.get("total_seconds"),
        "dir_name": album.get("dir_name"),
        "filenames": sorted(filenames),
    }
    blob = json.dumps(payload, sort_keys=True, default=str).encode()
    return hashlib.sha1(blob).hexdigest()


def _get_album_lock(album_id: str) -> threading.Lock:
    with _inflight_lock:
        if album_id not in _inflight:
            _inflight[album_id] = threading.Lock()
        return _inflight[album_id]


def invalidate(album_id: str | None = None) -> None:
    """Drop cached resolutions. With no arg, clears everything (used by
    tests and by paths that can't reason about which entries went stale,
    like a manual restore-from-snapshot)."""
    with _cache_lock:
        if album_id is None:
            _cache.clear()
            return
        for key in [k for k in _cache if k[0] == album_id]:
            del _cache[key]


# ---------- Tier 0 strategies ----------


@dataclass
class _AnchorCandidate:
    release_group_mbid: str
    score: int  # MB Lucene score equivalent (0-100)
    source: Source
    strategy: str
    notes: str
    release_mbid: str | None = None


def _try_dir_search(
    album: dict[str, Any],
    *,
    user_hint: str = "",
    blocked: set[str] | None = None,
) -> _AnchorCandidate | None:
    """Tier 0b: classic find_release path. Already auto-caches a confident
    anchor via cache.set_album_mb_anchor as a side effect.

    When `user_hint` is supplied, find_release uses it to rerank MB
    candidates (substring match against title/artist credit) — Tier 2
    bias on top of Tier 0b's pick.

    `blocked` is the per-album rejected-MBID set populated by the
    "Wrong match" UI affordance. Forwarded to mb_lookup so the search
    skips the rejected release-group(s).
    """
    match = mb_lookup.find_release(
        album, hint=user_hint, fetch_cover=False, fetch_tracklist=False,
        exclude_release_group_mbids=blocked,
    )
    if not match.mbid or match.score < mb_lookup.MEDIUM_CONFIDENCE_SCORE:
        return None
    return _AnchorCandidate(
        release_group_mbid=match.mbid,
        score=match.score,
        source="musicbrainz",
        strategy="dir-search",
        notes=match.notes or "",
    )


def _try_fingerprint_vote(
    album: dict[str, Any],
    *,
    blocked: set[str] | None = None,
) -> _AnchorCandidate | None:
    """Tier 0c: ensure fingerprints exist for every track, look up each
    recording's release-groups, vote.

    Skipped silently when fpcalc / AcoustID isn't available — the caller's
    cascade just falls through to AI (or stays unanchored).
    """
    available, reason = fp_module.is_available()
    if not available:
        _log.debug("fingerprint-vote skipped: %s", reason)
        return None

    try:
        fps = fp_module.fingerprint_album(album["id"], album["path"])
    except fp_module.FingerprintError as e:
        _log.info("fingerprint-vote couldn't fingerprint %s: %s", album["id"], e)
        return None
    if not fps:
        return None

    matched = [f for f in fps if f.acoustid_recording_mbid]
    if not matched:
        return None

    # Map each recording to its release-groups, weight votes by AcoustID score.
    # Skip blocklisted release-groups outright so they can't win — the
    # whole point of "Wrong match" is that we should never re-anchor to
    # a rejected MBID, even if every per-track fingerprint points at it.
    blocked_set = blocked or set()
    rg_votes: Counter[str] = Counter()
    rg_titles: dict[str, str] = {}
    for tf in matched:
        weight = max(1, int(round((tf.acoustid_score or 0) * 100)))
        rgs = mb_lookup.get_recording_release_groups(tf.acoustid_recording_mbid)
        if not rgs:
            continue
        for rg in rgs:
            rg_mbid = rg["release_group_mbid"]
            if rg_mbid in blocked_set:
                continue
            rg_votes[rg_mbid] += weight
            rg_titles.setdefault(rg_mbid, rg["title"])

    if not rg_votes:
        return None

    winner_mbid, winner_weight = rg_votes.most_common(1)[0]
    total_weight = sum(rg_votes.values())
    share = winner_weight / total_weight

    # Translate vote share into a 0–100 score so it speaks the same
    # language as MB Lucene scores. >=80% of weighted votes pointing at
    # one release-group is high confidence; 50–80% is medium; below is
    # not worth anchoring.
    if share >= 0.80:
        score = max(mb_lookup.HIGH_CONFIDENCE_SCORE, int(share * 100))
    elif share >= 0.50:
        score = mb_lookup.MEDIUM_CONFIDENCE_SCORE
    else:
        return None

    return _AnchorCandidate(
        release_group_mbid=winner_mbid,
        score=score,
        source="fingerprint+mb",
        strategy="fingerprint-vote",
        notes=(
            f"Fingerprinted {len(matched)}/{len(fps)} tracks; "
            f"{int(share * 100)}% of weighted votes → "
            f'"{rg_titles.get(winner_mbid, "")}" ({winner_mbid})'
        ),
    )


# ---------- Tier 1: refine from anchor ----------


def _refine_from_anchor(
    resolution: AlbumResolution,
    album: dict[str, Any],
    flac_tracks: list[dict[str, Any]],
    *,
    fingerprint_map: dict[str, tuple[str | None, float | None]],
) -> None:
    """Fetch the canonical tracklist for the anchored release-group and
    align FLACs to MB recordings. Mutates `resolution` in place.

    Alignment rules (cheapest first):
      1. By tracknumber if both sides have one and durations agree (±2s).
      2. By duration alone if only one MB recording matches a FLAC's
         length within tolerance.
      3. Unresolved if neither matches — increments unresolved_track_count.
    """
    if resolution.anchor is None:
        return
    mbid = resolution.anchor.value

    # Use the cached find_release_cached path — it'll fetch tracklist
    # without redoing the search step (anchor already in cache).
    match = mb_lookup.find_release_cached(
        album, fetch_cover=True, fetch_tracklist=True,
    )
    if not match.mbid or match.mbid != mbid:
        resolution.notes.append("Tier 1: anchor present but tracklist fetch missed")
        return

    if match.title:
        resolution.canonical_title = FieldFact(match.title, "musicbrainz", 0.95)
    if match.artist:
        resolution.canonical_album_artist = FieldFact(match.artist, "musicbrainz", 0.95)
    if match.year:
        resolution.canonical_year = FieldFact(match.year, "musicbrainz", 0.95)
    if match.primary_genre:
        # Genre tags are crowd-sourced; lower confidence than identity fields.
        resolution.canonical_genre = FieldFact(match.primary_genre, "musicbrainz", 0.55)
    if match.cover_url and match.cover_url_verified:
        resolution.cover_url = FieldFact(match.cover_url, "musicbrainz", 0.95)

    mb_tracks = match.tracklist or []
    if not mb_tracks:
        resolution.notes.append("Tier 1: anchored but MB returned no tracklist")
        # Without a tracklist we can't refine per-track. Tier 1 ends here.
        return

    # Build lookup tables for matching.
    by_position: dict[int, Any] = {t.position: t for t in mb_tracks}
    used_positions: set[int] = set()

    def _match_by_position(flac_tracknumber: str | int | None, flac_duration: float | None) -> Any:
        try:
            tn = int(flac_tracknumber) if flac_tracknumber else None
        except (TypeError, ValueError):
            tn = None
        if tn is None:
            return None
        cand = by_position.get(tn)
        if cand is None or tn in used_positions:
            return None
        if flac_duration and cand.length_s and abs(flac_duration - cand.length_s) > 2.0:
            return None
        return cand

    def _match_by_duration(flac_duration: float | None) -> Any:
        if not flac_duration:
            return None
        candidates = [
            t for t in mb_tracks
            if t.position not in used_positions
            and t.length_s
            and abs(flac_duration - t.length_s) <= 2.0
        ]
        # Only commit to a duration-only match if it's unambiguous.
        return candidates[0] if len(candidates) == 1 else None

    for ft in flac_tracks:
        filename = ft.get("filename") or ""
        rec_mbid, fp_score = fingerprint_map.get(filename, (None, None))
        track = TrackResolution(
            filename=filename,
            recording_mbid=rec_mbid,
            fingerprint_score=fp_score,
        )

        mb_track = _match_by_position(ft.get("tracknumber"), ft.get("duration"))
        if mb_track is None:
            mb_track = _match_by_duration(ft.get("duration"))

        if mb_track is not None:
            used_positions.add(mb_track.position)
            track.canonical_title = FieldFact(mb_track.title, "musicbrainz", 0.95)
            if mb_track.artist:
                track.canonical_artist = FieldFact(mb_track.artist, "musicbrainz", 0.95)
            track.canonical_tracknumber = FieldFact(
                str(mb_track.position), "musicbrainz", 0.95,
            )
            track.canonical_duration_s = mb_track.length_s
        else:
            resolution.unresolved_track_count += 1

        resolution.tracks.append(track)


# ---------- Public entry point ----------


def resolve_album(
    album_id: str,
    *,
    user_hint: str = "",
    force: bool = False,
) -> AlbumResolution:
    """Run the cascade for one album. Returns the populated
    AlbumResolution. Idempotent + memoized — repeat calls with the same
    `(album_id, content_hash)` return the cached result.

    `force=True` skips the cache (useful when the user explicitly clicks
    "Refresh from MusicBrainz").

    AI is NOT invoked here. Callers that want to escalate look at
    `result.anchor is None` or `result.unresolved_track_count > 0` and
    enqueue an AI job themselves with the resolution payload as
    additional context.
    """
    album = cached_album_dict(album_id)
    if album is None:
        return AlbumResolution(
            album_id=album_id,
            notes=["Album not in cache; rescan first"],
        )

    filenames = list_album_filenames(album["path"])
    content_hash = _content_hash(album, filenames)
    cache_key = (album_id, content_hash)

    if not force:
        with _cache_lock:
            cached = _cache.get(cache_key)
        if cached is not None:
            return cached

    # Snapshot the pre-cascade anchor so we can detect newly-written
    # anchors after _resolve_uncached returns. A side-effect anchor
    # write (Tier 0b or 0c) satisfies the unidentified-album check's
    # invariant; without firing run_post_scan_checks the finding stays
    # open until the next library scan. This is the side-effect-mutation
    # / stale-findings anti-pattern documented in services/post_scan.
    prev_anchor = album.get("mb_release_group_mbid")

    # Coalesce concurrent resolves for the same album onto one worker.
    with _get_album_lock(album_id):
        if not force:
            with _cache_lock:
                cached = _cache.get(cache_key)
            if cached is not None:
                return cached

        resolution = _resolve_uncached(album, user_hint=user_hint)

        with _cache_lock:
            _cache[cache_key] = resolution

    if prev_anchor is None and resolution.anchor is not None:
        try:
            from .post_scan import run_post_scan_checks, library_path_for_checks
            library_path = library_path_for_checks()
            if library_path:
                run_post_scan_checks(library_path)
        except Exception:
            _log.exception(
                "post-resolver check rerun failed for album %s (continuing)",
                album_id,
            )
        # Phase 1 of move E: publish alongside the direct call. The
        # subscriber-driven post-scan rerun will be a no-op against
        # the now-already-rerun state, but it carries the resolver
        # invalidation (next read picks up the new canonical fields)
        # and findings-stream notify for free.
        try:
            from .. import library_events
            library_events.publish(library_events.AlbumMbAnchorSet(
                album_id=album_id,
                source=resolution.anchor.source if resolution.anchor else "",
            ))
        except Exception:
            _log.exception(
                "library_events publish failed for resolver anchor on %s",
                album_id,
            )

    return resolution


def _resolve_uncached(album: dict[str, Any], *, user_hint: str) -> AlbumResolution:
    resolution = AlbumResolution(album_id=album["id"])

    flac_tracks = read_tracks(album["path"])

    # Pre-build the fingerprint map (filename → (recording_mbid, score))
    # so Tier 1 can attach per-track AcoustID anchors regardless of which
    # Tier 0 strategy won. Read-only — doesn't trigger fpcalc.
    cached_fps = cache_module.get_track_fingerprints(album["id"])
    fingerprint_map: dict[str, tuple[str | None, float | None]] = {
        f["filename"]: (f.get("acoustid_recording_mbid"), f.get("acoustid_score"))
        for f in cached_fps
    }

    # Per-album blocklist of MB release-groups the user has rejected
    # (via the "Wrong match" UI). Forwarded to every tier so we don't
    # silently re-anchor to one of them via fingerprint vote / dir
    # search / cache.
    blocked = cache_module.get_blocked_mb_release_groups(album["id"])

    # Tier 0a: cache hit. Skip a cached anchor that's been blocklisted —
    # this normally won't happen (the clear-anchor path wipes the cache
    # before adding to the blocklist) but guard against drift if a
    # caller persists an anchor through a different code path.
    anchor_row = cache_module.get_album_mb_anchor(album["id"])
    if (
        anchor_row
        and anchor_row.get("release_group_mbid")
        and anchor_row["release_group_mbid"] not in blocked
    ):
        score = int(anchor_row.get("score") or 0)
        resolution.anchor = FieldFact(anchor_row["release_group_mbid"], "cache", score / 100.0)
        resolution.anchor_strategy = "cache"
        resolution.notes.append(f"Tier 0a: anchored from cache (score {score})")
    else:
        if blocked:
            resolution.notes.append(
                f"Blocklist active ({len(blocked)} release-group(s) rejected via Wrong match)"
            )
        # Tier 0b: dir-search (hint biases candidate reranking when set).
        candidate = _try_dir_search(album, user_hint=user_hint, blocked=blocked)
        if candidate:
            resolution.anchor = FieldFact(
                candidate.release_group_mbid, candidate.source,
                candidate.score / 100.0,
            )
            resolution.anchor_strategy = candidate.strategy
            resolution.notes.append(f"Tier 0b: {candidate.notes}")
        else:
            # Tier 0c: fingerprint vote.
            candidate = _try_fingerprint_vote(album, blocked=blocked)
            if candidate:
                resolution.anchor = FieldFact(
                    candidate.release_group_mbid, candidate.source,
                    candidate.score / 100.0,
                )
                resolution.anchor_strategy = candidate.strategy
                resolution.notes.append(f"Tier 0c: {candidate.notes}")
                # Persist the fingerprint-derived anchor so future runs
                # short-circuit at Tier 0a.
                try:
                    cache_module.set_album_mb_anchor(
                        album["id"],
                        release_group_mbid=candidate.release_group_mbid,
                        release_mbid=None,
                        score=candidate.score,
                    )
                except Exception:
                    _log.exception("Couldn't persist fingerprint-derived anchor for %s", album["id"])

    # Refresh fingerprint map in case Tier 0c just populated it.
    if not fingerprint_map:
        cached_fps = cache_module.get_track_fingerprints(album["id"])
        fingerprint_map = {
            f["filename"]: (f.get("acoustid_recording_mbid"), f.get("acoustid_score"))
            for f in cached_fps
        }

    # Tier 1: refine from anchor.
    if resolution.anchor is not None:
        _refine_from_anchor(resolution, album, flac_tracks, fingerprint_map=fingerprint_map)
    else:
        # Without an anchor we can't refine. Still attach per-track
        # fingerprint info so the UI / AI escalation has something useful.
        for ft in flac_tracks:
            filename = ft.get("filename") or ""
            rec_mbid, fp_score = fingerprint_map.get(filename, (None, None))
            resolution.tracks.append(TrackResolution(
                filename=filename,
                recording_mbid=rec_mbid,
                fingerprint_score=fp_score,
            ))
        resolution.unresolved_track_count = len(flac_tracks)
        resolution.notes.append("No anchor found via Tiers 0a-0c; AI escalation may be required")

    if user_hint:
        resolution.notes.append(f"Hint applied to dir-search reranking: {user_hint!r}")

    return resolution
