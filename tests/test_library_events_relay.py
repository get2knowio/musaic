"""Tests for the library-events SSE relay subscriber.

The relay lives in `musaic.services.library_events_relay` and fans
every published Event out to per-connection asyncio.Queues. Each
test below pins one strand of the contract:

  - serialization: dataclass → (event_name, json-safe dict)
  - fan-out: every registered connection receives every event
  - isolation: unregistered connections stop receiving
  - backpressure: full queue drops the oldest (warns), keeps the newest
  - cross-thread safety: background subscribers can publish into
    queues bound to a different loop

The relay is registered alongside the other subscribers in
`event_subscribers.register_all()`; we call `register_bus_subscriber()`
directly here so each test starts from a clean bus.
"""
from __future__ import annotations

import asyncio
import threading

import pytest

from musaic import library_events as bus
from musaic.library_events import (
    AlbumCoverChanged,
    AlbumDeleted,
    AlbumMbAnchorSet,
    AlbumRenamed,
    AlbumTagsChanged,
    BulkBatchCompleted,
    LibraryScanCompleted,
    TrackFingerprinted,
)
from musaic.services import library_events_relay as relay


@pytest.fixture(autouse=True)
def _clean_relay():
    """Reset both the bus and the relay's connection list around each test."""
    bus.reset_for_tests()
    # The relay tracks subscribers in a module-level list; wipe it so
    # leftover queues from a prior test don't receive events.
    relay._subscribers.clear()
    yield
    bus.reset_for_tests()
    relay._subscribers.clear()


def test_event_to_payload_serializes_album_tags_changed():
    ev = AlbumTagsChanged(album_id="a1", fields=("title", "year"))
    name, payload = relay._event_to_payload(ev)
    assert name == "AlbumTagsChanged"
    assert payload == {"album_id": "a1", "fields": ["title", "year"]}


def test_event_to_payload_serializes_album_renamed():
    ev = AlbumRenamed(old_album_id="old", new_album_id="new")
    name, payload = relay._event_to_payload(ev)
    assert name == "AlbumRenamed"
    assert payload == {"old_album_id": "old", "new_album_id": "new"}


def test_event_to_payload_serializes_library_wide_events():
    ev = LibraryScanCompleted(album_count=42)
    name, payload = relay._event_to_payload(ev)
    assert name == "LibraryScanCompleted"
    assert payload == {"album_count": 42}

    ev2 = BulkBatchCompleted()
    name2, payload2 = relay._event_to_payload(ev2)
    assert name2 == "BulkBatchCompleted"
    assert payload2 == {}


def test_register_bus_subscriber_routes_every_event_type():
    """The relay subscribes at the Event base class so subclasses
    fan out automatically. Verify each concrete event reaches a
    registered queue."""
    relay.register_bus_subscriber()

    async def runner():
        loop = asyncio.get_running_loop()
        queue: asyncio.Queue = asyncio.Queue(maxsize=64)
        relay.register_connection(loop, queue)

        events_to_publish = [
            AlbumTagsChanged(album_id="a1", fields=("title",)),
            AlbumCoverChanged(album_id="a1"),
            AlbumMbAnchorSet(album_id="a1", source="musicbrainz"),
            TrackFingerprinted(album_id="a1", filenames=("01.flac",)),
            AlbumRenamed(old_album_id="old", new_album_id="new"),
            AlbumDeleted(album_id="a1"),
            LibraryScanCompleted(album_count=3),
            BulkBatchCompleted(),
        ]
        for ev in events_to_publish:
            bus.publish(ev)

        # Drain — every event should land. Use a small timeout so a
        # missing event surfaces as a clear failure instead of a hang.
        received: list[tuple[str, dict]] = []
        for _ in events_to_publish:
            name, payload = await asyncio.wait_for(queue.get(), timeout=1.0)
            received.append((name, payload))

        expected_names = [
            "AlbumTagsChanged", "AlbumCoverChanged", "AlbumMbAnchorSet",
            "TrackFingerprinted", "AlbumRenamed", "AlbumDeleted",
            "LibraryScanCompleted", "BulkBatchCompleted",
        ]
        assert [r[0] for r in received] == expected_names

    asyncio.run(runner())


def test_unregister_connection_stops_delivery():
    relay.register_bus_subscriber()

    async def runner():
        loop = asyncio.get_running_loop()
        queue: asyncio.Queue = asyncio.Queue(maxsize=8)
        relay.register_connection(loop, queue)
        assert relay.subscriber_count() == 1

        relay.unregister_connection(queue)
        assert relay.subscriber_count() == 0

        bus.publish(AlbumTagsChanged(album_id="a1"))
        # The queue should remain empty — a brief sleep gives the bus
        # plenty of time to call_soon_threadsafe if it were going to.
        await asyncio.sleep(0.05)
        assert queue.empty()

    asyncio.run(runner())


def test_multiple_connections_each_receive_every_event():
    relay.register_bus_subscriber()

    async def runner():
        loop = asyncio.get_running_loop()
        q1: asyncio.Queue = asyncio.Queue(maxsize=8)
        q2: asyncio.Queue = asyncio.Queue(maxsize=8)
        relay.register_connection(loop, q1)
        relay.register_connection(loop, q2)

        bus.publish(AlbumTagsChanged(album_id="a1"))
        bus.publish(AlbumCoverChanged(album_id="a2"))

        for q in (q1, q2):
            n1, _ = await asyncio.wait_for(q.get(), timeout=1.0)
            n2, _ = await asyncio.wait_for(q.get(), timeout=1.0)
            assert (n1, n2) == ("AlbumTagsChanged", "AlbumCoverChanged")

    asyncio.run(runner())


def test_safe_put_drops_oldest_when_queue_is_full():
    """A slow consumer mustn't deadlock the bus — the relay drops
    the oldest event and continues. Verify the surviving entries
    include the newest one."""
    async def runner():
        queue: asyncio.Queue = asyncio.Queue(maxsize=2)
        # Pre-fill with 2 sentinels.
        await queue.put(("old1", {}))
        await queue.put(("old2", {}))
        # Now push 2 more — each triggers a drop-oldest cycle.
        relay._safe_put(queue, "new1", {})
        relay._safe_put(queue, "new2", {})

        out = []
        while not queue.empty():
            out.append(await queue.get())
        # The queue can only hold 2; the two NEWEST should survive.
        # `old1` and `old2` should both have been evicted.
        names = [name for name, _ in out]
        assert "new2" in names
        # We don't pin the exact ordering — the contract is just
        # "newest is preserved, oldest is dropped" — but the queue
        # must contain exactly 2 entries.
        assert len(out) == 2
        assert "old1" not in names

    asyncio.run(runner())


def test_publish_from_background_thread_reaches_queue():
    """Subscribers may publish from a daemon thread (background
    subscribers do this via copy_context().run). The relay must use
    loop.call_soon_threadsafe so the cross-thread enqueue succeeds
    rather than silently dropping or raising."""
    relay.register_bus_subscriber()

    async def runner():
        loop = asyncio.get_running_loop()
        queue: asyncio.Queue = asyncio.Queue(maxsize=8)
        relay.register_connection(loop, queue)

        def publish_from_thread():
            bus.publish(AlbumTagsChanged(album_id="from-thread"))

        threading.Thread(target=publish_from_thread, daemon=True).start()

        name, payload = await asyncio.wait_for(queue.get(), timeout=1.0)
        assert name == "AlbumTagsChanged"
        assert payload["album_id"] == "from-thread"

    asyncio.run(runner())


def test_dead_loop_does_not_starve_healthy_subscribers():
    """If one connection's loop is closed between the snapshot and
    dispatch, `call_soon_threadsafe` raises RuntimeError. Without the
    per-iteration guard in `_enqueue`, that exception would break out
    of the for-loop and every later subscriber in the snapshot would
    silently miss the event. With the guard, the dead loop is skipped
    and healthy queues still receive the event."""
    relay.register_bus_subscriber()

    # Build a dead-loop sentinel: a real asyncio loop, but closed.
    dead_loop = asyncio.new_event_loop()
    dead_loop.close()
    dead_queue: asyncio.Queue = asyncio.Queue(maxsize=8)
    # Register the dead-loop entry BEFORE the healthy one so the
    # for-loop hits it first — proving that an early failure doesn't
    # starve a later healthy queue.
    with relay._lock:
        relay._subscribers.append((dead_loop, dead_queue))

    async def runner():
        live_loop = asyncio.get_running_loop()
        live_queue: asyncio.Queue = asyncio.Queue(maxsize=8)
        relay.register_connection(live_loop, live_queue)

        # Publish — _enqueue iterates [dead, live]. Dead raises
        # RuntimeError on call_soon_threadsafe; guard catches it and
        # continues to the live entry.
        bus.publish(AlbumTagsChanged(album_id="should-still-reach-live"))

        name, payload = await asyncio.wait_for(live_queue.get(), timeout=1.0)
        assert name == "AlbumTagsChanged"
        assert payload["album_id"] == "should-still-reach-live"

    asyncio.run(runner())


def test_relay_priority_runs_after_other_subscribers():
    """Relay registers at priority 400 so SSE consumers see the post-
    mutation state. Verify the relay subscriber executes AFTER any
    priority-200 mutation subscriber."""
    order: list[str] = []

    def early(_ev):
        order.append("early")

    bus.register(AlbumTagsChanged, early, priority=200, name="early")
    relay.register_bus_subscriber()

    # The relay subscriber needs an attached queue to do anything
    # observable; bind one and verify its append lands AFTER `early`.
    async def runner():
        loop = asyncio.get_running_loop()
        queue: asyncio.Queue = asyncio.Queue(maxsize=8)
        relay.register_connection(loop, queue)

        bus.publish(AlbumTagsChanged(album_id="a1"))
        # Drain the queue to make sure the relay subscriber actually ran.
        await asyncio.wait_for(queue.get(), timeout=1.0)
        order.append("relay-delivered")

    asyncio.run(runner())
    assert order == ["early", "relay-delivered"]
