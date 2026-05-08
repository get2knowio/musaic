"""Library config, scan, album browse, cover, tracks."""
from __future__ import annotations

import logging
import mimetypes
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from fastapi import APIRouter, HTTPException, Query, Request
from fastapi.responses import FileResponse, Response, StreamingResponse
from pydantic import BaseModel

from .. import cache as cache_module
from .. import config as config_module
from .. import thumbs as thumbs_module
from ..scanner import (
    album_to_dict,
    find_album_by_id,
    get_embedded_cover,
    incremental_scan,
)
from ..services.albums import (
    albums_for,
    invalidate_cache,
    resolve_album_dict,
    read_tracks,
    sort_albums,
)
from ..services.post_scan import run_post_scan_checks
from ..services.sse import sse_event

_log = logging.getLogger(__name__)
router = APIRouter()


class ConfigPayload(BaseModel):
    library_dir: str


class ConfigResponse(BaseModel):
    library_dir: str | None
    exists: bool


@router.get("/api/config", response_model=ConfigResponse)
def get_config() -> ConfigResponse:
    cfg = config_module.load_config()
    exists = bool(cfg.library_dir) and Path(cfg.library_dir).is_dir()
    return ConfigResponse(library_dir=cfg.library_dir, exists=exists)


@router.post("/api/config", response_model=ConfigResponse)
def update_config(payload: ConfigPayload) -> ConfigResponse:
    target = Path(payload.library_dir).expanduser()
    if not target.is_dir():
        raise HTTPException(status_code=400, detail=f"Not a directory: {target}")
    cfg = config_module.set_library_dir(str(target))
    invalidate_cache()
    return ConfigResponse(library_dir=cfg.library_dir, exists=True)


@router.get("/api/albums")
def list_albums(refresh: bool = Query(False, description="Drop the cache so the next scan rebuilds from disk")) -> dict[str, Any]:
    """Return the cached album list. Does NOT scan — call /api/scan/stream
    after to populate or refresh. Useful for the frontend to check the
    cache state on page load."""
    cfg = config_module.load_config()
    if not cfg.library_dir:
        raise HTTPException(status_code=409, detail="No library directory configured")
    if not Path(cfg.library_dir).is_dir():
        raise HTTPException(status_code=409, detail=f"Configured library directory does not exist: {cfg.library_dir}")
    if refresh:
        cache_module.drop_snapshot(cfg.library_dir)
    snap = cache_module.load_snapshot(cfg.library_dir)
    return {
        "library_dir": cfg.library_dir,
        "count": len(snap.albums),
        "scanned_at": snap.scanned_at,
        "albums": sort_albums(snap.albums),
    }


@router.get("/api/scan/stream")
def scan_stream(force: bool = Query(False, description="Ignore cache; re-read every album")) -> StreamingResponse:
    """Stream a library scan as Server-Sent Events.

    Events emitted (event name → payload):
      - `start`    : { library_dir, total, cached_count, force }
      - `album`    : an album dict, with extra `_status` ("cached" | "fresh")
      - `progress` : { scanned, total }
      - `removed`  : { id, path }
      - `done`     : { count, scanned_at }
      - `error`    : { detail }
    """
    cfg = config_module.load_config()
    if not cfg.library_dir:
        raise HTTPException(status_code=409, detail="No library directory configured")
    if not Path(cfg.library_dir).is_dir():
        raise HTTPException(status_code=409, detail=f"Configured library directory does not exist: {cfg.library_dir}")

    library_dir = cfg.library_dir

    def event_gen():
        # Background pool for thumbnail generation. Sized small to avoid disk
        # I/O contention; album events fire as soon as scanned (don't wait).
        thumb_pool = ThreadPoolExecutor(max_workers=4, thread_name_prefix="mm-thumb")
        try:
            cached_by_path: dict[str, dict[str, Any]] = {}
            if not force:
                snap = cache_module.load_snapshot(library_dir)
                cached_by_path = dict(snap.albums_by_path)

            collected: dict[str, dict[str, Any]] = {}
            removed: list[dict[str, Any]] = []
            total = 0
            scanned = 0
            started = False

            for status, payload in incremental_scan(library_dir, cached_by_path):
                if status == "discovered":
                    total = payload["total"]
                    yield sse_event("start", {
                        "library_dir": library_dir,
                        "total": total,
                        "cached_count": len(cached_by_path),
                        "force": force,
                    })
                    started = True
                    continue

                if status == "removed":
                    removed.append({"id": payload.get("id"), "path": payload.get("path")})
                    if payload.get("id"):
                        thumbs_module.delete_thumb(payload["id"])
                    yield sse_event("removed", {"id": payload.get("id"), "path": payload.get("path")})
                    continue

                # "cached" or "fresh" → an album dict
                album = dict(payload)
                album["_status"] = status
                collected[album["path"]] = album
                scanned += 1

                force_thumb = (status == "fresh")
                thumb_pool.submit(thumbs_module.ensure_thumb, album, force=force_thumb)

                yield sse_event("album", album)
                step = 1 if total < 200 else max(1, total // 100)
                if scanned == total or scanned % step == 0:
                    yield sse_event("progress", {"scanned": scanned, "total": total})

            if not started:
                yield sse_event("start", {
                    "library_dir": library_dir, "total": 0, "cached_count": 0, "force": force,
                })

            scanned_at = datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")
            new_snap = cache_module.LibrarySnapshot(
                library_dir=library_dir,
                scanned_at=scanned_at,
                albums_by_path={p: {k: v for k, v in a.items() if k != "_status"} for p, a in collected.items()},
            )
            cache_module.save_snapshot(new_snap)

            try:
                imported = cache_module.import_track_tag_sidecars(new_snap)
                if imported:
                    _log.info("Imported track tag sidecars for %d album(s)", imported)
            except Exception:
                _log.exception("track tag sidecar import failed")

            run_post_scan_checks(library_dir)

            yield sse_event("done", {
                "count": len(collected),
                "removed_count": len(removed),
                "scanned_at": scanned_at,
            })
        except Exception as e:
            yield sse_event("error", {"detail": str(e)})
        finally:
            thumb_pool.shutdown(wait=True, cancel_futures=False)

    return StreamingResponse(
        event_gen(),
        media_type="text/event-stream",
        headers={
            "Cache-Control": "no-cache, no-transform",
            "X-Accel-Buffering": "no",
            "Connection": "keep-alive",
        },
    )


@router.post("/api/scan/rescan")
def force_rescan() -> dict[str, Any]:
    """Drop the cache for the configured library so the next /api/scan/stream
    call does a full re-read. Returns immediately; the actual scan happens
    when the client opens the SSE stream."""
    cfg = config_module.load_config()
    if not cfg.library_dir:
        raise HTTPException(status_code=409, detail="No library directory configured")
    cache_module.drop_snapshot(cfg.library_dir)
    return {"status": "cache cleared", "library_dir": cfg.library_dir}


@router.get("/api/albums/{album_id}")
def get_album(album_id: str) -> dict[str, Any]:
    cfg = config_module.load_config()
    if not cfg.library_dir:
        raise HTTPException(status_code=409, detail="No library directory configured")
    for album in albums_for(cfg.library_dir):
        if album["id"] == album_id:
            return album
    found = find_album_by_id(cfg.library_dir, album_id)
    if found is None:
        raise HTTPException(status_code=404, detail="Album not found")
    return album_to_dict(found)


@router.get("/api/albums/{album_id}/cover")
def get_cover(
    album_id: str,
    request: Request,
    size: str = Query("thumb", description="thumb (default, 384px) or full (original)"),
) -> Response:
    """Serve cover art for an album.

    Default behaviour is `?size=thumb` — a pre-extracted 384px JPEG served from
    `~/.media_manager/thumbs/{id}.jpg`, with `Cache-Control` and ETag headers so
    browsers don't re-fetch on reload. Generated on-the-fly the first time it's
    requested if the scan didn't pre-build it.

    `?size=full` returns the original image — the on-disk cover.jpg if present
    (preferred, kernel sendfile), otherwise the largest embedded picture.
    """
    album = resolve_album_dict(album_id)

    if size == "full":
        disk = thumbs_module.get_disk_cover_path(album)
        if disk is not None:
            mime, _ = mimetypes.guess_type(str(disk))
            return FileResponse(str(disk), media_type=mime or "image/jpeg",
                                headers={"Cache-Control": "public, max-age=86400"})
        if album.get("has_embedded_cover"):
            result = get_embedded_cover(album.get("sample_track", ""))
            if result:
                data, mime = result
                return Response(content=data, media_type=mime,
                                headers={"Cache-Control": "public, max-age=86400"})
        raise HTTPException(status_code=404, detail="No cover art for this album")

    # Default: thumbnail
    path = thumbs_module.thumb_path(album_id)
    if not path.is_file():
        generated = thumbs_module.ensure_thumb(album)
        if generated is None:
            raise HTTPException(status_code=404, detail="No cover art for this album")
        path = generated

    etag = thumbs_module.thumb_etag(album_id)
    if etag and request.headers.get("if-none-match") == etag:
        return Response(status_code=304, headers={"ETag": etag})

    headers = {
        "Cache-Control": "public, max-age=86400",
    }
    if etag:
        headers["ETag"] = etag
    return FileResponse(str(path), media_type="image/jpeg", headers=headers)


@router.get("/api/albums/{album_id}/tracks")
def get_tracks(album_id: str) -> dict[str, Any]:
    album = resolve_album_dict(album_id)
    return {"album_id": album_id, "tracks": read_tracks(album["path"])}
