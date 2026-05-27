"""Integration tests for the library_events subscribers.

These verify the wiring between events and the side effects they
trigger (resolver invalidation, thumb drop, post-scan rerun,
findings-runner notify) by stubbing the side-effect targets and
asserting they were called with the right arguments.

The bus's contract (dispatch, ordering, recursion guard) is
covered in test_library_events.py; these tests only cover what
the subscribers do once invoked.
"""
from __future__ import annotations

import threading
import time

import pytest

from musaic import library_events as bus
from musaic.library_events import (
    AlbumCoverChanged,
    AlbumDeleted,
    AlbumMbAnchorSet,
    AlbumRenamed,
    AlbumTagsChanged,
    LibraryScanCompleted,
    TrackFingerprinted,
)
from musaic.services import event_subscribers


@pytest.fixture(autouse=True)
def _isolated_bus(monkeypatch, tmp_path):
    """Reset the bus + register subscribers fresh per test. Also
    stub out the heavy side effects so the subscriber tests don't
    require a real library."""
    bus.reset_for_tests()
    # Stub run_post_scan_checks so we don't actually run checks during
    # subscriber tests — we only care that it was called with the
    # right args.
    calls = {"rerun_calls": [], "invalidate_calls": [], "thumb_calls": [], "notify_calls": 0}
    def fake_rerun(library_dir, **kw):
        calls["rerun_calls"].append((library_dir, kw))
    def fake_invalidate(album_id):
        calls["invalidate_calls"].append(album_id)
    def fake_delete_thumb(album_id):
        calls["thumb_calls"].append(album_id)
    def fake_notify():
        calls["notify_calls"] += 1

    monkeypatch.setattr(event_subscribers, "run_post_scan_checks", fake_rerun)
    monkeypatch.setattr(
        event_subscribers.resolver_service, "invalidate", fake_invalidate,
    )
    monkeypatch.setattr(
        event_subscribers.thumbs_module, "delete_thumb", fake_delete_thumb,
    )
    monkeypatch.setattr(
        event_subscribers.checks_runner, "notify_changed", fake_notify,
    )
    # Stub the library_dir so subscribers don't return early on
    # "no library configured."
    class FakeCfg:
        library_dir = str(tmp_path / "library")
    monkeypatch.setattr(
        event_subscribers.config_module, "load_config", lambda: FakeCfg(),
    )

    event_subscribers.register_all()
    yield calls
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


# ---------- _resolver_invalidate ----------


def test_tags_changed_invalidates_resolver(_isolated_bus):
    bus.publish(AlbumTagsChanged(album_id="a1"))
    _wait_for_threads()
    assert "a1" in _isolated_bus["invalidate_calls"]


def test_mb_anchor_set_invalidates_resolver(_isolated_bus):
    bus.publish(AlbumMbAnchorSet(album_id="a2", source="cache"))
    _wait_for_threads()
    assert "a2" in _isolated_bus["invalidate_calls"]


def test_track_fingerprinted_invalidates_resolver(_isolated_bus):
    bus.publish(TrackFingerprinted(album_id="a3", filenames=("01.flac",)))
    _wait_for_threads()
    assert "a3" in _isolated_bus["invalidate_calls"]


def test_renamed_invalidates_both_old_and_new(_isolated_bus):
    """Rename produces a new id; both must be invalidated — old to
    avoid stale cache hits, new because the next read may want a
    fresh memo."""
    bus.publish(AlbumRenamed(old_album_id="a-old", new_album_id="a-new"))
    _wait_for_threads()
    assert "a-old" in _isolated_bus["invalidate_calls"]
    assert "a-new" in _isolated_bus["invalidate_calls"]


# ---------- _thumb_drop ----------


def test_cover_changed_drops_thumb(_isolated_bus):
    bus.publish(AlbumCoverChanged(album_id="a1"))
    _wait_for_threads()
    assert "a1" in _isolated_bus["thumb_calls"]


def test_album_deleted_drops_thumb(_isolated_bus):
    bus.publish(AlbumDeleted(album_id="a2"))
    _wait_for_threads()
    assert "a2" in _isolated_bus["thumb_calls"]


def test_renamed_drops_old_thumb(_isolated_bus):
    """Rename leaves the old thumb orphaned; drop it so the next
    /cover request re-extracts for the new id."""
    bus.publish(AlbumRenamed(old_album_id="old", new_album_id="new"))
    _wait_for_threads()
    assert "old" in _isolated_bus["thumb_calls"]


def test_tags_changed_does_not_drop_thumb(_isolated_bus):
    """Tag changes don't invalidate the cover thumbnail — keep the
    side effects scoped to what actually changed."""
    bus.publish(AlbumTagsChanged(album_id="a1"))
    _wait_for_threads()
    assert _isolated_bus["thumb_calls"] == []


# ---------- _post_scan_rerun ----------


def test_album_scoped_event_triggers_scoped_rerun(_isolated_bus):
    """Album-scoped events should fire run_post_scan_checks with
    album_ids=[that one album] so the rerun is fast and the
    runner's auto-resolve doesn't touch unrelated findings."""
    bus.publish(AlbumTagsChanged(album_id="a1"))
    _wait_for_threads()
    assert len(_isolated_bus["rerun_calls"]) == 1
    library_dir, kw = _isolated_bus["rerun_calls"][0]
    assert kw == {"album_ids": ["a1"]}


def test_library_scan_completed_triggers_full_rerun(_isolated_bus):
    """LibraryScanCompleted is the one library-wide event — fires
    a full post-scan, no album_ids scope."""
    bus.publish(LibraryScanCompleted(album_count=42))
    _wait_for_threads()
    assert len(_isolated_bus["rerun_calls"]) == 1
    _, kw = _isolated_bus["rerun_calls"][0]
    assert kw == {"album_ids": None} or kw == {}


def test_renamed_reruns_against_new_album_id(_isolated_bus):
    """Rename produces a new id; the rerun must target the new id
    because that's the canonical album row in the cache now."""
    bus.publish(AlbumRenamed(old_album_id="old", new_album_id="new"))
    _wait_for_threads()
    # Exactly one rerun call, scoped to the new id (NOT both).
    assert len(_isolated_bus["rerun_calls"]) == 1
    _, kw = _isolated_bus["rerun_calls"][0]
    assert kw == {"album_ids": ["new"]}


def test_rerun_subscriber_short_circuits_when_already_running(_isolated_bus):
    """If we're inside a post-scan rerun (e.g. a check is running
    and its side effect publishes an event), the subscriber must
    detect the contextvar and skip — otherwise we recurse."""
    token = bus.in_post_scan_rerun.set(True)
    try:
        bus.publish(AlbumTagsChanged(album_id="a1"))
        _wait_for_threads()
    finally:
        bus.in_post_scan_rerun.reset(token)
    # The background subscriber inherited the flag and bailed.
    assert _isolated_bus["rerun_calls"] == []


# ---------- _findings_runner_notify ----------


def test_every_event_wakes_the_findings_stream(_isolated_bus):
    """The notify subscriber is registered for the base Event class,
    so every event type wakes the SSE consumers."""
    for ev in (
        AlbumTagsChanged(album_id="a"),
        AlbumCoverChanged(album_id="b"),
        AlbumDeleted(album_id="c"),
        LibraryScanCompleted(album_count=0),
    ):
        bus.publish(ev)
    _wait_for_threads()
    assert _isolated_bus["notify_calls"] == 4
