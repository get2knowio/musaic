"""Post-scan check fan-out — runs every `runs_during_scan=True` check
across the library (or a per-album subset) and lets the runner's
auto-resolve clear satisfied findings.

This module is the LOW-LEVEL worker. The CANONICAL entry point is the
`library_events` bus + the `_post_scan_rerun` subscriber, which
drives the right scoping based on what event was published. Direct
calls to `run_post_scan_checks` from outside `event_subscribers.py`
should be rare and well-justified — the bus exists specifically to
make the right reruns happen automatically without producers having
to remember.

----------------------------------------------------------------------
How state mutations propagate today
----------------------------------------------------------------------

The flow:

  1. A producer mutates state (writes tags, sets an MB anchor,
     fingerprints an album, renames files, etc.).
  2. The producer publishes a `library_events` event naming the
     domain change — `AlbumTagsChanged`, `AlbumMbAnchorSet`,
     `TrackFingerprinted`, `AlbumRenamed`, `AlbumDeleted`,
     `LibraryScanCompleted`, `BulkBatchCompleted`.
  3. Subscribers in `services.event_subscribers` fan out the
     concerns the producer used to invoke manually:
       * `_resolver_invalidate` — drops the per-album resolver memo
       * `_thumb_drop` — drops the album's pre-extracted thumb
       * `_post_scan_rerun` — calls THIS module's `run_post_scan_
         checks` (background thread, scope_kind='albums' for album-
         scoped events, 'all' for library-wide ones)
       * `_filename_mismatch_rerun` — the one non-runs_during_scan
         check the bus drives (per-album on AlbumRenamed)
       * `_findings_runner_notify` — wakes any open SSE
         /api/checks/stream socket so the UI refetches

  4. The producer keeps going. The bus has done its job before the
     producer's HTTP response is returned (subscribers run inline
     or get a background thread spawned).

The producer's only responsibility is `publish(...)`. The bus handles
the rest, identically for every callsite. This was move E from issue
#1; the migration retired ~150 lines of ad-hoc fan-out across 9
producers and made the stale-findings bug class mechanically
unreachable for new code paths.

----------------------------------------------------------------------
Recursion guard
----------------------------------------------------------------------

Both `run_post_scan_checks` and `run_post_scan_checks_async` set
`library_events.in_post_scan_rerun` for the duration of the call.
Subscribers consult the flag and skip when it's set — this prevents
the runner from re-entering itself when a check writes data that
publishes an event that would route back here. The flag inherits
into background subscriber threads via `contextvars.copy_context().
run(...)` (the bus's responsibility — see `library_events.py`).

Idempotency is the real safety net. Even if the guard fails open,
the second pass through the runner finds the same state (Tier 0a MB
anchor cache hits replace Tier 0b/0c side-effect writes; tags-out-of-
sync sees no new diffs; etc.). The guard is an optimization, not a
correctness mechanism.

----------------------------------------------------------------------
Two variants
----------------------------------------------------------------------

Two functions because the call sites are split between sync generators
(SSE scan + apply) and async coroutines (bulk dispatcher). Calling the
sync version inside a running event loop raises (asyncio.run does);
calling the async version from sync code can't await. Both honor the
same `album_ids` keyword for scoped reruns.
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


def run_post_scan_checks(
    library_dir: str,
    *,
    album_ids: list[str] | None = None,
) -> None:
    """Run every check that opted in via runs_during_scan=True. Errors per
    check are logged but never raise out — a flaky check should never break
    the scan flow.

    `album_ids`, when supplied, scopes every check to those albums
    (`scope_kind="albums"`). Default `None` runs the whole library
    (`scope_kind="all"`). The runner's auto-resolve honors the scope so
    a per-album rerun doesn't over-resolve unrelated findings.

    Sets `library_events.in_post_scan_rerun` for the duration of the
    call. This guards every code path that publishes events from
    inside a runs_during_scan check — without it, the dual-fire from
    direct callers (scan tail, apply background thread) would spawn
    follow-on `_post_scan_rerun` subscribers that do duplicate work.
    Idempotent against the bus subscriber's own check (the subscriber
    early-exits when the flag is set; this function then sets it
    again on entry — same result). See
    `docs/library_events_design.md#second-design-review-pass-on-the-
    guard` for the full reasoning.

    Called from SYNC generator code (the SSE scan + apply endpoints) running
    on a FastAPI worker thread. Drives each check's coroutine on a fresh
    event loop with `asyncio.run`. **Don't call this from async context** —
    asyncio.run() raises inside a running event loop. Use
    `run_post_scan_checks_async` instead from `async def` callers.
    """
    # Cheap self-guard: if some caller managed to invoke this from
    # inside another post-scan rerun, return rather than recursing.
    # Defensive — the subscriber's early-exit already covers the
    # bus-driven path, but a stray direct call inside a check.run()
    # would otherwise fall through.
    from .. import library_events
    if library_events.in_post_scan_rerun.get():
        _log.debug(
            "run_post_scan_checks: skipped (already inside one) "
            "for library_dir=%s album_ids=%s", library_dir, album_ids,
        )
        return

    from ..checks import REGISTRY as _check_registry
    library_path = str(Path(library_dir).expanduser().resolve())
    scope_kind = "albums" if album_ids else "all"
    token = library_events.in_post_scan_rerun.set(True)
    try:
        for cls in _check_registry:
            if not cls.meta.runs_during_scan:
                continue
            try:
                asyncio.run(checks_runner.run_check(
                    cls.meta.id,
                    library_path=library_path,
                    scope_kind=scope_kind,
                    album_ids=album_ids,
                ))
            except Exception:
                _log.exception(
                    "post-scan check %s failed; continuing", cls.meta.id,
                )
    finally:
        library_events.in_post_scan_rerun.reset(token)


async def run_post_scan_checks_async(
    library_dir: str,
    *,
    album_ids: list[str] | None = None,
) -> None:
    """Async-context counterpart to `run_post_scan_checks`. Drives each
    runs_during_scan check directly with `await` so it can be called from
    inside an async generator (the bulk quick-fix-all endpoint, in
    particular). Same error-swallowing semantics: per-check failures get
    logged but never propagate.

    `album_ids` behaves identically to the sync variant — scope to that
    set when supplied, whole library when None.

    Sets `library_events.in_post_scan_rerun` for the duration of the
    coroutine — same posture as the sync variant. asyncio tasks
    inherit the parent task's context, so events published inside
    `await checks_runner.run_check(...)` see the flag and their
    background subscribers do too (via the bus's copy_context wrap).
    """
    from .. import library_events
    if library_events.in_post_scan_rerun.get():
        _log.debug(
            "run_post_scan_checks_async: skipped (already inside one) "
            "for library_dir=%s album_ids=%s", library_dir, album_ids,
        )
        return

    from ..checks import REGISTRY as _check_registry
    library_path = str(Path(library_dir).expanduser().resolve())
    scope_kind = "albums" if album_ids else "all"
    token = library_events.in_post_scan_rerun.set(True)
    try:
        for cls in _check_registry:
            if not cls.meta.runs_during_scan:
                continue
            try:
                await checks_runner.run_check(
                    cls.meta.id,
                    library_path=library_path,
                    scope_kind=scope_kind,
                    album_ids=album_ids,
                )
            except Exception:
                _log.exception(
                    "post-scan check %s failed; continuing", cls.meta.id,
                )
    finally:
        library_events.in_post_scan_rerun.reset(token)
