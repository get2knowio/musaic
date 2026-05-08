"""The streaming apply endpoint — accept AI-suggested metadata + cover
changes and write them to disk + cache, emitting per-file progress
events along the way. Mutation logic lives in
`services.apply.apply_album_changes_iter`; this router just resolves
the album, opens the SSE socket, and forwards every event."""
from __future__ import annotations

from fastapi import APIRouter, File, HTTPException, UploadFile
from fastapi.responses import StreamingResponse
from pydantic import BaseModel

from .. import config as config_module
from ..services.albums import resolve_album_dict
from ..services.apply import apply_album_changes_iter
from ..services.sse import sse_event

router = APIRouter()

# Hard cap on uploaded cover sizes. The normalize-to-JPEG step also bounds
# the on-disk result, but we should reject obvious nonsense at the door
# before we read the whole multipart body into memory. 25 MB covers the
# pathological "scanned 600-DPI sleeve as PNG" case without giving
# attackers a free OOM.
_MAX_COVER_UPLOAD_BYTES = 25 * 1024 * 1024
# Whitelist of MIME prefixes we'll accept. Pillow happily decodes more
# (e.g. WEBP), so we use a permissive prefix check rather than an exact
# allowlist — the normalize_to_jpeg call is the actual sanitizer.
_ALLOWED_COVER_MIME_PREFIXES = ("image/",)


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

    Solves two problems at once:
      1. Per-file progress UX — the frontend gets a `file_started` /
         `file_done` event for each FLAC so it can show a marching spinner.
      2. The "library re-scans after every apply" flicker bug — instead of
         dropping the entire library cache, we rescan ONLY the affected
         album dir and emit an `updated_album` event with the fresh row.
         The frontend updates one tile in place.

    Event sequence: see `services.apply.apply_album_changes_iter`.
    """
    album = resolve_album_dict(album_id)
    cfg = config_module.load_config()
    library_dir = cfg.library_dir or ""

    def event_gen():
        for ev_name, ev_payload in apply_album_changes_iter(
            album=album,
            library_dir=library_dir,
            suggestions=payload.suggestions,
            track_suggestions=payload.track_suggestions,
            cover_url=payload.cover_url,
            dry_run=payload.dry_run,
        ):
            yield sse_event(ev_name, ev_payload)

    return StreamingResponse(
        event_gen(),
        media_type="text/event-stream",
        headers={
            "Cache-Control": "no-cache, no-transform",
            "X-Accel-Buffering": "no",
            "Connection": "keep-alive",
        },
    )


@router.post("/api/albums/{album_id}/cover/upload")
async def upload_cover(album_id: str, cover: UploadFile = File(...)) -> StreamingResponse:
    """Embed a user-supplied image as the album's cover art.

    Powers the manual-upload affordance on the album sheet — used when
    the user has the right artwork in hand (clipboard, file on disk,
    URL paste) and Claude can't or didn't find it.

    The bytes are read into memory, validated, normalized to JPEG q95,
    then funneled through the same `apply_album_changes_iter` pipeline
    the AI cover flow uses, so the on-disk result is identical (every
    FLAC re-embedded + cover.jpg sidecar + thumb invalidation +
    post-scan check rerun). Streamed as SSE so the UI can show
    per-file progress.
    """
    raw_mime = (cover.content_type or "").lower()
    if not any(raw_mime.startswith(p) for p in _ALLOWED_COVER_MIME_PREFIXES):
        raise HTTPException(
            status_code=400,
            detail=f"Unsupported file type {raw_mime!r} — please upload an image",
        )

    cover_bytes = await cover.read()
    if not cover_bytes:
        raise HTTPException(status_code=400, detail="Empty file")
    if len(cover_bytes) > _MAX_COVER_UPLOAD_BYTES:
        raise HTTPException(
            status_code=413,
            detail=(
                f"Cover too large ({len(cover_bytes)} bytes). "
                f"Max accepted is {_MAX_COVER_UPLOAD_BYTES // (1024 * 1024)} MB."
            ),
        )

    album = resolve_album_dict(album_id)
    cfg = config_module.load_config()
    library_dir = cfg.library_dir or ""

    def event_gen():
        for ev_name, ev_payload in apply_album_changes_iter(
            album=album,
            library_dir=library_dir,
            cover_bytes=cover_bytes,
            cover_mime=raw_mime,
        ):
            yield sse_event(ev_name, ev_payload)

    return StreamingResponse(
        event_gen(),
        media_type="text/event-stream",
        headers={
            "Cache-Control": "no-cache, no-transform",
            "X-Accel-Buffering": "no",
            "Connection": "keep-alive",
        },
    )
