"""The streaming apply endpoint — accept AI-suggested metadata + cover
changes and write them to disk + cache, emitting per-file progress
events along the way. Mutation logic lives in
`services.apply.apply_album_changes_iter`; this router just resolves
the album, opens the SSE socket, and forwards every event."""
from __future__ import annotations

from fastapi import APIRouter
from fastapi.responses import StreamingResponse
from pydantic import BaseModel

from .. import config as config_module
from ..services.albums import resolve_album_dict
from ..services.apply import apply_album_changes_iter
from ..services.sse import sse_event

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
