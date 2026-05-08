"""Check X1 — albums not yet anchored to a MusicBrainz release.

Detects albums where `mb_release_group_mbid` is NULL — i.e. we've never
identified them on MusicBrainz. Each Fix click runs the deterministic
resolver (Tier 0a → 0b dir-search → 0c fingerprint vote) which writes
back the MBID on a confident match, unlocking everything downstream
that depends on it: cover lookups short-circuit the search step, the
year-fallback path can browse releases without re-searching, and the
duplicate-tracks check has a stable identity to group on.

Two-tier fix path:
  1. Try the deterministic resolver first. Tier 0b (dir-name MB search)
     handles albums whose folder names match MB's canonical form;
     Tier 0c (per-track AcoustID fingerprint vote) handles the
     misnamed-folder cases as long as fpcalc + an AcoustID API key
     are configured. Either tier writes back via
     cache.set_album_mb_anchor when it succeeds — free, no AI quota.
  2. If the resolver returns no anchor — usually because the dir
     name is too far from canonical AND fingerprinting is unavailable
     or split-vote — enqueue an AI metadata job. The AI can use
     WebSearch / WebFetch to disambiguate and returns the MBID in its
     suggestion. Worker validates the MBID format, verifies it exists
     against MB live, then persists via the same anchor path.

Bulk-action ("Identify on MusicBrainz") runs the same resolver pass —
the bulk path now benefits from fingerprint-vote anchoring for free,
not just dir-search. AI escalation never happens from a bulk pass; the
per-finding Fix click is the only path that can spend AI quota.
"""
from __future__ import annotations

import logging
from typing import Iterable

from .. import db as db_module
from .. import jobs as jobs_module
from ..services import resolver as resolver_service
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
        # ai-job: per-finding Fix falls through to AI when the
        # deterministic resolver can't anchor the album. The HintDialog
        # flow lets the user supply context (regional pressing, year,
        # alternate name) before kicking off the AI job. The AI's job
        # result includes the release-group MBID; the worker validates
        # the format, verifies it exists in MB, then persists via
        # cache.set_album_mb_anchor (same anchor write the resolver
        # uses). The bulk action stays resolver-only — see try_quick_fix.
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

        Without a hint: try the deterministic resolver first
        (dir-search + fingerprint vote — free). Only fall through to
        AI when the resolver couldn't anchor.

        With a hint: skip straight to AI — the user added context for
        a reason, and silently bypassing them with a deterministic
        match would discard their input.
        """
        if finding.album_id is None:
            return FixResult(ok=False, message="No album to identify")

        if not user_hint:
            det_result = self._try_resolver(finding.album_id)
            if det_result.ok:
                return det_result

        album = _load_album(finding.album_id)
        if album is None:
            return FixResult(ok=False, message="Album not found in cache")

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
        """Bulk-mode entry point. Resolver-only — never enqueues AI from
        a bulk pass (the bulk action is for sweeping cheap fixes, not
        committing N AI jobs in one click). The resolver covers both
        dir-name MB search AND fingerprint vote, so the bulk action now
        anchors more albums than the old MB-direct-only path did.

        Returns None when the album row vanished from the cache — the
        bulk dispatcher counts that as 'not-applicable' rather than
        'skipped' (an environmental issue, not 'we tried and failed').
        """
        if finding.album_id is None:
            return None
        # Distinguish missing-album from can't-anchor for the bulk
        # dispatcher: missing = "not-applicable", can't-anchor = "skipped".
        if _load_album(finding.album_id) is None:
            return None
        return self._try_resolver(finding.album_id)

    def _try_resolver(self, album_id: str) -> FixResult:
        """Run the resolver. Anchor → success; no anchor → caller
        decides whether to escalate (per-finding fix) or report a soft
        failure (bulk pass). Anchor writes happen as a side-effect of
        the resolver's Tier 0b/0c (cache.set_album_mb_anchor)."""
        try:
            resolution = resolver_service.resolve_album(album_id)
        except Exception as e:
            log.exception("Resolver failed for album %s", album_id)
            return FixResult(ok=False, message=f"Resolver error: {e}")

        if resolution.anchor is None:
            return FixResult(
                ok=False,
                message=(
                    "Couldn't identify the album from the directory name "
                    "or per-track fingerprints. Try editing the directory "
                    "name to the canonical \"Artist - Album\" form and "
                    "re-running, or use the per-finding Fix to escalate "
                    "to AI."
                ),
            )

        score = int((resolution.anchor.confidence or 0) * 100)
        log.info(
            "Identified %s via %s: mbid=%s score=%d",
            album_id, resolution.anchor_strategy,
            resolution.anchor.value, score,
        )
        return FixResult(
            ok=True,
            message=(
                f"Anchored to release-group {resolution.anchor.value} "
                f"via {resolution.anchor_strategy} (score {score}/100)"
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
