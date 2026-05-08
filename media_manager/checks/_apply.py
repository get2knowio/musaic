"""Direct-apply helper for check fix paths.

When a check's `fix()` decides it can resolve a finding without going
through the AI job queue (e.g. MusicBrainz returned a confident match),
it calls `apply_direct()` to write the tag / cover changes immediately.

The actual mutation pipeline lives in `services/apply.py` — this is a
thin synchronous wrapper that drains the generator and reports a
single (ok, message) outcome. The streaming SSE endpoint consumes the
same generator and forwards every event to the client.
"""
from __future__ import annotations

import contextvars
import logging
from pathlib import Path

from .. import config as config_module

_log = logging.getLogger(__name__)


# Context variable that the bulk quick-fix-all dispatcher flips to True
# while it's iterating findings. apply_direct() consults it to suppress
# the otherwise-default post-scan check rerun, since the bulk caller does
# one final rerun after the entire batch instead of N reruns interleaved
# with N applies.
in_bulk_run: contextvars.ContextVar[bool] = contextvars.ContextVar(
    "in_bulk_run", default=False,
)


def apply_direct(
    *,
    album_id: str,
    suggestions: dict[str, str] | None = None,
    track_suggestions: dict[str, dict[str, str]] | None = None,
    cover_url: str | None = None,
    rerun_checks: bool = True,
) -> tuple[bool, str]:
    """Synchronously apply changes to an album. Returns (ok, message).

    `suggestions` is the album-level dict (title/album_artist/year/genre).
    `track_suggestions` is keyed by FLAC filename; per-file changes match
    the AI suggestion shape (tracknumber/title/artist).
    `cover_url` triggers a cover download + embed if non-empty.

    `rerun_checks` controls whether the post-scan check tail fires after
    the apply. Default True. Set False during bulk runs — re-running ALL
    checks after EVERY single apply is wasteful (3 checks × N findings =
    N×O(library) re-scans), and the bulk caller can do one final rerun at
    the end if desired.

    Failures get logged and surfaced as the message; we never raise to
    the check's fix() — it should always return a clean FixResult.
    """
    from .. import cache as cache_module
    from ..services.apply import apply_album_changes_iter

    # Resolve album from cache. We don't have ready access to a
    # "find_album_by_id" helper that doesn't go through the FastAPI dep
    # chain, so look it up directly via the cache snapshot.
    cfg = config_module.load_config()
    if not cfg.library_dir:
        return False, "No library configured"
    library_dir = str(Path(cfg.library_dir).expanduser().resolve())
    snapshot = cache_module.load_snapshot(library_dir)
    album = next(
        (a for a in snapshot.albums_by_path.values() if a.get("id") == album_id),
        None,
    )
    if album is None:
        return False, "Album not found in cache"

    # Bulk callers (quick-fix-all) flip the in_bulk_run contextvar so
    # we skip the per-apply post-scan rerun; the bulk endpoint does one
    # rerun at the very end after all findings are processed.
    rerun = rerun_checks and not in_bulk_run.get()

    saved_count = 0
    error_detail: str | None = None
    try:
        for ev_name, ev_payload in apply_album_changes_iter(
            album=album,
            library_dir=library_dir,
            suggestions=suggestions or {},
            track_suggestions=track_suggestions or {},
            cover_url=cover_url,
            dry_run=False,
            rerun_post_scan_checks=rerun,
        ):
            if ev_name == "file_done" and ev_payload.get("saved"):
                saved_count += 1
            elif ev_name == "error":
                error_detail = ev_payload.get("detail") or "apply failed"
    except Exception as e:
        _log.exception("apply_direct failed for album %s", album_id)
        return False, f"Apply failed: {e}"

    if error_detail:
        return False, error_detail
    return True, f"Wrote {saved_count} file{'s' if saved_count != 1 else ''}"
