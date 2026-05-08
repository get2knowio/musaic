"""AI job queue endpoints. The worker that consumes the queue lives in
`media_manager.services.worker`; this module is just the REST + SSE
surface."""
from __future__ import annotations

import asyncio
import logging
from typing import Any

from fastapi import APIRouter, HTTPException, Query
from fastapi.responses import StreamingResponse
from pydantic import BaseModel

from .. import cache as cache_module
from .. import jobs as jobs_module
from ..services.albums import cached_album_dict
from ..services.sse import jobs_changed, notify_jobs_changed, sse_event
from ..services.worker import queue_state_dict

_log = logging.getLogger(__name__)
router = APIRouter()


class JobEnqueuePayload(BaseModel):
    album_id: str
    job_type: str  # "metadata" | "cover" | "metadata+cover"
    hint: str = ""


class ActedOnPayload(BaseModel):
    action: str  # "applied" | "discarded"


def _job_album_dict(album_id: str) -> dict[str, Any]:
    """Wrap the cached lookup with a 404 — used by enqueue."""
    album = cached_album_dict(album_id)
    if album is None:
        raise HTTPException(status_code=404, detail=f"Album {album_id!r} not in cache; rescan first")
    return album


@router.post("/api/ai/jobs")
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
    notify_jobs_changed()
    return jobs_module.job_to_dict(job)


@router.get("/api/ai/jobs")
def list_jobs(status: str | None = None) -> dict[str, Any]:
    statuses = status.split(",") if status else None
    rows = jobs_module.list_jobs(statuses=statuses)
    return {"jobs": [jobs_module.job_to_dict(j) for j in rows]}


# IMPORTANT: This route MUST be registered before /api/ai/jobs/{job_id}.
# FastAPI matches routes in registration order — without this ordering,
# `/api/ai/jobs/stream` matches the parameterized route with job_id="stream"
# and returns 404 (no such job).
@router.get("/api/ai/jobs/stream")
async def jobs_stream() -> StreamingResponse:
    """SSE stream of job-state changes. Emits a `jobs` event whenever any
    job is enqueued / status-changed / deleted, with the current full list
    in the payload. Cheap snapshot — list is bounded to the last 200."""
    async def event_gen():
        snapshot = [jobs_module.job_to_dict(j) for j in jobs_module.list_jobs()]
        yield sse_event("jobs", {
            "jobs": snapshot,
            "queue_state": queue_state_dict(),
        })
        try:
            while True:
                try:
                    await asyncio.wait_for(jobs_changed.wait(), timeout=30.0)
                    jobs_changed.clear()
                    snapshot = [jobs_module.job_to_dict(j) for j in jobs_module.list_jobs()]
                    yield sse_event("jobs", {
                        "jobs": snapshot,
                        "queue_state": queue_state_dict(),
                    })
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


@router.get("/api/ai/jobs/active/{album_id}")
def active_job_for_album(album_id: str) -> dict[str, Any] | None:
    """Used by the detail sheet on mount — find the most recent unacted-on
    job for this album so its result can be displayed inline."""
    job = jobs_module.get_active_job_for_album(album_id)
    return jobs_module.job_to_dict(job) if job else None


@router.get("/api/ai/jobs/{job_id}")
def get_job(job_id: str) -> dict[str, Any]:
    job = jobs_module.get_job(job_id)
    if job is None:
        raise HTTPException(status_code=404, detail="Job not found")
    return jobs_module.job_to_dict(job)


@router.post("/api/ai/jobs/{job_id}/acted-on")
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
    # and the missing-mbid / canonical-mismatch checks know not to keep
    # nudging the user about an album they already weighed in on.
    # Cover-only jobs don't count.
    if job.job_type in ("metadata", "metadata+cover"):
        try:
            cache_module.mark_metadata_ai_acted_on(job.album_id)
        except Exception:
            _log.exception(
                "Couldn't stamp metadata_ai_acted_on_at for album %s",
                job.album_id,
            )
    notify_jobs_changed()
    return jobs_module.job_to_dict(jobs_module.get_job(job_id))


@router.post("/api/ai/jobs/{job_id}/retry")
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
        raise HTTPException(status_code=409, detail="Job state changed; refresh and try again")
    notify_jobs_changed()
    return jobs_module.job_to_dict(updated)


@router.post("/api/ai/queue/resume")
def resume_queue() -> dict[str, Any]:
    """Clear the auto-paused flag so the worker resumes dispatching."""
    from .. import settings as settings_module
    settings_module.set_ai_queue_paused(False)
    notify_jobs_changed()
    return {"queue_state": queue_state_dict()}


@router.delete("/api/ai/jobs/{job_id}")
def delete_job(job_id: str) -> dict[str, Any]:
    # We can't actually cancel a job mid-flight — the SDK call is a blocking
    # subprocess we don't have a handle into — so refuse to delete it. The
    # row would otherwise vanish while the worker is still running, leading
    # to silent state divergence.
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
    notify_jobs_changed()
    return {"deleted": True, "id": job_id}


@router.delete("/api/ai/jobs")
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
        notify_jobs_changed()
    return {"deleted_count": n, "statuses": statuses}
