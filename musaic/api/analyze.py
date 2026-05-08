"""Streaming audio-analysis endpoint.

Wraps `services.audio_analysis.analyze_album_iter` in an SSE response
so the album detail panel can show per-track progress as BPM / ReplayGain
land. Same shape as `api.apply` — resolve the album, open a stream,
forward every event verbatim.
"""
from __future__ import annotations

from fastapi import APIRouter, HTTPException
from fastapi.responses import StreamingResponse
from pydantic import BaseModel

from .. import config as config_module
from ..services.albums import resolve_album_dict
from ..services.audio_analysis import AnalysisKind, analyze_album_iter
from ..services.sse import sse_event

router = APIRouter()


class AnalyzePayload(BaseModel):
    what: AnalysisKind  # "bpm" | "replaygain"


@router.post("/api/albums/{album_id}/analyze/stream")
def analyze_album_stream(album_id: str, payload: AnalyzePayload) -> StreamingResponse:
    """Run BPM detection or ReplayGain analysis across an album's FLACs.

    Streams per-file progress + final outcome. The frontend invalidates
    its `tracks` query on `done` so the new BPM/RG values land in the
    track list without a manual reload.
    """
    if payload.what not in ("bpm", "replaygain"):
        raise HTTPException(status_code=400, detail="`what` must be 'bpm' or 'replaygain'")

    album = resolve_album_dict(album_id)
    cfg = config_module.load_config()
    library_dir = cfg.library_dir or ""

    def event_gen():
        for ev_name, ev_payload in analyze_album_iter(
            album=album,
            library_dir=library_dir,
            what=payload.what,
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
