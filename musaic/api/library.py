"""Library config, scan, album browse, cover, tracks."""
from __future__ import annotations

import asyncio
import logging
import mimetypes
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
)
from ..services.albums import (
    albums_for,
    invalidate_cache,
    resolve_album_dict,
    read_tracks,
    sort_albums,
)
from ..services import scan as scan_service
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


@router.get("/api/scan/status")
def scan_status() -> dict[str, Any]:
    """Current scan state. Lets the UI detect an in-flight scan on page
    load (rather than relying on the SSE stream alone)."""
    return scan_service.get_state()


@router.get("/api/scan/stream")
async def scan_stream(force: bool = Query(False, description="Ignore cache; re-read every album")) -> StreamingResponse:
    """Stream a library scan as Server-Sent Events.

    The scan itself runs as a background asyncio task, so a client
    disconnect no longer halts the scan — late subscribers (or
    reconnects) catch up via the in-process event buffer.

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

    # Kick off a scan if none is running. If one IS running, the client
    # subscribes to it from the beginning of its event log (so a fresh
    # connect mid-scan replays "start", every "album" so far, etc.).
    if not scan_service.is_running():
        await scan_service.start_scan(cfg.library_dir, force=force)

    async def event_gen():
        try:
            async for _seq, name, payload in scan_service.subscribe(from_seq=0):
                yield sse_event(name, payload)
        except asyncio.CancelledError:
            # Client disconnected. The scan task keeps running in the
            # background — the next subscriber will catch up.
            return

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
async def force_rescan() -> dict[str, Any]:
    """Drop the cache for the configured library and kick off a fresh
    background scan. Returns immediately; clients observe progress via
    /api/scan/stream or /api/scan/status."""
    cfg = config_module.load_config()
    if not cfg.library_dir:
        raise HTTPException(status_code=409, detail="No library directory configured")
    cache_module.drop_snapshot(cfg.library_dir)
    state = await scan_service.start_scan(cfg.library_dir, force=True)
    return {"status": "scan started", "library_dir": cfg.library_dir, "scan": state}


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
    `~/.musaic/thumbs/{id}.jpg`, with `Cache-Control` and ETag headers so
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
