"""FastAPI entry point for Media Manager."""
from __future__ import annotations

import argparse
import asyncio
import contextlib
import json
import logging
import mimetypes
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from concurrent.futures import ThreadPoolExecutor
from fastapi import FastAPI, File, HTTPException, Query, Request, UploadFile
from fastapi.responses import FileResponse, JSONResponse, Response, StreamingResponse
from fastapi.staticfiles import StaticFiles
from pydantic import BaseModel

from mutagen.flac import FLAC

from . import cache as cache_module
from . import config as config_module
from . import jobs as jobs_module
from . import mb_lookup
from . import thumbs as thumbs_module
from . import checks as checks_module
from .checks import runner as checks_runner
from .scanner import (
    album_to_dict,
    find_album_by_id,
    get_embedded_cover,
    incremental_scan,
    scan_albums_list,
    scan_one_album,
)
from .tags import (
    apply_to_album_iter,
    download_cover,
    normalize_to_jpeg,
    save_cover_to_album_dir,
)


STATIC_DIR = Path(__file__).parent / "static"

app = FastAPI(title="Media Manager", version="0.1.0")


def _sort_albums(albums: list[dict]) -> list[dict]:
    return sorted(
        albums,
        key=lambda a: (
            (a.get("album_artist") or a.get("artist") or "").lower(),
            (a.get("year") or ""),
            (a.get("title") or "").lower(),
        ),
    )


def _albums_for(library_dir: str, *, refresh: bool = False) -> list[dict]:
    """Return the cached album list. If `refresh`, drop the cache first
    (a fresh scan will be triggered by the next /api/scan/stream call).
    """
    if refresh:
        cache_module.drop_snapshot(library_dir)
    snap = cache_module.load_snapshot(library_dir)
    return _sort_albums(snap.albums)


def _invalidate_cache() -> None:
    """Remove cache for the currently-configured library."""
    cfg = config_module.load_config()
    if cfg.library_dir:
        cache_module.drop_snapshot(cfg.library_dir)


def _cached_album_dict(album_id: str) -> dict[str, Any] | None:
    """Look up an album by id from the persistent cache. Returns None if not
    found. Does NOT walk the disk."""
    cfg = config_module.load_config()
    if not cfg.library_dir:
        return None
    snap = cache_module.load_snapshot(cfg.library_dir)
    for album in snap.albums:
        if album.get("id") == album_id:
            return album
    return None


def _resolve_album_dict(album_id: str) -> dict[str, Any]:
    """Return the album dict by id. Raises 409 if no library configured,
    404 if not in cache (caller may need to scan first)."""
    cfg = config_module.load_config()
    if not cfg.library_dir:
        raise HTTPException(status_code=409, detail="No library directory configured")
    album = _cached_album_dict(album_id)
    if album is None:
        # Try a one-shot live lookup as a fallback (cache may be stale or empty)
        live = find_album_by_id(cfg.library_dir, album_id)
        if live is None:
            raise HTTPException(status_code=404, detail="Album not found (try rescanning the library)")
        return album_to_dict(live)
    return album


# ---------- API models ----------

class ConfigPayload(BaseModel):
    library_dir: str


class ConfigResponse(BaseModel):
    library_dir: str | None
    exists: bool


class ApplyPayload(BaseModel):
    suggestions: dict[str, str] = {}
    # Per-file overrides keyed by FLAC filename.
    # Allowed inner keys: "title", "artist", "tracknumber".
    track_suggestions: dict[str, dict[str, str]] = {}
    cover_url: str | None = None
    dry_run: bool = False


# ---------- API routes ----------

@app.get("/api/config", response_model=ConfigResponse)
def get_config() -> ConfigResponse:
    cfg = config_module.load_config()
    exists = bool(cfg.library_dir) and Path(cfg.library_dir).is_dir()
    return ConfigResponse(library_dir=cfg.library_dir, exists=exists)


@app.post("/api/config", response_model=ConfigResponse)
def update_config(payload: ConfigPayload) -> ConfigResponse:
    target = Path(payload.library_dir).expanduser()
    if not target.is_dir():
        raise HTTPException(status_code=400, detail=f"Not a directory: {target}")
    cfg = config_module.set_library_dir(str(target))
    _invalidate_cache()
    return ConfigResponse(library_dir=cfg.library_dir, exists=True)


@app.get("/api/albums")
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
        "albums": _sort_albums(snap.albums),
    }


def _sse_event(event: str, data: dict[str, Any]) -> str:
    return f"event: {event}\ndata: {json.dumps(data)}\n\n"


def _run_post_scan_checks(library_dir: str) -> None:
    """Run every check that opted in via runs_during_scan=True. Errors per
    check are logged but never raise out — a flaky check should never break
    the scan flow.

    Called from SYNC generator code (the SSE scan + apply endpoints) running
    on a FastAPI worker thread. Drives each check's coroutine on a fresh
    event loop with `asyncio.run`. **Don't call this from async context** —
    asyncio.run() raises inside a running event loop. Use
    `_run_post_scan_checks_async` instead from `async def` callers.
    """
    from .checks import REGISTRY as _check_registry
    from pathlib import Path as _Path
    library_path = str(_Path(library_dir).expanduser().resolve())
    for cls in _check_registry:
        if not cls.meta.runs_during_scan:
            continue
        try:
            asyncio.run(checks_runner.run_check(
                cls.meta.id,
                library_path=library_path,
                scope_kind="all",
            ))
        except Exception:
            logging.getLogger(__name__).exception(
                "post-scan check %s failed; continuing", cls.meta.id,
            )


async def _run_post_scan_checks_async(library_dir: str) -> None:
    """Async-context counterpart to `_run_post_scan_checks`. Drives each
    runs_during_scan check directly with `await` so it can be called from
    inside an async generator (the bulk quick-fix-all endpoint, in
    particular). Same error-swallowing semantics: per-check failures get
    logged but never propagate.
    """
    from .checks import REGISTRY as _check_registry
    from pathlib import Path as _Path
    library_path = str(_Path(library_dir).expanduser().resolve())
    for cls in _check_registry:
        if not cls.meta.runs_during_scan:
            continue
        try:
            await checks_runner.run_check(
                cls.meta.id,
                library_path=library_path,
                scope_kind="all",
            )
        except Exception:
            logging.getLogger(__name__).exception(
                "post-scan check %s failed; continuing", cls.meta.id,
            )


@app.get("/api/scan/stream")
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
                    yield _sse_event("start", {
                        "library_dir": library_dir,
                        "total": total,
                        "cached_count": len(cached_by_path),
                        "force": force,
                    })
                    started = True
                    continue

                if status == "removed":
                    removed.append({"id": payload.get("id"), "path": payload.get("path")})
                    # Garbage-collect the thumb file too.
                    if payload.get("id"):
                        thumbs_module.delete_thumb(payload["id"])
                    yield _sse_event("removed", {"id": payload.get("id"), "path": payload.get("path")})
                    continue

                # "cached" or "fresh" → an album dict
                album = dict(payload)
                album["_status"] = status
                collected[album["path"]] = album
                scanned += 1

                # Kick off thumbnail generation in the background. For "fresh"
                # albums we always regenerate (art may have changed); for
                # "cached" we skip if the thumb file already exists.
                force_thumb = (status == "fresh")
                thumb_pool.submit(thumbs_module.ensure_thumb, album, force=force_thumb)

                yield _sse_event("album", album)
                # Throttle progress events a bit — every album for small libs is fine,
                # but coalesce when total is large.
                step = 1 if total < 200 else max(1, total // 100)
                if scanned == total or scanned % step == 0:
                    yield _sse_event("progress", {"scanned": scanned, "total": total})

            if not started:
                # Library directory had no album dirs at all
                yield _sse_event("start", {
                    "library_dir": library_dir, "total": 0, "cached_count": 0, "force": force,
                })

            # Persist the new snapshot (drops removed entries automatically).
            scanned_at = datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")
            new_snap = cache_module.LibrarySnapshot(
                library_dir=library_dir,
                scanned_at=scanned_at,
                albums_by_path={p: {k: v for k, v in a.items() if k != "_status"} for p, a in collected.items()},
            )
            cache_module.save_snapshot(new_snap)

            # Import per-album track-tag sidecars for any album that doesn't
            # yet have track_tags rows in the DB. No-op once a session has
            # touched the DB for an album — the DB is authoritative from
            # then on.
            try:
                imported = cache_module.import_track_tag_sidecars(new_snap)
                if imported:
                    _log.info("Imported track tag sidecars for %d album(s)", imported)
            except Exception:
                _log.exception("track tag sidecar import failed")

            # Run the post-scan tail of cheap health checks. Errors per check
            # are logged but never break the scan stream — the user gets
            # their `done` event regardless.
            _run_post_scan_checks(library_dir)

            yield _sse_event("done", {
                "count": len(collected),
                "removed_count": len(removed),
                "scanned_at": scanned_at,
            })
        except Exception as e:
            yield _sse_event("error", {"detail": str(e)})
        finally:
            # Wait for in-flight thumb jobs to finish (best-effort), then close.
            thumb_pool.shutdown(wait=True, cancel_futures=False)

    return StreamingResponse(
        event_gen(),
        media_type="text/event-stream",
        headers={
            "Cache-Control": "no-cache, no-transform",
            "X-Accel-Buffering": "no",  # disable proxy buffering if any
            "Connection": "keep-alive",
        },
    )


@app.post("/api/scan/rescan")
def force_rescan() -> dict[str, Any]:
    """Drop the cache for the configured library so the next /api/scan/stream
    call does a full re-read. Returns immediately; the actual scan happens
    when the client opens the SSE stream."""
    cfg = config_module.load_config()
    if not cfg.library_dir:
        raise HTTPException(status_code=409, detail="No library directory configured")
    cache_module.drop_snapshot(cfg.library_dir)
    return {"status": "cache cleared", "library_dir": cfg.library_dir}


@app.get("/api/albums/{album_id}")
def get_album(album_id: str) -> dict[str, Any]:
    cfg = config_module.load_config()
    if not cfg.library_dir:
        raise HTTPException(status_code=409, detail="No library directory configured")
    for album in _albums_for(cfg.library_dir):
        if album["id"] == album_id:
            return album
    # cache miss — try a live lookup in case the cache is stale
    found = find_album_by_id(cfg.library_dir, album_id)
    if found is None:
        raise HTTPException(status_code=404, detail="Album not found")
    return album_to_dict(found)


@app.get("/api/albums/{album_id}/cover")
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
    album = _resolve_album_dict(album_id)

    if size == "full":
        # Prefer on-disk cover (cheap), fall back to embedded.
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

    # --- Default: thumbnail ---
    path = thumbs_module.thumb_path(album_id)
    if not path.is_file():
        # Lazy generation if scan didn't pre-build it (e.g. cache hit before
        # this album was thumbed, or album just added).
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


# ---------- Album tracks ----------

def _read_tracks(album_dir: str) -> list[dict[str, Any]]:
    """Read per-track Vorbis tags for every FLAC in the album dir.

    Includes the standard editing fields (title/artist/etc.) plus the
    audio-analysis fields (bpm, replaygain_*) that the detail panel
    surfaces. Missing fields come back as empty strings or None.
    """
    tracks: list[dict[str, Any]] = []
    album_path = Path(album_dir)
    if not album_path.is_dir():
        return tracks

    def first(tags_lookup: dict, key: str) -> str:
        # Vorbis comment keys are case-insensitive; mutagen lowercases them.
        v = tags_lookup.get(key) or tags_lookup.get(key.lower()) or []
        if not v:
            return ""
        val = v[0] if isinstance(v, list) else v
        return str(val)

    for entry in sorted(album_path.iterdir()):
        if not entry.is_file() or entry.suffix.lower() != ".flac" or entry.name.startswith("._"):
            continue
        try:
            audio = FLAC(str(entry))
            tags = audio.tags.as_dict() if audio.tags else {}
            bpm_str = first(tags, "bpm")
            try:
                bpm_val: float | None = float(bpm_str) if bpm_str else None
            except ValueError:
                bpm_val = None
            tracks.append({
                "filename": entry.name,
                "tracknumber": first(tags, "tracknumber"),
                "title": first(tags, "title"),
                "artist": first(tags, "artist"),
                "duration": round(audio.info.length, 1) if audio.info else None,
                # Audio analysis (None / "" if not yet computed)
                "bpm": bpm_val,
                "replaygain_track_gain": first(tags, "replaygain_track_gain") or None,
                "replaygain_track_peak": first(tags, "replaygain_track_peak") or None,
                "replaygain_album_gain": first(tags, "replaygain_album_gain") or None,
                "replaygain_album_peak": first(tags, "replaygain_album_peak") or None,
            })
        except Exception as e:
            tracks.append({"filename": entry.name, "error": str(e)})
    return tracks


@app.get("/api/albums/{album_id}/tracks")
def get_tracks(album_id: str) -> dict[str, Any]:
    album = _resolve_album_dict(album_id)
    return {"album_id": album_id, "tracks": _read_tracks(album["path"])}


# ---------- User-defined track tags (v9) ----------
#
# Endpoints for the user's own tagging taxonomy ("Cocktail Hour",
# "Christmas", "Instrumental", etc.) — distinct from the FLAC GENRE
# field. Stored per-track in the cache + a sidecar JSON in each album
# directory; album-level views compute the intersection.

class TrackTagPayload(BaseModel):
    tag: str

class AlbumTagsReplacePayload(BaseModel):
    """Wholesale replace of the album's track tags. Map keyed by track
    filename → list of tag strings."""
    track_tags: dict[str, list[str]]

class AlbumTagApplyPayload(BaseModel):
    """Apply a tag to the entire album. `filenames` may be omitted, in
    which case the server fans out to every FLAC in the album dir."""
    tag: str
    filenames: list[str] | None = None


def _list_album_filenames(album_dir: str) -> list[str]:
    """Filenames of FLAC tracks in the album dir, sorted. Used to fan
    out album-level tag operations across every track."""
    p = Path(album_dir)
    if not p.is_dir():
        return []
    return sorted(
        e.name for e in p.iterdir()
        if e.is_file() and e.suffix.lower() == ".flac" and not e.name.startswith("._")
    )


def _persist_track_tags_sidecar(album_id: str, album_path: str) -> None:
    """Re-serialize the album's tags to the dot-prefixed sidecar JSON.
    Best-effort: failures are logged but never raised, since the DB is
    authoritative for the running session."""
    try:
        from . import track_tags_sidecar as _sidecar
        track_tag_map = cache_module.get_album_track_tags(album_id)
        _sidecar.write_sidecar(album_path, track_tag_map)
    except Exception:
        _log.exception(
            "Failed to write track-tag sidecar for album %s", album_id
        )


def _track_count_for(album: dict[str, Any]) -> int:
    """Album track count for the intersection helper. Prefers the
    cached value but falls back to a directory listing for fresh
    albums that haven't been re-saved yet."""
    n = int(album.get("track_count") or 0)
    if n > 0:
        return n
    return len(_list_album_filenames(album["path"]))


@app.get("/api/albums/{album_id}/tags")
def get_album_tags(album_id: str) -> dict[str, Any]:
    """Return the album's full tag state.

    Shape:
        {
            "album_id": "...",
            "track_tags":   {filename: [tag, ...], ...},
            "album_tags":   [tag, ...]   # intersection — tags on every track
        }
    """
    album = _resolve_album_dict(album_id)
    track_tags = cache_module.get_album_track_tags(album_id)
    track_count = _track_count_for(album)
    album_tags = cache_module.get_album_tag_intersection(album_id, track_count)
    return {
        "album_id": album_id,
        "track_tags": track_tags,
        "album_tags": album_tags,
    }


@app.put("/api/albums/{album_id}/tags")
def replace_album_tags(album_id: str, payload: AlbumTagsReplacePayload) -> dict[str, Any]:
    """Replace the album's track tags wholesale. Used by the
    bulk-edit dialog (e.g. paste a JSON snapshot to migrate from
    flac-jacket). The sidecar is rewritten to match."""
    album = _resolve_album_dict(album_id)
    cache_module.replace_album_track_tags(album_id, payload.track_tags or {})
    _persist_track_tags_sidecar(album_id, album["path"])
    return get_album_tags(album_id)


@app.post("/api/albums/{album_id}/tags")
def add_album_tag(album_id: str, payload: AlbumTagApplyPayload) -> dict[str, Any]:
    """Apply `tag` to every track on the album (or the subset given in
    `filenames`). Idempotent — already-tagged tracks are left alone."""
    album = _resolve_album_dict(album_id)
    tag = (payload.tag or "").strip()
    if not tag:
        raise HTTPException(status_code=400, detail="tag is required")
    filenames = payload.filenames or _list_album_filenames(album["path"])
    if not filenames:
        raise HTTPException(status_code=409, detail="album has no tracks")
    cache_module.add_album_tag(album_id, tag, filenames)
    _persist_track_tags_sidecar(album_id, album["path"])
    return get_album_tags(album_id)


@app.delete("/api/albums/{album_id}/tags/{tag}")
def remove_album_tag(album_id: str, tag: str) -> dict[str, Any]:
    """Strip `tag` from every track on the album."""
    album = _resolve_album_dict(album_id)
    tag = (tag or "").strip()
    if not tag:
        raise HTTPException(status_code=400, detail="tag is required")
    cache_module.remove_album_tag(album_id, tag)
    _persist_track_tags_sidecar(album_id, album["path"])
    return get_album_tags(album_id)


@app.post("/api/albums/{album_id}/tracks/{filename}/tags")
def add_one_track_tag(album_id: str, filename: str, payload: TrackTagPayload) -> dict[str, Any]:
    """Add a tag to a single track. Filename is the FLAC's basename
    (URL-encode special characters at the call site)."""
    album = _resolve_album_dict(album_id)
    tag = (payload.tag or "").strip()
    if not tag:
        raise HTTPException(status_code=400, detail="tag is required")
    cache_module.add_track_tag(album_id, filename, tag)
    _persist_track_tags_sidecar(album_id, album["path"])
    return get_album_tags(album_id)


@app.delete("/api/albums/{album_id}/tracks/{filename}/tags/{tag}")
def remove_one_track_tag(album_id: str, filename: str, tag: str) -> dict[str, Any]:
    """Remove a tag from a single track."""
    album = _resolve_album_dict(album_id)
    tag = (tag or "").strip()
    if not tag:
        raise HTTPException(status_code=400, detail="tag is required")
    cache_module.remove_track_tag(album_id, filename, tag)
    _persist_track_tags_sidecar(album_id, album["path"])
    return get_album_tags(album_id)


@app.get("/api/tags")
def list_tags() -> dict[str, Any]:
    """All tags the user has ever applied, with usage counts. Powers
    the autocomplete dropdown in the tag editor."""
    return {"tags": cache_module.list_all_tags()}


@app.get("/api/tags/{tag}/tracks")
def tracks_for_tag(tag: str) -> dict[str, Any]:
    """Every (album_id, filename) pair carrying `tag`. The
    playlist-construction query."""
    tag = (tag or "").strip()
    if not tag:
        raise HTTPException(status_code=400, detail="tag is required")
    return {"tag": tag, "tracks": cache_module.find_tracks_with_tag(tag)}


# ---------- Tag vocabulary (v10) ----------
#
# The user-curated approved list of tags that powers the picker UIs in
# the album/track tag editors. See cache.py vocabulary helpers for the
# semantics; this layer is mostly thin REST + sidecar resync orchestration
# after any mutation that touches track_tags.

class VocabularyAddPayload(BaseModel):
    tag: str
    description: str = ""
    color: str | None = None
    sort_order: int | None = None

class VocabularyPatchPayload(BaseModel):
    description: str | None = None
    color: str | None = None
    sort_order: int | None = None

class VocabularyRenamePayload(BaseModel):
    new_tag: str

class VocabularyMergePayload(BaseModel):
    into_tag: str


def _resync_sidecars_for_albums(album_ids: list[str]) -> None:
    """Helper: after a vocabulary mutation that touched track_tags,
    rewrite the on-disk sidecar JSON for each affected album so the
    file reflects the new tag set. Best-effort; failures are logged."""
    if not album_ids:
        return
    pairs = cache_module.get_albums_for_resync(album_ids)
    if not pairs:
        return
    from . import track_tags_sidecar as _sidecar
    for entry in pairs:
        try:
            tag_map = cache_module.get_album_track_tags(entry["id"])
            _sidecar.write_sidecar(entry["path"], tag_map)
        except Exception:
            _log.exception(
                "Vocabulary mutation: failed to resync sidecar for album %s",
                entry["id"],
            )


@app.get("/api/vocabulary")
def list_vocabulary() -> dict[str, Any]:
    """The full vocabulary plus orphans (tags in track_tags but not in
    the vocabulary). The Settings panel renders both."""
    return {
        "vocabulary": cache_module.list_vocabulary(),
        "orphans": cache_module.find_orphan_tags(),
    }


@app.post("/api/vocabulary")
def add_vocabulary_entry(payload: VocabularyAddPayload) -> dict[str, Any]:
    tag = (payload.tag or "").strip()
    if not tag:
        raise HTTPException(status_code=400, detail="tag is required")
    cache_module.add_vocabulary_entry(
        tag,
        description=payload.description or "",
        color=payload.color,
        sort_order=payload.sort_order,
    )
    return list_vocabulary()


@app.patch("/api/vocabulary/{tag}")
def patch_vocabulary_entry(tag: str, payload: VocabularyPatchPayload) -> dict[str, Any]:
    tag = (tag or "").strip()
    if not tag:
        raise HTTPException(status_code=400, detail="tag is required")
    cache_module.update_vocabulary_entry(
        tag,
        description=payload.description,
        color=payload.color,
        sort_order=payload.sort_order,
    )
    return list_vocabulary()


@app.post("/api/vocabulary/{tag}/rename")
def rename_vocabulary_entry(tag: str, payload: VocabularyRenamePayload) -> dict[str, Any]:
    tag = (tag or "").strip()
    new_tag = (payload.new_tag or "").strip()
    if not tag or not new_tag:
        raise HTTPException(status_code=400, detail="tag and new_tag are required")
    try:
        result = cache_module.rename_vocabulary_entry(tag, new_tag)
    except ValueError as e:
        raise HTTPException(status_code=409, detail=str(e))
    _resync_sidecars_for_albums(result["albums"])
    return {**list_vocabulary(), "rename_result": result}


@app.post("/api/vocabulary/{tag}/merge")
def merge_vocabulary_entry(tag: str, payload: VocabularyMergePayload) -> dict[str, Any]:
    """Merge `tag` into `payload.into_tag`. Both must exist before the
    call (the destination is the survivor)."""
    tag = (tag or "").strip()
    into_tag = (payload.into_tag or "").strip()
    if not tag or not into_tag:
        raise HTTPException(status_code=400, detail="tag and into_tag are required")
    try:
        result = cache_module.merge_vocabulary_entries(tag, into_tag)
    except ValueError as e:
        raise HTTPException(status_code=400, detail=str(e))
    _resync_sidecars_for_albums(result["albums"])
    return {**list_vocabulary(), "merge_result": result}


@app.delete("/api/vocabulary/{tag}")
def delete_vocabulary_entry(
    tag: str,
    strip_from_tracks: bool = Query(False, description="Also delete every track_tags row carrying this tag"),
) -> dict[str, Any]:
    tag = (tag or "").strip()
    if not tag:
        raise HTTPException(status_code=400, detail="tag is required")
    try:
        result = cache_module.delete_vocabulary_entry(
            tag, strip_from_tracks=strip_from_tracks,
        )
    except ValueError as e:
        raise HTTPException(status_code=400, detail=str(e))
    if strip_from_tracks:
        _resync_sidecars_for_albums(result["albums"])
    return {**list_vocabulary(), "delete_result": result}


@app.post("/api/vocabulary/orphans/{tag}/promote")
def promote_orphan_tag(tag: str) -> dict[str, Any]:
    """Add an orphan tag to the vocabulary (no track_tags changes).
    Convenience over POST /api/vocabulary — preserves the exact tag
    string the user has already been using on tracks."""
    tag = (tag or "").strip()
    if not tag:
        raise HTTPException(status_code=400, detail="tag is required")
    cache_module.add_vocabulary_entry(tag)
    return list_vocabulary()


# ---------- AI endpoints ----------
#
# The synchronous /api/ai/suggest-metadata/{album_id} and /api/ai/suggest-
# cover/{album_id} endpoints used to live here as the original way to drive
# AI-assisted metadata/cover lookups. They've been fully replaced by the
# AI job queue (POST /api/ai/jobs + the SSE stream + the worker in this
# file) — the queue path handles MBID persistence, retry, parallelism, and
# pause/resume, none of which the sync endpoints did. They were removed
# along with their api.ts wrappers since the frontend stopped calling
# them; if a future caller needs synchronous AI suggestions, they should
# enqueue a job and poll, rather than reviving the sync path with its
# silent gaps (no _persist_ai_mbid, no MBID in the response shape).


# ---------- AI job queue ----------

_log = logging.getLogger(__name__)

# Bumped whenever any job's status changes — the SSE stream uses it as a
# cheap "something happened, re-check" trigger. asyncio.Event lets us avoid
# polling the DB on a fixed interval.
_jobs_changed = asyncio.Event()
# Set by the worker so external consumers can know the worker is alive.
_worker_running = False


def _notify_jobs_changed() -> None:
    """Signal the SSE stream + the worker that the jobs table changed."""
    _jobs_changed.set()


def _queue_state_dict() -> dict[str, Any]:
    """Snapshot of worker / queue settings + counts. Bundled into the jobs SSE
    payload so a single subscriber (`useJobsStream`) gets everything in one
    cache update — no second connection for queue state."""
    from . import settings as settings_module
    return {
        "paused": settings_module.ai_queue_paused(),
        "max_parallel": settings_module.ai_max_parallel(),
        "running": jobs_module.count_running(),
        "queued": jobs_module.count_queued(),
    }


class JobEnqueuePayload(BaseModel):
    album_id: str
    job_type: str  # "metadata" | "cover" | "metadata+cover"
    hint: str = ""


class ActedOnPayload(BaseModel):
    action: str  # "applied" | "discarded"


class SettingsPatchPayload(BaseModel):
    ai_max_parallel: int | None = None
    ai_queue_paused: bool | None = None
    # AcoustID API key (write-only — never echoed back). Empty string clears
    # the key. None means "leave it alone" (as with all PATCH fields).
    acoustid_api_key: str | None = None
    # FireCrawl API key — see settings.firecrawl_api_key for behavior.
    # Same write-only semantics as the AcoustID key.
    firecrawl_api_key: str | None = None
    # Snapshot policy — how many automatic backups to keep, and the
    # minimum interval between auto-snapshots in hours. See db_backup.py.
    snapshot_retention_count: int | None = None
    snapshot_min_interval_hours: float | None = None
    # Filename templates used by the filename-mismatch health check
    # and its rename apply path. Empty string clears (falls back to
    # default); None means leave alone.
    filename_template_single_artist: str | None = None
    filename_template_compilation: str | None = None
    # Album-directory templates — same rationale as filename
    # templates but for the album folder names.
    dir_template_single_artist: str | None = None
    dir_template_compilation: str | None = None


def _job_album_dict(album_id: str) -> dict[str, Any]:
    """Wrap the cached lookup with a 404 — used by enqueue & worker."""
    album = _cached_album_dict(album_id)
    if album is None:
        raise HTTPException(status_code=404, detail=f"Album {album_id!r} not in cache; rescan first")
    return album


@app.post("/api/ai/jobs")
def enqueue_job(payload: JobEnqueuePayload) -> dict[str, Any]:
    if payload.job_type not in jobs_module.JOB_TYPES:
        raise HTTPException(status_code=400, detail=f"Unknown job_type {payload.job_type!r}")
    album = _job_album_dict(payload.album_id)
    try:
        job = jobs_module.enqueue(
            album_id=album["id"],
            album_path=album.get("path", ""),
            album_title=album.get("title") or album.get("dir_name") or "",
            job_type=payload.job_type,
            hint=payload.hint,
        )
    except jobs_module.JobConflictError as e:
        # 409 with the existing job id, so the frontend can offer "view it"
        raise HTTPException(
            status_code=409,
            detail={
                "error": "active_job_exists",
                "message": str(e),
                "existing_job_id": e.existing_job_id,
                "existing_status": e.status,
            },
        )
    _notify_jobs_changed()
    return jobs_module.job_to_dict(job)


@app.get("/api/ai/jobs")
def list_jobs(status: str | None = None) -> dict[str, Any]:
    statuses = status.split(",") if status else None
    rows = jobs_module.list_jobs(statuses=statuses)
    return {"jobs": [jobs_module.job_to_dict(j) for j in rows]}


# IMPORTANT: This route MUST be registered before /api/ai/jobs/{job_id}.
# FastAPI matches routes in registration order — without this ordering,
# `/api/ai/jobs/stream` matches the parameterized route with job_id="stream"
# and returns 404 (no such job).
@app.get("/api/ai/jobs/stream")
async def jobs_stream() -> StreamingResponse:
    """SSE stream of job-state changes. Emits a `jobs` event whenever any
    job is enqueued / status-changed / deleted, with the current full list
    in the payload. Cheap snapshot — list is bounded to the last 200."""
    async def event_gen():
        # Initial snapshot so the client doesn't have to call list_jobs first.
        snapshot = [jobs_module.job_to_dict(j) for j in jobs_module.list_jobs()]
        yield _sse_event("jobs", {
            "jobs": snapshot,
            "queue_state": _queue_state_dict(),
        })
        try:
            while True:
                # Block until something changes (or 30s heartbeat for proxies).
                try:
                    await asyncio.wait_for(_jobs_changed.wait(), timeout=30.0)
                    _jobs_changed.clear()
                    snapshot = [jobs_module.job_to_dict(j) for j in jobs_module.list_jobs()]
                    yield _sse_event("jobs", {
                        "jobs": snapshot,
                        "queue_state": _queue_state_dict(),
                    })
                except asyncio.TimeoutError:
                    # Heartbeat — keeps connections alive through proxies.
                    yield ": ping\n\n"
        except asyncio.CancelledError:
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


@app.get("/api/ai/jobs/active/{album_id}")
def active_job_for_album(album_id: str) -> dict[str, Any] | None:
    """Used by the detail sheet on mount — find the most recent unacted-on
    job for this album so its result can be displayed inline."""
    job = jobs_module.get_active_job_for_album(album_id)
    return jobs_module.job_to_dict(job) if job else None


@app.get("/api/ai/jobs/{job_id}")
def get_job(job_id: str) -> dict[str, Any]:
    job = jobs_module.get_job(job_id)
    if job is None:
        raise HTTPException(status_code=404, detail="Job not found")
    return jobs_module.job_to_dict(job)


@app.post("/api/ai/jobs/{job_id}/acted-on")
def mark_job_acted_on(job_id: str, payload: ActedOnPayload) -> dict[str, Any]:
    job = jobs_module.get_job(job_id)
    if job is None:
        raise HTTPException(status_code=404, detail="Job not found")
    if job.status != "completed":
        raise HTTPException(
            status_code=409,
            detail=f"Can only mark a completed job as acted-on (current status: {job.status})",
        )
    if payload.action not in ("applied", "discarded"):
        raise HTTPException(status_code=400, detail="action must be 'applied' or 'discarded'")
    jobs_module.mark_acted_on(job_id, payload.action)
    # Stamp the album as "AI metadata reviewed" so the UI can show it
    # and the missing-mbid / canonical-mismatch checks know not to
    # keep nudging the user about an album they already weighed in on.
    # Cover-only jobs don't count — they don't tell us anything about
    # the metadata. Both "applied" and "discarded" actions count: the
    # user looked at AI's suggestion and made a decision either way.
    if job.job_type in ("metadata", "metadata+cover"):
        try:
            cache_module.mark_metadata_ai_acted_on(job.album_id)
        except Exception:
            _log.exception(
                "Couldn't stamp metadata_ai_acted_on_at for album %s",
                job.album_id,
            )
    _notify_jobs_changed()
    return jobs_module.job_to_dict(jobs_module.get_job(job_id))  # fresh state


@app.post("/api/ai/jobs/{job_id}/retry")
def retry_job(job_id: str) -> dict[str, Any]:
    """Retry a failed job. Resets it to 'queued' and bumps created_at so it
    sorts at the bottom of the queue. Resuming the queue is the user's
    responsibility (we keep the paused flag so a quick retry cluster doesn't
    accidentally re-pause on the next failure)."""
    job = jobs_module.get_job(job_id)
    if job is None:
        raise HTTPException(status_code=404, detail="Job not found")
    if job.status != "failed":
        raise HTTPException(
            status_code=409,
            detail=f"Can only retry a failed job (current status: {job.status})",
        )
    updated = jobs_module.retry_failed(job_id)
    if updated is None:
        # Race: status flipped between the two reads. Fail the request rather
        # than silently doing nothing.
        raise HTTPException(status_code=409, detail="Job state changed; refresh and try again")
    _notify_jobs_changed()
    return jobs_module.job_to_dict(updated)


@app.post("/api/ai/queue/resume")
def resume_queue() -> dict[str, Any]:
    """Clear the auto-paused flag so the worker resumes dispatching."""
    from . import settings as settings_module
    settings_module.set_ai_queue_paused(False)
    _notify_jobs_changed()
    return {"queue_state": _queue_state_dict()}


@app.get("/api/settings")
def get_settings() -> dict[str, Any]:
    """All persistent settings, with defaults filled in."""
    from . import settings as settings_module
    return settings_module.all_settings()


@app.patch("/api/settings")
def patch_settings(payload: SettingsPatchPayload) -> dict[str, Any]:
    """Update one or more settings. Only the fields present in the payload
    are written; absent fields are left alone."""
    from . import settings as settings_module
    if payload.ai_max_parallel is not None:
        if payload.ai_max_parallel < 1 or payload.ai_max_parallel > 8:
            raise HTTPException(
                status_code=400,
                detail="ai_max_parallel must be between 1 and 8",
            )
        settings_module.set_ai_max_parallel(payload.ai_max_parallel)
    if payload.ai_queue_paused is not None:
        settings_module.set_ai_queue_paused(payload.ai_queue_paused)
    if payload.acoustid_api_key is not None:
        settings_module.set_acoustid_api_key(payload.acoustid_api_key)
    if payload.firecrawl_api_key is not None:
        settings_module.set_firecrawl_api_key(payload.firecrawl_api_key)
    if payload.snapshot_retention_count is not None:
        if payload.snapshot_retention_count < 0 or payload.snapshot_retention_count > 100:
            raise HTTPException(
                status_code=400,
                detail="snapshot_retention_count must be between 0 and 100",
            )
        settings_module.set_snapshot_retention(payload.snapshot_retention_count)
    if payload.snapshot_min_interval_hours is not None:
        if payload.snapshot_min_interval_hours < 0:
            raise HTTPException(
                status_code=400,
                detail="snapshot_min_interval_hours must be >= 0",
            )
        settings_module.set_snapshot_min_interval_hours(payload.snapshot_min_interval_hours)
    if payload.filename_template_single_artist is not None:
        settings_module.set_filename_template_single_artist(
            payload.filename_template_single_artist,
        )
    if payload.filename_template_compilation is not None:
        settings_module.set_filename_template_compilation(
            payload.filename_template_compilation,
        )
    if payload.dir_template_single_artist is not None:
        settings_module.set_dir_template_single_artist(
            payload.dir_template_single_artist,
        )
    if payload.dir_template_compilation is not None:
        settings_module.set_dir_template_compilation(
            payload.dir_template_compilation,
        )
    # Wake the worker so a max_parallel bump (or unpause) takes effect now.
    _notify_jobs_changed()
    return settings_module.all_settings()


# ---------- DB backup / restore + snapshots ----------
#
# The library DB at ~/.media_manager/library.db carries data that's
# expensive to recover after a wipe (paid AI-driven MBID anchors, paid
# AcoustID fingerprints, the user's tag taxonomy). We expose:
#
#   GET    /api/db/info                      summary stats
#   GET    /api/db/backup                    download a fresh snapshot
#   POST   /api/db/restore                   restore from uploaded file
#   GET    /api/db/snapshots                 list existing on-disk snapshots
#   POST   /api/db/snapshots                 take a manual snapshot now
#   DELETE /api/db/snapshots/{name}          remove one snapshot
#   POST   /api/db/snapshots/{name}/restore  restore from one snapshot
#
# Snapshots are auto-taken on app startup if the configured interval
# has elapsed; retention is enforced at create time.


@app.get("/api/db/info")
def db_info() -> dict[str, Any]:
    from . import db_backup
    return db_backup.db_info()


@app.get("/api/db/backup")
def db_backup_download() -> FileResponse:
    """Stream a consistent snapshot of the live DB as a download.
    Used for off-machine backups (e.g. dropping the file in iCloud /
    Time Machine / a USB stick). Doesn't touch the snapshot dir."""
    from . import db_backup
    from starlette.background import BackgroundTask
    path = db_backup.make_backup()
    filename = db_backup.suggested_backup_filename()
    # Clean up the temp file after FastAPI has streamed it.
    return FileResponse(
        str(path),
        media_type="application/octet-stream",
        filename=filename,
        background=BackgroundTask(lambda: path.unlink(missing_ok=True)),
    )


@app.post("/api/db/restore")
async def db_restore(file: UploadFile = File(...)) -> dict[str, Any]:
    """Restore the live DB from an uploaded backup file. Destructive:
    the existing DB is moved aside as library.db.bak-{timestamp} but
    the in-memory state of any in-flight requests will be stale until
    a refresh."""
    from . import db_backup
    import tempfile, os as _os
    fd, tmp_path = tempfile.mkstemp(prefix="mm-restore-", suffix=".db")
    try:
        try:
            while True:
                chunk = await file.read(1024 * 1024)
                if not chunk:
                    break
                _os.write(fd, chunk)
        finally:
            _os.close(fd)
        try:
            result = db_backup.restore_from(tmp_path)
        except db_backup.RestoreError as e:
            raise HTTPException(status_code=400, detail=str(e))
        return result
    finally:
        try:
            _os.unlink(tmp_path)
        except OSError:
            pass


@app.get("/api/db/snapshots")
def list_snapshots() -> dict[str, Any]:
    from . import db_backup
    return {"snapshots": db_backup.list_snapshots()}


@app.post("/api/db/snapshots")
def create_snapshot() -> dict[str, Any]:
    """Take a manual snapshot now, bypassing the interval gate.
    Retention is still enforced (older snapshots may get pruned)."""
    from . import db_backup, settings as settings_module
    retention = settings_module.snapshot_retention()
    if retention <= 0:
        raise HTTPException(
            status_code=409,
            detail="Snapshotting is disabled (retention count is 0). "
                   "Set snapshot_retention_count > 0 in Settings first.",
        )
    return db_backup.create_snapshot(retention=retention)


@app.delete("/api/db/snapshots/{name}")
def delete_snapshot(name: str) -> dict[str, Any]:
    from . import db_backup
    try:
        deleted = db_backup.delete_snapshot(name)
    except db_backup.RestoreError as e:
        raise HTTPException(status_code=400, detail=str(e))
    return {"deleted": deleted, "name": name}


@app.post("/api/db/snapshots/{name}/restore")
def restore_snapshot(name: str) -> dict[str, Any]:
    from . import db_backup
    try:
        return db_backup.restore_from_snapshot(name)
    except db_backup.RestoreError as e:
        raise HTTPException(status_code=400, detail=str(e))


@app.delete("/api/ai/jobs/{job_id}")
def delete_job(job_id: str) -> dict[str, Any]:
    # We can't actually cancel a job mid-flight — the SDK call is a blocking
    # subprocess we don't have a handle into — so refuse to delete it. The
    # row would otherwise vanish while the worker is still running, leading
    # to silent state divergence (and a worker that tries to mark_completed
    # on a row that no longer exists).
    job = jobs_module.get_job(job_id)
    if job is None:
        raise HTTPException(status_code=404, detail="Job not found")
    if job.status == "running":
        raise HTTPException(
            status_code=409,
            detail="Can't delete a job while it's running. Wait for it to finish.",
        )
    deleted = jobs_module.delete_job(job_id)
    if not deleted:
        raise HTTPException(status_code=404, detail="Job not found")
    _notify_jobs_changed()
    return {"deleted": True, "id": job_id}


@app.delete("/api/ai/jobs")
def bulk_delete_jobs(status: str = Query(..., description="comma-separated statuses to delete (e.g. 'acted_on,failed')")) -> dict[str, Any]:
    statuses = [s.strip() for s in status.split(",") if s.strip()]
    if not statuses:
        raise HTTPException(status_code=400, detail="status query param is required")
    invalid = [s for s in statuses if s not in jobs_module.STATUSES]
    if invalid:
        raise HTTPException(status_code=400, detail=f"Unknown status: {invalid}")
    if "running" in statuses:
        raise HTTPException(
            status_code=409,
            detail="Bulk delete cannot include 'running' jobs. Wait for them to finish.",
        )
    n = jobs_module.delete_jobs_by_status(statuses)
    if n > 0:
        _notify_jobs_changed()
    return {"deleted_count": n, "statuses": statuses}


# ---------- Health checks ----------
#
# Routing note: the SSE stream endpoint MUST come before any
# /api/checks/{id} parameterized route, same lesson we learned with the AI
# jobs stream — FastAPI matches in registration order, and "stream" would
# otherwise be eaten as a check_id. Same goes for /api/checks/findings,
# which would match {id}=findings.


class RunCheckPayload(BaseModel):
    scope: str = "all"  # "all" | "incremental" | "albums"
    album_ids: list[str] | None = None


class IgnoreFindingPayload(BaseModel):
    reason: str | None = None


def _library_path_for_checks() -> str:
    """Resolve the configured library directory; 409 if absent."""
    cfg = config_module.load_config()
    if not cfg.library_dir:
        raise HTTPException(status_code=409, detail="No library configured")
    return str(Path(cfg.library_dir).expanduser().resolve())


@app.get("/api/checks/stream")
async def checks_stream() -> StreamingResponse:
    """SSE stream of check / finding state changes. Emits a single 'state'
    event per change, carrying the full check-summary list. The frontend
    cache is small enough that diffing isn't worth the complexity."""
    async def event_gen():
        # Initial snapshot.
        yield _sse_event("state", {"checks": checks_runner.list_checks_with_summary()})
        ev = checks_runner.changed_event()
        try:
            while True:
                try:
                    await asyncio.wait_for(ev.wait(), timeout=30.0)
                    ev.clear()
                    yield _sse_event("state", {"checks": checks_runner.list_checks_with_summary()})
                except asyncio.TimeoutError:
                    yield ": ping\n\n"
        except asyncio.CancelledError:
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


@app.get("/api/checks/findings")
def list_all_findings(
    album_id: str | None = Query(None, description="filter to one album"),
    check_id: str | None = Query(None, description="filter to one check"),
    include_ignored: bool = Query(False),
    include_resolved: bool = Query(False),
) -> dict[str, Any]:
    """Open findings across checks. The detail-sheet 'Findings' section
    calls this with ?album_id= ; the Health detail page calls it with
    ?check_id= ."""
    return {
        "findings": checks_runner.list_findings(
            check_id=check_id,
            album_id=album_id,
            include_ignored=include_ignored,
            include_resolved=include_resolved,
        ),
    }


@app.get("/api/checks")
def list_checks() -> dict[str, Any]:
    """All registered checks + last-run summary + open finding counts.
    Drives the Health page's index-card grid."""
    return {"checks": checks_runner.list_checks_with_summary()}


@app.get("/api/checks/{check_id}")
def get_check(check_id: str) -> dict[str, Any]:
    out = checks_runner.get_check_with_runs(check_id)
    if out is None:
        raise HTTPException(status_code=404, detail=f"Unknown check_id: {check_id!r}")
    return out


@app.post("/api/checks/{check_id}/run")
async def run_check_now(check_id: str, payload: RunCheckPayload) -> dict[str, Any]:
    """Kick off a check. Returns the final run row when the check finishes.

    For the cheap checks Phase 2 ships, runs complete in well under a second
    so this is fine as a sync request. Once the heavy A1 (FLAC integrity)
    check lands, we'll likely change this to fire-and-forget + SSE updates.
    """
    if payload.scope not in ("all", "incremental", "albums"):
        raise HTTPException(status_code=400, detail="scope must be 'all'|'incremental'|'albums'")
    library_path = _library_path_for_checks()
    try:
        run = await checks_runner.run_check(
            check_id,
            library_path=library_path,
            scope_kind=payload.scope,
            album_ids=payload.album_ids,
        )
    except ValueError as e:
        raise HTTPException(status_code=400, detail=str(e))
    return run


@app.post("/api/checks/{check_id}/findings/{finding_id}/ignore")
def ignore_finding(check_id: str, finding_id: int, payload: IgnoreFindingPayload) -> dict[str, Any]:
    finding = checks_runner.get_finding(finding_id)
    if finding is None or finding["check_id"] != check_id:
        raise HTTPException(status_code=404, detail="Finding not found")
    updated = checks_runner.ignore_finding(finding_id, reason=payload.reason)
    return updated  # type: ignore[return-value]


@app.post("/api/checks/{check_id}/findings/{finding_id}/unignore")
def unignore_finding(check_id: str, finding_id: int) -> dict[str, Any]:
    finding = checks_runner.get_finding(finding_id)
    if finding is None or finding["check_id"] != check_id:
        raise HTTPException(status_code=404, detail="Finding not found")
    updated = checks_runner.unignore_finding(finding_id)
    return updated  # type: ignore[return-value]


class FixFindingPayload(BaseModel):
    """Optional body for the fix-finding endpoint. The hint is free-form
    text the user typed in the finding-row Fix dialog (mirrors the
    existing detail-sheet AI hint dialog). Forwarded to the check's
    fix() and, for AI-job checks, plumbed into the enqueued job's
    `hint` field. Absent body = no hint = pre-existing default behavior."""
    hint: str = ""


@app.post("/api/checks/{check_id}/findings/{finding_id}/fix")
def fix_finding(
    check_id: str,
    finding_id: int,
    payload: FixFindingPayload | None = None,
) -> dict[str, Any]:
    finding = checks_runner.get_finding(finding_id)
    if finding is None or finding["check_id"] != check_id:
        raise HTTPException(status_code=404, detail="Finding not found")
    user_hint = (payload.hint if payload else "") or ""
    try:
        result = checks_runner.fix_finding(finding_id, user_hint=user_hint)
    except ValueError as e:
        raise HTTPException(status_code=400, detail=str(e))
    return {
        "ok":      result.ok,
        "message": result.message,
        "job_id":  result.job_id,
    }


class DeleteDuplicateFilePayload(BaseModel):
    filename: str


@app.get("/api/checks/duplicate-tracks/findings/{finding_id}/canonical")
def duplicate_finding_canonical(finding_id: int) -> dict[str, Any]:
    """Look up MusicBrainz's canonical title + artist for a duplicate
    cluster's recording_mbid. Powers the resolve dialog's "MB says: …"
    line, which lets the user disambiguate which mistagged file is which.

    Lazy-loaded (this endpoint hits MB live, the check itself doesn't) so
    the duplicate-tracks check stays fast — most clusters never have
    their dialog opened. Recordings are immutable, so a single lookup
    per MBID per server lifetime is sufficient (cached via
    mb_lookup.get_recording_canonical's lru_cache).

    Returns:
      {title, artist}                         — MB found it
      {title: null, artist: null, reason: …}  — no MBID, MB miss, or HTTP error
    """
    finding = checks_runner.get_finding(finding_id)
    if finding is None or finding["check_id"] != "duplicate-tracks":
        raise HTTPException(status_code=404, detail="Finding not found")
    detail = finding.get("detail") or {}
    mbid = detail.get("recording_mbid")
    if not mbid:
        # Cluster has no MBID (AcoustID couldn't identify any of the files
        # it grouped together). Tell the dialog to render without the MB
        # line rather than failing the request.
        return {
            "title": None,
            "artist": None,
            "mbid": None,
            "reason": "no recording MBID for this cluster",
        }
    canonical = mb_lookup.get_recording_canonical(mbid)
    if canonical is None:
        return {
            "title": None,
            "artist": None,
            "mbid": mbid,
            "reason": "MusicBrainz didn't return a recording for that MBID",
        }
    return {
        "title": canonical["title"],
        "artist": canonical["artist"],
        "mbid": mbid,
        "reason": None,
    }


class ApplyClusterSortPayload(BaseModel):
    """Sort string the user picked from the inconsistent-artist-sort
    Resolve dialog. Validated minimally — must be non-empty, max 200
    chars (room for unusually-long band names without letting an
    accidental whole-paragraph paste through). The check itself
    decides which albums get the value via the finding's stored
    cluster member list."""
    sort_value: str


@app.post("/api/checks/inconsistent-artist-sort/findings/{finding_id}/apply-sort")
def apply_inconsistent_artist_sort(
    finding_id: int,
    payload: ApplyClusterSortPayload,
) -> dict[str, Any]:
    """Custom action for the inconsistent-artist-sort check: bulk-write
    the user's chosen sort value to album_artist_sort across every
    album in the finding's cluster. Goes through apply_direct so the
    actual FLAC tags get rewritten, not just our cache."""
    from .checks import inconsistent_artist_sort
    finding = checks_runner.get_finding(finding_id)
    if finding is None or finding["check_id"] != "inconsistent-artist-sort":
        raise HTTPException(status_code=404, detail="Finding not found")
    sort_value = (payload.sort_value or "").strip()
    if not sort_value:
        raise HTTPException(status_code=400, detail="sort_value is required")
    if len(sort_value) > 200:
        raise HTTPException(status_code=400, detail="sort_value too long (max 200 chars)")
    detail = finding.get("detail") or {}
    members = detail.get("members") or []
    if not members:
        raise HTTPException(status_code=400, detail="Finding has no cluster members")

    # Members may be in the v11+ shape (with "kind") or the legacy v10
    # shape (album-only, fields named "id"/"album_artist_sort"). Adapt
    # legacy entries on the fly so we don't have to re-run the check
    # before existing findings can be resolved.
    normalized = [_normalize_cluster_member(m) for m in members]
    normalized = [m for m in normalized if m and m.get("album_id")]
    if not normalized:
        raise HTTPException(status_code=400, detail="Cluster members missing album_id")

    applied = inconsistent_artist_sort.apply_cluster_sort(
        members=normalized, sort_value=sort_value,
    )
    distinct_albums = len({m["album_id"] for m in normalized})
    return {
        "ok":             applied > 0,
        "applied":        applied,
        "cluster_size":   len(normalized),
        "distinct_albums": distinct_albums,
        "sort_value":     sort_value,
        "message":        (
            f"Wrote sort={sort_value!r} to {applied} of "
            f"{distinct_albums} album(s) in the cluster."
        ),
    }


def _normalize_cluster_member(m: dict) -> dict | None:
    """Coerce a cluster-member dict into the v11+ shape regardless of
    whether it came from a fresh run (already correct) or a legacy
    finding stored under the v10 schema. Returns None for entries
    that are too malformed to act on."""
    if not isinstance(m, dict):
        return None
    if "kind" in m:
        # Already v11+. Trust the shape but coerce filenames into a
        # list (in case it's null / scalar / weird).
        if m.get("kind") == "track":
            fns = m.get("filenames") or []
            if not isinstance(fns, list):
                fns = [fns] if fns else []
            return {**m, "filenames": [str(x) for x in fns if x]}
        return m
    # Legacy v10: album-kind entry with field names "id" /
    # "album_artist" / "album_artist_sort".
    album_id = m.get("id") or m.get("album_id")
    if not album_id:
        return None
    return {
        "kind":         "album",
        "album_id":     album_id,
        "dir_name":     m.get("dir_name"),
        "album_artist": m.get("album_artist"),
        "sort":         m.get("album_artist_sort") or "",
    }


class FilenameRenameEntry(BaseModel):
    """One file in the rename plan. `old` and `new` are basenames
    (no directory component) — the apply path joins them onto the
    album dir."""
    old: str
    new: str


class DirRenameEntry(BaseModel):
    """Optional album-directory rename. When present, runs FIRST so
    file renames execute inside the new dir."""
    old: str
    new: str


class FilenameRenamePayload(BaseModel):
    plan: list[FilenameRenameEntry]
    dir_rename: DirRenameEntry | None = None


@app.post("/api/checks/filename-mismatch/findings/{finding_id}/apply")
def apply_filename_rename(
    finding_id: int,
    payload: FilenameRenamePayload,
) -> dict[str, Any]:
    """Custom action for the filename-mismatch check: execute the
    user-confirmed rename plan. Validates the finding belongs to the
    right check, then runs the renames through filename_mismatch.
    execute_renames (two-pass + track-tag migration + post-rename
    rescan). When `dir_rename` is supplied, the album directory is
    renamed first and FK references migrate to the new id."""
    from .checks import filename_mismatch
    finding = checks_runner.get_finding(finding_id)
    if finding is None or finding["check_id"] != "filename-mismatch":
        raise HTTPException(status_code=404, detail="Finding not found")
    album_id = finding.get("album_id")
    if not album_id:
        raise HTTPException(status_code=400, detail="Finding has no album")
    if not payload.plan and payload.dir_rename is None:
        raise HTTPException(status_code=400, detail="plan or dir_rename is required")
    plan_dicts = [{"old": e.old, "new": e.new} for e in payload.plan]
    dir_rename_dict = (
        {"old": payload.dir_rename.old, "new": payload.dir_rename.new}
        if payload.dir_rename else None
    )
    result = filename_mismatch.execute_renames(
        album_id=album_id, plan=plan_dicts, dir_rename=dir_rename_dict,
    )
    if not result.get("ok") and result.get("renamed", 0) == 0 and not result.get("dir_renamed"):
        raise HTTPException(
            status_code=409,
            detail=result.get("message") or "Rename failed",
        )
    # Post-apply check rerun so the finding clears immediately.
    try:
        cfg = config_module.load_config()
        if cfg.library_dir:
            _run_post_scan_checks(cfg.library_dir)
    except Exception:
        _log.exception("post-rename check rerun failed (continuing)")
    return result


class MbCanonicalApplyPayload(BaseModel):
    accept_album_artist: bool = False
    accept_track_filenames: list[str] = []


@app.post("/api/checks/mb-canonical-mismatch/findings/{finding_id}/apply")
def apply_mb_canonical(
    finding_id: int,
    payload: MbCanonicalApplyPayload,
) -> dict[str, Any]:
    """Custom action for the mb-canonical-mismatch check: apply the
    user-confirmed subset of the diffs. Re-fetches MB at apply time
    (cached) so we use the same values the user saw."""
    from .checks import mb_canonical_mismatch
    finding = checks_runner.get_finding(finding_id)
    if finding is None or finding["check_id"] != "mb-canonical-mismatch":
        raise HTTPException(status_code=404, detail="Finding not found")
    album_id = finding.get("album_id")
    if not album_id:
        raise HTTPException(status_code=400, detail="Finding has no album")
    if not payload.accept_album_artist and not payload.accept_track_filenames:
        raise HTTPException(
            status_code=400,
            detail="Pick at least one field to apply",
        )
    result = mb_canonical_mismatch.apply_canonical(
        album_id=album_id,
        accept_album_artist=payload.accept_album_artist,
        accept_track_filenames=payload.accept_track_filenames,
    )
    if not result.get("ok"):
        raise HTTPException(
            status_code=409,
            detail=result.get("message") or "Apply failed",
        )
    return result


@app.post("/api/checks/duplicate-tracks/findings/{finding_id}/delete-file")
def delete_duplicate_file(finding_id: int, payload: DeleteDuplicateFilePayload) -> dict[str, Any]:
    """Custom action for the duplicate-tracks check: delete one of the
    duplicate FLACs that the user has chosen to remove.

    Validates the filename is one of the cluster's listed duplicates (so a
    misdirected request can't accidentally nuke an unrelated file), deletes
    the file from disk, drops the matching track_fingerprints row, and
    triggers a post-scan rerun so the finding auto-resolves once the
    cluster is down to 1 file.
    """
    finding = checks_runner.get_finding(finding_id)
    if finding is None or finding["check_id"] != "duplicate-tracks":
        raise HTTPException(status_code=404, detail="Finding not found")
    detail = finding.get("detail") or {}
    listed_filenames = list(detail.get("filenames") or [])
    if payload.filename not in listed_filenames:
        raise HTTPException(
            status_code=400,
            detail=f"Filename {payload.filename!r} is not one of this finding's duplicates",
        )

    album_id = finding.get("album_id")
    if not album_id:
        raise HTTPException(status_code=400, detail="Finding has no album to act on")

    # Resolve the album path via the cache so we can locate the file on disk.
    album = _cached_album_dict(album_id)
    if album is None:
        raise HTTPException(status_code=404, detail=f"Album {album_id!r} not in cache")
    album_path = Path(album.get("path") or "")
    target = album_path / payload.filename
    # Sanity: refuse to delete anything outside the album dir (defense
    # against weird inputs, even though we've already validated against
    # the finding's listed filenames).
    try:
        target_resolved = target.resolve(strict=True)
        album_resolved = album_path.resolve(strict=True)
        file_already_gone = False
    except FileNotFoundError:
        # Idempotent recovery: the file is missing on disk, but the
        # finding's detail still references it (and the fingerprint row
        # may still be present). Most common cause: an earlier delete
        # attempt unlinked the file but failed before cleaning up the
        # cache state (e.g. the recent NameError crash on db_module).
        # Treat the click as "finish the cleanup" rather than 404'ing —
        # the file is already in the desired state, we just need to
        # catch the cache up so the finding can resolve.
        target_resolved = None
        album_resolved = album_path.resolve()
        file_already_gone = True
    if not file_already_gone and album_resolved not in target_resolved.parents:
        raise HTTPException(status_code=400, detail="Refusing to delete file outside album directory")

    # Soft-delete: move the file into the app's trash directory rather
    # than unlinking it permanently. The user can restore from
    # ~/.media_manager/trash/<album_dir>/<filename> if they change
    # their mind. See trash.py for layout + collision rules. Skipped
    # entirely on the file_already_gone path — there's nothing to move.
    trashed_path: Path | None = None
    if not file_already_gone:
        from . import trash as trash_module
        try:
            trashed_path = trash_module.send_to_trash(
                target_resolved,
                album_dir_name=album.get("dir_name"),
            )
        except trash_module.TrashError as e:
            raise HTTPException(status_code=500, detail=str(e))

    # Drop the fingerprint row for the moved (or already-missing) file.
    # The next scan will also catch up via FK cascade if the album row
    # goes away, but we want the duplicate-tracks check to see the
    # cluster shrink immediately.
    cache_module.delete_track_fingerprint(album_id, payload.filename)

    # Re-scan the album so the cache reflects the deleted file (track_count
    # drops by one, the file no longer appears in the album dir).
    #
    # Pull `library_dir` from config rather than from the cached album dict.
    # The album dict from `_cached_album_dict` doesn't include a
    # `library_path` key (the row → dict mapping in `cache._row_to_album`
    # never carried it through), so the older `album.get("library_path")
    # or str(album_path.parent.resolve())` fallback always landed on
    # `parent.resolve()` — correct for albums sitting one level under the
    # library root, wrong for nested layouts (multi-disc rips, year-bucket
    # subfolders), where the upsert would persist under a different
    # library_path than the rest of the library and ghost-orphan the
    # album from `cache.list_albums(library_path)` lookups.
    try:
        fresh = scan_one_album(album_path)
        if fresh is None:
            cache_module.delete_album(album_id)
        else:
            cfg = config_module.load_config()
            if not cfg.library_dir:
                raise RuntimeError("No library directory configured for post-delete rescan")
            library_dir = str(Path(cfg.library_dir).expanduser().resolve())
            cache_module.upsert_album(library_dir, album_to_dict(fresh))
    except Exception:
        logging.getLogger(__name__).exception(
            "post-delete rescan failed for %s (continuing)", album_id,
        )

    # Run the post-scan tail so the duplicate-tracks finding (and any
    # related findings — track-gaps may also clear) auto-resolves.
    try:
        cfg = config_module.load_config()
        if cfg.library_dir:
            _run_post_scan_checks(str(Path(cfg.library_dir).expanduser().resolve()))
    except Exception:
        logging.getLogger(__name__).exception(
            "post-delete check rerun failed (continuing)",
        )

    if file_already_gone:
        return {
            "ok": True,
            "message": (
                f"{payload.filename} was already gone from disk — cleaned up "
                f"the cache so this duplicate finding will clear."
            ),
            "trashed_to": None,
        }
    return {
        "ok": True,
        "message": f"Moved {payload.filename} to trash",
        "trashed_to": str(trashed_path) if trashed_path else None,
    }


@app.post("/api/checks/{check_id}/quick-fix-all")
async def quick_fix_all(check_id: str) -> StreamingResponse:
    """SSE-streamed bulk run of try_quick_fix() across every open finding
    for this check. Lets the user trim the deterministic-fix portion of a
    long list without committing AI work for the rest.

    Pacing: one second between MB calls (MusicBrainz allows 1 req/s for
    unauthenticated traffic). For 80 findings that's ~80+ seconds; we
    stream per-finding outcomes so the UI can show progress.

    Events emitted (event name → payload):
      - `start`    : { total }
      - `progress` : { finding_id, outcome, message }
                       outcome ∈ "fixed" | "skipped" | "error" | "not-applicable"
      - `done`     : { total, fixed, skipped, errors, not_applicable }
      - `error`    : { detail }   (only on a fatal mid-run failure)
    """
    cls = checks_module.get(check_id)
    if cls is None:
        raise HTTPException(status_code=404, detail=f"Unknown check_id: {check_id!r}")

    findings = checks_runner.list_findings(check_id=check_id)
    finding_ids = [f["id"] for f in findings]

    async def event_gen():
        # Flip the bulk-mode contextvar so apply_direct() skips its
        # per-apply post-scan rerun. We do one batch rerun at the very
        # end, which is N times faster than running it per-finding.
        from .checks._apply import in_bulk_run
        token = in_bulk_run.set(True)

        yield _sse_event("start", {"total": len(finding_ids)})
        counts = {"fixed": 0, "skipped": 0, "errors": 0, "not_applicable": 0}
        try:
            for i, fid in enumerate(finding_ids):
                # MB rate limit: 1 req/s. Skip the sleep on the first one.
                if i > 0:
                    await asyncio.sleep(1.0)
                try:
                    # try_quick_fix is sync (calls requests.* + apply); offload
                    # so the event loop stays responsive for SSE flushes.
                    # Use copy_context() so the in_bulk_run flag follows the
                    # call into the worker thread.
                    import contextvars
                    ctx = contextvars.copy_context()
                    result = await asyncio.to_thread(
                        ctx.run, checks_runner.try_quick_fix_finding, fid,
                    )
                except Exception as e:
                    counts["errors"] += 1
                    yield _sse_event("progress", {
                        "finding_id": fid,
                        "outcome": "error",
                        "message": f"{type(e).__name__}: {e}",
                    })
                    continue
                if result is None:
                    counts["not_applicable"] += 1
                    yield _sse_event("progress", {
                        "finding_id": fid,
                        "outcome": "not-applicable",
                        "message": "Check has no quick-fix path",
                    })
                elif result.ok:
                    counts["fixed"] += 1
                    yield _sse_event("progress", {
                        "finding_id": fid,
                        "outcome": "fixed",
                        "message": result.message,
                    })
                else:
                    counts["skipped"] += 1
                    yield _sse_event("progress", {
                        "finding_id": fid,
                        "outcome": "skipped",
                        "message": result.message,
                    })

            # Single post-scan rerun for the whole batch. Resolves any
            # findings the bulk-applied data made stale.
            #
            # MUST use the async variant — we're inside a `async def
            # event_gen` running on the main event loop. The sync version
            # internally calls `asyncio.run()` which would raise
            # RuntimeError("cannot be called from a running event loop"),
            # the except clause would silently swallow it, and findings
            # would never be marked resolved (which was the original bug
            # this was added to fix).
            if counts["fixed"] > 0:
                try:
                    library_path = _library_path_for_checks()
                    await _run_post_scan_checks_async(library_path)
                except Exception:
                    logging.getLogger(__name__).exception(
                        "post-bulk check rerun failed; continuing",
                    )

            yield _sse_event("done", {"total": len(finding_ids), **counts})
        except asyncio.CancelledError:
            # Client disconnected. Don't keep churning MB requests.
            return
        except Exception as e:
            yield _sse_event("error", {"detail": f"{type(e).__name__}: {e}"})
        finally:
            in_bulk_run.reset(token)

    return StreamingResponse(
        event_gen(),
        media_type="text/event-stream",
        headers={
            "Cache-Control": "no-cache, no-transform",
            "X-Accel-Buffering": "no",
            "Connection": "keep-alive",
        },
    )


# ---------- Background worker ----------

# AI-assigned MBID confidence score. We don't have an MB Lucene score for an
# AI-supplied MBID, but we DO know the AI claimed high confidence and we've
# verified the MBID exists in MB. Reference mb_lookup.HIGH_CONFIDENCE_SCORE
# directly so a future threshold bump in mb_lookup auto-updates this path —
# previously this was a hardcoded 88 that mirrored the same value, but the
# parallel had to be remembered (and the constants would silently drift if
# either side moved). Treat AI matches as exactly the deterministic-path
# anchor threshold: same downstream behavior, no special-casing.
_AI_MBID_CONFIDENCE = mb_lookup.HIGH_CONFIDENCE_SCORE


def _persist_ai_mbid(*, album_id: str, mbid: str) -> None:
    """Verify an AI-supplied MBID against MusicBrainz and persist the
    anchor on success. Synchronous (called via asyncio.to_thread).

    The verification step is critical: claude-agent-sdk responses can
    contain hallucinated UUIDs, and a well-formed-but-fake one would
    poison the cache and cause every downstream MB call for this album
    to point at nonexistent data. We GET the release-group detail
    endpoint before writing — 200 means it's real, anything else means
    we drop the suggestion silently. (GET rather than HEAD because MB
    returns small JSON either way and the GET response gives clearer
    failure modes if we want to log them later.)
    """
    import requests
    url = mb_lookup.MB_RELEASE_GROUP_DETAIL.format(mbid=mbid)
    try:
        resp = requests.get(
            url,
            params={"fmt": "json"},
            headers={"User-Agent": mb_lookup.USER_AGENT},
            timeout=10,
        )
    except requests.RequestException as e:
        _log.warning(
            "AI MBID verify failed for album=%s mbid=%s: %s",
            album_id, mbid, e,
        )
        # Don't record a failure here — this is a transient network
        # issue with our verification request, not "AI couldn't find
        # it." Leave the album in the not-yet-tried state so the next
        # check run can retry.
        return
    if resp.status_code != 200:
        _log.info(
            "AI returned non-existent MBID for album=%s mbid=%s "
            "(MB returned HTTP %s); ignoring",
            album_id, mbid, resp.status_code,
        )
        # AI gave us a fake/wrong UUID. Treat that as "AI tried,
        # nothing real found" so we don't ask it again next pass.
        try:
            cache_module.record_mb_lookup_failure(album_id)
        except Exception:
            _log.exception(
                "Couldn't record MB lookup failure for album=%s after "
                "MB rejected AI-supplied MBID",
                album_id,
            )
        return
    cache_module.set_album_mb_anchor(
        album_id,
        release_group_mbid=mbid,
        release_mbid=None,  # AI gave us release-group; release-id is unknown
        score=_AI_MBID_CONFIDENCE,
    )
    _log.info(
        "Persisted AI-identified MB anchor for album=%s mbid=%s",
        album_id, mbid,
    )

    # Kick off a check rerun so the missing-mbid finding clears
    # immediately rather than waiting for the next library scan.
    # Without this the user has to wonder whether the AI MBID write
    # actually happened — the visible signal of success is "the
    # finding goes away," and that signal needs to fire promptly.
    try:
        cfg = config_module.load_config()
        if cfg.library_dir:
            library_path = str(Path(cfg.library_dir).expanduser().resolve())
            _run_post_scan_checks(library_path)
    except Exception:
        # Best-effort — never let a check-rerun failure disturb the
        # successful MBID persist that just happened.
        _log.exception(
            "post-MBID-persist check rerun failed for album=%s (continuing)",
            album_id,
        )


async def _run_one_job(job: jobs_module.Job) -> None:
    """Execute a single job and persist its result/error.

    Cover lookup currently goes through `ai.suggest_cover` — Sonnet works
    for the vast majority of albums and gives us smart disambiguation
    between releases. `mb_lookup.find_cover` exists as a non-AI fallback
    (direct MusicBrainz HTTP via the back-compat shim) for the day we
    want it; not wired in here yet.

    Failure policy: when a job fails, we set `ai_queue_paused=True` so the
    queue stops dispatching new jobs. This is deliberate — most failures we
    see are capacity / rate-limit related, and continuing to fire jobs into
    a degraded state usually just produces more failures. The user sees the
    pause banner on the Jobs page and can Resume after retrying / waiting.
    """
    from . import ai
    from . import settings as settings_module

    jobs_module.mark_running(job.id)
    _notify_jobs_changed()

    try:
        # Re-resolve album from cache (it may have been mutated since enqueue).
        album = _cached_album_dict(job.album_id)
        if album is None:
            raise RuntimeError(f"Album {job.album_id!r} no longer in cache")
        tracks = _read_tracks(album["path"])

        result: dict[str, Any] = {}

        if job.job_type in ("metadata", "metadata+cover"):
            meta = await ai.suggest_metadata(album, tracks, user_hint=job.hint)
            result["metadata"] = {
                "suggestions": meta.suggestions,
                "tracks": [
                    {"filename": t.filename, "suggestions": t.suggestions}
                    for t in meta.tracks
                ],
                "notes": meta.notes,
                "confidence": meta.confidence,
                "mb_release_group_mbid": meta.mb_release_group_mbid,
            }
            # Side-effect: if AI confidently identified the album on
            # MusicBrainz, persist the MBID anchor in the cache. This is
            # independent of the user's tag-apply decision — the MBID
            # is a free metadata anchor, not a destructive tag write.
            # Verified against MB live before persisting so a well-formed
            # but fake UUID doesn't poison the cache.
            if meta.mb_release_group_mbid:
                try:
                    await asyncio.to_thread(
                        _persist_ai_mbid,
                        album_id=job.album_id,
                        mbid=meta.mb_release_group_mbid,
                    )
                except Exception:
                    # MBID persistence is best-effort; never fail the job
                    # over it. The tag suggestions are still useful even
                    # when the MBID write fails.
                    _log.exception(
                        "AI MBID persist failed for job %s (album=%s, mbid=%s)",
                        job.id, job.album_id, meta.mb_release_group_mbid,
                    )
            else:
                # AI didn't return an MBID — record the lookup attempt
                # so we don't keep asking AI to identify the same album
                # on every health-check rerun. Within the staleness
                # window (mb_lookup.CACHE_REFRESH_DAYS) the album won't
                # surface as missing-mbid; after that it can retry, in
                # case MB has gained the release in the meantime.
                try:
                    await asyncio.to_thread(
                        cache_module.record_mb_lookup_failure,
                        job.album_id,
                    )
                    _log.info(
                        "AI couldn't identify album=%s on MB; recorded "
                        "lookup attempt so future checks skip it for "
                        "the staleness window.",
                        job.album_id,
                    )
                except Exception:
                    _log.exception(
                        "Couldn't record MB lookup failure for album=%s",
                        job.album_id,
                    )

        if job.job_type in ("cover", "metadata+cover"):
            cover = await ai.suggest_cover(album, tracks, user_hint=job.hint)
            result["cover"] = {
                "cover_url": cover.cover_url,
                "source": cover.source,
                "notes": cover.notes,
                "confidence": cover.confidence,
            }

        jobs_module.mark_completed(job.id, result)
        _log.info("job %s (%s, album=%s) completed", job.id, job.job_type, job.album_id)
    except Exception as e:
        # Most failures are the known opaque CLI exit-1 pattern — the
        # downstream message is already informative; full traceback adds
        # noise. The user sees the failure in the AI Jobs dashboard.
        msg = str(e)
        if "exit code 1" in msg and "Check stderr output" in msg:
            _log.warning("job %s (%s, album=%s) failed — moving on",
                         job.id, job.job_type, job.album_id)
        else:
            _log.exception("job %s (%s, album=%s) failed",
                           job.id, job.job_type, job.album_id)
        jobs_module.mark_failed(job.id, msg)
        # Auto-pause the queue on first error. The user will see the banner
        # and can hit Resume after retrying / waiting on rate limits.
        try:
            settings_module.set_ai_queue_paused(True)
            _log.warning("AI queue paused due to job failure; user must resume manually")
        except Exception:
            _log.exception("Failed to set ai_queue_paused after job failure")
    finally:
        _notify_jobs_changed()


async def _worker_loop():
    """Dispatcher loop. Runs up to `ai_max_parallel` jobs concurrently and
    pauses dispatch when `ai_queue_paused=True` (set automatically on any
    job failure; cleared by the user via Resume).

    Implementation notes:
      - Each iteration peeks at the queue. If we have a free slot AND the
        queue isn't paused, dispatch the next queued job as a fire-and-forget
        task. Otherwise wait on `_jobs_changed` (or a 30s timeout) and re-evaluate.
      - We track in-flight job IDs in a local set so we don't dispatch the
        same row twice between the "mark_running" SQL writes (the running
        count in DB lags the dispatch by a few ms).
      - Every per-job step is wrapped — exceptions inside _run_one_job are
        already caught there, but a DB / settings hiccup at the dispatcher
        level shouldn't kill the loop.
    """
    from . import settings as settings_module

    global _worker_running
    _worker_running = True

    # Recover any jobs left in 'running' state from a previous unclean
    # shutdown — without this, they'd be invisible (worker only picks up
    # 'queued' rows) and the queue would be effectively stalled.
    try:
        reset = jobs_module.reset_stuck_running()
        if reset > 0:
            _log.info("Reset %d stuck running job(s) to queued on worker startup", reset)
            _notify_jobs_changed()
    except Exception:
        _log.exception("Worker startup recovery failed (continuing)")

    # In-flight job IDs we've dispatched but whose mark_running write may not
    # have landed yet. Used to skip them during get_next_queued so the same
    # row isn't picked up by two concurrent dispatch iterations.
    in_flight: set[str] = set()

    async def _run_and_track(job: jobs_module.Job) -> None:
        try:
            await _run_one_job(job)
        finally:
            in_flight.discard(job.id)
            # Wake the dispatcher — a slot just freed up.
            _jobs_changed.set()

    _log.info("AI job worker started")
    try:
        while True:
            try:
                paused = settings_module.ai_queue_paused()
                max_parallel = settings_module.ai_max_parallel()

                # Don't dispatch while paused. Stay responsive to changes
                # (resume button, settings update) by waking on the event.
                if paused:
                    try:
                        await asyncio.wait_for(_jobs_changed.wait(), timeout=30.0)
                    except asyncio.TimeoutError:
                        pass
                    _jobs_changed.clear()
                    continue

                # Capacity check. count_running reflects DB state, which lags
                # dispatch by a tick — combine with our local in_flight set
                # for an accurate live total.
                running = max(jobs_module.count_running(), len(in_flight))
                if running >= max_parallel:
                    try:
                        await asyncio.wait_for(_jobs_changed.wait(), timeout=30.0)
                    except asyncio.TimeoutError:
                        pass
                    _jobs_changed.clear()
                    continue

                # Pull the next queued job (skipping anything we've already
                # dispatched but hasn't flipped to 'running' yet).
                job = jobs_module.get_next_queued_excluding(in_flight)
                if job is None:
                    try:
                        await asyncio.wait_for(_jobs_changed.wait(), timeout=30.0)
                    except asyncio.TimeoutError:
                        pass
                    _jobs_changed.clear()
                    continue

                in_flight.add(job.id)
                # Fire-and-forget: don't await, so we can immediately re-loop
                # and consider dispatching another concurrent job.
                asyncio.create_task(_run_and_track(job), name=f"ai-job-{job.id}")
                # Yield control briefly so mark_running has a chance to land
                # before our next get_next_queued_excluding read.
                await asyncio.sleep(0)
            except asyncio.CancelledError:
                raise
            except Exception:
                _log.exception("Worker dispatcher iteration failed; sleeping 5s and continuing")
                await asyncio.sleep(5.0)
    except asyncio.CancelledError:
        _log.info("AI job worker stopped")
        raise
    finally:
        _worker_running = False


@contextlib.asynccontextmanager
async def _lifespan(_app: FastAPI):
    """Start the AI job worker on app startup, cancel it on shutdown.
    Also takes an automatic DB snapshot if the configured interval
    has elapsed since the last one — protects the user against
    accidental data loss (the kind of mistake we've now fixed in
    drop_snapshot but want belt-and-suspenders for anyway)."""
    # Snapshot first, before the worker can start mutating state.
    try:
        from . import db_backup, settings as settings_module
        retention = settings_module.snapshot_retention()
        interval = settings_module.snapshot_min_interval_hours()
        snap = db_backup.maybe_auto_snapshot(
            retention=retention, min_interval_hours=interval,
        )
        if snap:
            _log.info(
                "Auto-snapshot taken on startup: %s (%d bytes); pruned %d older",
                snap["filename"], snap["size_bytes"], len(snap.get("pruned") or []),
            )
    except Exception:
        _log.exception("Auto-snapshot on startup failed (continuing)")

    task = asyncio.create_task(_worker_loop(), name="ai-job-worker")
    try:
        yield
    finally:
        task.cancel()
        with contextlib.suppress(asyncio.CancelledError):
            await task


# Wire the lifespan handler into the existing FastAPI app instance. (FastAPI
# accepts either constructor `lifespan=` or `app.router.lifespan_context`.)
app.router.lifespan_context = _lifespan


# ---------- Apply endpoint ----------
#
# Only the streaming variant (`.../apply/stream`) is exposed. The earlier
# synchronous `/apply` endpoint was dead code — the React frontend has
# always used the SSE version via `useApplyStream` — so it was deleted to
# keep the apply semantics in one place. If we ever want a non-streaming
# apply (e.g. for a "dry-run preview" workflow), reintroduce it from
# scratch with whatever the new use case actually needs.


@app.post("/api/albums/{album_id}/apply/stream")
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
    album = _resolve_album_dict(album_id)
    cfg = config_module.load_config()
    library_dir = cfg.library_dir or ""

    def event_gen():
        cover_bytes: bytes | None = None
        cover_mime: str | None = None
        try:
            # 1. Download cover (if requested) — emit progress events.
            if payload.cover_url:
                yield _sse_event("cover_downloading", {"url": payload.cover_url})
                downloaded = download_cover(payload.cover_url)
                if downloaded is None:
                    yield _sse_event("error", {"detail": f"Failed to download cover from {payload.cover_url}"})
                    return
                raw_bytes, raw_mime = downloaded
                # Normalize to JPEG quality 95 for embedded use + cover.jpg sidecar.
                # Cheaper per-FLAC payload than embedding a multi-MB PNG, and
                # universal compatibility. Lossy, but quality 95 is visually
                # lossless for album art at typical sizes.
                cover_bytes = normalize_to_jpeg(raw_bytes, quality=95)
                cover_mime = "image/jpeg"
                yield _sse_event("cover_downloaded", {
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
                # change renames a file (e.g. "05 - foo.flac" → "05 Foo.flac"),
                # follow that rename in the track_tags table so the user's
                # custom tags don't get orphaned to the old filename.
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
                yield _sse_event(ev_name, ev_payload)

            if payload.dry_run:
                # No state changed; we're done.
                return

            # 3. If a new cover was supplied, also drop it as cover.jpg next
            #    to the FLAC files. Players that read sidecar art (Plex, the
            #    OS thumbnailer) pick this up; our scanner prefers the
            #    on-disk file over re-extracting from FLAC, which is faster.
            if cover_bytes is not None:
                saved = save_cover_to_album_dir(album["path"], cover_bytes)
                if saved is not None:
                    yield _sse_event("cover_saved", {"path": str(saved), "filename": saved.name})

            # 4. Invalidate the thumb if art changed.
            if had_art_change:
                thumbs_module.delete_thumb(album_id)

            # 4b. If any file was renamed and the album has track tags,
            #     resync the sidecar so the on-disk JSON reflects the new
            #     filenames. The DB rows were updated above; this brings
            #     persistence into agreement.
            if had_renames:
                try:
                    track_tag_map = cache_module.get_album_track_tags(album_id)
                    if track_tag_map:
                        from . import track_tags_sidecar as _sidecar
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
                # Album dir disappeared somehow.
                cache_module.delete_album(album_id)
                yield _sse_event("removed_album", {"id": album_id, "path": album["path"]})
            else:
                fresh_dict = album_to_dict(fresh)
                if library_dir:
                    cache_module.upsert_album(library_dir, fresh_dict)
                yield _sse_event("updated_album", fresh_dict)

            # 6. Re-run the cheap post-scan health checks so any findings the
            #    apply just fixed (missing tags, track gaps, no cover art)
            #    auto-resolve immediately — the user shouldn't have to wait
            #    until the next full library scan to see the Health page
            #    update. This is the same dispatcher the scan endpoint runs;
            #    cost is microseconds for the three SQL-pure checks today.
            if library_dir:
                _run_post_scan_checks(library_dir)
        except Exception as e:
            yield _sse_event("error", {"detail": str(e)})

    return StreamingResponse(
        event_gen(),
        media_type="text/event-stream",
        headers={
            "Cache-Control": "no-cache, no-transform",
            "X-Accel-Buffering": "no",
            "Connection": "keep-alive",
        },
    )


# ---------- Static frontend ----------

# Mount the static directory at /static so app.js / styles.css load from there,
# and serve /index.html (and /) explicitly.
app.mount("/static", StaticFiles(directory=str(STATIC_DIR)), name="static")


@app.get("/")
def index() -> FileResponse:
    return FileResponse(str(STATIC_DIR / "index.html"))


# ---------- CLI entry point ----------

def _configure_logging() -> None:
    """Set up our application logger so log.info() / log.warning() messages
    actually appear (Python's default level is WARNING, which is why all our
    INFO-level diagnostics were invisible until now).

    Two handlers:
      - stdout (mirrors what you'd see by tailing the terminal)
      - rotating file at $CONFIG_DIR/server.log (5 MB × 5 generations =
        ~25 MB cap; survives terminal close / restart so we can grep
        debugging output after the fact)

    Level is INFO by default; override with MEDIA_MANAGER_LOG_LEVEL=DEBUG
    when chasing something specific.
    """
    import logging.handlers
    import os
    from pathlib import Path

    from .config import CONFIG_DIR

    level_name = os.environ.get("MEDIA_MANAGER_LOG_LEVEL", "INFO").upper()
    level = getattr(logging, level_name, logging.INFO)

    fmt = logging.Formatter(
        "%(asctime)s [%(levelname)s] %(name)s: %(message)s",
        datefmt="%Y-%m-%d %H:%M:%S",
    )

    # Application logger — touch only `media_manager.*` and below; leave
    # uvicorn's loggers alone so its access-log format stays intact.
    app_log = logging.getLogger("media_manager")
    app_log.setLevel(level)
    # Idempotent: don't double-attach handlers if run() is called twice
    # (e.g. under --reload).
    if not any(getattr(h, "_mm_marker", False) for h in app_log.handlers):
        stream = logging.StreamHandler()
        stream.setFormatter(fmt)
        stream._mm_marker = True  # type: ignore[attr-defined]
        app_log.addHandler(stream)

        try:
            CONFIG_DIR.mkdir(parents=True, exist_ok=True)
            file_path = Path(CONFIG_DIR) / "server.log"
            file_handler = logging.handlers.RotatingFileHandler(
                file_path, maxBytes=5 * 1024 * 1024, backupCount=5,
            )
            file_handler.setFormatter(fmt)
            file_handler._mm_marker = True  # type: ignore[attr-defined]
            app_log.addHandler(file_handler)
            print(f"[media-manager] Logging to {file_path} (level={level_name})")
        except Exception as e:
            # Never let log-file setup fail the boot.
            print(f"[media-manager] Warning: couldn't set up file log ({e})")

    # Don't bubble up to root — root has its own default handler that
    # would duplicate every line.
    app_log.propagate = False


def run() -> None:
    """`media-manager` console-script entry point."""
    parser = argparse.ArgumentParser(description="Media Manager web UI")
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8765)
    parser.add_argument("--reload", action="store_true", help="Enable hot reload (development)")
    args = parser.parse_args()

    _configure_logging()

    import os
    auth = "CLAUDE_CODE_OAUTH_TOKEN" if os.environ.get("CLAUDE_CODE_OAUTH_TOKEN") else (
        "ANTHROPIC_API_KEY" if os.environ.get("ANTHROPIC_API_KEY") else None
    )
    if auth:
        print(f"[media-manager] Claude auth: using {auth} from environment")
    else:
        print("[media-manager] Warning: no CLAUDE_CODE_OAUTH_TOKEN or ANTHROPIC_API_KEY found — AI features will fail")

    import uvicorn

    uvicorn.run(
        "media_manager.main:app",
        host=args.host,
        port=args.port,
        reload=args.reload,
    )


if __name__ == "__main__":
    run()
