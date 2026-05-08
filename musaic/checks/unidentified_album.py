"""Merged identification check.

Replaces the previous `missing-fingerprints` + `missing-mbid` pair. Both
were prerequisites masquerading as findings: an unfingerprinted album
can't anchor via AcoustID, and an unanchored album can't power cover /
canonical-tag work. The user wants both done; the resolver already
treats them as one cascade. So one check, one fix path.

A finding fires when an album is missing **either** signal:

    A. no rows in `track_fingerprints` (zero AcoustID coverage), AND/OR
    B. `mb_release_group_mbid IS NULL` AND the staleness window for
       MB lookup has elapsed (mb_lookup.CACHE_REFRESH_DAYS).

The summary line tells the user which side(s) are missing so they
aren't surprised when one Fix click runs both fpcalc and the resolver.

Fix cascade (per-finding, with optional user_hint):

  1. **Fingerprint** (skipped if already done) — runs `fpcalc` +
     AcoustID for every track. Cheap if fpcalc + key are configured;
     surfaces a clear "install fpcalc" message otherwise.
  2. **Resolve** — runs `services.resolver.resolve_album` so dir-search
     and per-track fingerprint vote get a chance. Anchor writes via
     `cache.set_album_mb_anchor`.
  3. **AI** — falls through to a `metadata` job when the resolver still
     can't anchor (or when the user supplied a hint, which is always
     a signal to skip the deterministic shortcuts and let AI use the
     extra context).

Bulk action (`Identify all`) runs steps 1+2 for every open finding —
never AI, matching the old missing-mbid behavior. The bulk dispatcher
counts:

    - **fixed** when the resolver anchored the album,
    - **skipped** when both prerequisites ran but no confident anchor
      emerged (user can still per-finding-fix to escalate to AI),
    - **not-applicable** when the album row vanished from the cache.
"""
from __future__ import annotations

import logging
from datetime import datetime, timedelta, timezone
from typing import Iterable

from .. import db as db_module
from .. import fingerprint as fingerprint_module
from .. import jobs as jobs_module
from .. import mb_lookup
from ..services import resolver as resolver_service
from .base import Check, CheckMeta, CheckScope, Finding, FixResult

log = logging.getLogger(__name__)


class UnidentifiedAlbum(Check):
    meta = CheckMeta(
        id="unidentified-album",
        label="Album not yet identified",
        description=(
            "Albums that haven't been fully identified yet — either no "
            "AcoustID fingerprints have been computed for the tracks or "
            "no MusicBrainz release-group has been matched. Both are "
            "prerequisites: fingerprints power per-track recognition + "
            "duplicate detection, and the MBID anchor unlocks cover "
            "lookups, canonical tags, and tracklist matching. Run "
            "\"Identify all\" to fingerprint + match the whole library "
            "in one pass."
        ),
        category="external",
        cost="cheap",
        runs_during_scan=True,
        # ai-job: per-finding Fix falls through to AI when fingerprint +
        # resolver can't anchor the album. Bulk stays deterministic.
        fix_kind="ai-job",
        bulk_action_label="Identify all",
    )

    def run(self, scope: CheckScope) -> Iterable[Finding]:
        # One SQL query that joins both prerequisites. The COALESCE +
        # parameterized cutoff applies the staleness window so albums
        # we've recently *tried* and missed don't keep nagging the user.
        cutoff = (
            datetime.now(timezone.utc)
            - timedelta(days=mb_lookup.CACHE_REFRESH_DAYS)
        ).isoformat(timespec="seconds").replace("+00:00", "Z")

        sql = """
            SELECT
                a.id,
                a.dir_name,
                a.dir_artist,
                a.dir_album,
                a.title,
                a.album_artist,
                a.track_count,
                a.mb_release_group_mbid,
                a.mb_last_checked_at,
                (NOT EXISTS (
                    SELECT 1 FROM track_fingerprints tf
                    WHERE tf.album_id = a.id
                )) AS needs_fingerprints,
                (
                    a.mb_release_group_mbid IS NULL
                    AND (
                        a.mb_last_checked_at IS NULL
                        OR a.mb_last_checked_at < ?
                    )
                ) AS needs_anchor
            FROM albums a
            WHERE a.library_path = ?
              AND a.track_count > 0
              AND (
                NOT EXISTS (
                    SELECT 1 FROM track_fingerprints tf
                    WHERE tf.album_id = a.id
                )
                OR (
                    a.mb_release_group_mbid IS NULL
                    AND (
                        a.mb_last_checked_at IS NULL
                        OR a.mb_last_checked_at < ?
                    )
                )
              )
        """
        with db_module.connect() as conn:
            rows = conn.execute(sql, (cutoff, scope.library_path, cutoff)).fetchall()

        for row in rows:
            needs_fp = bool(row["needs_fingerprints"])
            needs_anchor = bool(row["needs_anchor"])
            previously_attempted = (
                row["mb_last_checked_at"] is not None
                and not row["mb_release_group_mbid"]
            )

            summary = _summarize(
                needs_fp=needs_fp,
                needs_anchor=needs_anchor,
                previously_attempted=previously_attempted,
                last_checked_at=row["mb_last_checked_at"],
            )
            yield Finding(
                album_id=row["id"],
                summary=summary,
                severity="info",
                detail={
                    "dir_name":             row["dir_name"],
                    "dir_artist":           row["dir_artist"],
                    "dir_album":            row["dir_album"],
                    "track_count":          row["track_count"],
                    "needs_fingerprints":   needs_fp,
                    "needs_anchor":         needs_anchor,
                    "previously_attempted": previously_attempted,
                    "mb_last_checked_at":   row["mb_last_checked_at"],
                },
            )

    def fix(self, finding: Finding, *, user_hint: str = "") -> FixResult:
        """Per-finding fix path with the full cascade (fingerprint →
        resolver → AI fallback). A user_hint forces the AI step (the
        user added context for a reason — silently bypassing them with a
        deterministic match would discard their input)."""
        if finding.album_id is None:
            return FixResult(ok=False, message="No album to identify")
        album = _load_album(finding.album_id)
        if album is None:
            return FixResult(ok=False, message="Album not found in cache")

        # Step 1: fingerprint when missing. Capture details so we can
        # describe the multi-step outcome in one message.
        fp_message = ""
        if not _has_fingerprints(finding.album_id):
            fp_outcome = _try_fingerprint(finding.album_id, album["path"])
            fp_message = fp_outcome.message
            if not fp_outcome.ok and not user_hint:
                # Without a hint and without fingerprints, dir-search is
                # the only resolver tier left — try it, but a soft fail
                # here doesn't block the resolver attempt.
                log.debug("fingerprinting %s soft-failed: %s",
                          finding.album_id, fp_outcome.message)

        # Step 2: resolver — only when no hint. A hint signals "AI, with
        # this extra context" and the resolver would silently swallow it.
        if not user_hint:
            resolver_outcome = _try_resolver(finding.album_id)
            if resolver_outcome.ok:
                # Compose the message with any fingerprint detail so the
                # user sees both halves of the cascade at once.
                msg = resolver_outcome.message
                if fp_message:
                    msg = f"{fp_message}; {msg}"
                return FixResult(ok=True, message=msg)

        # Step 3: AI fallback. Enqueue a metadata job — the worker will
        # validate the resulting MBID and persist the anchor.
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
        """Bulk-mode entry point. Runs fingerprint + resolver but never
        AI. Returns None for vanished albums so the bulk dispatcher
        counts them as 'not-applicable' rather than 'skipped'."""
        if finding.album_id is None:
            return None
        album = _load_album(finding.album_id)
        if album is None:
            return None

        # Step 1: fingerprint if missing. A soft fail here (e.g. fpcalc
        # not installed) shouldn't suppress the resolver attempt — the
        # dir-search tier still has a shot.
        fp_message = ""
        if not _has_fingerprints(finding.album_id):
            fp_outcome = _try_fingerprint(finding.album_id, album["path"])
            fp_message = fp_outcome.message

        # Step 2: resolver.
        resolver_outcome = _try_resolver(finding.album_id)
        if resolver_outcome.ok:
            msg = resolver_outcome.message
            if fp_message:
                msg = f"{fp_message}; {msg}"
            return FixResult(ok=True, message=msg)

        # Soft fail — resolver couldn't anchor. Leave for per-finding AI.
        msg = resolver_outcome.message
        if fp_message:
            msg = f"{fp_message}; {msg}"
        return FixResult(ok=False, message=msg)


# ---- Helpers --------------------------------------------------------


def _has_fingerprints(album_id: str) -> bool:
    with db_module.connect() as conn:
        row = conn.execute(
            "SELECT 1 FROM track_fingerprints WHERE album_id = ? LIMIT 1",
            (album_id,),
        ).fetchone()
    return row is not None


def _load_album(album_id: str) -> dict | None:
    with db_module.connect() as conn:
        row = conn.execute(
            "SELECT id, path, dir_name, title, artist, album_artist, year, "
            "       genre, dir_artist, dir_album, is_compilation, track_count "
            "FROM albums WHERE id = ?",
            (album_id,),
        ).fetchone()
    return dict(row) if row else None


def _try_fingerprint(album_id: str, album_path: str) -> FixResult:
    """Run fpcalc + AcoustID. Captures the same human-readable error
    surfaces the standalone missing-fingerprints check used to."""
    try:
        results = fingerprint_module.fingerprint_album(album_id, album_path)
    except fingerprint_module.FingerprintError as e:
        log.info(
            "Fingerprinting %s skipped (%s): %s",
            album_id, e.code, e.detail or e,
        )
        human = {
            "fpcalc-missing": (
                "fpcalc binary not installed. Install with "
                "`brew install chromaprint` to enable AcoustID matching."
            ),
            "no-api-key": (
                "AcoustID API key not set. Configure it under "
                "Settings → Track fingerprinting."
            ),
            "pyacoustid-missing": (
                "pyacoustid Python library not installed. Reinstall "
                "Musaic's dependencies."
            ),
            "album-missing": "Album directory missing from disk.",
            "no-fingerprints": "Couldn't fingerprint any files in this album.",
        }.get(e.code, e.detail or str(e))
        return FixResult(ok=False, message=human)

    if not results:
        return FixResult(ok=False, message="Album had no FLACs to fingerprint")

    matched = sum(1 for r in results if r.acoustid_recording_mbid)
    log.info(
        "Fingerprinted %s: %d files, %d AcoustID matches",
        album_id, len(results), matched,
    )
    return FixResult(
        ok=True,
        message=f"fingerprinted {len(results)} files ({matched} AcoustID matches)",
    )


def _try_resolver(album_id: str) -> FixResult:
    """Run the resolver cascade and report what happened."""
    try:
        resolution = resolver_service.resolve_album(album_id)
    except Exception as e:
        log.exception("Resolver failed for album %s", album_id)
        return FixResult(ok=False, message=f"resolver error: {e}")

    if resolution.anchor is None:
        return FixResult(
            ok=False,
            message=(
                "couldn't identify the album from the directory name "
                "or fingerprints — try editing the directory name to "
                "the canonical \"Artist - Album\" form, or use the "
                "per-finding Fix to escalate to AI"
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
            f"anchored to release-group {resolution.anchor.value} "
            f"via {resolution.anchor_strategy} (score {score}/100)"
        ),
    )


def _summarize(
    *,
    needs_fp: bool,
    needs_anchor: bool,
    previously_attempted: bool,
    last_checked_at: str | None,
) -> str:
    """Human-readable description of which prerequisites are missing."""
    if needs_fp and needs_anchor:
        if previously_attempted:
            return (
                "Not fingerprinted; MusicBrainz match was attempted "
                f"({last_checked_at}) but staleness window has elapsed"
            )
        return "Not fingerprinted and not yet identified on MusicBrainz"
    if needs_fp:
        return "Not yet fingerprinted (AcoustID coverage missing)"
    # needs_anchor only
    if previously_attempted:
        return (
            f"Re-checking after staleness window — last MB lookup was {last_checked_at}"
        )
    return "Not yet identified on MusicBrainz"
