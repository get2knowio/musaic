"""Check X3 — albums not yet fingerprinted with Chromaprint + AcoustID.

Detects albums that have zero rows in `track_fingerprints`. Each Fix
click runs `fingerprint.fingerprint_album` which:

  - shells out to fpcalc (~1 sec per file)
  - submits each fingerprint to AcoustID's lookup API
  - persists fingerprint + recording_mbid + score per file

Pure deterministic; no AI involvement. Required for the duplicate-tracks
check to find anything (it queries `track_fingerprints`), and a strong
booster for the track-gaps fingerprint tier (which can renumber files
even when their titles are wrong).

Failure modes the user cares about:

  - **fpcalc not installed** — surface as "fpcalc-missing"; the bulk
    button effectively becomes a "skip everything" button. The user
    needs to `brew install chromaprint`.
  - **AcoustID API key missing** — same idea; user needs to set the
    key in Settings.
  - **AcoustID API/network errors per file** — handled per-file by
    fingerprint.fingerprint_album; we still cache the local fingerprint
    so a retry can re-query without re-fpcalc'ing.

We don't fall through to anything else — fingerprinting is the only way
to get fingerprints. If it can't run, the finding stays open until
the user fixes the prerequisite.
"""
from __future__ import annotations

import logging
from typing import Iterable

from .. import db as db_module
from .. import fingerprint as fingerprint_module
from .base import Check, CheckMeta, CheckScope, Finding, FixResult

log = logging.getLogger(__name__)


class MissingFingerprints(Check):
    meta = CheckMeta(
        id="missing-fingerprints",
        label="Albums not yet fingerprinted",
        description=(
            "Albums whose tracks haven't been fingerprinted yet. Fingerprints "
            "power per-track recognition: the duplicate-tracks check uses "
            "them to find redundant copies, and the track-gaps fix uses "
            "them to renumber files when titles are wrong. Requires "
            "fpcalc (brew install chromaprint) and an AcoustID API key "
            "in Settings."
        ),
        category="audio",
        # Detection is cheap (one SQL query). The FIX is medium-cost
        # (1 sec of fpcalc per track + AcoustID API call), but cost is
        # advisory — it controls scheduling defaults, not whether we run.
        cost="medium",
        runs_during_scan=True,
        fix_kind="deterministic",
        bulk_action_label="Fingerprint all",
    )

    def run(self, scope: CheckScope) -> Iterable[Finding]:
        # An album "needs fingerprinting" if it has at least one FLAC and
        # no fingerprint rows. We could also surface partial coverage
        # (some tracks fingerprinted, others not) but that's rare and the
        # extra SQL complexity isn't worth it for v1 — a partial album
        # will still get caught when its last fingerprint expires or the
        # user runs the check explicitly.
        sql = """
            SELECT a.id, a.dir_name, a.path, a.track_count
            FROM albums a
            WHERE a.library_path = ?
              AND a.track_count > 0
              AND NOT EXISTS (
                SELECT 1 FROM track_fingerprints tf
                WHERE tf.album_id = a.id
              )
        """
        with db_module.connect() as conn:
            rows = conn.execute(sql, (scope.library_path,)).fetchall()

        for row in rows:
            yield Finding(
                album_id=row["id"],
                summary=f"Not yet fingerprinted ({row['track_count']} tracks)",
                severity="info",
                detail={
                    "dir_name":    row["dir_name"],
                    "track_count": row["track_count"],
                },
            )

    def fix(self, finding: Finding, *, user_hint: str = "") -> FixResult:
        # user_hint is ignored — fingerprinting is deterministic and a
        # text hint can't influence fpcalc / AcoustID. Accept the kwarg
        # to match the base class signature so the runner's keyword
        # forwarding doesn't blow up.
        return self._do_fingerprint(finding)

    def try_quick_fix(self, finding: Finding) -> FixResult | None:
        # No fallback path; bulk and single-click do the same work.
        return self._do_fingerprint(finding)

    def _do_fingerprint(self, finding: Finding) -> FixResult:
        if finding.album_id is None:
            return FixResult(ok=False, message="No album to fingerprint")
        album = _load_album(finding.album_id)
        if album is None:
            return FixResult(ok=False, message="Album not found in cache")

        try:
            results = fingerprint_module.fingerprint_album(
                finding.album_id, album["path"],
            )
        except fingerprint_module.FingerprintError as e:
            # Surface the user-actionable cases (fpcalc / key missing) with
            # a clear message; transient errors (api-error, network) get
            # the raw detail so the log line is useful for diagnosis.
            log.info(
                "Fingerprinting %s skipped (%s): %s",
                finding.album_id, e.code, e.detail or e,
            )
            human = {
                "fpcalc-missing": (
                    "fpcalc binary not installed. Install with "
                    "`brew install chromaprint` and try again."
                ),
                "no-api-key": (
                    "AcoustID API key not set. Configure it under "
                    "Settings → Track fingerprinting."
                ),
                "pyacoustid-missing": (
                    "pyacoustid Python library not installed. Reinstall "
                    "Media Manager's dependencies."
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
            finding.album_id, len(results), matched,
        )
        return FixResult(
            ok=True,
            message=f"Fingerprinted {len(results)} files ({matched} matched on AcoustID)",
        )


def _load_album(album_id: str) -> dict | None:
    with db_module.connect() as conn:
        row = conn.execute(
            "SELECT id, path, dir_name, track_count "
            "FROM albums WHERE id = ?",
            (album_id,),
        ).fetchone()
    return dict(row) if row else None
