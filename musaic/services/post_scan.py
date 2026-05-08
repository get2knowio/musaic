"""Post-scan check fan-out — run every check that opted in via
`runs_during_scan=True` after a scan, apply, AI MBID persist, etc.

Two variants because the call sites are split between sync generators
(SSE scan + apply) and async coroutines (bulk quick-fix-all). Calling
the sync version inside a running event loop raises; calling the async
version from sync code can't await.
"""
from __future__ import annotations

import asyncio
import logging
from pathlib import Path

from fastapi import HTTPException

from .. import config as config_module
from ..checks import runner as checks_runner

_log = logging.getLogger(__name__)


def library_path_for_checks() -> str:
    """Resolve the configured library directory; 409 if absent."""
    cfg = config_module.load_config()
    if not cfg.library_dir:
        raise HTTPException(status_code=409, detail="No library configured")
    return str(Path(cfg.library_dir).expanduser().resolve())


def run_post_scan_checks(library_dir: str) -> None:
    """Run every check that opted in via runs_during_scan=True. Errors per
    check are logged but never raise out — a flaky check should never break
    the scan flow.

    Called from SYNC generator code (the SSE scan + apply endpoints) running
    on a FastAPI worker thread. Drives each check's coroutine on a fresh
    event loop with `asyncio.run`. **Don't call this from async context** —
    asyncio.run() raises inside a running event loop. Use
    `run_post_scan_checks_async` instead from `async def` callers.
    """
    from ..checks import REGISTRY as _check_registry
    library_path = str(Path(library_dir).expanduser().resolve())
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
            _log.exception(
                "post-scan check %s failed; continuing", cls.meta.id,
            )


async def run_post_scan_checks_async(library_dir: str) -> None:
    """Async-context counterpart to `run_post_scan_checks`. Drives each
    runs_during_scan check directly with `await` so it can be called from
    inside an async generator (the bulk quick-fix-all endpoint, in
    particular). Same error-swallowing semantics: per-check failures get
    logged but never propagate.
    """
    from ..checks import REGISTRY as _check_registry
    library_path = str(Path(library_dir).expanduser().resolve())
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
            _log.exception(
                "post-scan check %s failed; continuing", cls.meta.id,
            )
