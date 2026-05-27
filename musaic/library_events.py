"""In-process pub/sub for library-state mutations.

Design context: `docs/library_events_design.md`. This module is the
canonical fan-out for cross-cutting concerns (post-scan reruns,
resolver invalidation, thumb drops, SSE notify) that used to live
ad-hoc at every mutation site. Producers publish events; subscribers
registered at app startup react.

Architectural move E from issue #1, fully migrated: Phase 1 shipped
the bus + dual-fire, Phase 2 deleted every direct `run_post_scan_
checks` caller in favor of `publish(...)`, Phase 3 rewrote the
post_scan module docstring to reflect the new flow.

Scope discipline (kept tight on purpose):
  - In-process only. Not a message bus, not a queue, not durable.
  - Single library. Subscribers look up `library_path` via
    `library_path_for_checks()` rather than carrying it on events.
  - Events name the domain change, not the desired side effect.
  - Events carry minimum identity (album_id, fields), not full payloads.

Dispatch model:
  - Synchronous in-process by default; sync subscribers run inline
    in the publisher's frame.
  - Subscribers can opt into `background=True` for slow work
    (post-scan rerun is the only one today). Background subscribers
    run in a daemon thread whose context is COPIED from the publisher
    via `contextvars.copy_context().run(...)` — this is critical
    for the recursion guard to propagate, see below.
  - Failed subscribers log and continue. Events are facts, not
    commands; we don't roll back.

Ordering:
  - Subscribers register with an integer `priority` (lower = earlier).
  - Cross-event ordering is NOT guaranteed — events fired in sequence
    are dispatched in sequence, but interleaving with other producers'
    events isn't promised.

Recursion guard:
  - `_in_post_scan_rerun: ContextVar[bool]` is checked by the
    `_post_scan_rerun` subscriber to detect "we're already inside one
    of these reruns; don't recurse." The bus itself stays dumb — the
    subscriber owns its re-entry semantics.
  - Background dispatch uses `contextvars.copy_context().run(...)` so
    the flag follows the work into the daemon thread.
  - Idempotency is the real safety net: even if the guard fails open,
    Tier 0a cache hits replace Tier 0b/0c side-effect writes on the
    second pass; nothing new gets published, recursion terminates.
"""
from __future__ import annotations

import contextvars
import logging
import threading
from collections import defaultdict
from dataclasses import dataclass, field
from typing import Any, Callable, Type, TypeVar


_log = logging.getLogger(__name__)


# ---------- Recursion guard ----------

# Set to True by `_post_scan_rerun` while it's running so a nested
# publish chain that loops back into post_scan returns immediately
# instead of re-entering the check runner.
#
# Owned by the subscriber, not the bus — keeps the bus's contract
# minimal and lets the subscriber test its own short-circuit logic
# in isolation.
in_post_scan_rerun: contextvars.ContextVar[bool] = contextvars.ContextVar(
    "in_post_scan_rerun", default=False,
)


# ---------- Event types ----------

# Frozen dataclasses give us identity + hashability + readable repr
# for free. Subscribers pattern-match on `type(ev)` rather than a
# discriminator string — typo-safe, IDE-navigable.


@dataclass(frozen=True)
class Event:
    """Marker base. Subscribers register by event type, not by name."""


@dataclass(frozen=True)
class AlbumScopedEvent(Event):
    """Events that name a single album. The `_post_scan_rerun`
    subscriber translates `album_id` into a scope_kind='albums'
    rerun (cheap — touches one album, not the whole library)."""
    album_id: str


@dataclass(frozen=True)
class LibraryScanCompleted(Event):
    """Fired once after a full library scan finishes. Subscribers run
    library-wide passes (full post-scan rerun). Per-album events are
    NOT emitted during scan — see the design doc's discussion of why
    coalescing batch work matters. One library-wide event, not N
    per-album ones."""
    album_count: int


@dataclass(frozen=True)
class AlbumTagsChanged(AlbumScopedEvent):
    """Vorbis tags on this album changed. `fields` names which tags
    changed (e.g. ['title', 'year']); subscribers can filter on it,
    but most subscribers care that *something* changed, not which.

    Producers: services.apply.apply_album_changes_iter,
    audio_analysis.analyze_album_iter."""
    fields: tuple[str, ...] = field(default_factory=tuple)


@dataclass(frozen=True)
class AlbumCoverChanged(AlbumScopedEvent):
    """Album's embedded cover art changed. Distinct from
    AlbumTagsChanged so subscribers like the thumb-drop can react
    selectively. Producer: services.apply.apply_album_changes_iter
    when cover_url or cover_bytes was applied."""


@dataclass(frozen=True)
class AlbumRenamed(Event):
    """Album directory was renamed; the album_id changed because the
    id is sha1(path). Carries BOTH ids so subscribers know whether
    to drop caches keyed by the old one and rebuild for the new.

    Producer: filename_mismatch.execute_renames dir-rename branch."""
    old_album_id: str
    new_album_id: str


@dataclass(frozen=True)
class AlbumMbAnchorSet(AlbumScopedEvent):
    """A MusicBrainz release-group anchor was written for this album.
    Triggers post-scan rerun so unidentified-album findings clear,
    and resolver-cache invalidation so subsequent reads pick up
    canonical fields.

    Producers: mb_lookup.find_release (side effect), services.worker
    AI-MBID-persist path, services.resolver Tier 0b/0c writes."""
    source: str = ""  # "musicbrainz" | "ai" | "fingerprint+mb" | "cache"


@dataclass(frozen=True)
class TrackFingerprinted(AlbumScopedEvent):
    """One or more track fingerprints landed for this album. Triggers
    post-scan rerun so unidentified-album / duplicate-tracks /
    track-gaps findings can re-evaluate against the new data.

    Producer: fingerprint.fingerprint_album (and its callers in the
    resolver, eager pool, and per-check fix paths)."""
    filenames: tuple[str, ...] = field(default_factory=tuple)


@dataclass(frozen=True)
class AlbumDeleted(AlbumScopedEvent):
    """Album row was removed from the cache (album dir disappeared
    from disk, library reconfigured, etc.). Subscribers should drop
    any state keyed by album_id and let the post-scan rerun
    auto-resolve any orphan findings.

    Producer: cache.delete_album."""


@dataclass(frozen=True)
class BulkBatchCompleted(Event):
    """A bulk-fix loop just finished. Producers (the
    quick-fix-all endpoint, future batch operations) set
    `checks._apply.in_bulk_run` for the duration of the loop to
    suppress per-event reruns, then publish this event AFTER
    resetting the flag. Subscribers fire one coalesced rerun
    covering the cumulative state from the batch.

    Library-wide (not album-scoped) because a bulk loop touches
    many albums and the runner's library-wide pass is more
    efficient than N scoped reruns."""


# ---------- Registry ----------


_EventT = TypeVar("_EventT", bound=Event)
Subscriber = Callable[[Any], None]  # callable taking a specific Event subclass


@dataclass
class _Registration:
    callback: Subscriber
    background: bool
    priority: int
    name: str

    def __lt__(self, other: "_Registration") -> bool:
        return self.priority < other.priority


# Module-level state — process-singleton bus. Lock guards the dict
# during register/unregister; dispatch iterates a snapshot so a
# subscriber can register/unregister without deadlocking.
_lock = threading.Lock()
_subscribers: dict[Type[Event], list[_Registration]] = defaultdict(list)


def register(
    event_type: Type[_EventT],
    callback: Callable[[_EventT], None],
    *,
    background: bool = False,
    priority: int = 200,
    name: str | None = None,
) -> None:
    """Subscribe `callback` to events of type `event_type` (and
    subclasses — registering for `AlbumScopedEvent` catches every
    album-scoped subclass).

    `background=True` runs the callback in a daemon thread; the
    publisher returns immediately. Contextvars set by the publisher
    are PROPAGATED to the thread via `copy_context().run(...)` —
    this is load-bearing for the recursion guard, do not bypass.

    `priority` orders subscribers within a single event type
    (lower = earlier). Defaults: invalidation=100, mutation=200,
    notification=300. Same-priority order is registration order.

    `name` is purely for log readability; defaults to the callback's
    `__qualname__`."""
    reg = _Registration(
        callback=callback,
        background=background,
        priority=priority,
        name=name or getattr(callback, "__qualname__", repr(callback)),
    )
    with _lock:
        _subscribers[event_type].append(reg)
        # Stable sort: priority first, registration order on ties.
        _subscribers[event_type].sort(key=lambda r: r.priority)


def unregister(event_type: Type[Event], callback: Subscriber) -> None:
    """Remove the first registration matching `callback` from
    `event_type`'s subscriber list. Used mostly by tests; production
    code registers once at startup and never unregisters."""
    with _lock:
        regs = _subscribers.get(event_type, [])
        for i, r in enumerate(regs):
            if r.callback is callback:
                regs.pop(i)
                return


def reset_for_tests() -> None:
    """Wipe all subscribers. Tests use this in their setup so a
    test's subscribers don't leak into the next test."""
    with _lock:
        _subscribers.clear()


def publish(event: Event) -> None:
    """Dispatch `event` to every registered subscriber whose
    declared type is `event`'s class or a superclass.

    Sync subscribers run inline, in priority order. Background
    subscribers spawn a daemon thread (contextvars copied) and
    return immediately. A failing subscriber is logged and the
    dispatch continues — events are facts, not commands."""
    # Walk the class hierarchy so subclasses fan out to base-class
    # subscribers too. AlbumTagsChanged → AlbumScopedEvent → Event.
    to_call: list[_Registration] = []
    with _lock:
        for cls in type(event).__mro__:
            if cls in _subscribers:
                to_call.extend(_subscribers[cls])
    # Re-sort across all matching classes so a high-priority
    # superclass subscriber still runs before a low-priority subclass
    # one. Same-priority ties preserve mro+registration order from
    # the extend above.
    to_call.sort(key=lambda r: r.priority)

    for reg in to_call:
        try:
            if reg.background:
                ctx = contextvars.copy_context()
                threading.Thread(
                    target=ctx.run,
                    args=(reg.callback, event),
                    daemon=True,
                    name=f"library_events:{reg.name}",
                ).start()
            else:
                reg.callback(event)
        except Exception:
            _log.exception(
                "subscriber %s failed for event %s (continuing)",
                reg.name, type(event).__name__,
            )
