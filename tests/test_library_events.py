"""Phase 1 tests for `library_events`.

The three load-bearing tests gating Phase 1 (per the design doc's
recursion-guard review) are:

  1. test_in_flight_rerun_short_circuits_nested_publish
  2. test_background_subscriber_inherits_in_flight_flag
  3. test_concurrent_unrelated_rerun_is_not_suppressed

If any of these turn red, the guard's framing is wrong and we
revisit before Phase 2.

Plus a battery of basic dispatch / ordering / isolation tests so
producers can rely on the contract.
"""
from __future__ import annotations

import contextvars
import threading
import time
from dataclasses import dataclass

import pytest

from musaic import library_events as bus
from musaic.library_events import (
    AlbumCoverChanged,
    AlbumDeleted,
    AlbumMbAnchorSet,
    AlbumRenamed,
    AlbumScopedEvent,
    AlbumTagsChanged,
    Event,
    LibraryScanCompleted,
    TrackFingerprinted,
)


@pytest.fixture(autouse=True)
def _clean_bus():
    """Each test starts with a fresh subscriber registry."""
    bus.reset_for_tests()
    yield
    bus.reset_for_tests()


def _wait_for_threads(timeout: float = 1.0) -> None:
    """Spin until every library_events: daemon thread has exited.
    Background subscribers run in named threads so we can identify
    ours without waiting on every daemon in the process."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        active = [
            t for t in threading.enumerate()
            if t.name.startswith("library_events:")
        ]
        if not active:
            return
        time.sleep(0.005)
    raise AssertionError(
        f"timed out waiting for background subscribers: "
        f"{[t.name for t in threading.enumerate() if t.name.startswith('library_events:')]}"
    )


# ---------- Basic dispatch ----------


def test_sync_subscriber_receives_event():
    seen: list[Event] = []
    bus.register(AlbumTagsChanged, lambda ev: seen.append(ev))
    ev = AlbumTagsChanged(album_id="a1", fields=("title",))
    bus.publish(ev)
    assert seen == [ev]


def test_subscriber_for_base_class_catches_subclass_events():
    """Registering for AlbumScopedEvent should catch every album-
    scoped subclass — that's how `_post_scan_rerun` plans to subscribe
    so a single registration covers tags / cover / anchor / fingerprint
    / delete events."""
    seen: list[AlbumScopedEvent] = []
    bus.register(AlbumScopedEvent, lambda ev: seen.append(ev))

    bus.publish(AlbumTagsChanged(album_id="a1"))
    bus.publish(AlbumCoverChanged(album_id="a2"))
    bus.publish(AlbumMbAnchorSet(album_id="a3", source="cache"))
    bus.publish(AlbumDeleted(album_id="a4"))
    # Library-scoped event should NOT match.
    bus.publish(LibraryScanCompleted(album_count=42))

    assert [ev.album_id for ev in seen] == ["a1", "a2", "a3", "a4"]


def test_no_subscribers_is_a_no_op():
    """Publishing into a void must not raise — producers shouldn't
    have to care whether anyone's listening."""
    bus.publish(AlbumTagsChanged(album_id="a1"))  # asserts no raise


def test_unregister_removes_only_the_matching_callback():
    seen: list[str] = []
    cb1 = lambda ev: seen.append("cb1")
    cb2 = lambda ev: seen.append("cb2")
    bus.register(AlbumTagsChanged, cb1)
    bus.register(AlbumTagsChanged, cb2)
    bus.unregister(AlbumTagsChanged, cb1)

    bus.publish(AlbumTagsChanged(album_id="a"))
    assert seen == ["cb2"]


# ---------- Ordering ----------


def test_subscribers_run_in_priority_order():
    """Lower priority runs first — the design assigns invalidation
    (100) before mutation (200) before notification (300) so the
    UI never refetches stale data."""
    order: list[int] = []
    bus.register(AlbumTagsChanged, lambda ev: order.append(300), priority=300)
    bus.register(AlbumTagsChanged, lambda ev: order.append(100), priority=100)
    bus.register(AlbumTagsChanged, lambda ev: order.append(200), priority=200)
    bus.publish(AlbumTagsChanged(album_id="a"))
    assert order == [100, 200, 300]


def test_base_and_subclass_subscribers_interleave_by_priority():
    """A high-priority subscriber on the BASE class must still run
    before a low-priority one on a SUBCLASS — the bus has to
    re-sort across the type hierarchy, not just within each level."""
    order: list[str] = []
    bus.register(
        AlbumScopedEvent,
        lambda ev: order.append("base@100"),
        priority=100,
    )
    bus.register(
        AlbumTagsChanged,
        lambda ev: order.append("sub@300"),
        priority=300,
    )
    bus.publish(AlbumTagsChanged(album_id="a"))
    assert order == ["base@100", "sub@300"]


# ---------- Isolation ----------


def test_subscriber_exception_does_not_block_other_subscribers():
    """One bad subscriber must not kill the dispatch — events are
    facts, not commands; we keep going."""
    seen: list[str] = []

    def boom(ev):
        seen.append("boom")
        raise RuntimeError("intentional")

    bus.register(AlbumTagsChanged, boom, priority=100)
    bus.register(
        AlbumTagsChanged, lambda ev: seen.append("ok"), priority=200,
    )
    bus.publish(AlbumTagsChanged(album_id="a"))
    # Both subscribers fired; the second one wasn't blocked.
    assert seen == ["boom", "ok"]


# ---------- Background dispatch ----------


def test_background_subscriber_runs_off_publishers_thread():
    pub_thread = threading.current_thread().name
    seen_thread: list[str] = []
    done = threading.Event()

    def cb(ev):
        seen_thread.append(threading.current_thread().name)
        done.set()

    bus.register(AlbumTagsChanged, cb, background=True)
    bus.publish(AlbumTagsChanged(album_id="a"))
    assert done.wait(1.0), "background subscriber didn't run"
    assert seen_thread[0] != pub_thread
    assert seen_thread[0].startswith("library_events:")
    _wait_for_threads()


# ---------- Recursion guard (load-bearing) ----------


def test_in_flight_rerun_short_circuits_nested_publish():
    """LOAD-BEARING #1.

    Scenario: a subscriber that consults `in_post_scan_rerun` is
    inside a dispatch chain that re-publishes the same event type.
    The nested call must see the flag and short-circuit; otherwise
    we recurse indefinitely.

    Models the real-world case: `_post_scan_rerun` runs checks that
    write anchors that publish AlbumMbAnchorSet that route back to
    `_post_scan_rerun`.
    """
    call_count = {"n": 0}

    def guarded_rerun(ev):
        if bus.in_post_scan_rerun.get():
            return
        call_count["n"] += 1
        token = bus.in_post_scan_rerun.set(True)
        try:
            # Simulate a check writing an anchor, which republishes:
            bus.publish(AlbumMbAnchorSet(album_id=ev.album_id, source="x"))
        finally:
            bus.in_post_scan_rerun.reset(token)

    bus.register(AlbumScopedEvent, guarded_rerun)
    bus.publish(AlbumTagsChanged(album_id="a1"))
    # Guard fires once; the inner publish sees the flag and skips.
    assert call_count["n"] == 1


def test_background_subscriber_inherits_in_flight_flag():
    """LOAD-BEARING #2.

    Contextvars do NOT auto-propagate to new threads. The bus must
    wrap background subscribers in `copy_context().run(...)` or the
    guard silently fails open. This is the trap the design-review
    pass flagged explicitly.

    Models: publisher sets the flag, then publishes a background
    event; the background subscriber MUST see the same flag value
    as the publisher's frame.
    """
    seen_flag: list[bool] = []
    done = threading.Event()

    def cb(ev):
        seen_flag.append(bus.in_post_scan_rerun.get())
        done.set()

    bus.register(AlbumTagsChanged, cb, background=True)
    # Publisher's frame sets the flag (mimics "we're inside a rerun").
    token = bus.in_post_scan_rerun.set(True)
    try:
        bus.publish(AlbumTagsChanged(album_id="a"))
    finally:
        bus.in_post_scan_rerun.reset(token)
    assert done.wait(1.0), "background subscriber didn't run"
    assert seen_flag == [True], (
        "background subscriber didn't inherit contextvar — bus is "
        "missing copy_context().run() wrap and the recursion guard "
        "would silently fail open"
    )
    _wait_for_threads()


def test_concurrent_unrelated_rerun_is_not_suppressed():
    """LOAD-BEARING #3.

    The false-positive guard. An unrelated request running in a
    different thread must NOT be suppressed just because someone
    somewhere set the contextvar. The guard is per-dispatch-chain,
    not per-process.

    Models: a long-running checks-stream subscriber is rendering
    findings (sets the flag in its thread); meanwhile a user clicks
    Fix on an unrelated finding (publishes from a different thread).
    The fix's downstream rerun MUST proceed.
    """
    other_thread_saw_flag: list[bool] = []
    other_thread_done = threading.Event()

    def cb(ev):
        # Read the flag from the publisher's context (whatever it
        # was at publish time). We expect default=False here because
        # the publishing thread never set it — even though a
        # DIFFERENT thread did.
        other_thread_saw_flag.append(bus.in_post_scan_rerun.get())
        other_thread_done.set()

    bus.register(AlbumTagsChanged, cb)

    # Thread A: sets the flag and holds it.
    flag_held = threading.Event()
    release_flag = threading.Event()

    def thread_a():
        token = bus.in_post_scan_rerun.set(True)
        try:
            flag_held.set()
            release_flag.wait(1.0)
        finally:
            bus.in_post_scan_rerun.reset(token)

    ta = threading.Thread(target=thread_a, name="thread-a")
    ta.start()
    assert flag_held.wait(1.0), "thread A didn't reach the flag-held barrier"

    # Thread B: publishes while A holds the flag. B's subscriber
    # runs in B's context — must NOT see A's flag.
    def thread_b():
        bus.publish(AlbumTagsChanged(album_id="a"))

    tb = threading.Thread(target=thread_b, name="thread-b")
    tb.start()
    tb.join(1.0)

    release_flag.set()
    ta.join(1.0)

    assert other_thread_done.is_set(), "B's subscriber didn't run"
    assert other_thread_saw_flag == [False], (
        "thread B's subscriber saw thread A's contextvar — the "
        "guard is leaking across thread boundaries and would "
        "incorrectly suppress unrelated reruns"
    )


# ---------- Misc safety ----------


def test_contextvar_resets_after_subscriber_raises():
    """If a subscriber sets the guard then raises, the finally
    block must reset it. Otherwise the flag leaks to the next
    event on the same thread."""
    def bad(ev):
        token = bus.in_post_scan_rerun.set(True)
        try:
            raise RuntimeError("oops")
        finally:
            bus.in_post_scan_rerun.reset(token)

    bus.register(AlbumTagsChanged, bad)
    bus.publish(AlbumTagsChanged(album_id="a"))
    # After the dispatch returns, the flag is back to default.
    assert bus.in_post_scan_rerun.get() is False


def test_publish_into_void_does_not_warn_or_raise():
    """A producer should be able to publish any event type without
    knowing whether subscribers exist. No-op silently."""
    for ev in (
        AlbumTagsChanged(album_id="a"),
        AlbumCoverChanged(album_id="b"),
        AlbumMbAnchorSet(album_id="c"),
        TrackFingerprinted(album_id="d"),
        AlbumDeleted(album_id="e"),
        AlbumRenamed(old_album_id="f", new_album_id="g"),
        LibraryScanCompleted(album_count=0),
    ):
        bus.publish(ev)  # no raise
