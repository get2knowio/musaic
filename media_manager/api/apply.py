"""The streaming apply endpoint — accept AI-suggested metadata + cover
changes and write them to disk + cache, emitting per-file progress
events along the way."""
from __future__ import annotations

import logging
from typing import Any

from fastapi import APIRouter
from fastapi.responses import StreamingResponse
from pydantic import BaseModel

from .. import cache as cache_module
from .. import config as config_module
from .. import thumbs as thumbs_module
from ..scanner import album_to_dict, scan_one_album
from ..services.albums import resolve_album_dict
from ..services.post_scan import run_post_scan_checks
from ..services.sse import sse_event
from ..tags import (
    apply_to_album_iter,
    download_cover,
    normalize_to_jpeg,
    save_cover_to_album_dir,
)

_log = logging.getLogger(__name__)
router = APIRouter()


class ApplyPayload(BaseModel):
    suggestions: dict[str, str] = {}
    # Per-file overrides keyed by FLAC filename.
    # Allowed inner keys: "title", "artist", "tracknumber".
    track_suggestions: dict[str, dict[str, str]] = {}
    cover_url: str | None = None
    dry_run: bool = False


@router.post("/api/albums/{album_id}/apply/stream")
def apply_changes_stream(album_id: str, payload: ApplyPayload) -> StreamingResponse:
    """Stream an album apply as Server-Sent Events.

    Replaces the synchronous /apply endpoint for the new React frontend.
    Solves two problems at once:

      1. Per-file progress UX — the frontend gets a `file_started` /
         `file_done` event for each FLAC so it can show a marching spinner.
      2. The "library re-scans after every apply" flicker bug — instead of
         dropping the entire library cache, this endpoint rescans ONLY the
         affected album dir and emits an `updated_album` event with the
         fresh row. The frontend updates one tile in place.

    Event sequence:
      - `cover_downloading` { url }                         (only if cover_url set)
      - `cover_downloaded`  { bytes, mime }                 (only if cover_url set)
      - `start`             { album_path, file_count, ... }
      - `file_started`      { index, filename }
      - `file_done`         FileChange dict
      - …repeat per file…
      - `done`              { saved_count, art_changed_count, errors, ... }
      - `updated_album`     refreshed album dict (or `removed_album` if dir gone)
      - `error`             { detail }                      (on any failure)
    """
    album = resolve_album_dict(album_id)
    cfg = config_module.load_config()
    library_dir = cfg.library_dir or ""

    def event_gen():
        cover_bytes: bytes | None = None
        cover_mime: str | None = None
        try:
            # 1. Download cover (if requested) — emit progress events.
            if payload.cover_url:
                yield sse_event("cover_downloading", {"url": payload.cover_url})
                downloaded = download_cover(payload.cover_url)
                if downloaded is None:
                    yield sse_event("error", {"detail": f"Failed to download cover from {payload.cover_url}"})
                    return
                raw_bytes, raw_mime = downloaded
                # Normalize to JPEG quality 95 for embedded use + cover.jpg sidecar.
                # Cheaper per-FLAC payload than embedding a multi-MB PNG, and
                # universal compatibility. Lossy, but quality 95 is visually
                # lossless for album art at typical sizes.
                cover_bytes = normalize_to_jpeg(raw_bytes, quality=95)
                cover_mime = "image/jpeg"
                yield sse_event("cover_downloaded", {
                    "source_bytes": len(raw_bytes),
                    "source_mime": raw_mime,
                    "bytes": len(cover_bytes),
                    "mime": cover_mime,
                })

            # 2. Stream the per-file write events.
            had_art_change = cover_bytes is not None
            had_renames = False
            for ev_name, ev_payload in apply_to_album_iter(
                album["path"],
                payload.suggestions or {},
                track_suggestions=payload.track_suggestions or {},
                cover_bytes=cover_bytes,
                cover_mime=cover_mime,
                dry_run=payload.dry_run,
            ):
                if ev_name == "file_done" and ev_payload.get("art_changed"):
                    had_art_change = True
                # Track-tag rename migration: when a per-track tracknumber
                # change renames a file, follow that rename in the
                # track_tags table so the user's custom tags don't get
                # orphaned to the old filename.
                if ev_name == "file_renamed" and not payload.dry_run:
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
                yield sse_event(ev_name, ev_payload)

            if payload.dry_run:
                return

            # 3. If a new cover was supplied, also drop it as cover.jpg next
            #    to the FLAC files. Players that read sidecar art (Plex, the
            #    OS thumbnailer) pick this up; our scanner prefers the
            #    on-disk file over re-extracting from FLAC, which is faster.
            if cover_bytes is not None:
                saved = save_cover_to_album_dir(album["path"], cover_bytes)
                if saved is not None:
                    yield sse_event("cover_saved", {"path": str(saved), "filename": saved.name})

            # 4. Invalidate the thumb if art changed.
            if had_art_change:
                thumbs_module.delete_thumb(album_id)

            # 4b. If any file was renamed and the album has track tags,
            #     resync the sidecar so the on-disk JSON reflects the new
            #     filenames.
            if had_renames:
                try:
                    track_tag_map = cache_module.get_album_track_tags(album_id)
                    if track_tag_map:
                        from .. import track_tags_sidecar as _sidecar
                        _sidecar.write_sidecar(album["path"], track_tag_map)
                except Exception:
                    _log.exception(
                        "Failed to resync track-tag sidecar after rename for album %s",
                        album_id,
                    )

            # 5. Re-scan THIS ONE album, upsert into SQLite, emit updated_album.
            #    Critical: do NOT drop the whole library snapshot.
            fresh = scan_one_album(album["path"])
            if fresh is None:
                cache_module.delete_album(album_id)
                yield sse_event("removed_album", {"id": album_id, "path": album["path"]})
            else:
                fresh_dict = album_to_dict(fresh)
                if library_dir:
                    cache_module.upsert_album(library_dir, fresh_dict)
                yield sse_event("updated_album", fresh_dict)

            # 6. Re-run the cheap post-scan health checks so any findings the
            #    apply just fixed (missing tags, track gaps, no cover art)
            #    auto-resolve immediately.
            if library_dir:
                run_post_scan_checks(library_dir)
        except Exception as e:
            yield sse_event("error", {"detail": str(e)})

    return StreamingResponse(
        event_gen(),
        media_type="text/event-stream",
        headers={
            "Cache-Control": "no-cache, no-transform",
            "X-Accel-Buffering": "no",
            "Connection": "keep-alive",
        },
    )
