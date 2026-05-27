"""Tests for the eager fingerprinting pool.

Real `fpcalc` work needs the binary + AcoustID API key + actual FLAC
fixtures, none of which we can stage in tests. So we cover the
control-plane behavior:
  - skips silently when fpcalc / AcoustID is unavailable (the common
    case on installs without the optional dep)
  - is idempotent: a second `warm_library` call while the first is
    running returns the existing state without spawning a duplicate
  - `stop()` is a no-op when nothing's running
"""
from __future__ import annotations

import asyncio

import pytest

from musaic import fingerprint as fp_module
from musaic.services import fingerprint_pool


@pytest.fixture(autouse=True)
def _reset_pool_state():
    """Reset the singleton state between tests so the previous run
    doesn't bleed into the next one."""
    fingerprint_pool._state.update({
        "status": "idle",
        "library_dir": None,
        "total": 0,
        "completed": 0,
        "errors": 0,
        "current_album_id": None,
        "started_at": None,
        "completed_at": None,
        "skip_reason": None,
    })
    fingerprint_pool._task = None
    yield
    fingerprint_pool._task = None


def test_warm_library_marks_unavailable_when_fpcalc_missing(monkeypatch):
    """When fp_module.is_available reports no fpcalc / no API key, the
    pool short-circuits to "unavailable" without scheduling any work."""
    monkeypatch.setattr(
        fp_module, "is_available",
        lambda: (False, "fpcalc binary not found"),
    )
    state = asyncio.run(fingerprint_pool.warm_library("/some/library"))
    assert state["status"] == "unavailable"
    assert state["skip_reason"] == "fpcalc binary not found"
    assert state["library_dir"] == "/some/library"
    # No background task should have been spawned.
    assert fingerprint_pool._task is None


def test_warm_library_is_idempotent_while_running(monkeypatch):
    """A second warm_library call while one is already running returns
    the in-flight state instead of spawning another task."""
    # Pretend a pool is already running.
    fingerprint_pool._state["status"] = "running"
    fingerprint_pool._state["library_dir"] = "/lib"

    monkeypatch.setattr(
        fp_module, "is_available", lambda: (True, "ok"),
    )
    state = asyncio.run(fingerprint_pool.warm_library("/lib"))
    assert state["status"] == "running"
    # No new task spawned.
    assert fingerprint_pool._task is None


def test_stop_is_noop_when_nothing_running():
    """stop() must be safe to call when no pool is in flight (used by
    the lifespan shutdown handler regardless of pool state)."""
    asyncio.run(fingerprint_pool.stop())
    assert fingerprint_pool._task is None


def test_warm_library_signals_changed_event_on_unavailable(monkeypatch):
    """The SSE stream awaits `changed_event()`; flipping to
    "unavailable" should wake it. Without this signal, the stream
    would only fire once every 30s (keepalive timeout) on installs
    without fpcalc — a noticeable lag in the titlebar pill."""
    monkeypatch.setattr(
        fp_module, "is_available",
        lambda: (False, "fpcalc binary not found"),
    )
    # Make sure the changed event starts clear so we observe the set.
    fingerprint_pool.changed_event().clear()
    asyncio.run(fingerprint_pool.warm_library("/some/library"))
    assert fingerprint_pool.changed_event().is_set()
