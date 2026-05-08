"""Eager fingerprinting pool — runs after each library scan to populate
`track_fingerprints` rows for albums that don't yet have them.

Why: the metadata resolver's Tier 0c (fingerprint vote) anchors albums
whose folder names don't match MusicBrainz's canonical form, but only
when fingerprints exist. Computing them on demand inside the resolver
adds noticeable latency (~1s per track plus AcoustID round-trip) the
first time a user opens an album. Eager warming keeps that path
snappy AND means a "Run all checks" pass picks up the
fingerprint-vote benefit immediately instead of one-album-at-a-time.

Lazy filling stays as the fallback — anything the eager pool didn't
get to (or that arrived after the most recent scan) gets handled by
the resolver's on-demand call.

Skipped silently when fpcalc / AcoustID isn't configured. Albums are
processed serially; per-track AcoustID HTTP lookups inside
`fingerprint_album` naturally pace via the underlying request budget.
"""
from __future__ import annotations

import asyncio
import logging
import threading
from datetime import datetime, timezone
from typing import Any

from .. import db as db_module
from .. import fingerprint as fp_module

_log = logging.getLogger(__name__)


# Single shared state record. Sized for one-pool-at-a-time semantics
# (which matches the typical post-scan invocation). Mutated only inside
# `_state_lock`; readers should also take the lock for a coherent
# snapshot.
_state: dict[str, Any] = {
    "status": "idle",        # idle | running | done | cancelled | unavailable
    "library_dir": None,
    "total": 0,
    "completed": 0,
    "errors": 0,
    "current_album_id": None,
    "started_at": None,
    "completed_at": None,
    "skip_reason": None,     # populated when status == "unavailable"
}
_state_lock = threading.Lock()
_task: asyncio.Task | None = None


def get_state() -> dict[str, Any]:
    with _state_lock:
        return dict(_state)


def is_running() -> bool:
    return _state["status"] == "running"


async def warm_library(library_dir: str) -> dict[str, Any]:
    """Kick off background fingerprinting for every album in `library_dir`
    lacking complete fingerprints. Idempotent: if a pool is already
    running, returns the current state without starting another. Returns
    immediately — actual work happens on the asyncio.create_task'd
    coroutine.

    Status flips to:
      - "unavailable" if fpcalc / AcoustID API key isn't configured
      - "running" while the pool is iterating
      - "done"  when the pool completes (with possibly nonzero `errors`)
      - "cancelled" if `stop()` interrupted it
    """
    global _task

    if is_running():
        return get_state()

    available, reason = fp_module.is_available()
    if not available:
        with _state_lock:
            _state.update({
                "status": "unavailable",
                "library_dir": library_dir,
                "skip_reason": reason,
                "started_at": _utc_iso_now(),
                "completed_at": _utc_iso_now(),
                "total": 0,
                "completed": 0,
                "errors": 0,
                "current_album_id": None,
            })
        _log.info("Eager fingerprinting unavailable: %s", reason)
        return get_state()

    with _state_lock:
        _state.update({
            "status": "running",
            "library_dir": library_dir,
            "total": 0,
            "completed": 0,
            "errors": 0,
            "current_album_id": None,
            "started_at": _utc_iso_now(),
            "completed_at": None,
            "skip_reason": None,
        })
    _task = asyncio.create_task(_run_pool(library_dir), name="fingerprint-pool")
    return get_state()


async def stop() -> None:
    """Cancel the in-flight pool. Returns when the task is fully gone.
    Safe to call when nothing's running."""
    global _task
    if _task and not _task.done():
        _task.cancel()
        try:
            await _task
        except asyncio.CancelledError:
            pass
    _task = None


def _candidates(library_dir: str) -> list[tuple[str, str]]:
    """Albums in `library_dir` that need fingerprinting — those whose
    track count exceeds the number of `track_fingerprints` rows. Returns
    a list of (album_id, album_path) tuples.

    Uses a LEFT JOIN so albums with zero fingerprints (the common case
    for a freshly-scanned library) appear with fp_count=0.
    """
    sql = """
        SELECT a.id, a.path, a.track_count,
               COALESCE(fp.fp_count, 0) AS fp_count
        FROM albums a
        LEFT JOIN (
            SELECT album_id, COUNT(*) AS fp_count
            FROM track_fingerprints
            GROUP BY album_id
        ) fp ON a.id = fp.album_id
        WHERE a.library_path = ?
          AND a.track_count > 0
          AND a.track_count > COALESCE(fp.fp_count, 0)
    """
    with db_module.connect() as conn:
        rows = conn.execute(sql, (library_dir,)).fetchall()
    return [(r["id"], r["path"]) for r in rows]


async def _run_pool(library_dir: str) -> None:
    try:
        candidates = _candidates(library_dir)
        with _state_lock:
            _state["total"] = len(candidates)
        _log.info(
            "Eager fingerprinting starting: %d album(s) need fingerprints",
            len(candidates),
        )

        for album_id, album_path in candidates:
            with _state_lock:
                _state["current_album_id"] = album_id
            try:
                await asyncio.to_thread(
                    fp_module.fingerprint_album, album_id, album_path,
                )
                with _state_lock:
                    _state["completed"] += 1
            except fp_module.FingerprintError as e:
                # Per-album failures (missing dir, individual fpcalc
                # error) are expected — log once and continue. A flaky
                # FLAC shouldn't kill the whole pool.
                _log.info("Eager fingerprint skipped for %s: %s", album_id, e)
                with _state_lock:
                    _state["errors"] += 1
            except Exception:
                _log.exception("Unexpected error fingerprinting %s", album_id)
                with _state_lock:
                    _state["errors"] += 1
            # Yield to the loop. AcoustID's rate budget is shared across
            # all our requests; a small inter-album sleep is cheap
            # politeness.
            await asyncio.sleep(0.5)
    except asyncio.CancelledError:
        _log.info(
            "Eager fingerprinting cancelled (was at album %s)",
            _state.get("current_album_id"),
        )
        with _state_lock:
            _state["status"] = "cancelled"
        raise
    finally:
        with _state_lock:
            _state["current_album_id"] = None
            _state["completed_at"] = _utc_iso_now()
            if _state["status"] == "running":
                _state["status"] = "done"
                _log.info(
                    "Eager fingerprinting done: completed=%d errors=%d",
                    _state["completed"], _state["errors"],
                )


def _utc_iso_now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")
