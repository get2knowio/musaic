"""Check C1 — albums with no cover art (embedded or sidecar).

Mirrors the existing "missing-art" sidebar filter chip but with durable
findings (so the user can ignore individual cases like "this rare
bootleg has no cover anywhere", and so weekly runs can show
"how many albums still have no cover" as a trend).

Important coordination with the AI-job retry surface
----------------------------------------------------

Albums with a recent failed-or-no-cover AI attempt are *excluded* from
new findings. Otherwise the user clicks "Fix all" → 12 jobs run → 8 fail
because Claude / MusicBrainz couldn't find a cover → next post-scan tail
re-creates 8 findings → user gets perpetual ghost work. The same
"recent prior attempt" definition is used by the frontend's
`usePriorCoverAttempts` hook, so the two surfaces stay consistent.

"Recent" = within the last 30 days. After that we let the user re-try
naturally — Claude's coverage and our own MusicBrainz fallback may have
changed.

Fix path is two-tier:
  1. Try MusicBrainz directly — if a confident release-group match has a
     verified Cover Art Archive front-cover, apply directly. Most albums
     fall here.
  2. Fall through to enqueueing an AI cover job whenever MB is uncertain
     or doesn't have a cover. Covers the long tail (bootlegs, regional
     variants) where AI's fuzzy reasoning still has value.
"""
from __future__ import annotations

import logging
from typing import Iterable

from .. import db as db_module
from .. import jobs as jobs_module
from .. import mb_lookup
from . import _apply as direct_apply
from .base import Check, CheckMeta, CheckScope, Finding, FixResult

log = logging.getLogger(__name__)


# How long after a failed/no-cover attempt to suppress re-flagging the album.
# Tunable; if it bites users we can move it to settings.
_RECENT_ATTEMPT_DAYS = 30


class NoCoverArt(Check):
    meta = CheckMeta(
        id="no-cover-art",
        label="No cover art",
        description=(
            "Albums missing both embedded cover art and a sidecar "
            "cover.jpg / folder.jpg / front.jpg. Fix tries MusicBrainz "
            "/ Cover Art Archive first (free) and falls back to AI only "
            "when MB doesn't have a cover. Albums with a recent failed "
            "lookup are skipped so bulk passes don't keep retrying "
            "dead-ends."
        ),
        category="cover",
        cost="cheap",
        runs_during_scan=True,
        fix_kind="ai-job",
        bulk_action_label="Try MusicBrainz on all",
    )

    def run(self, scope: CheckScope) -> Iterable[Finding]:
        # SQL with two conditions:
        #   1. The album genuinely has no cover (embedded or sidecar).
        #   2. There's no recent prior cover-or-combo job that ended without
        #      a cover (failed, OR acted_on with cover_url null/missing).
        #
        # The job_type filter is intentionally narrow: `metadata`-only
        # jobs never produce a cover_url (the worker only writes one for
        # 'cover' / 'metadata+cover' job types), so a recent failed
        # metadata-only job tells us nothing about whether a cover is
        # findable for this album. Including them would mute findings
        # for albums whose tags couldn't be auto-fixed but whose covers
        # we've never even tried.
        #
        # SQLite's json_extract on a NULL column yields NULL, so the
        # COALESCE → '' coerces it to an empty string for the comparison.
        # Date comparison works lexicographically because we store ISO-8601
        # in UTC.
        sql = f"""
            SELECT id, dir_name, dir_artist, dir_album
            FROM albums
            WHERE library_path = ?
              AND has_embedded_cover = 0
              AND cover_file IS NULL
              AND NOT EXISTS (
                SELECT 1 FROM ai_jobs j
                WHERE j.album_id = albums.id
                  AND j.job_type IN ('cover', 'metadata+cover')
                  AND j.created_at > datetime('now', '-{_RECENT_ATTEMPT_DAYS} days')
                  AND (
                    j.status = 'failed'
                    OR (
                      j.status = 'acted_on'
                      AND COALESCE(json_extract(j.result_json, '$.cover.cover_url'), '') = ''
                    )
                  )
              )
        """
        with db_module.connect() as conn:
            rows = conn.execute(sql, (scope.library_path,)).fetchall()

        for row in rows:
            yield Finding(
                album_id=row["id"],
                summary="No cover art (embedded or sidecar)",
                severity="info",
                detail={
                    "dir_name":   row["dir_name"],
                    "dir_artist": row["dir_artist"],
                    "dir_album":  row["dir_album"],
                },
            )

    def fix(self, finding: Finding, *, user_hint: str = "") -> FixResult:
        if finding.album_id is None:
            return FixResult(ok=False, message="No album to fix")
        album = _load_album(finding.album_id)
        if album is None:
            return FixResult(ok=False, message="Album not found in cache")

        # Skip the MB shortcut when the user supplied a hint — they're
        # giving Claude context (regional variant, year, etc.) that MB
        # alone can't act on, and silently bypassing the AI call would
        # discard their input. Only short-circuit on the no-hint path.
        if not user_hint:
            mb_result = self._try_mb(finding, album)
            if mb_result is not None:
                return mb_result

        # Fall back to enqueueing an AI cover job. The hint flows into the
        # job's stored `hint` field, which the worker passes to ai.suggest_cover.
        try:
            job = jobs_module.enqueue(
                album_id=finding.album_id,
                album_path=album["path"] or "",
                album_title=album["title"] or album["dir_name"] or "",
                job_type="cover",
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
            message="Asking Claude to find cover art for this album",
            job_id=job.id,
        )

    def try_quick_fix(self, finding: Finding) -> FixResult | None:
        # Unknown-album cases (no album_id, or the row is no longer in
        # the cache) return None so the bulk dispatcher buckets them as
        # 'not-applicable' rather than 'skipped' — a missing cache row
        # is environmental, not "MB / CAA didn't have a cover."
        if finding.album_id is None:
            return None
        album = _load_album(finding.album_id)
        if album is None:
            return None
        mb_result = self._try_mb(finding, album)
        if mb_result is not None and mb_result.ok:
            return mb_result
        return FixResult(
            ok=False,
            message="MusicBrainz / Cover Art Archive doesn't have a cover for this album",
        )

    # ---- Shared MB attempt -------------------------------------------
    def _try_mb(self, finding: Finding, album: dict) -> FixResult | None:
        match = mb_lookup.find_release(
            album, fetch_cover=True, fetch_tracklist=False,
        )
        log.info(
            "MB cover lookup for album %s: score=%d confident=%s cover=%s",
            finding.album_id, match.score, match.confident,
            "verified" if match.cover_url_verified else (match.cover_url or "none"),
        )
        if not (match.confident and match.cover_url and match.cover_url_verified):
            return None
        ok, msg = direct_apply.apply_direct(
            album_id=finding.album_id,
            cover_url=match.cover_url,
        )
        if not ok:
            log.warning("Cover MB apply FAILED for %s: %s", finding.album_id, msg)
            return None
        return FixResult(
            ok=True,
            message=f"Embedded cover from MusicBrainz / Cover Art Archive ({msg})",
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
