"""Shared SSE helpers and the jobs-changed event that routers + worker
both signal on."""
from __future__ import annotations

import asyncio
import json
from typing import Any


def sse_event(event: str, data: dict[str, Any]) -> str:
    return f"event: {event}\ndata: {json.dumps(data)}\n\n"


# Bumped whenever any job's status changes — the SSE stream uses it as a
# cheap "something happened, re-check" trigger. Shared between the worker
# loop, the jobs router, and any settings change that should wake the
# dispatcher (max_parallel bump, queue resume).
jobs_changed = asyncio.Event()


def notify_jobs_changed() -> None:
    jobs_changed.set()
