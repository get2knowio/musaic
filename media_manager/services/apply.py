"""Single mutation pipeline for album writes.

Both the streaming `/api/albums/{id}/apply/stream` endpoint and the
deterministic check-fix path (`checks/_apply.py`) consume the generator
here. The streaming endpoint forwards every yielded `(event, payload)`
to the SSE socket; the check-fix path drains the iterator without
forwarding and inspects the final outcome.

This is the consolidation called out in `docs/IMPROVEMENTS.md` as
Finding 4 / Simplification 2 — previously the two call sites had
divergent housekeeping (track-tag rename migration, cover.jpg sidecar
save, JPEG normalization), producing subtly different on-disk results.

Event sequence (omitting events whose preconditions don't fire):
  ("cover_downloading",  {url})
  ("cover_downloaded",   {bytes, mime, source_bytes, source_mime})
  ("start",              passthrough from apply_to_album_iter)
  ("file_started",       passthrough)
  ("file_done",          passthrough)
  ("file_renamed",       passthrough)
  ("done",               passthrough — apply_to_album_iter's final summary)
  ("cover_saved",        {path, filename})            # only when cover changed
  ("updated_album",      fresh album dict)            # or:
  ("removed_album",      {id, path})                  # if dir disappeared
  ("error",              {detail})                    # any fatal failure
"""
from __future__ import annotations

import logging
from typing import Any, Iterator

from .. import cache as cache_module
from .. import thumbs as thumbs_module
from ..scanner import album_to_dict, scan_one_album
from ..tags import (
    apply_to_album_iter,
    download_cover,
    normalize_to_jpeg,
    save_cover_to_album_dir,
)
from .post_scan import run_post_scan_checks

_log = logging.getLogger(__name__)


def apply_album_changes_iter(
    *,
    album: dict[str, Any],
    library_dir: str,
    suggestions: dict[str, str] | None = None,
    track_suggestions: dict[str, dict[str, str]] | None = None,
    cover_url: str | None = None,
    dry_run: bool = False,
    rerun_post_scan_checks: bool = True,
) -> Iterator[tuple[str, dict[str, Any]]]:
    """Apply changes to one album, yielding events for every side effect.

    `library_dir` should be the resolved library root — used by the
    upsert step so the album row lands at the right `library_path`.
    Pass `""` to skip the upsert (rare; only valid in tests).

    `rerun_post_scan_checks` controls whether the cheap post-scan
    checks run after a successful apply. The bulk quick-fix-all caller
    sets this False (and does one rerun at the end of the batch).
    """
    suggestions = suggestions or {}
    track_suggestions = track_suggestions or {}
    album_id = album["id"]
    album_path = album["path"]

    cover_bytes: bytes | None = None
    cover_mime: str | None = None

    try:
        # 1. Cover download (if requested) + normalize to JPEG q95.
        if cover_url:
            yield "cover_downloading", {"url": cover_url}
            downloaded = download_cover(cover_url)
            if downloaded is None:
                yield "error", {"detail": f"Failed to download cover from {cover_url}"}
                return
            raw_bytes, raw_mime = downloaded
            cover_bytes = normalize_to_jpeg(raw_bytes, quality=95)
            cover_mime = "image/jpeg"
            yield "cover_downloaded", {
                "source_bytes": len(raw_bytes),
                "source_mime": raw_mime,
                "bytes": len(cover_bytes),
                "mime": cover_mime,
            }

        # 2. Per-file write events.
        had_art_change = cover_bytes is not None
        had_renames = False
        for ev_name, ev_payload in apply_to_album_iter(
            album_path,
            suggestions,
            track_suggestions=track_suggestions,
            cover_bytes=cover_bytes,
            cover_mime=cover_mime,
            dry_run=dry_run,
        ):
            if ev_name == "file_done" and ev_payload.get("art_changed"):
                had_art_change = True
            # Track-tag rename migration: when a per-track tracknumber
            # change renames a file, follow that rename in the
            # track_tags table so the user's custom tags don't get
            # orphaned to the old filename.
            if ev_name == "file_renamed" and not dry_run:
                old_fn = ev_payload.get("old_filename")
                new_fn = ev_payload.get("new_filename")
                if old_fn and new_fn and old_fn != new_fn:
                    try:
                        cache_module.rename_track_filename(
                            album_id, old_fn, new_fn,
                        )
                        had_renames = True
                    except Exception:
                        _log.exception(
                            "Failed to migrate track tags %s → %s on album %s",
                            old_fn, new_fn, album_id,
                        )
            yield ev_name, ev_payload

        if dry_run:
            return

        # 3. cover.jpg sidecar so external tools (Plex, OS thumbnailer)
        #    pick up the new art without re-extracting from FLAC.
        if cover_bytes is not None:
            saved = save_cover_to_album_dir(album_path, cover_bytes)
            if saved is not None:
                yield "cover_saved", {"path": str(saved), "filename": saved.name}

        # 4. Thumb invalidation.
        if had_art_change:
            thumbs_module.delete_thumb(album_id)

        # 5. Track-tags sidecar resync after renames.
        if had_renames:
            try:
                track_tag_map = cache_module.get_album_track_tags(album_id)
                if track_tag_map:
                    from .. import track_tags_sidecar as _sidecar
                    _sidecar.write_sidecar(album_path, track_tag_map)
            except Exception:
                _log.exception(
                    "Failed to resync track-tag sidecar after rename for album %s",
                    album_id,
                )

        # 6. Targeted re-scan + upsert (don't drop the whole snapshot).
        fresh = scan_one_album(album_path)
        if fresh is None:
            cache_module.delete_album(album_id)
            yield "removed_album", {"id": album_id, "path": album_path}
        else:
            fresh_dict = album_to_dict(fresh)
            if library_dir:
                cache_module.upsert_album(library_dir, fresh_dict)
            yield "updated_album", fresh_dict

        # 7. Post-scan check rerun so any findings the apply just satisfied
        #    auto-resolve. Skipped during bulk runs (one batch rerun fires
        #    at the very end instead).
        if rerun_post_scan_checks and library_dir:
            run_post_scan_checks(library_dir)
    except Exception as e:
        _log.exception("apply_album_changes_iter failed for album %s", album_id)
        yield "error", {"detail": str(e)}
