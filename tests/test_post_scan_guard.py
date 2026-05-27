"""Tests for the recursion guard on `run_post_scan_checks`.

This is the hardening pass after Phase 1 of library_events. The
subscriber's early-exit catches bus-driven cases, but direct
callers (scan tail, apply background thread, sync fix endpoint)
also needed coverage — otherwise a check publishing events from
inside a direct rerun spawns follow-on subscribers that duplicate
the work.

The invariant: while `run_post_scan_checks` (sync OR async) is
in-flight on the current dispatch chain, any nested call returns
immediately, and any background `_post_scan_rerun` subscriber
spawned from inside it sees the flag and skips.
"""
from __future__ import annotations

import asyncio
import threading
import time

import pytest

from musaic import library_events as bus
from musaic.services import post_scan


@pytest.fixture(autouse=True)
def _isolated_bus():
    bus.reset_for_tests()
    yield
    bus.reset_for_tests()


def _wait_for_threads(timeout: float = 1.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        active = [
            t for t in threading.enumerate()
            if t.name.startswith("library_events:")
        ]
        if not active:
            return
        time.sleep(0.005)
    raise AssertionError("timed out waiting for background subscribers")


# ---------- Sync variant ----------


def test_sync_sets_and_resets_flag(monkeypatch, tmp_path):
    """The flag is True for the duration of the rerun and back to
    False afterwards (the finally: reset MUST run)."""
    seen_inside: list[bool] = []

    # Stub the registry so we don't actually run real checks.
    monkeypatch.setattr(post_scan, "checks_runner", _FakeRunner(seen_inside))
    monkeypatch.setattr(
        "musaic.checks.REGISTRY",
        [_FakeCheckCls("a", runs_during_scan=True)],
    )

    assert bus.in_post_scan_rerun.get() is False
    post_scan.run_post_scan_checks(str(tmp_path))
    assert bus.in_post_scan_rerun.get() is False
    # Inside the check.run() call (driven by our fake runner), the
    # flag was True.
    assert seen_inside == [True]


def test_sync_nested_call_short_circuits(monkeypatch, tmp_path):
    """A direct call to run_post_scan_checks from inside another one
    (which can happen if a check.run() naively calls it) returns
    immediately without invoking the runner a second time."""
    runner_call_count = {"n": 0}

    def fake_run_check(check_id, **kw):
        runner_call_count["n"] += 1
        # Inside the first rerun, try to call run_post_scan_checks
        # again — this must short-circuit.
        post_scan.run_post_scan_checks(str(tmp_path))
        return _async_none()

    monkeypatch.setattr(
        post_scan.checks_runner, "run_check", fake_run_check,
    )
    monkeypatch.setattr(
        "musaic.checks.REGISTRY",
        [_FakeCheckCls("a", runs_during_scan=True)],
    )

    post_scan.run_post_scan_checks(str(tmp_path))
    # The runner was called exactly once. The nested call returned
    # without iterating the registry again.
    assert runner_call_count["n"] == 1


def test_sync_flag_resets_even_if_check_raises(monkeypatch, tmp_path):
    """The finally: reset must run even when a check explodes —
    otherwise the contextvar leaks to the next call on this thread."""
    def boom(check_id, **kw):
        # Wrap raise in a coroutine so asyncio.run sees it.
        async def _boom():
            raise RuntimeError("intentional")
        return _boom()

    monkeypatch.setattr(
        post_scan.checks_runner, "run_check", boom,
    )
    monkeypatch.setattr(
        "musaic.checks.REGISTRY",
        [_FakeCheckCls("a", runs_during_scan=True)],
    )

    post_scan.run_post_scan_checks(str(tmp_path))
    # Check raised, post_scan logged + continued, flag reset.
    assert bus.in_post_scan_rerun.get() is False


def test_sync_publish_from_inside_check_does_not_spawn_subscriber_rerun(
    monkeypatch, tmp_path,
):
    """The whole point of the harden: when a check inside a direct
    `run_post_scan_checks` call publishes an event, the bus's
    background `_post_scan_rerun` subscriber MUST see the flag
    (inherited via copy_context) and skip — otherwise we spawn a
    redundant rerun thread per published event."""
    rerun_count = {"n": 0}

    # Stand-in for the real `_post_scan_rerun` subscriber: checks
    # the flag, increments only when it would have called rerun.
    def fake_rerun_subscriber(ev):
        if bus.in_post_scan_rerun.get():
            return
        rerun_count["n"] += 1

    bus.register(
        bus.AlbumScopedEvent, fake_rerun_subscriber,
        background=True, priority=200,
    )

    def fake_run_check(check_id, **kw):
        # Inside a check.run(), publish a side-effect event.
        bus.publish(bus.AlbumMbAnchorSet(album_id="a1", source="x"))
        return _async_none()

    monkeypatch.setattr(post_scan.checks_runner, "run_check", fake_run_check)
    monkeypatch.setattr(
        "musaic.checks.REGISTRY",
        [_FakeCheckCls("a", runs_during_scan=True)],
    )

    post_scan.run_post_scan_checks(str(tmp_path))
    _wait_for_threads()
    # Background subscriber inherited the flag → no extra rerun.
    assert rerun_count["n"] == 0


# ---------- Async variant ----------


def test_async_sets_and_resets_flag(monkeypatch, tmp_path):
    seen_inside: list[bool] = []

    async def fake_run_check(check_id, **kw):
        seen_inside.append(bus.in_post_scan_rerun.get())

    monkeypatch.setattr(post_scan.checks_runner, "run_check", fake_run_check)
    monkeypatch.setattr(
        "musaic.checks.REGISTRY",
        [_FakeCheckCls("a", runs_during_scan=True)],
    )

    asyncio.run(post_scan.run_post_scan_checks_async(str(tmp_path)))
    assert bus.in_post_scan_rerun.get() is False
    assert seen_inside == [True]


def test_async_nested_call_short_circuits(monkeypatch, tmp_path):
    runner_call_count = {"n": 0}

    async def fake_run_check(check_id, **kw):
        runner_call_count["n"] += 1
        # Nested await — must short-circuit via the flag.
        await post_scan.run_post_scan_checks_async(str(tmp_path))

    monkeypatch.setattr(post_scan.checks_runner, "run_check", fake_run_check)
    monkeypatch.setattr(
        "musaic.checks.REGISTRY",
        [_FakeCheckCls("a", runs_during_scan=True)],
    )

    asyncio.run(post_scan.run_post_scan_checks_async(str(tmp_path)))
    assert runner_call_count["n"] == 1


# ---------- Test helpers ----------


class _FakeCheckCls:
    """Stand-in for a Check class with the minimum `.meta` interface
    `run_post_scan_checks` reads."""
    def __init__(self, check_id: str, *, runs_during_scan: bool):
        self.meta = type("M", (), {
            "id": check_id,
            "runs_during_scan": runs_during_scan,
        })()


class _FakeRunner:
    """Stub for `checks_runner` that records whether the flag was
    set when `run_check` was invoked."""
    def __init__(self, seen_inside: list[bool]):
        self.seen_inside = seen_inside

    def run_check(self, check_id, **kw):
        self.seen_inside.append(bus.in_post_scan_rerun.get())
        return _async_none()


def _async_none():
    """Return a coroutine that does nothing — so asyncio.run inside
    run_post_scan_checks has something to drive."""
    async def _noop():
        return None
    return _noop()
