"""Background library scan — runs as an asyncio task that survives SSE
client disconnects.

Previously the scan was driven entirely inside the SSE generator: a
client disconnect raised `asyncio.CancelledError` mid-loop, killing the
scan and leaving the cache in a half-built state (especially bad after
a `force=true` rescan that just dropped the snapshot). Now the scan
runs in the background; SSE clients are pure observers, late
subscribers get the full event log, and disconnects don't stop work.

State model:
  - At most one scan runs at a time. Re-issuing `start_scan(force=...)`
    while a scan is in flight returns the existing state (no-op).
  - The event buffer holds every event from the current scan,
    capped at 50_000 (safety net — real libraries top out around a few
    thousand events).
  - When a fresh scan starts, the buffer is cleared.
  - Subscribers ask for events `from_seq=N onward`; the helper yields
    everything already buffered then waits for new arrivals.
"""
from __future__ import annotations

import asyncio
import logging
import threading
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Iterator

from .. import cache as cache_module
from .. import thumbs as thumbs_module
from ..scanner import incremental_scan

_log = logging.getLogger(__name__)

_MAX_BUFFER = 50_000


class _ScanState:
    """All scan-related state — mutated only inside `_lock` to keep
    the SSE subscriber loop coherent with the worker thread's writes."""

    def __init__(self) -> None:
        self.status: str = "idle"  # idle | running | done | error
        self.library_dir: str | None = None
        self.force: bool = False
        self.total: int = 0
        self.scanned: int = 0
        self.cached_count: int = 0
        self.removed_count: int = 0
        self.started_at: str | None = None
        self.completed_at: str | None = None
        self.error: str | None = None
        # (seq, event_name, payload) — capped at _MAX_BUFFER.
        self.events: list[tuple[int, str, dict[str, Any]]] = []
        self._next_seq: int = 0

    def to_dict(self) -> dict[str, Any]:
        return {
            "status": self.status,
            "library_dir": self.library_dir,
            "force": self.force,
            "total": self.total,
            "scanned": self.scanned,
            "cached_count": self.cached_count,
            "removed_count": self.removed_count,
            "started_at": self.started_at,
            "completed_at": self.completed_at,
            "error": self.error,
        }


_state = _ScanState()
_lock = asyncio.Lock()
_changed = asyncio.Event()
_task: asyncio.Task | None = None


def get_state() -> dict[str, Any]:
    """Snapshot of the current scan state. Safe to call from any thread."""
    return _state.to_dict()


def is_running() -> bool:
    return _state.status == "running"


async def start_scan(library_dir: str, *, force: bool) -> dict[str, Any]:
    """Kick off a scan if one isn't already running. Returns the
    current state regardless. Idempotent — repeated calls while a scan
    is in flight return the in-flight state."""
    global _task
    async with _lock:
        if _state.status == "running":
            return _state.to_dict()
        _reset_state(library_dir, force=force)
        _state.status = "running"
        _state.started_at = _utc_iso_now()
        _changed.set()  # let any waiting subscribers see status flip
        _task = asyncio.create_task(_run_scan(library_dir, force), name="library-scan")
        return _state.to_dict()


def _reset_state(library_dir: str, *, force: bool) -> None:
    _state.status = "idle"
    _state.library_dir = library_dir
    _state.force = force
    _state.total = 0
    _state.scanned = 0
    _state.cached_count = 0
    _state.removed_count = 0
    _state.started_at = None
    _state.completed_at = None
    _state.error = None
    _state.events = []
    _state._next_seq = 0


async def subscribe(from_seq: int = 0) -> Iterator[tuple[int, str, dict[str, Any]]]:
    """Async generator over scan events.

    Yields any already-buffered events with `seq >= from_seq` first, then
    waits for new ones. Returns when the scan reports a terminal state
    (done/error) AND all buffered events have been delivered.

    Multiple subscribers can read concurrently — events are append-only.
    """
    last_yielded = from_seq - 1
    while True:
        # Clear BEFORE drain so any worker push that races between drain
        # and wait still wakes us. asyncio.Event.wait() returns instantly
        # when the flag is already set without yielding to the loop —
        # which would starve the call_soon_threadsafe queue feeding
        # _state.events from the worker thread.
        _changed.clear()

        new_events = [e for e in _state.events if e[0] > last_yielded]
        for evt in new_events:
            yield evt
            last_yielded = evt[0]

        # If the scan finished and we've drained everything, we're done.
        if _state.status in ("done", "error") and last_yielded >= _state._next_seq - 1:
            return

        try:
            await asyncio.wait_for(_changed.wait(), timeout=30.0)
        except asyncio.TimeoutError:
            # Heartbeat fall-through; re-check the buffer.
            pass


# Internal: producer-side helpers ------------------------------------------


def _push_event(event: str, payload: dict[str, Any]) -> None:
    """Append an event to the buffer and wake subscribers. Called from
    the worker thread via `loop.call_soon_threadsafe` — the closure
    around `_emit` ensures we touch shared state on the event loop
    thread, not the worker thread."""
    seq = _state._next_seq
    _state._next_seq += 1
    _state.events.append((seq, event, payload))
    if len(_state.events) > _MAX_BUFFER:
        # Drop oldest. Late subscribers will lose pre-trim events but
        # any actively-connected client is already past them.
        _state.events = _state.events[-_MAX_BUFFER:]
    _changed.set()


async def _run_scan(library_dir: str, force: bool) -> None:
    """The async wrapper that drives the sync scan generator on a worker
    thread. Posts each emitted event back to the event loop via
    `call_soon_threadsafe` so subscribers see them immediately."""
    loop = asyncio.get_running_loop()
    done_evt = threading.Event()

    def emit_threadsafe(event: str, payload: dict[str, Any]) -> None:
        loop.call_soon_threadsafe(_push_event, event, payload)
        # Also wake the subscriber loop. _push_event already does this on
        # the loop thread, so no extra signaling needed here.

    try:
        await asyncio.to_thread(_scan_worker, library_dir, force, emit_threadsafe, done_evt)
    except Exception as e:
        _log.exception("scan task failed")
        # The worker thread already pushed an "error" event if it could;
        # set state regardless so subscribers see the terminal status.
        _state.status = "error"
        _state.error = str(e)
        _state.completed_at = _utc_iso_now()
        _changed.set()
        # Publish LibraryScanCompleted even on the error path so
        # `useLibraryEventsStream` invalidates `["albums-snapshot"]`
        # and per-album caches against whatever partial state landed
        # in the DB. Without this, a failed scan could leave an
        # inconsistent snapshot displayed indefinitely (staleTime:
        # Infinity on every bus-driven query).
        try:
            from .. import library_events
            library_events.publish(
                library_events.LibraryScanCompleted(album_count=_state.scanned),
            )
        except Exception:
            _log.exception("library_events publish failed on scan-error path")
        return

    # Worker finished cleanly. Publish LibraryScanCompleted; the
    # `_post_scan_rerun_library_wide` subscriber handles the rerun
    # off the FastAPI worker thread so the scan can report 'done'
    # without waiting on findings to settle. The subscriber's
    # `_findings_runner_notify` peer wakes any open Health page
    # tab once the rerun completes.
    #
    # Phase 2 of move E (issue #1): the prior dual-fire path
    # awaited run_post_scan_checks_async here; the bus subscriber
    # replaces it entirely.
    try:
        from .. import library_events
        library_events.publish(
            library_events.LibraryScanCompleted(album_count=_state.scanned),
        )
    except Exception:
        _log.exception("library_events publish failed (continuing)")

    # Eager fingerprinting: kick off a background pool that walks the
    # library and computes AcoustID fingerprints for albums lacking
    # them. The pool is fire-and-forget — we don't await it here, so
    # the scan reports "done" immediately and the user gets their
    # fresh album list. Skipped silently when fpcalc / AcoustID isn't
    # configured. Stops the previous pool first so a fresh scan that
    # added albums starts a clean pass.
    try:
        from . import fingerprint_pool
        await fingerprint_pool.stop()
        await fingerprint_pool.warm_library(library_dir)
    except Exception:
        _log.exception("Failed to kick off eager fingerprinting (continuing)")

    if _state.status != "error":
        _state.status = "done"
        _state.completed_at = _utc_iso_now()
    _changed.set()


def _scan_worker(
    library_dir: str,
    force: bool,
    emit: "callable[[str, dict[str, Any]], None]",
    done_evt: threading.Event,
) -> None:
    """Synchronous scan body — moved verbatim from the old SSE generator
    in `api/library.py`. Calls `emit(event, payload)` for each yielded
    event. Runs on a worker thread (via `asyncio.to_thread`)."""
    thumb_pool = ThreadPoolExecutor(max_workers=4, thread_name_prefix="mm-thumb")
    try:
        cached_by_path: dict[str, dict[str, Any]] = {}
        if not force:
            snap = cache_module.load_snapshot(library_dir)
            cached_by_path = dict(snap.albums_by_path)

        collected: dict[str, dict[str, Any]] = {}
        removed: list[dict[str, Any]] = []
        total = 0
        scanned = 0
        started = False

        for status, payload in incremental_scan(library_dir, cached_by_path):
            if status == "discovered":
                total = payload["total"]
                _state.total = total
                _state.cached_count = len(cached_by_path)
                emit("start", {
                    "library_dir": library_dir,
                    "total": total,
                    "cached_count": len(cached_by_path),
                    "force": force,
                })
                started = True
                continue

            if status == "removed":
                removed.append({"id": payload.get("id"), "path": payload.get("path")})
                if payload.get("id"):
                    thumbs_module.delete_thumb(payload["id"])
                _state.removed_count += 1
                emit("removed", {"id": payload.get("id"), "path": payload.get("path")})
                continue

            # "cached" or "fresh" → an album dict
            album = dict(payload)
            album["_status"] = status
            collected[album["path"]] = album
            scanned += 1
            _state.scanned = scanned

            force_thumb = (status == "fresh")
            thumb_pool.submit(thumbs_module.ensure_thumb, album, force=force_thumb)

            emit("album", album)
            step = 1 if total < 200 else max(1, total // 100)
            if scanned == total or scanned % step == 0:
                emit("progress", {"scanned": scanned, "total": total})

        if not started:
            emit("start", {
                "library_dir": library_dir, "total": 0, "cached_count": 0, "force": force,
            })

        scanned_at = _utc_iso_now()
        new_snap = cache_module.LibrarySnapshot(
            library_dir=library_dir,
            scanned_at=scanned_at,
            albums_by_path={p: {k: v for k, v in a.items() if k != "_status"} for p, a in collected.items()},
        )
        cache_module.save_snapshot(new_snap)

        try:
            imported = cache_module.import_track_tag_sidecars(new_snap)
            if imported:
                _log.info("Imported track tag sidecars for %d album(s)", imported)
        except Exception:
            _log.exception("track tag sidecar import failed")

        emit("done", {
            "count": len(collected),
            "removed_count": len(removed),
            "scanned_at": scanned_at,
        })
    except Exception as e:
        _log.exception("scan worker failed")
        emit("error", {"detail": str(e)})
        # Re-raise so _run_scan flips state to error.
        raise
    finally:
        thumb_pool.shutdown(wait=True, cancel_futures=False)
        done_evt.set()


def _utc_iso_now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")
