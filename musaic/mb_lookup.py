"""Direct MusicBrainz / Cover Art Archive lookups.

This module replaced an earlier `cover_lookup` module that only looked at
cover URLs; everything now goes through this single MB query which
returns *everything* the health checks and fix paths need:

  - artist credit (canonical)
  - release-group title
  - first-release year
  - top genre tag (best-effort; MB tags are crowd-sourced sludge)
  - Cover Art Archive front-cover URL (HEAD-verified)
  - canonical per-track tracklist for the matched release

Why this lives next to `ai.py` rather than inside it: MusicBrainz answers
deterministically and free of AI quota. We want the check fix paths to
try MB FIRST — most of what the AI was doing for our missing-tags / no-
cover / track-gaps checks is identification, which MB already does
authoritatively. AI stays for the long tail (bootlegs, regional variants,
disambiguation, genre).

Rate limits: MusicBrainz allows 1 unauthenticated request/sec. We make
2-4 requests per `find_release()` (search → release-group browse →
optional CAA HEAD → optional release fetch for tracklist), so a single
album lookup is well within budget. If we ever wire bulk Fix-all through
this path, add a process-wide token bucket here.

Caching (v7): on a confident match we persist the release-group MBID and
score to the album row via cache.set_album_mb_anchor. The
`find_release_cached()` entry point reads that back so subsequent fix
paths don't re-pay the search cost — they fetch the release-detail JSON
directly by MBID. Stale entries (older than CACHE_REFRESH_DAYS) refresh
naturally on next access.
"""
from __future__ import annotations

import logging
import time
from dataclasses import dataclass, field
from typing import Any

import requests

log = logging.getLogger(__name__)


USER_AGENT = "musaic/0.1 (https://github.com/get2knowio/musaic)"

MB_RELEASE_GROUP_SEARCH = "https://musicbrainz.org/ws/2/release-group/"
# Release-group detail (same prefix as the search endpoint, but distinct
# semantics — search appends ?query=…, detail appends /{mbid}). Keeping
# them as separate symbols means a future reorganization of the search
# constant won't silently break the detail-fetch path used by
# _persist_ai_mbid for AI MBID verification.
MB_RELEASE_GROUP_DETAIL = "https://musicbrainz.org/ws/2/release-group/{mbid}"
MB_RELEASE_GROUP_BROWSE = "https://musicbrainz.org/ws/2/release"  # ?release-group=...
MB_RELEASE_DETAIL = "https://musicbrainz.org/ws/2/release/{mbid}"
MB_RECORDING_DETAIL = "https://musicbrainz.org/ws/2/recording/{mbid}"
CAA_RELEASE_GROUP_FRONT = "https://coverartarchive.org/release-group/{mbid}/front"


# Minimum scores to qualify each tier — used by both the fix dispatcher and
# UI confidence pills.
#
# Calibration note: MB's Lucene scoring is quirky. A perfect string match on
# both artist + release returns 100. Common real-world variations ("The"
# prefixes, '&' vs 'and', MB has the act named slightly differently) score
# in the high 80s. We originally set HIGH at 95, which silently skipped MB
# direct-apply for cases like "Ray Conniff" vs "The Ray Conniff Singers"
# even though the match was clearly correct. 88 catches the common
# variations while still rejecting the truly ambiguous (Sinatra and Conniff
# both have a "Young At Heart").
HIGH_CONFIDENCE_SCORE = 88
MEDIUM_CONFIDENCE_SCORE = 70

# How long a cached MB anchor is trusted before find_release_cached() does
# a fresh search. MB metadata DOES drift (artist credit reformats, year
# corrections, etc.), but slowly enough that a quarterly refresh is fine.
# Scoped to the cached path; fix() flows that explicitly want fresh data
# can keep calling find_release() directly.
CACHE_REFRESH_DAYS = 90


@dataclass
class MBTrack:
    position: int           # 1-based; flat across discs (linear sequence)
    title: str
    length_ms: int | None = None
    artist: str | None = None  # only meaningful for compilations
    recording_mbid: str | None = None  # canonical recording — used by fingerprint match

    @property
    def length_s(self) -> float | None:
        """Convenience accessor in seconds — duration comparisons are
        almost always against FLAC track lengths, which we already
        track in seconds (via mutagen's `audio.info.length`)."""
        return self.length_ms / 1000.0 if self.length_ms is not None else None


@dataclass
class MusicBrainzMatch:
    """Result of a release-group search + supplementary fetches.

    `confident` is the single boolean fix dispatchers care about: True only
    when the search score is above HIGH_CONFIDENCE_SCORE AND we have at
    least one piece of useful structured data. False = either no match, low
    confidence, or all fields blank — caller should fall through to AI.
    """
    confident: bool = False
    score: int = 0
    confidence: str = "low"   # "low" | "medium" | "high"
    mbid: str | None = None   # release-group MBID
    release_mbid: str | None = None  # specific release used for tracklist
    artist: str | None = None
    title: str | None = None
    year: str | None = None         # 4-digit string
    primary_genre: str | None = None
    cover_url: str | None = None
    cover_url_verified: bool = False
    tracklist: list[MBTrack] = field(default_factory=list)
    notes: str = ""
    source: str = "MusicBrainz"


# ---------- Public entry points ----------

def find_release(
    album: dict[str, Any],
    *,
    hint: str = "",
    fetch_cover: bool = True,
    fetch_tracklist: bool = True,
    exclude_release_group_mbids: set[str] | None = None,
    timeout: int = 15,
) -> MusicBrainzMatch:
    """One-shot lookup. Returns a (possibly empty) MusicBrainzMatch.

    `hint` is currently informational only — appended to notes on failure.
    `fetch_cover` and `fetch_tracklist` are independently togglable so
    callers can skip work they don't need (e.g. the missing-core-tags
    fix doesn't need a tracklist). Cover and tracklist add 1 HTTP request
    each; if both are disabled the function makes a single search call.
    """
    artist = (
        album.get("dir_artist")
        or album.get("album_artist")
        or album.get("artist")
        or ""
    ).strip()
    title = (album.get("dir_album") or album.get("title") or "").strip()
    is_compilation = bool(album.get("is_compilation"))

    if is_compilation:
        artist = "Various Artists"

    if not title or not artist:
        return MusicBrainzMatch(
            notes=f"Need both artist and title; got artist={artist!r} title={title!r}",
            source="MusicBrainz (insufficient input)",
        )

    # 1. Search MB for a matching release-group.
    try:
        resp = requests.get(
            MB_RELEASE_GROUP_SEARCH,
            params={"query": _mb_query(artist, title), "fmt": "json", "limit": 5},
            headers={"User-Agent": USER_AGENT},
            timeout=timeout,
        )
    except requests.RequestException as e:
        log.warning("MusicBrainz search failed: %s", e)
        return MusicBrainzMatch(
            notes=f"{type(e).__name__}: {e}",
            source="MusicBrainz (network error)",
        )

    if resp.status_code != 200:
        return MusicBrainzMatch(
            notes=f"HTTP {resp.status_code} from MusicBrainz",
            source="MusicBrainz (HTTP error)",
        )

    try:
        data = resp.json()
    except ValueError:
        return MusicBrainzMatch(
            notes="Search response was not valid JSON",
            source="MusicBrainz (parse error)",
        )

    groups = data.get("release-groups", []) or []
    # Filter out blocklisted release-groups before any further reasoning.
    # Used by the "Wrong match" UI affordance — without this the same
    # rejected release-group would keep winning.
    blocked = exclude_release_group_mbids or set()
    if blocked:
        groups = [g for g in groups if g.get("id") not in blocked]
    if not groups:
        notes = f"No release-group match for artist={artist!r} title={title!r}"
        if hint:
            notes += f' (your hint: "{hint}")'
        if blocked:
            notes += f" (after excluding {len(blocked)} blocklisted release-group(s))"
        return MusicBrainzMatch(
            notes=notes,
            source="MusicBrainz (no match)",
        )

    # Hint-biased reranking. The hint is treated as the user's
    # high-confidence ground truth (e.g. "this is the 1979 original, not
    # the 2012 remaster"). When set, prefer candidates whose title or
    # artist credit contains the hint substring; among those, the
    # highest-scoring one wins. Fall through to plain top-by-score when
    # no candidate matches the hint at all.
    top = groups[0]
    hint_q = (hint or "").strip().lower()
    if hint_q:
        def _candidate_text(g: dict[str, Any]) -> str:
            credits = g.get("artist-credit") or []
            artist_str = " ".join(ac.get("name", "") for ac in credits)
            return f"{g.get('title', '')} {artist_str}".lower()

        matched = [g for g in groups if hint_q in _candidate_text(g)]
        if matched:
            top = max(matched, key=lambda g: int(g.get("score", 0) or 0))
    mbid = top.get("id")
    score = int(top.get("score", 0) or 0)
    rg_title = top.get("title") or title
    rg_artist_credit = top.get("artist-credit") or []
    rg_artist = ", ".join(ac.get("name", "") for ac in rg_artist_credit) or artist
    first_release_date = top.get("first-release-date") or ""
    year = first_release_date[:4] if len(first_release_date) >= 4 and first_release_date[:4].isdigit() else None
    # Fallback: not every release-group has first-release-date populated
    # (it depends on an editor backfilling it). When missing, browse the
    # actual releases in the group and take the earliest with a date.
    # One extra HTTP hop, well within MB's 1 req/s budget.
    if year is None and top.get("id"):
        year = _earliest_release_year(top["id"], timeout=timeout)
    tags = top.get("tags") or []
    primary_genre = _pick_primary_genre(tags)
    confidence = _confidence_from_score(score)

    if not mbid:
        return MusicBrainzMatch(
            score=score,
            confidence=confidence,
            notes="Top match had no MBID",
            source="MusicBrainz (malformed result)",
        )

    match = MusicBrainzMatch(
        confident=score >= HIGH_CONFIDENCE_SCORE,
        score=score,
        confidence=confidence,
        mbid=mbid,
        artist=rg_artist or None,
        title=rg_title or None,
        year=year,
        primary_genre=primary_genre,
        notes=f'Matched "{rg_artist} — {rg_title}" (score {score}/100, MBID {mbid})',
        source=f"MusicBrainz (release-group {mbid})",
    )

    # 2. Optionally HEAD the CAA URL for cover art.
    if fetch_cover:
        cover_url = CAA_RELEASE_GROUP_FRONT.format(mbid=mbid)
        ok, reason = _head_cover(cover_url, timeout=timeout)
        if ok:
            match.cover_url = cover_url
            match.cover_url_verified = True
        else:
            # Cover unavailable doesn't invalidate the metadata match — just
            # leave cover_url None and note why for diagnostics.
            match.notes += f"\nCover not in CAA: {reason}"

    # 3. Optionally fetch the canonical tracklist (one extra request for the
    # earliest release in the group).
    if fetch_tracklist:
        try:
            release_mbid, tracks = _fetch_tracklist(mbid, timeout=timeout)
            match.release_mbid = release_mbid
            match.tracklist = tracks
        except Exception as e:
            log.warning("MB tracklist fetch failed for %s: %s", mbid, e)
            match.notes += f"\nTracklist fetch failed: {type(e).__name__}: {e}"

    # 4. Persist confident matches to the album row so subsequent fix paths
    # can short-circuit the search step. We DO NOT cache low-confidence or
    # no-match results — those should re-search next time around in case
    # the user's library state has improved (better dir name, fresh tags).
    album_id = album.get("id")
    if match.confident and album_id:
        try:
            from . import cache as cache_module
            cache_module.set_album_mb_anchor(
                album_id,
                release_group_mbid=mbid,
                release_mbid=match.release_mbid,
                score=score,
            )
        except Exception:
            # Cache write failure must never break the user-facing fix flow.
            log.exception("Failed to cache MB anchor for album %s", album_id)

    return match


# ---------- Cached entry point ----------

def find_release_cached(
    album: dict[str, Any],
    *,
    hint: str = "",
    fetch_cover: bool = True,
    fetch_tracklist: bool = True,
    exclude_release_group_mbids: set[str] | None = None,
    timeout: int = 15,
) -> MusicBrainzMatch:
    """Like find_release() but consults the per-album MB anchor cache first.

    Behavior:
      - No cache hit (album never identified) → fall through to find_release.
      - Stale cache hit (last_checked_at older than CACHE_REFRESH_DAYS) →
        same as no hit; refresh.
      - Fresh cache hit → fetch only what the caller asked for, by MBID,
        skipping the search step. Saves the per-call 1-2 second MB round
        trip, AND avoids burning the rate-limit budget when bulk fix-all
        is iterating through many findings.

    Always returns a MusicBrainzMatch — when the cache is fresh and the
    caller didn't request cover/tracklist, the result has just the
    identity fields (mbid, score, confident=True) populated.
    """
    from . import cache as cache_module
    album_id = album.get("id")
    if not album_id:
        return find_release(
            album, hint=hint, fetch_cover=fetch_cover,
            fetch_tracklist=fetch_tracklist,
            exclude_release_group_mbids=exclude_release_group_mbids,
            timeout=timeout,
        )

    blocked = exclude_release_group_mbids or set()
    anchor = cache_module.get_album_mb_anchor(album_id)
    # If the cached anchor is itself blocklisted, treat as cache-miss so
    # the underlying find_release re-queries (with the blocklist applied
    # to the candidate filter).
    if (
        anchor is None
        or _is_stale(anchor.get("last_checked_at"))
        or anchor.get("release_group_mbid") in blocked
    ):
        return find_release(
            album, hint=hint, fetch_cover=fetch_cover,
            fetch_tracklist=fetch_tracklist,
            exclude_release_group_mbids=blocked,
            timeout=timeout,
        )

    # Cache hit. Build a minimal match from cached identity fields and
    # fetch only the bits the caller wants.
    mbid = anchor["release_group_mbid"]
    score = int(anchor["score"] or 0)
    match = MusicBrainzMatch(
        confident=score >= HIGH_CONFIDENCE_SCORE,
        score=score,
        confidence=_confidence_from_score(score),
        mbid=mbid,
        release_mbid=anchor.get("release_mbid"),
        notes=f"Using cached MB anchor (release-group {mbid})",
        source=f"MusicBrainz (release-group {mbid}, cached)",
    )

    if fetch_cover:
        cover_url = CAA_RELEASE_GROUP_FRONT.format(mbid=mbid)
        ok, reason = _head_cover(cover_url, timeout=timeout)
        if ok:
            match.cover_url = cover_url
            match.cover_url_verified = True
        else:
            match.notes += f"\nCover not in CAA: {reason}"

    if fetch_tracklist:
        try:
            release_mbid, tracks = _fetch_tracklist(mbid, timeout=timeout)
            previous_release_mbid = match.release_mbid
            match.release_mbid = release_mbid or match.release_mbid
            match.tracklist = tracks
            # Write the freshly-discovered release_mbid back to the
            # anchor cache so subsequent cache-hit calls don't re-pay
            # the _fetch_tracklist cost just to learn the same fact.
            # The most common case this matters for is AI-anchored
            # albums: _persist_ai_mbid writes release_mbid=None (the AI
            # gives us release-group only), and without this back-write
            # we'd hit the MB tracklist endpoint on every fix-flow that
            # wants the canonical track list. Skip the write if nothing
            # changed (cache already had the same value, or the fetch
            # didn't yield a release_mbid).
            if (
                match.release_mbid
                and match.release_mbid != previous_release_mbid
            ):
                try:
                    cache_module.set_album_mb_anchor(
                        album_id,
                        release_group_mbid=mbid,
                        release_mbid=match.release_mbid,
                        score=score,
                    )
                except Exception:
                    # Same posture as find_release(): cache write
                    # failure must never break the user-facing fix
                    # flow. Logged only.
                    log.exception(
                        "Failed to backfill release_mbid for album %s",
                        album_id,
                    )
        except Exception as e:
            log.warning("MB tracklist fetch (cached path) failed for %s: %s", mbid, e)
            match.notes += f"\nTracklist fetch failed: {type(e).__name__}: {e}"

    log.info(
        "MB cache hit for album %s (mbid=%s score=%d) — skipped search",
        album_id, mbid, score,
    )
    return match


def _is_stale(last_checked_at: str | None) -> bool:
    """Cache age check. Missing or unparseable timestamp = stale (refresh)."""
    if not last_checked_at:
        return True
    try:
        from datetime import datetime, timezone, timedelta
        ts = datetime.fromisoformat(last_checked_at.replace("Z", "+00:00"))
        return datetime.now(timezone.utc) - ts > timedelta(days=CACHE_REFRESH_DAYS)
    except Exception:
        return True


# ---------- Recording lookup ----------
#
# The duplicate-tracks check stores the AcoustID-matched recording_mbid on
# every cluster it surfaces, but doesn't know what that recording IS by
# name. The resolve dialog wants "MB says: <title> — <artist>" so the user
# can disambiguate which mistagged copy is which.
#
# get_recording_canonical fetches /ws/2/recording/{mbid} once and caches
# in-process. MB recordings are immutable, so a single lookup per MBID per
# server lifetime is sufficient — no cache invalidation needed.

from functools import lru_cache


@lru_cache(maxsize=2048)
def get_recording_release_groups(mbid: str, timeout: int = 10) -> list[dict[str, str]] | None:
    """Fetch the release-groups that contain a given recording MBID.

    Returns a list of `{release_group_mbid, title}` dicts, or None on
    network / parse failure. Empty list means MB knows the recording
    but it isn't on any release-group (rare — mostly bootleg / unreleased
    items with `?inc=releases` data missing).

    Used by the metadata resolver's fingerprint-vote tier: each per-track
    AcoustID hit gives us a recording_mbid; mapping those to release-groups
    and counting the most common one anchors the album for free.
    """
    if not mbid:
        return None
    try:
        resp = requests.get(
            MB_RECORDING_DETAIL.format(mbid=mbid),
            params={"fmt": "json", "inc": "releases+release-groups"},
            headers={"User-Agent": USER_AGENT},
            timeout=timeout,
        )
    except requests.RequestException as e:
        log.warning("MB recording release-groups lookup failed for %s: %s", mbid, e)
        return None
    if resp.status_code == 404:
        return None
    if resp.status_code != 200:
        log.warning("MB recording release-groups HTTP %s for %s", resp.status_code, mbid)
        return None
    try:
        data = resp.json()
    except ValueError:
        return None

    # Recording → releases → release-group. Multiple releases can share the
    # same release-group (different pressings of the same album); de-dupe.
    seen: dict[str, str] = {}
    for release in data.get("releases") or []:
        rg = release.get("release-group") or {}
        rg_mbid = rg.get("id")
        if not rg_mbid or rg_mbid in seen:
            continue
        seen[rg_mbid] = rg.get("title") or ""
    return [{"release_group_mbid": k, "title": v} for k, v in seen.items()]


@lru_cache(maxsize=2048)
def get_recording_canonical(mbid: str, timeout: int = 10) -> dict[str, str] | None:
    """Fetch a MusicBrainz recording's canonical title + primary artist.

    Returns:
      - {"title": "...", "artist": "..."} on success.
      - None if the MBID isn't found, the request fails, or the response
        shape is unexpected. Callers should treat None as "MB couldn't tell
        us; fall back to whatever the user has in their tags."

    Cached in-process via lru_cache so repeated dialog opens for the same
    recording hit one HTTP call total per server run.
    """
    if not mbid:
        return None
    try:
        resp = requests.get(
            MB_RECORDING_DETAIL.format(mbid=mbid),
            params={"fmt": "json", "inc": "artist-credits"},
            headers={"User-Agent": USER_AGENT},
            timeout=timeout,
        )
    except requests.RequestException as e:
        log.warning("MB recording lookup failed for %s: %s", mbid, e)
        return None
    if resp.status_code == 404:
        return None
    if resp.status_code != 200:
        log.warning("MB recording lookup HTTP %s for %s", resp.status_code, mbid)
        return None
    try:
        data = resp.json()
    except ValueError:
        return None

    title = (data.get("title") or "").strip()
    if not title:
        return None
    # artist-credit is a list of credit segments — joined with their
    # joinphrases ("X & Y", "X feat. Y"). For a duplicate-resolve dialog,
    # the human-readable joined form is what we want.
    artist = _join_artist_credits(data.get("artist-credit") or [])
    return {"title": title, "artist": artist}


def _join_artist_credits(credits: list[dict[str, Any]]) -> str:
    """Reassemble MB's artist-credit list into a single display string.
    Each entry has {name, joinphrase}; concatenating in order yields the
    canonical credit (e.g. "Simon & Garfunkel", "Beyoncé feat. Jay-Z")."""
    parts: list[str] = []
    for c in credits:
        name = (c.get("name") or c.get("artist", {}).get("name") or "").strip()
        if not name:
            continue
        parts.append(name)
        join = c.get("joinphrase") or ""
        if join:
            parts.append(join)
    return "".join(parts).strip()


# ---------- Backwards-compat shim ----------
# Some older code paths expected the dict shape from the now-removed
# cover_lookup.find_cover(). Keep the same function signature here so
# callers don't have to migrate.

def find_cover(album: dict[str, Any], *, hint: str = "", timeout: int = 15) -> dict[str, Any]:
    """Legacy entry point matching the old `cover_lookup.find_cover()`
    return shape (the cover_lookup module no longer exists; this lives
    here as `mb_lookup.find_cover`).

    Returns {cover_url, source, notes, confidence}. New code should call
    find_release() directly so it can also use the metadata fields.
    """
    match = find_release(album, hint=hint, fetch_cover=True, fetch_tracklist=False, timeout=timeout)
    return {
        "cover_url": match.cover_url,
        "source": match.source,
        "notes": match.notes,
        "confidence": match.confidence,
    }


# ---------- Internal helpers ----------

def _mb_query(artist: str, title: str) -> str:
    """Lucene query escaping + AND of two phrase-matched fields."""
    def _q(s: str) -> str:
        escaped = s.replace("\\", "\\\\").replace('"', '\\"')
        return f'"{escaped}"'
    return f"artist:{_q(artist)} AND release:{_q(title)}"


def _confidence_from_score(score: int) -> str:
    if score >= HIGH_CONFIDENCE_SCORE:
        return "high"
    if score >= MEDIUM_CONFIDENCE_SCORE:
        return "medium"
    return "low"


def _pick_primary_genre(tags: list[dict[str, Any]]) -> str | None:
    """MB tags are user-submitted with vote counts. Pick the one with the
    highest count, but only if it has at least 2 votes — single-vote tags
    are noise. Returns None if nothing qualifies."""
    if not tags:
        return None
    best = max(tags, key=lambda t: int(t.get("count", 0) or 0))
    if int(best.get("count", 0) or 0) < 2:
        return None
    name = (best.get("name") or "").strip()
    return name or None


def _head_cover(url: str, *, timeout: int) -> tuple[bool, str]:
    """HEAD with redirects so we follow CAA's 307 to the actual image."""
    try:
        resp = requests.head(
            url, timeout=timeout, allow_redirects=True,
            headers={"User-Agent": USER_AGENT},
        )
    except requests.RequestException as e:
        return False, f"{type(e).__name__}: {e}"
    if resp.status_code == 200:
        return True, "ok"
    if resp.status_code == 404:
        return False, "no front-cover for this release"
    return False, f"HTTP {resp.status_code}"


def _fetch_tracklist(release_group_mbid: str, *, timeout: int) -> tuple[str, list[MBTrack]]:
    """Browse releases in the group, pick the earliest with a usable
    tracklist, and return (release_mbid, tracks).

    Why earliest: we want the *canonical* tracklist that matches the user's
    rip, which is almost always the original release. Reissues / remasters
    usually add bonus tracks we don't want fitting in the gap detection.

    MB allows 1 req/sec; we make at most 2 requests here.
    """
    # Browse releases in the group, with their media + recordings inline.
    try:
        resp = requests.get(
            MB_RELEASE_GROUP_BROWSE,
            params={
                "release-group": release_group_mbid,
                "fmt": "json",
                # `artist-credits` is REQUIRED for per-recording artist
                # to appear in the response — without it, the embedded
                # `recording` object has no `artist-credit` field and
                # MBTrack.artist comes back None. That's silently fatal
                # for compilations: tags-out-of-sync gets canonical
                # titles but no canonical artists, so the apply
                # corrects titles while leaving wrong artists in place.
                "inc": "media+recordings+artist-credits",
                "limit": 25,
            },
            headers={"User-Agent": USER_AGENT},
            timeout=timeout,
        )
    except requests.RequestException as e:
        raise RuntimeError(f"release browse failed: {e}") from e
    if resp.status_code != 200:
        raise RuntimeError(f"HTTP {resp.status_code} from release browse")

    # Be polite to MB — pace any chained requests within this lookup.
    time.sleep(1.0)

    data = resp.json()
    releases = data.get("releases") or []
    if not releases:
        return "", []

    def _first_release_date(r: dict[str, Any]) -> str:
        return r.get("date") or "9999-99-99"

    releases.sort(key=_first_release_date)
    for release in releases:
        media = release.get("media") or []
        if not media:
            continue
        # Linearize multi-disc tracklists (disc 1 first, then disc 2 starting
        # at the next number) — same convention the user's library uses.
        tracks: list[MBTrack] = []
        position = 0
        usable = True
        for medium in media:
            medium_tracks = medium.get("tracks") or []
            if not medium_tracks:
                usable = False
                break
            for t in medium_tracks:
                position += 1
                rec = t.get("recording") or {}
                title = (t.get("title") or rec.get("title") or "").strip()
                if not title:
                    usable = False
                    break
                length_ms = t.get("length") or rec.get("length")
                tracks.append(MBTrack(
                    position=position,
                    title=title,
                    length_ms=int(length_ms) if length_ms else None,
                    artist=_artist_credit_string(rec.get("artist-credit") or []),
                    recording_mbid=rec.get("id"),
                ))
            if not usable:
                break
        if usable and tracks:
            return release.get("id") or "", tracks
    return "", []


def _earliest_release_year(release_group_mbid: str, *, timeout: int) -> str | None:
    """Browse releases in the group and return the earliest 4-digit year.

    Used when the release-group itself has no first-release-date — most
    common for older catalogue entries where nobody backfilled the date
    on the group but every release has one. Returns None on any failure;
    callers treat that the same as "MB doesn't have it."
    """
    try:
        resp = requests.get(
            MB_RELEASE_GROUP_BROWSE,
            params={
                "release-group": release_group_mbid,
                "fmt": "json",
                "limit": 25,
            },
            headers={"User-Agent": USER_AGENT},
            timeout=timeout,
        )
    except requests.RequestException as e:
        log.debug("release browse for year fallback failed: %s", e)
        return None
    if resp.status_code != 200:
        return None

    releases = (resp.json() or {}).get("releases") or []
    earliest: str | None = None
    for r in releases:
        date = r.get("date") or ""
        if len(date) >= 4 and date[:4].isdigit():
            year = date[:4]
            if earliest is None or year < earliest:
                earliest = year
    return earliest


def _artist_credit_string(credit: list[dict[str, Any]]) -> str | None:
    if not credit:
        return None
    parts = []
    for entry in credit:
        name = (entry.get("name") or (entry.get("artist") or {}).get("name") or "").strip()
        if name:
            parts.append(name)
            join = entry.get("joinphrase") or ""
            if join:
                parts.append(join)
    return "".join(parts).strip() or None
