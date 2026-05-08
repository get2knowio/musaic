"""Check X1 — albums not yet anchored to a MusicBrainz release.

Detects albums where `mb_release_group_mbid` is NULL — i.e. we've never
identified them on MusicBrainz. Each Fix click runs `find_release_cached`
which writes back the MBID on a confident match, unlocking everything
downstream that depends on it: cover lookups short-circuit the search
step, the year-fallback path can browse releases without re-searching,
and the duplicate-tracks check has a stable identity to group on.

Two-tier fix path:
  1. Try MB-direct first. If `dir_artist` + `dir_album` produce a confident
     match, the MBID gets written back via `cache.set_album_mb_anchor`
     (free, no AI quota burned).
  2. If MB-direct fails — usually because the directory name doesn't fit
     MB's canonical "Artist - Album" form (e.g. "Caravelli" vs MB's "The
     Caravelli Singers") — enqueue an AI metadata job. The AI can use
     WebSearch / WebFetch to disambiguate and returns the MBID in its
     suggestion. Worker validates the MBID format, verifies it exists
     against MB live, then persists via the same anchor path.

Bulk-action ("Identify on MusicBrainz") still runs MB-direct only — the
bulk path is for sweeping the library deterministically; per-album AI
disambiguation goes through the per-finding Fix click.
"""
from __future__ import annotations

import logging
from typing import Iterable

from .. import db as db_module
from .. import jobs as jobs_module
from .. import mb_lookup
from .base import Check, CheckMeta, CheckScope, Finding, FixResult

log = logging.getLogger(__name__)


class MissingMbid(Check):
    meta = CheckMeta(
        id="missing-mbid",
        label="Albums not yet identified on MusicBrainz",
        description=(
            "Albums not yet matched to a MusicBrainz release. The MBID anchor "
            "is the foundation other checks build on — it powers cover "
            "lookups, year fallbacks, and tracklist matching, and cuts every "
            "future MB request from a search down to a direct fetch. "
            "Run \"Identify on MusicBrainz\" to anchor the whole library at once."
        ),
        category="external",
        cost="cheap",
        runs_during_scan=True,
        # ai-job: per-finding Fix falls through to AI when MB-direct
        # can't disambiguate the dir-name. The HintDialog flow lets the
        # user supply context (regional pressing, year, alternate name)
        # before kicking off the AI job. The AI's job result includes
        # the release-group MBID; the worker validates the format,
        # verifies it exists in MB, then persists via
        # cache.set_album_mb_anchor (same anchor write the deterministic
        # path uses). The bulk action stays MB-direct only — see
        # try_quick_fix.
        fix_kind="ai-job",
        bulk_action_label="Identify on MusicBrainz",
    )

    def run(self, scope: CheckScope) -> Iterable[Finding]:
        # Albums with NO MBID yet AND that haven't recently been
        # attempted (either via MB-direct or AI). The
        # mb_last_checked_at IS NULL clause is what skips the
        # "AI tried, nothing found" cohort so we don't keep
        # nagging the user to re-run AI on un-identifiable albums.
        # The staleness window (mb_lookup.CACHE_REFRESH_DAYS, 90 days)
        # is enforced via a date math comparison: rows whose
        # last_checked_at is older than the cutoff get re-flagged
        # so MB / AI can take another swing — useful when MB has
        # gained the release in the meantime.
        from .. import mb_lookup
        from datetime import datetime, timedelta, timezone
        cutoff = (
            datetime.now(timezone.utc)
            - timedelta(days=mb_lookup.CACHE_REFRESH_DAYS)
        ).isoformat(timespec="seconds").replace("+00:00", "Z")
        sql = """
            SELECT id, dir_name, dir_artist, dir_album, title, album_artist,
                   mb_last_checked_at
            FROM albums
            WHERE library_path = ?
              AND mb_release_group_mbid IS NULL
              AND (
                mb_last_checked_at IS NULL
                OR mb_last_checked_at < ?
              )
        """
        with db_module.connect() as conn:
            rows = conn.execute(sql, (scope.library_path, cutoff)).fetchall()

        for row in rows:
            # If we'd previously attempted but the staleness window
            # expired, surface that in the summary so the user knows
            # this isn't a brand-new "never tried" finding.
            previously_attempted = row["mb_last_checked_at"] is not None
            summary = (
                "Re-checking after staleness window — last MB lookup "
                f"was {row['mb_last_checked_at']}"
                if previously_attempted
                else "Not yet identified on MusicBrainz"
            )
            yield Finding(
                album_id=row["id"],
                summary=summary,
                severity="info",
                detail={
                    "dir_name":              row["dir_name"],
                    "dir_artist":            row["dir_artist"],
                    "dir_album":             row["dir_album"],
                    "previously_attempted":  previously_attempted,
                    "mb_last_checked_at":    row["mb_last_checked_at"],
                },
            )

    def fix(self, finding: Finding, *, user_hint: str = "") -> FixResult:
        """Two-tier fix path.

        Without a hint: try MB-direct first (free + deterministic).
        Only fall through to AI when MB couldn't disambiguate. AI uses
        WebSearch to figure out the canonical name, returns the MBID,
        and the worker writes it back via cache.set_album_mb_anchor.

        With a hint: skip the MB-direct shortcut and go straight to AI
        — the user added context for a reason, and silently bypassing
        them with an MB-direct match would discard their input.
        """
        if finding.album_id is None:
            return FixResult(ok=False, message="No album to identify")
        album = _load_album(finding.album_id)
        if album is None:
            return FixResult(ok=False, message="Album not found in cache")

        if not user_hint:
            mb_result = self._try_mb_direct(finding, album)
            if mb_result is not None and mb_result.ok:
                return mb_result

        # Fall through to AI metadata job. The job's result includes
        # mb_release_group_mbid when AI confidently identified the
        # release; the worker validates + verifies + persists it.
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
            message="Asking Claude to identify this album on MusicBrainz",
            job_id=job.id,
        )

    def try_quick_fix(self, finding: Finding) -> FixResult | None:
        """Bulk-mode entry point. MB-direct only — never enqueues AI
        from a bulk pass (the bulk action is for sweeping cheap fixes,
        not committing N AI jobs in one click).

        Both unknown-album cases (no album_id on the finding, or the
        album row vanished from the cache) return None so the bulk
        dispatcher counts them as 'not-applicable' rather than
        'skipped' — a missing cache row is an environmental issue,
        not 'MB couldn't find it.'
        """
        if finding.album_id is None:
            return None
        album = _load_album(finding.album_id)
        if album is None:
            return None
        return self._try_mb_direct(finding, album)

    def _try_mb_direct(self, finding: Finding, album: dict) -> FixResult:
        """Run the deterministic MusicBrainz search. find_release writes
        the anchor back via cache.set_album_mb_anchor on a confident
        match. Don't fetch cover or tracklist here — that's extra HTTP
        round-trips for a check whose only goal is the MBID."""
        match = mb_lookup.find_release(
            album, fetch_cover=False, fetch_tracklist=False,
        )
        if match.confident and match.mbid:
            log.info(
                "Identified %s on MusicBrainz: mbid=%s score=%d",
                finding.album_id, match.mbid, match.score,
            )
            return FixResult(
                ok=True,
                message=f"Anchored to release-group {match.mbid} (score {match.score}/100)",
            )
        log.info(
            "Couldn't identify %s on MusicBrainz: score=%d notes=%s",
            finding.album_id, match.score, (match.notes or "").splitlines()[0][:120],
        )
        return FixResult(
            ok=False,
            message=(
                f"MusicBrainz didn't confidently match (score {match.score}/100). "
                f"Try editing the directory name to the canonical "
                f"\"Artist - Album\" form and re-running."
            ),
        )


def _load_album(album_id: str) -> dict | None:
    with db_module.connect() as conn:
        row = conn.execute(
            "SELECT id, path, dir_name, title, artist, album_artist, year, "
            "       genre, dir_artist, dir_album, is_compilation "
            "FROM albums WHERE id = ?",
            (album_id,),
        ).fetchone()
    return dict(row) if row else None
