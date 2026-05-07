"""Check M1 — albums missing one or more core tags.

Detects albums where any of the four album-level fields the player UIs
treat as required (`title`, `album_artist`/`artist`, `year`, `genre`) is
null or whitespace-only. Pure SQL, runs as the post-scan tail.

Fix path is two-tier:
  1. Try MusicBrainz directly — for `title` / `album_artist` / `year`,
     a confident MB match deterministically gives us the answer in a
     couple of seconds, no AI quota burned. Apply directly.
  2. Fall through to enqueueing an AI metadata job whenever MB doesn't
     have a confident match OR any of the missing fields couldn't be
     filled in by MB (genre being the common one — MB tags are too
     unreliable to apply without a human in the loop).
"""
from __future__ import annotations

import logging
from dataclasses import dataclass
from typing import Iterable

from .. import db as db_module
from .. import jobs as jobs_module
from .. import mb_lookup
from . import _apply as direct_apply
from .base import Check, CheckMeta, CheckScope, Finding, FixResult

log = logging.getLogger(__name__)


class MissingCoreTags(Check):
    meta = CheckMeta(
        id="missing-core-tags",
        label="Missing core tags",
        description=(
            "Albums missing title, artist, year, or genre. Fix tries "
            "MusicBrainz first (free, deterministic) and only falls back to "
            "AI when MB can't help or genre is the missing field — MB tags "
            "are too unreliable to apply blind. Run the bulk action to skim "
            "MB-resolvable cases off the list before queuing AI work."
        ),
        category="metadata",
        cost="cheap",
        runs_during_scan=True,
        fix_kind="ai-job",
        bulk_action_label="Try MusicBrainz on all",
    )

    def run(self, scope: CheckScope) -> Iterable[Finding]:
        # One SQL query for the whole library — much faster than walking
        # scope.albums in Python. The COALESCE/NULLIF/TRIM dance treats
        # whitespace-only values as missing.
        sql = """
            SELECT id, dir_name, title, album_artist, artist, year, genre
            FROM albums
            WHERE library_path = ?
              AND (
                COALESCE(NULLIF(TRIM(title), ''), '') = ''
                OR COALESCE(NULLIF(TRIM(year), ''), '') = ''
                OR COALESCE(NULLIF(TRIM(genre), ''), '') = ''
                OR (
                    COALESCE(NULLIF(TRIM(album_artist), ''), '') = ''
                    AND COALESCE(NULLIF(TRIM(artist), ''), '') = ''
                )
              )
        """
        with db_module.connect() as conn:
            rows = conn.execute(sql, (scope.library_path,)).fetchall()

        for row in rows:
            missing: list[str] = []
            if not (row["title"] or "").strip():
                missing.append("title")
            if not (row["album_artist"] or row["artist"] or "").strip():
                missing.append("artist")
            if not (row["year"] or "").strip():
                missing.append("year")
            if not (row["genre"] or "").strip():
                missing.append("genre")
            if not missing:
                # Defensive: SQL filter said this row qualified but the
                # Python re-check disagrees. Skip rather than emit a
                # nonsensical finding.
                continue

            yield Finding(
                album_id=row["id"],
                summary=f"Missing: {', '.join(missing)}",
                severity="warn",
                detail={
                    "missing_fields": missing,
                    "dir_name": row["dir_name"],
                },
            )

    def fix(self, finding: Finding, *, user_hint: str = "") -> FixResult:
        if finding.album_id is None:
            return FixResult(ok=False, message="Library-level finding has no album to fix")
        album = _load_album(finding.album_id)
        if album is None:
            return FixResult(ok=False, message="Album not found in cache")

        # Skip MB shortcut when the user supplied a hint — they're
        # disambiguating something MB alone can't resolve, and silently
        # bypassing the AI call would discard their input.
        if not user_hint:
            mb_outcome = self._try_mb(finding, album)
            # `mb_outcome.fully_resolved` → no AI needed.
            if mb_outcome.fully_resolved:
                return FixResult(ok=True, message=mb_outcome.message)

        # Otherwise enqueue AI for whatever's still missing. The hint
        # flows into the job's stored `hint` field, which the worker
        # passes to ai.suggest_metadata.
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
            message="Asking Claude to identify this album and fill in the missing tags",
            job_id=job.id,
        )

    def try_quick_fix(self, finding: Finding) -> FixResult | None:
        """MB-only path used by the bulk 'Try MusicBrainz on all' action.
        Never enqueues AI; returns FixResult(ok=False) when MB couldn't
        fully resolve the finding so the caller knows to leave it alone.

        Returns None when the finding has no actionable album (missing
        album_id or the row is no longer in the cache) so the bulk
        dispatcher counts it as 'not-applicable' rather than 'skipped'
        — the bucket label otherwise misleads users into thinking 'MB
        couldn't help' when the real issue is an environmental cache
        miss.
        """
        if finding.album_id is None:
            return None
        album = _load_album(finding.album_id)
        if album is None:
            return None

        mb_outcome = self._try_mb(finding, album)
        if mb_outcome.fully_resolved:
            return FixResult(ok=True, message=mb_outcome.message)
        # Partial apply: MB filled at least one missing field but not all
        # of them (genre is the common left-over since MB tags are too
        # noisy to auto-apply). Treat as a successful fix in the bulk
        # summary — we made real progress and the next post-scan rerun
        # will reopen a smaller finding the user can finish off. The
        # alternative is to count partials as 'skipped', which the user
        # reads as 'nothing happened' even though a write succeeded.
        if mb_outcome.applied:
            return FixResult(ok=True, message=mb_outcome.message)
        return FixResult(
            ok=False,
            message=mb_outcome.message or "MusicBrainz couldn't fully resolve this finding",
        )

    # ---- Shared MB-attempt internals ------------------------------------

    # Fields MB can populate from a release-group match. Genre is excluded
    # — MB tags are crowd-sourced and too inconsistent to auto-apply.
    _MB_FILLABLE_FIELDS: frozenset[str] = frozenset({"title", "artist", "year"})

    def _try_mb(self, finding: Finding, album: dict) -> "_MBOutcome":
        """Run the MB lookup, apply whatever it can fill, and report what
        happened. Used by both fix() and try_quick_fix() so they stay in
        sync as the MB heuristics evolve."""
        missing_fields = _resolve_missing_fields(finding, album)

        # Short-circuit: if none of the missing fields are ones MB can fill,
        # skip the HTTP call entirely. The common case is "Missing: genre"
        # only — MB doesn't auto-apply genre, so a query is pure waste of
        # MB's rate-limit budget AND ours.
        coverable = [f for f in missing_fields if f in self._MB_FILLABLE_FIELDS]
        if not coverable:
            log.debug(
                "MB skip for %s — only non-fillable fields requested (%s)",
                finding.album_id, missing_fields,
            )
            return _MBOutcome(
                fully_resolved=False,
                applied=False,
                message=f"MusicBrainz can't fill {missing_fields} (not in MB)",
            )

        # Use the cached entry point so we skip the search step for albums
        # we've already MB-identified on a previous fix.
        match = mb_lookup.find_release_cached(
            album, fetch_cover=False, fetch_tracklist=False,
        )
        log.debug(
            "MB lookup for album %s (%s — %s): score=%d confident=%s "
            "year=%s artist=%s title=%s",
            finding.album_id, album.get("dir_artist"), album.get("dir_album"),
            match.score, match.confident, match.year, match.artist, match.title,
        )
        if not match.confident:
            log.debug(
                "MB not confident for %s (score=%d threshold=%d) — no MB apply",
                finding.album_id, match.score, mb_lookup.HIGH_CONFIDENCE_SCORE,
            )
            return _MBOutcome(
                fully_resolved=False,
                applied=False,
                message=f"MusicBrainz didn't confidently match (score {match.score}/100)",
            )

        # Map MB result onto the field names we'd write. Genre is intentionally
        # not included: MB tags are noisy and we'd rather have the user (or AI)
        # decide.
        mb_fill: dict[str, str] = {}
        if "title" in missing_fields and match.title:
            mb_fill["title"] = match.title
        if "artist" in missing_fields and match.artist:
            mb_fill["album_artist"] = match.artist
        if "year" in missing_fields and match.year:
            mb_fill["year"] = match.year

        still_missing = [
            f for f in missing_fields
            if not (
                (f == "title" and mb_fill.get("title")) or
                (f == "artist" and mb_fill.get("album_artist")) or
                (f == "year" and mb_fill.get("year"))
            )
        ]

        if not mb_fill:
            log.debug(
                "MB matched %s but had nothing to fill (asked=%s) — no apply",
                finding.album_id, missing_fields,
            )
            return _MBOutcome(
                fully_resolved=False,
                applied=False,
                message=f"MusicBrainz matched but couldn't fill {missing_fields}",
            )

        ok, msg = direct_apply.apply_direct(
            album_id=finding.album_id,
            suggestions=mb_fill,
        )
        if not ok:
            log.warning(
                "MB direct-apply FAILED for %s (%s)", finding.album_id, msg,
            )
            return _MBOutcome(
                fully_resolved=False,
                applied=False,
                message=f"MusicBrainz apply failed: {msg}",
            )

        filled = ", ".join(f"{k}={v!r}" for k, v in mb_fill.items())
        log.info(
            "MB %s apply succeeded for %s: %s (still_missing=%s)",
            "full" if not still_missing else "partial",
            finding.album_id, filled, still_missing,
        )
        if not still_missing:
            return _MBOutcome(
                fully_resolved=True,
                applied=True,
                message=f"Filled in from MusicBrainz: {filled}",
            )
        return _MBOutcome(
            fully_resolved=False,
            applied=True,
            message=f"Partially filled from MusicBrainz: {filled} "
                    f"(still missing: {', '.join(still_missing)})",
        )


# ---- Module-level helpers -----------------------------------------------

@dataclass
class _MBOutcome:
    fully_resolved: bool   # MB covered everything the finding asked for
    applied: bool          # we wrote at least one field (could be partial)
    message: str


def _load_album(album_id: str) -> dict | None:
    with db_module.connect() as conn:
        row = conn.execute(
            "SELECT id, path, dir_name, title, artist, album_artist, year, genre, "
            "       dir_artist, dir_album, is_compilation "
            "FROM albums WHERE id = ?",
            (album_id,),
        ).fetchone()
    return dict(row) if row else None


def _resolve_missing_fields(finding: Finding, album: dict) -> list[str]:
    """Prefer the finding's stored detail; fall back to a live re-check
    against the cache row for findings predating that detail field."""
    explicit = list((finding.detail or {}).get("missing_fields") or [])
    if explicit:
        return explicit
    out: list[str] = []
    for k, label in (("title", "title"), ("year", "year"), ("genre", "genre")):
        if not (album.get(k) or "").strip():
            out.append(label)
    if not (album.get("album_artist") or album.get("artist") or "").strip():
        out.append("artist")
    return out
