"""System-level routes that are intentionally cheap and dependency-free.

Distinct from `api/health.py`, which despite its filename hosts the
*checks* framework (`/api/checks/*`) — a heavy, DB-backed subsystem unfit
as a liveness probe. The Electron desktop shell (issue #8) spawns the
backend as a localhost sidecar and polls `/api/health` to know when the
server is ready to receive the window's `loadURL`, so this route must not
touch the DB, the filesystem, or any optional subsystem.
"""
from __future__ import annotations

from fastapi import APIRouter

router = APIRouter()


@router.get("/api/health")
def health() -> dict[str, str]:
    """Liveness/readiness probe. Returns as soon as the ASGI app can
    serve a request — no DB, no scan state, no checks. The Electron
    main process backoff-polls this until it returns 200 before
    pointing the renderer at the sidecar origin."""
    return {"status": "ok"}
