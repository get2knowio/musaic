"""Subscribers for `musaic.library_events`.

This is Phase 1 of move E from issue #1 (design doc:
`docs/library_events_design.md`). Each subscriber owns one
cross-cutting concern that endpoints currently invoke ad-hoc; the
goal is to delete those direct calls once dual-firing has been
verified in production for a while.

Subscribers registered here, in priority order:

  100 — `_resolver_invalidate`   drop resolver memo for the album
  100 — `_thumb_drop`            drop the album's pre-extracted thumb
  200 — `_post_scan_rerun`       scope-aware run_post_scan_checks
                                  (background; owns recursion guard)
  300 — `_findings_runner_notify` wake checks_runner.changed_event()
                                   so /api/checks/stream re-renders

The recursion guard lives in `_post_scan_rerun` (not at the bus
level); the bus stays domain-agnostic. See `docs/library_events_
design.md#second-design-review-pass-on-the-guard` for the rationale.

Phase 1 leaves all existing direct `run_post_scan_checks` calls in
place — these subscribers fire ALONGSIDE them. Idempotency makes the
double-fire safe (a second post-scan rerun over the same state finds
the same findings). Phase 2 removes the direct calls one producer at
a time.
"""
from __future__ import annotations

import logging

from .. import config as config_module
from .. import library_events as events
from .. import thumbs as thumbs_module
from ..checks import runner as checks_runner
from ..library_events import (
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
from . import resolver as resolver_service
from .post_scan import run_post_scan_checks

_log = logging.getLogger(__name__)


# ---------- Subscribers ----------


def _resolver_invalidate(ev: AlbumScopedEvent) -> None:
    """Drop the per-album resolver memo so the next read sees current
    state. The resolver's content_hash catches some changes but not
    tag-only edits; safest is to invalidate on any mutation.

    Cheap (in-memory dict pop), so it's safe to fire from any thread
    inline."""
    try:
        resolver_service.invalidate(ev.album_id)
    except Exception:
        _log.exception("_resolver_invalidate failed for %s", ev.album_id)


def _resolver_invalidate_for_rename(ev: AlbumRenamed) -> None:
    """AlbumRenamed carries old_id+new_id; invalidate both so neither
    a stale cache hit on the old id nor an unwritten memo on the new
    one trips up the next read."""
    try:
        resolver_service.invalidate(ev.old_album_id)
        resolver_service.invalidate(ev.new_album_id)
    except Exception:
        _log.exception(
            "_resolver_invalidate_for_rename failed (%s → %s)",
            ev.old_album_id, ev.new_album_id,
        )


def _thumb_drop(ev: AlbumScopedEvent) -> None:
    """Drop the album's pre-extracted thumbnail so the next /cover
    request re-extracts. Fires on cover change, delete, rename.

    Cheap (one unlink), safe inline."""
    try:
        thumbs_module.delete_thumb(ev.album_id)
    except Exception:
        _log.exception("_thumb_drop failed for %s", ev.album_id)


def _thumb_drop_for_rename(ev: AlbumRenamed) -> None:
    """Drop the OLD album's thumb on rename — the new id needs a
    fresh extract on next load, and the old thumb file is orphaned."""
    try:
        thumbs_module.delete_thumb(ev.old_album_id)
    except Exception:
        _log.exception("_thumb_drop_for_rename failed for %s", ev.old_album_id)


def _post_scan_rerun(ev: AlbumScopedEvent) -> None:
    """Run every `runs_during_scan=True` check scoped to the album
    the event names. Owns the recursion guard via the
    `library_events.in_post_scan_rerun` contextvar.

    The guard's invariant: at most one post-scan rerun in-flight per
    dispatch chain. A check that writes an anchor (publishes an
    AlbumMbAnchorSet) would otherwise route back here and re-enter
    the runner — potentially indefinitely. The contextvar inherits
    into the background thread via `contextvars.copy_context().run`
    (the bus's responsibility, tested in
    `test_background_subscriber_inherits_in_flight_flag`).

    Idempotency is the real safety net — even if the guard fails open,
    Tier 0a cache hits replace Tier 0b/0c side-effect writes on the
    second pass; nothing new gets published. The guard is an
    optimization, not a correctness mechanism.

    Background=True at the bus level (see register()): each rerun
    takes seconds for medium checks and we don't want producers
    blocking on it.
    """
    if events.in_post_scan_rerun.get():
        _log.debug(
            "_post_scan_rerun: skipped for %s (already inside one)",
            ev.album_id,
        )
        return
    cfg = config_module.load_config()
    if not cfg.library_dir:
        return  # No library configured yet — nothing to rerun against.
    token = events.in_post_scan_rerun.set(True)
    try:
        run_post_scan_checks(cfg.library_dir, album_ids=[ev.album_id])
    except Exception:
        _log.exception(
            "_post_scan_rerun failed for %s (continuing)", ev.album_id,
        )
    finally:
        events.in_post_scan_rerun.reset(token)


def _post_scan_rerun_for_rename(ev: AlbumRenamed) -> None:
    """Rename produces a new album_id; the rerun must target the new
    id so the runner finds the (now-canonical) album row. Old id's
    findings are auto-resolved by the runner because the scope is
    by-album and the old row is gone."""
    if events.in_post_scan_rerun.get():
        return
    cfg = config_module.load_config()
    if not cfg.library_dir:
        return
    token = events.in_post_scan_rerun.set(True)
    try:
        run_post_scan_checks(
            cfg.library_dir, album_ids=[ev.new_album_id],
        )
    except Exception:
        _log.exception(
            "_post_scan_rerun_for_rename failed (%s → %s)",
            ev.old_album_id, ev.new_album_id,
        )
    finally:
        events.in_post_scan_rerun.reset(token)


def _post_scan_rerun_library_wide(ev: LibraryScanCompleted) -> None:
    """Library-scoped reruns at the end of a scan — equivalent to the
    current `run_post_scan_checks` call from `services.scan._run_scan`.
    Same recursion guard so a scan finishing inside another rerun
    doesn't recursively trigger a third."""
    if events.in_post_scan_rerun.get():
        return
    cfg = config_module.load_config()
    if not cfg.library_dir:
        return
    token = events.in_post_scan_rerun.set(True)
    try:
        run_post_scan_checks(cfg.library_dir)
    except Exception:
        _log.exception("_post_scan_rerun_library_wide failed (continuing)")
    finally:
        events.in_post_scan_rerun.reset(token)


def _findings_runner_notify(ev: Event) -> None:
    """Wake any open `/api/checks/stream` socket so connected
    clients refetch the findings list. Fires AFTER post-scan reruns
    (priority 300 vs 200) so the refresh sees the new state.

    `notify_changed` is non-blocking — it sets an asyncio.Event;
    the SSE handler does the actual fanout."""
    try:
        checks_runner.notify_changed()
    except Exception:
        _log.exception("_findings_runner_notify failed (continuing)")


# ---------- Registration ----------


def register_all() -> None:
    """Wire every subscriber to its event type(s). Called once from
    the FastAPI lifespan on startup. Safe to call from tests after
    `events.reset_for_tests()` to re-register in isolation."""
    # Invalidation tier (100) — fast, in-process, run inline.
    events.register(
        AlbumScopedEvent, _resolver_invalidate,
        priority=100, name="_resolver_invalidate",
    )
    events.register(
        AlbumRenamed, _resolver_invalidate_for_rename,
        priority=100, name="_resolver_invalidate_for_rename",
    )
    events.register(
        AlbumCoverChanged, _thumb_drop,
        priority=100, name="_thumb_drop",
    )
    events.register(
        AlbumDeleted, _thumb_drop,
        priority=100, name="_thumb_drop:on-delete",
    )
    events.register(
        AlbumRenamed, _thumb_drop_for_rename,
        priority=100, name="_thumb_drop_for_rename",
    )

    # Mutation tier (200) — slow (seconds), runs in background thread.
    # Subscribed at the base class so every album-scoped event triggers
    # a scoped rerun; LibraryScanCompleted gets its own library-wide
    # variant. Renames get a third variant that targets the new id.
    events.register(
        AlbumScopedEvent, _post_scan_rerun,
        priority=200, background=True, name="_post_scan_rerun",
    )
    events.register(
        AlbumRenamed, _post_scan_rerun_for_rename,
        priority=200, background=True, name="_post_scan_rerun_for_rename",
    )
    events.register(
        LibraryScanCompleted, _post_scan_rerun_library_wide,
        priority=200, background=True, name="_post_scan_rerun_library_wide",
    )

    # Notification tier (300) — wake SSE consumers. Last so it sees
    # post-rerun state.
    events.register(
        Event, _findings_runner_notify,
        priority=300, name="_findings_runner_notify",
    )
