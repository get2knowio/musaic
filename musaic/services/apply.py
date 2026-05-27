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

_log = logging.getLogger(__name__)


def apply_album_changes_iter(
    *,
    album: dict[str, Any],
    library_dir: str,
    suggestions: dict[str, str] | None = None,
    track_suggestions: dict[str, dict[str, str]] | None = None,
    cover_url: str | None = None,
    cover_bytes: bytes | None = None,
    cover_mime: str | None = None,
    dry_run: bool = False,
) -> Iterator[tuple[str, dict[str, Any]]]:
    """Apply changes to one album, yielding events for every side effect.

    `library_dir` should be the resolved library root — used by the
    upsert step so the album row lands at the right `library_path`.
    Pass `""` to skip the upsert (rare; only valid in tests).

    Cover input is one of:
      - `cover_url`: download via requests, normalize to JPEG q95.
      - `cover_bytes` + `cover_mime`: bytes already in hand (manual
        upload from the UI). Still normalized to JPEG q95.
      - neither: leave existing cover alone.

    Bulk-run coalescing (one rerun at end-of-batch instead of N per
    apply) is now handled by the `_post_scan_rerun` subscriber's
    `in_bulk_run` check — no parameter needed at this layer.
    """
    suggestions = suggestions or {}
    track_suggestions = track_suggestions or {}
    album_id = album["id"]
    album_path = album["path"]

    if cover_url and cover_bytes is not None:
        yield "error", {
            "detail": "Pass cover_url OR cover_bytes — not both. (Caller bug.)",
        }
        return

    final_cover_bytes: bytes | None = None
    final_cover_mime: str | None = None

    try:
        # 1a. Cover download path (apply with a remote URL).
        if cover_url:
            yield "cover_downloading", {"url": cover_url}
            downloaded = download_cover(cover_url)
            if downloaded is None:
                yield "error", {"detail": f"Failed to download cover from {cover_url}"}
                return
            raw_bytes, raw_mime = downloaded
            final_cover_bytes = normalize_to_jpeg(raw_bytes, quality=95)
            final_cover_mime = "image/jpeg"
            yield "cover_downloaded", {
                "source_bytes": len(raw_bytes),
                "source_mime": raw_mime,
                "bytes": len(final_cover_bytes),
                "mime": final_cover_mime,
            }
        # 1b. Direct-bytes path (manual upload). Same JPEG normalization.
        elif cover_bytes is not None:
            raw_mime = cover_mime or "image/jpeg"
            final_cover_bytes = normalize_to_jpeg(cover_bytes, quality=95)
            final_cover_mime = "image/jpeg"
            yield "cover_downloaded", {
                "source_bytes": len(cover_bytes),
                "source_mime": raw_mime,
                "bytes": len(final_cover_bytes),
                "mime": final_cover_mime,
            }

        # 2. Per-file write events.
        had_art_change = final_cover_bytes is not None
        had_renames = False
        for ev_name, ev_payload in apply_to_album_iter(
            album_path,
            suggestions,
            track_suggestions=track_suggestions,
            cover_bytes=final_cover_bytes,
            cover_mime=final_cover_mime,
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
        if final_cover_bytes is not None:
            saved = save_cover_to_album_dir(album_path, final_cover_bytes)
            if saved is not None:
                yield "cover_saved", {"path": str(saved), "filename": saved.name}

        # 4. Thumb invalidation now handled by the _thumb_drop
        # subscriber via AlbumCoverChanged (published at the end of
        # the generator, after every other mutation has landed).

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
                # Re-read the row post-upsert so the event carries the
                # MERGED dict (scanner-owned columns from this scan +
                # cached MB anchor / sort variants / etc. from other
                # subsystems). Without this, the frontend's local album
                # state loses fields like `mb_release_group_mbid`,
                # leading to "IDENTIFIED" badge + "MusicBrainz: not yet
                # checked" inconsistency in the album sheet.
                merged = cache_module.get_album_by_id(album_id)
                if merged is not None:
                    fresh_dict = merged
            yield "updated_album", fresh_dict

        # 6b. Publish the mutation events; subscribers fan out to
        #     the resolver invalidate + thumb drop + scoped post-scan
        #     rerun + findings-stream notify. Phase 2 of move E
        #     replaced the prior explicit direct calls with this
        #     single publish-driven path.
        #
        #     Note on bulk runs: the per-apply rerun used to be gated
        #     on `rerun_post_scan_checks` so the bulk dispatcher could
        #     suppress N reruns in favor of one end-of-batch rerun.
        #     That suppression now lives in the `_post_scan_rerun`
        #     subscriber, which honors `checks._apply.in_bulk_run`.
        #     The events still publish — invalidation + notify are
        #     cheap and bulk-mode-safe — only the post-scan rerun
        #     defers to the bulk dispatcher's end-of-batch call.
        try:
            from .. import library_events
            library_events.publish(library_events.AlbumTagsChanged(
                album_id=album_id,
                fields=tuple(suggestions.keys()) if suggestions else (),
            ))
            if had_art_change:
                library_events.publish(
                    library_events.AlbumCoverChanged(album_id=album_id),
                )
        except Exception:
            _log.exception("library_events publish failed for apply %s", album_id)
    except Exception as e:
        _log.exception("apply_album_changes_iter failed for album %s", album_id)
        yield "error", {"detail": str(e)}
