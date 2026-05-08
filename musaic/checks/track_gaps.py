"""Check M3 — track-number gaps and duplicates within an album.

Reads the per-album `track_numbers_json` populated by the scanner. We
ignore tracks whose tracknumber tag was missing or unparseable (those
are surfaced by the future per-track-tag check, M2). For the remaining
numbers, we yield up to two findings per album:

  - "Track-number gaps" if the sorted unique sequence has missing values
    between min and max (e.g. 1, 2, 4, 5 → missing 3).
  - "Duplicate track numbers" if any value appears more than once.

A truly empty `track_numbers` (rows from before v6 was scanned, or scan
errors) is treated as "skip" — no finding emitted, since we can't tell
the difference between "no tracks" and "no data."

Fix path is two-tier:
  1. Try MusicBrainz directly — for a confident release-group match with
     a tracklist, walk the local FLACs and match each by title to a
     canonical position. If ALL tracks match unambiguously, apply the
     renumbering directly via tags.apply_to_album. No AI quota burned,
     deterministic outcome.
  2. Fall through to enqueueing an AI metadata job whenever the MB match
     isn't confident, doesn't have a tracklist, or local titles can't be
     unambiguously mapped to canonical positions (off-by-one rips,
     bonus tracks, etc.).
"""
from __future__ import annotations

import logging
from collections import Counter
from pathlib import Path
from typing import Iterable

from mutagen.flac import FLAC

from .. import db as db_module
from .. import fingerprint as fingerprint_module
from .. import jobs as jobs_module
from .. import mb_lookup
from . import _apply as direct_apply
from .base import Check, CheckMeta, CheckScope, Finding, FixResult

log = logging.getLogger(__name__)


class TrackGaps(Check):
    meta = CheckMeta(
        id="track-gaps",
        label="Track-number gaps & duplicates",
        description=(
            "Albums whose track-number sequence has gaps (1, 2, 4) or "
            "repeats (1, 2, 2). Detected from tag inspection — fingerprints "
            "not required to detect, but Fix uses them when available. "
            "Cascade: MB title match → AcoustID fingerprint match → AI. "
            "Note: this is about the TAG; the duplicate-tracks check "
            "catches duplicate AUDIO regardless of tag."
        ),
        category="metadata",
        cost="cheap",
        runs_during_scan=True,
        fix_kind="ai-job",
        bulk_action_label="Try MusicBrainz on all",
    )

    def run(self, scope: CheckScope) -> Iterable[Finding]:
        for album in scope.albums:
            nums = album.get("track_numbers") or []
            # Strip None entries (missing tracknumber tags — out of scope for
            # this check). Also need at least 2 entries for "gaps" to mean
            # anything.
            present = sorted(n for n in nums if isinstance(n, int))
            if len(present) < 2:
                continue

            # Gaps: any pair (a, b) where b > a + 1 means values [a+1..b-1]
            # are missing.
            unique = sorted(set(present))
            missing: list[int] = []
            for prev, curr in zip(unique, unique[1:]):
                if curr - prev > 1:
                    missing.extend(range(prev + 1, curr))

            if missing:
                yield Finding(
                    album_id=album["id"],
                    summary=f"Track-number gaps: missing {', '.join(map(str, missing))}",
                    severity="warn",
                    detail={
                        "present":         unique,
                        "missing":         missing,
                        "expected_total":  album.get("track_count"),
                    },
                )

            # Duplicates: a value with count > 1.
            counts = Counter(present)
            dups = sorted(n for n, c in counts.items() if c > 1)
            if dups:
                yield Finding(
                    album_id=album["id"],
                    summary=f"Duplicate track numbers: {', '.join(map(str, dups))}",
                    severity="error",  # more severe — almost certainly a rip / tag bug
                    detail={
                        "present":     present,
                        "duplicates":  dups,
                    },
                )

    def fix(self, finding: Finding, *, user_hint: str = "") -> FixResult:
        if finding.album_id is None:
            return FixResult(ok=False, message="No album to fix")
        album = _load_album(finding.album_id)
        if album is None:
            return FixResult(ok=False, message="Album not found in cache")

        # Cascade: MB-by-title → MB-by-fingerprint → AI. Skip the
        # deterministic tier when the user provided a hint — they're
        # disambiguating something the MB / fingerprint paths already
        # missed (or that they want their context to influence).
        if not user_hint:
            det_result = self._try_deterministic(finding, album)
            if det_result is not None:
                return det_result

        # Fall back to AI metadata job. The hint flows into the job's
        # stored `hint` field, which the worker passes to
        # ai.suggest_metadata.
        try:
            job = jobs_module.enqueue(
                album_id=finding.album_id,
                album_path=album["path"] or "",
                album_title=album["title"] or album["dir_name"] or "",
                job_type="metadata",
                hint=user_hint or "",
            )
        except jobs_module.JobConflictError as e:
            return FixResult(
                ok=False,
                message=f"Already has an active job ({e.status})",
                job_id=e.existing_job_id,
            )
        return FixResult(
            ok=True,
            message="Asking Claude to verify the track sequence and propose a fix",
            job_id=job.id,
        )

    def try_quick_fix(self, finding: Finding) -> FixResult | None:
        if finding.album_id is None:
            return None
        album = _load_album(finding.album_id)
        if album is None:
            return FixResult(ok=False, message="Album not found in cache")

        det_result = self._try_deterministic(finding, album)
        if det_result is not None and det_result.ok:
            return det_result
        return FixResult(
            ok=False,
            message="No deterministic fix found (MB title + fingerprint both inconclusive)",
        )

    # ---- Cascade: title → fingerprint → caller decides on AI ----------
    def _try_deterministic(self, finding: Finding, album: dict) -> FixResult | None:
        """Two-tier deterministic attempt. Returns FixResult(ok=True) when
        either tier renumbers the album. Returns None when neither can —
        caller falls back to AI (fix()) or reports skipped (try_quick_fix).

        Tier 1: MB tracklist match by title (case-insensitive exact). Fast,
                no Chromaprint needed. Catches the common "wrong numbers,
                right titles" case.
        Tier 2: AcoustID fingerprint match against the MB tracklist's
                recording MBIDs. Slow (1-2s per file for fpcalc + a single
                AcoustID batch call), but identifies tracks regardless of
                what the local titles say. Catches the case where titles
                are also wrong/missing.
        """
        # Use the MBID-cached entry point so subsequent fixes for already-
        # identified albums skip the search step entirely.
        match = mb_lookup.find_release_cached(
            album, fetch_cover=False, fetch_tracklist=True,
        )
        log.info(
            "MB tracklist lookup for album %s: score=%d confident=%s tracks=%d",
            finding.album_id, match.score, match.confident, len(match.tracklist),
        )
        if not (match.confident and match.tracklist):
            return None

        # Tier 1: title match
        renumbering = _match_local_to_mb(album["path"], match.tracklist)
        if renumbering is not None:
            return self._apply_renumbering(finding, renumbering, source="MB title match")

        log.info(
            "Track-gaps tier 1 (title) couldn't map %s — trying fingerprints",
            finding.album_id,
        )

        # Tier 2: AcoustID fingerprint match
        renumbering = self._fingerprint_renumbering(finding, album, match.tracklist)
        if renumbering is not None:
            return self._apply_renumbering(finding, renumbering, source="AcoustID fingerprint match")

        return None

    def _fingerprint_renumbering(
        self, finding: Finding, album: dict, mb_tracks: list,
    ) -> dict[str, int] | None:
        """Use AcoustID fingerprints to map local files to canonical MB
        positions when title matching has failed. Returns the same
        {filename: position} shape `_match_local_to_mb` does, or None when
        fingerprinting can't deliver a clean mapping.

        Reasons to bail (return None) and fall through to AI:
          - fpcalc not installed / API key not set / network failure
          - any local file's recording_mbid doesn't appear in the MB tracklist
          - any local file got no AcoustID match
          - two local files match the same canonical position
        """
        # Quick precheck so we don't even read the cache when it's hopeless.
        ok, reason = fingerprint_module.is_available()
        if not ok:
            log.info(
                "Track-gaps fingerprint tier unavailable for %s: %s",
                finding.album_id, reason,
            )
            return None

        # MBID → canonical position. If MB itself has duplicate MBIDs (rare,
        # but possible on weird releases with the same recording reused),
        # we can't assign safely — punt to AI.
        position_by_recording: dict[str, int] = {}
        seen: set[str] = set()
        for t in mb_tracks:
            if not t.recording_mbid:
                continue  # missing in MB — can't use this position for matching
            if t.recording_mbid in seen:
                log.info(
                    "Track-gaps fingerprint tier: MB has duplicate recording "
                    "mbids in tracklist for %s; bailing", finding.album_id,
                )
                return None
            seen.add(t.recording_mbid)
            position_by_recording[t.recording_mbid] = t.position

        if not position_by_recording:
            return None  # MB tracklist had no recording_mbids at all

        try:
            fingerprints = fingerprint_module.fingerprint_album(
                finding.album_id, album["path"],
            )
        except fingerprint_module.FingerprintError as e:
            log.info(
                "Track-gaps fingerprint tier failed for %s (%s): %s",
                finding.album_id, e.code, e.detail or e,
            )
            return None

        out: dict[str, int] = {}
        used_positions: set[int] = set()
        for fp in fingerprints:
            if not fp.acoustid_recording_mbid:
                # Some files matched, others didn't — partial mapping is
                # unsafe (might leave gaps), so bail.
                log.info(
                    "Track-gaps fingerprint tier: %s has no AcoustID match for %s — bailing",
                    finding.album_id, fp.filename,
                )
                return None
            position = position_by_recording.get(fp.acoustid_recording_mbid)
            if position is None:
                log.info(
                    "Track-gaps fingerprint tier: %s recording %s not in MB tracklist — bailing",
                    finding.album_id, fp.acoustid_recording_mbid,
                )
                return None
            if position in used_positions:
                log.info(
                    "Track-gaps fingerprint tier: two files matched MB position %d on %s — bailing",
                    position, finding.album_id,
                )
                return None
            used_positions.add(position)
            out[fp.filename] = position

        if not out:
            return None
        log.info(
            "Track-gaps fingerprint tier mapped %d files to MB positions for %s",
            len(out), finding.album_id,
        )
        return out

    def _apply_renumbering(
        self, finding: Finding, renumbering: dict[str, int], *, source: str,
    ) -> FixResult | None:
        track_suggestions = {
            fname: {"tracknumber": str(pos)} for fname, pos in renumbering.items()
        }
        ok, msg = direct_apply.apply_direct(
            album_id=finding.album_id,
            track_suggestions=track_suggestions,
        )
        if not ok:
            log.warning(
                "Track-gaps apply FAILED for %s (%s): %s",
                finding.album_id, source, msg,
            )
            return None
        log.info(
            "Track-gaps renumber succeeded for %s via %s (%d tracks)",
            finding.album_id, source, len(renumbering),
        )
        return FixResult(
            ok=True,
            message=f"Renumbered {len(renumbering)} tracks via {source}",
        )


def _load_album(album_id: str) -> dict | None:
    with db_module.connect() as conn:
        row = conn.execute(
            "SELECT id, path, title, dir_name, artist, album_artist, "
            "       year, genre, dir_artist, dir_album, is_compilation "
            "FROM albums WHERE id = ?",
            (album_id,),
        ).fetchone()
    return dict(row) if row else None


def _match_local_to_mb(album_dir: str, mb_tracks: list) -> dict[str, int] | None:
    """Pair each local FLAC's title against MB's canonical tracklist.

    Returns a dict {filename: canonical_position} when EVERY local file
    matches exactly one canonical track AND every canonical position is
    used at most once. Returns None on any ambiguity, mismatch, or
    unreadable FLAC — the caller falls through to the AI path.

    v1 uses case-insensitive whitespace-normalized exact matching. That
    catches the common case (rip with accurate titles, bad numbers) while
    being safe enough to auto-apply. Fuzzy matching (ratio scoring,
    diacritic folding) would help a long tail of edge cases — leave that
    for v2 once we see how often v1 falls through.
    """
    album_path = Path(album_dir)
    if not album_path.is_dir():
        return None

    # MB title (normalized) → canonical position
    mb_by_title: dict[str, int] = {}
    duplicate_titles: set[str] = set()
    for t in mb_tracks:
        key = _norm(t.title)
        if not key:
            return None  # MB had a blank title — can't match safely
        if key in mb_by_title:
            duplicate_titles.add(key)
        mb_by_title[key] = t.position
    if duplicate_titles:
        # MB itself has same-title tracks (rare for canonical releases but
        # happens for things like multi-disc with reprises). Can't pick
        # safely — punt to AI.
        return None

    used_positions: set[int] = set()
    out: dict[str, int] = {}
    for entry in sorted(album_path.iterdir()):
        if not entry.is_file() or entry.suffix.lower() != ".flac" or entry.name.startswith("._"):
            continue
        try:
            audio = FLAC(str(entry))
            title = ""
            if audio.tags:
                values = audio.tags.get("title") or audio.tags.get("TITLE") or []
                if values:
                    title = str(values[0])
        except Exception:
            return None  # unreadable FLAC — bail
        key = _norm(title)
        if not key or key not in mb_by_title:
            return None
        position = mb_by_title[key]
        if position in used_positions:
            return None  # two local files matched the same canonical slot
        used_positions.add(position)
        out[entry.name] = position

    # Sanity: every local file got a position, and at least one position
    # actually changed (otherwise applying is a no-op).
    if not out:
        return None
    return out


def _norm(s: str) -> str:
    """Normalize a title for matching: case-fold, collapse internal
    whitespace, strip ends. Cheap; doesn't touch diacritics or punctuation
    (those bite less often than I'd expect — most rips and MB agree on
    punctuation when they agree on a title at all)."""
    return " ".join((s or "").strip().lower().split())
