"""SSE relay for `library_events`.

A single subscriber on the bus (registered alongside the rest in
`event_subscribers.register_all()`) serializes every published event
into a `(name, payload)` pair and fans it out to a list of per-
connection `asyncio.Queue` instances. The SSE endpoint
(`/api/library-events/stream`) creates one queue per client and drains
it as an event stream.

Design notes:

- The bus fires subscribers synchronously from the publisher's frame
  (or from a daemon thread, for the background ones). Either way we
  may not be on the asyncio event loop. The relay subscriber needs
  thread-safe enqueue, so each subscriber attaches both the asyncio
  loop *and* the queue at the time the SSE handler accepts the
  connection — pushing onto a queue across threads uses
  `loop.call_soon_threadsafe(queue.put_nowait, ...)`.

- Bounded queue per connection (256 events). When full, drop the
  oldest event and log a warning. Clients reconnect and re-fetch — the
  bus is a "something changed, refresh" signal, not durable state.

- The relay is registered at the lowest priority (400) so it fires
  AFTER every mutation/notification subscriber. That way the SSE
  consumer sees "the state has settled" rather than "a mutation is
  about to happen."

- Event payload shape: `{album_id?, old_album_id?, new_album_id?,
  fields?, filenames?, source?, album_count?}`. The frontend's
  invalidation map keys off the event name; the payload supplies the
  identity the invalidator needs (e.g. AlbumTagsChanged carries
  album_id so `["tracks", album_id]` can be invalidated precisely).
"""
from __future__ import annotations

import asyncio
import logging
import threading
from dataclasses import asdict, fields
from typing import Any

from .. import library_events as events


_log = logging.getLogger(__name__)

# Default per-connection capacity. 256 is generous — a single mutation
# typically emits one event; bulk loops emit dozens but suppress reruns
# until BulkBatchCompleted, so even a slow consumer is unlikely to fall
# this far behind during normal use.
QUEUE_MAXSIZE = 256


# Each entry: (loop, queue). Mutated only inside `_lock`.
_subscribers: list[tuple[asyncio.AbstractEventLoop, asyncio.Queue]] = []
_lock = threading.Lock()


def _event_to_payload(ev: events.Event) -> tuple[str, dict[str, Any]]:
    """Serialize an event into `(event_name, payload_dict)` for SSE.

    Uses the dataclass field set rather than `asdict()` directly so we
    keep dicts JSON-clean (no Path objects, no callables). Every event
    in `library_events.py` is a frozen dataclass of primitives + tuples
    so `asdict` is safe today, but the projection-through-fields keeps
    that contract explicit."""
    name = type(ev).__name__
    payload: dict[str, Any] = {}
    for f in fields(ev):
        value = getattr(ev, f.name)
        # Tuples (e.g. AlbumTagsChanged.fields) → lists for JSON.
        if isinstance(value, tuple):
            value = list(value)
        payload[f.name] = value
    return name, payload


def _enqueue(ev: events.Event) -> None:
    """The actual bus subscriber. Snapshots the subscriber list,
    serializes the event once, and fans out across every queue.

    Cross-thread push uses `loop.call_soon_threadsafe` because the
    background subscribers (post-scan rerun) fire from daemon threads
    and `Queue.put_nowait` is not thread-safe in that context."""
    name, payload = _event_to_payload(ev)
    with _lock:
        snapshot = list(_subscribers)
    for loop, queue in snapshot:
        loop.call_soon_threadsafe(_safe_put, queue, name, payload)


def _safe_put(queue: asyncio.Queue, name: str, payload: dict[str, Any]) -> None:
    """Push (name, payload) onto `queue`, dropping the oldest entry if
    full. Runs on the event loop via `call_soon_threadsafe` so the
    Queue methods are safe."""
    try:
        queue.put_nowait((name, payload))
    except asyncio.QueueFull:
        try:
            queue.get_nowait()
        except asyncio.QueueEmpty:
            pass
        try:
            queue.put_nowait((name, payload))
        except asyncio.QueueFull:
            # Defensive — another producer beat us. Drop the new event;
            # the client will reconnect + refetch on its next mutation.
            pass
        _log.warning(
            "library-events SSE queue overflow — dropped oldest event "
            "(client likely slow); newest=%s", name,
        )


def register_connection(
    loop: asyncio.AbstractEventLoop,
    queue: asyncio.Queue,
) -> None:
    """Add a per-connection (loop, queue) pair to the subscriber list.
    Called by the SSE handler when a client connects."""
    with _lock:
        _subscribers.append((loop, queue))


def unregister_connection(queue: asyncio.Queue) -> None:
    """Drop the registration for `queue`. Called by the SSE handler on
    client disconnect (CancelledError) so we stop publishing into a
    dead queue."""
    with _lock:
        for i, (_loop, q) in enumerate(_subscribers):
            if q is queue:
                _subscribers.pop(i)
                return


def subscriber_count() -> int:
    """Test / introspection helper — current number of attached SSE
    connections."""
    with _lock:
        return len(_subscribers)


def register_bus_subscriber() -> None:
    """Wire `_enqueue` to the bus at the Event base class so EVERY
    published event fans out. Called once from `event_subscribers.
    register_all()`. Priority 400 = runs after every other subscriber
    so SSE consumers see the post-mutation state, not the mid-mutation
    state."""
    events.register(
        events.Event, _enqueue,
        priority=400, name="_sse_event_relay",
    )
