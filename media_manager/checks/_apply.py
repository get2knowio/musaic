"""Direct-apply helper for check fix paths.

When a check's `fix()` decides it can resolve a finding without going
through the AI job queue (e.g. MusicBrainz returned a confident match),
it calls `apply_direct()` to write the tag / cover changes immediately.
Mirrors the housekeeping the streaming `/apply/stream` endpoint does:

  1. Download cover bytes if a cover_url was supplied.
  2. Call tags.apply_to_album for the actual write.
  3. Invalidate the thumb if cover changed.
  4. Re-scan the affected album and upsert into the cache.
  5. Run the post-scan checks so any findings the apply just satisfied
     auto-resolve immediately (the user's UI updates within a beat,
     same as after an AI-applied fix).

Lives in the `checks` subpackage to keep the import graph tidy: only the
checks need this; nothing else does. The streaming endpoint has its own
copy of the same dance because it also needs to emit per-step SSE events,
which we don't bother with here.
"""
from __future__ import annotations

import contextvars
import logging
from pathlib import Path
from typing import Any

log = logging.getLogger(__name__)


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
    the apply. Default True: appropriate for one-off fixes (the user
    expects findings to clear immediately). Set False during bulk runs —
    re-running ALL checks after EVERY single apply is enormously wasteful
    (3 checks × N findings = N×O(library) re-scans), and the bulk caller
    can do one final rerun at the end if desired.

    Failures get logged and surfaced as the message; we never raise to
    the check's fix() — it should always return a clean FixResult.
    """
    from .. import cache as cache_module
    from .. import config as config_module
    from .. import thumbs as thumbs_module
    from ..scanner import album_to_dict, scan_one_album
    from ..tags import (
        apply_to_album,
        download_cover,
        result_to_dict,
    )

    # 1. Resolve album from cache. We don't have ready access to a
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

    # 2. Cover download (if requested). Goes through the in-memory cache
    # populated by the verification step on AI cover suggestions; for MB
    # fixes the cache will be cold the first time, but the bytes get
    # cached on this fetch so any retry is instant.
    cover_bytes: bytes | None = None
    cover_mime: str | None = None
    if cover_url:
        downloaded = download_cover(cover_url)
        if downloaded is None:
            return False, f"Couldn't download cover from {cover_url}"
        cover_bytes, cover_mime = downloaded

    # 3. The actual write. apply_to_album normalizes JPEG and writes the
    # sidecar cover file as well as embedding into each FLAC.
    try:
        result = apply_to_album(
            album["path"],
            suggestions or {},
            cover_bytes=cover_bytes,
            cover_mime=cover_mime,
            track_suggestions=track_suggestions or {},
            dry_run=False,
        )
    except FileNotFoundError as e:
        return False, f"Album folder missing: {e}"
    except Exception as e:
        log.exception("apply_direct failed for album %s", album_id)
        return False, f"Apply failed: {e}"

    files = result_to_dict(result).get("files") or []
    art_changed = cover_bytes is not None or any(f.get("art_changed") for f in files)
    saved_count = sum(1 for f in files if f.get("saved"))

    # 4. Thumb invalidation when art changed.
    if art_changed:
        try:
            thumbs_module.delete_thumb(album_id)
        except Exception:
            log.exception("thumb invalidate failed for %s (continuing)", album_id)

    # 5. Re-scan + upsert so the cache reflects the new state.
    try:
        fresh = scan_one_album(album["path"])
        if fresh is None:
            cache_module.delete_album(album_id)
        else:
            cache_module.upsert_album(library_dir, album_to_dict(fresh))
    except Exception:
        log.exception("post-apply rescan failed for %s (continuing)", album_id)

    # 6. Trigger post-scan check rerun so the originating finding (and any
    # others the apply happened to satisfy) resolves on the spot. Bulk
    # callers (quick-fix-all) flip the `in_bulk_run` contextvar so we
    # skip these per-apply reruns; the bulk endpoint does one rerun at
    # the very end after all findings are processed.
    should_rerun = rerun_checks and not in_bulk_run.get()
    if should_rerun:
        try:
            from ..main import _run_post_scan_checks
            _run_post_scan_checks(library_dir)
        except Exception:
            log.exception("post-apply check rerun failed for %s (continuing)", album_id)

    return True, f"Wrote {saved_count} file{'s' if saved_count != 1 else ''}"
