"""Post-scan check fan-out — run every check that opted in via
`runs_during_scan=True` after a scan, apply, AI MBID persist, etc.

Two variants because the call sites are split between sync generators
(SSE scan + apply) and async coroutines (bulk quick-fix-all). Calling
the sync version inside a running event loop raises; calling the async
version from sync code can't await.

----------------------------------------------------------------------
The "stale findings" anti-pattern
----------------------------------------------------------------------

Health-check findings stay in the DB until a check actually re-runs and
discovers their underlying condition has gone away. The runner's
auto-resolve only fires for findings whose check ran on the same
library_path during the same call — there is no global "something
changed, re-evaluate everything" hook.

That makes any state-mutating helper that satisfies a check's invariant
**caller-responsible** for triggering a post-scan rerun. Helpers that
fall in this category:

  - `cache.upsert_album` / `cache.delete_album`  (album row state)
  - `cache.set_album_mb_anchor`                  (MB anchor — gates
                                                 unidentified-album +
                                                 tags-out-of-sync)
  - `fingerprint.fingerprint_album`              (track fingerprints —
                                                 gates unidentified-album)
  - `tags.apply_to_album_iter`                   (Vorbis comments — gates
                                                 tags-out-of-sync,
                                                 inconsistent-artist-sort)
  - on-disk renames                              (gates filename-mismatch)

Any caller of those helpers MUST call `run_post_scan_checks` (or its
async variant) afterwards or the satisfied finding will linger until the
next library scan. `services.apply.apply_album_changes_iter` already
does this — most call sites should funnel through it. The exceptions
that need to fire post-scan themselves:

  - the AI worker (writes anchor without going through apply)
  - the synchronous fix-finding endpoint (resolver-driven fixes don't
    go through apply)
  - the eager fingerprint pool (fingerprints in bulk after a scan)
  - the audio-analysis service (writes BPM/RG tags directly)
  - the resolver itself (Tier 0b/0c persists anchors as a side effect
    of read paths like the album-sheet badge query)

If you add a new state-mutating path, decide which bucket it lives in.
The grep `grep -rn 'run_post_scan_checks\\|apply_album_changes_iter' musaic/`
should show the path covered by one of these.
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
