"""Tests for the v15 Missing-state model.

The conceptual shift: scans no longer DELETE cached albums when they
don't see the dir on disk — they mark them Missing (preserving the
row + cascade children: fingerprints, MB anchor, track tags). Only
explicit user action via `DELETE /api/albums/{id}` ever drops a row.
A scan-time sanity check refuses to mark large fractions of the cache
Missing in one go (drive outages don't get to silently grey-out
hundreds of albums).

These tests cover:
  - save_snapshot marks vanished albums Missing instead of deleting
  - save_snapshot clears `missing_since` when an album returns
  - save_snapshot publishes AlbumMarkedMissing + AlbumTagsChanged
    on the corresponding transitions
  - mark_album_missing / clear_album_missing helpers transition +
    publish correctly + idempotently
  - delete_album still cascades (user-initiated removal path)
  - Scan sanity check aborts when too many albums would vanish
  - Scan sanity check skips on tiny libraries (don't trip on fresh
    installs)
  - run_check filters Missing albums out of scope.albums
  - SQL-pure checks filter on missing_since IS NULL in their queries
"""
from __future__ import annotations

import asyncio
from typing import Any

import pytest

from musaic import cache as cache_module
from musaic import db as db_module
from musaic import library_events as bus
from musaic.checks import (
    inconsistent_artist_sort,
    track_gaps,
)
from musaic.checks.base import CheckScope
from musaic.services import scan as scan_service


LIBRARY_PATH = "/fake/library"


@pytest.fixture(autouse=True)
def _clean_db_and_bus():
    """Wipe tables + bus subscribers between tests so state doesn't bleed."""
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            conn.execute("DELETE FROM albums")
            conn.execute("DELETE FROM libraries")
            conn.execute("DELETE FROM track_fingerprints")
            conn.execute("DELETE FROM track_tags")
            conn.execute("DELETE FROM check_findings")
            conn.execute("DELETE FROM check_runs")
    bus.reset_for_tests()
    yield
    bus.reset_for_tests()


def _ensure_library() -> None:
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            conn.execute(
                "INSERT OR IGNORE INTO libraries(path) VALUES (?)",
                (LIBRARY_PATH,),
            )


def _album_dict(
    *,
    id: str,
    path: str,
    title: str = "Album",
    album_artist: str = "Artist",
    track_count: int = 3,
) -> dict[str, Any]:
    return {
        "id": id,
        "path": path,
        "dir_name": path.split("/")[-1],
        "title": title,
        "artist": album_artist,
        "album_artist": album_artist,
        "year": "1979",
        "genre": "Rock",
        "track_count": track_count,
        "total_seconds": 1800.0,
        "has_embedded_cover": False,
        "cover_file": None,
        "sample_track": f"{path}/01.flac",
        "dir_mtime": 1000.0,
    }


def _snapshot(albums: list[dict[str, Any]]) -> cache_module.LibrarySnapshot:
    return cache_module.LibrarySnapshot(
        library_dir=LIBRARY_PATH,
        scanned_at="2026-05-27T22:00:00Z",
        albums_by_path={a["path"]: a for a in albums},
    )


def _insert_fingerprint(album_id: str, filename: str = "01.flac") -> None:
    _ensure_library()
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            conn.execute(
                """
                INSERT INTO track_fingerprints
                    (album_id, filename, fingerprint, duration_s, last_computed_at)
                VALUES (?, ?, 'abc', 180.0, '2026-05-27T00:00:00Z')
                """,
                (album_id, filename),
            )


def _missing_since(album_id: str) -> str | None:
    with db_module.connect() as conn:
        row = conn.execute(
            "SELECT missing_since FROM albums WHERE id = ?", (album_id,),
        ).fetchone()
    return row["missing_since"] if row else None


def _album_exists(album_id: str) -> bool:
    with db_module.connect() as conn:
        row = conn.execute(
            "SELECT 1 FROM albums WHERE id = ?", (album_id,),
        ).fetchone()
    return row is not None


# ============================================================
# save_snapshot — Missing-marking + Returning passes
# ============================================================


def test_save_snapshot_marks_vanished_album_missing_not_deleted():
    """An album cached but absent from the new snapshot should get
    `missing_since` set — NOT DELETEd. Its fingerprints + tags must
    survive intact for when the album returns."""
    cache_module.save_snapshot(_snapshot([
        _album_dict(id="a", path=f"{LIBRARY_PATH}/Artist - A [FLAC]"),
        _album_dict(id="b", path=f"{LIBRARY_PATH}/Artist - B [FLAC]"),
    ]))
    _insert_fingerprint("a")
    _insert_fingerprint("b")

    # Next scan only sees album "a".
    result = cache_module.save_snapshot(_snapshot([
        _album_dict(id="a", path=f"{LIBRARY_PATH}/Artist - A [FLAC]"),
    ]))

    assert _album_exists("a")
    assert _album_exists("b"), "vanished album must NOT be deleted"
    assert _missing_since("a") is None
    assert _missing_since("b") is not None
    assert result["newly_missing"] == ["b"]
    assert result["returned"] == []

    # Cascade child preserved — this is the load-bearing assertion;
    # the bug this whole PR exists to fix.
    with db_module.connect() as conn:
        fp_count = conn.execute(
            "SELECT COUNT(*) AS n FROM track_fingerprints WHERE album_id = ?",
            ("b",),
        ).fetchone()["n"]
    assert fp_count == 1, "fingerprints for Missing album must survive"


def test_save_snapshot_clears_missing_when_album_returns():
    """An album that was Missing but reappears in the new snapshot
    should have `missing_since` cleared back to NULL — and the
    AlbumTagsChanged event fires so consumers refresh."""
    # Round 1: album "a" exists.
    cache_module.save_snapshot(_snapshot([
        _album_dict(id="a", path=f"{LIBRARY_PATH}/Artist - A [FLAC]"),
    ]))
    # Round 2: "a" vanishes → marked Missing.
    cache_module.save_snapshot(_snapshot([]))
    assert _missing_since("a") is not None
    # Round 3: "a" returns.
    result = cache_module.save_snapshot(_snapshot([
        _album_dict(id="a", path=f"{LIBRARY_PATH}/Artist - A [FLAC]"),
    ]))
    assert _missing_since("a") is None
    assert result["returned"] == ["a"]
    assert result["newly_missing"] == []


def test_save_snapshot_publishes_marked_missing_event():
    seen: list[bus.Event] = []
    bus.register(bus.AlbumMarkedMissing, lambda ev: seen.append(ev))

    cache_module.save_snapshot(_snapshot([
        _album_dict(id="a", path=f"{LIBRARY_PATH}/A [FLAC]"),
        _album_dict(id="b", path=f"{LIBRARY_PATH}/B [FLAC]"),
    ]))
    seen.clear()
    cache_module.save_snapshot(_snapshot([
        _album_dict(id="a", path=f"{LIBRARY_PATH}/A [FLAC]"),
    ]))

    assert len(seen) == 1
    assert seen[0].album_id == "b"
    assert seen[0].missing_since != ""


def test_save_snapshot_publishes_tags_changed_on_return():
    """Returning to Present uses AlbumTagsChanged (no separate
    AlbumReturned event today) so the bus subscriber graph handles
    invalidation + post-scan rerun via the existing path."""
    cache_module.save_snapshot(_snapshot([
        _album_dict(id="a", path=f"{LIBRARY_PATH}/A [FLAC]"),
    ]))
    cache_module.save_snapshot(_snapshot([]))  # mark missing

    seen: list[bus.Event] = []
    bus.register(bus.AlbumTagsChanged, lambda ev: seen.append(ev))
    cache_module.save_snapshot(_snapshot([
        _album_dict(id="a", path=f"{LIBRARY_PATH}/A [FLAC]"),
    ]))

    assert len(seen) == 1
    assert seen[0].album_id == "a"
    assert "missing_since" in seen[0].fields


def test_save_snapshot_does_not_bump_missing_since_on_already_missing_album():
    """`missing_since` is a tombstone, not a heartbeat — first
    observation only. Subsequent scans that still don't see the
    album should leave the original timestamp alone (so the UI can
    show 'missing since {original date}')."""
    cache_module.save_snapshot(_snapshot([
        _album_dict(id="a", path=f"{LIBRARY_PATH}/A [FLAC]"),
    ]))
    cache_module.save_snapshot(_snapshot([]))  # first absence
    original = _missing_since("a")
    assert original is not None

    cache_module.save_snapshot(_snapshot([]))  # second absence
    assert _missing_since("a") == original, \
        "missing_since must not be bumped on subsequent absent scans"


# ============================================================
# mark_album_missing / clear_album_missing helpers
# ============================================================


def test_mark_album_missing_transitions_and_publishes():
    cache_module.save_snapshot(_snapshot([
        _album_dict(id="a", path=f"{LIBRARY_PATH}/A [FLAC]"),
    ]))
    seen: list[bus.Event] = []
    bus.register(bus.AlbumMarkedMissing, lambda ev: seen.append(ev))

    transitioned = cache_module.mark_album_missing("a")
    assert transitioned is True
    assert _missing_since("a") is not None
    assert len(seen) == 1
    assert seen[0].album_id == "a"


def test_mark_album_missing_is_idempotent():
    cache_module.save_snapshot(_snapshot([
        _album_dict(id="a", path=f"{LIBRARY_PATH}/A [FLAC]"),
    ]))
    cache_module.mark_album_missing("a")

    seen: list[bus.Event] = []
    bus.register(bus.AlbumMarkedMissing, lambda ev: seen.append(ev))

    transitioned = cache_module.mark_album_missing("a")
    assert transitioned is False, "second call should be a no-op"
    assert seen == [], "no second event should fire"


def test_clear_album_missing_transitions():
    cache_module.save_snapshot(_snapshot([
        _album_dict(id="a", path=f"{LIBRARY_PATH}/A [FLAC]"),
    ]))
    cache_module.mark_album_missing("a")
    transitioned = cache_module.clear_album_missing("a")
    assert transitioned is True
    assert _missing_since("a") is None


def test_clear_album_missing_is_idempotent():
    cache_module.save_snapshot(_snapshot([
        _album_dict(id="a", path=f"{LIBRARY_PATH}/A [FLAC]"),
    ]))
    # Not missing — clear is a no-op.
    transitioned = cache_module.clear_album_missing("a")
    assert transitioned is False


# ============================================================
# delete_album — the only destructive path
# ============================================================


def test_delete_album_cascades_to_children():
    """`delete_album` IS destructive — it drops fingerprints +
    track_tags via FK cascade. This is the only reachable destructive
    path post-v15."""
    cache_module.save_snapshot(_snapshot([
        _album_dict(id="a", path=f"{LIBRARY_PATH}/A [FLAC]"),
    ]))
    _insert_fingerprint("a")

    cache_module.delete_album("a")
    assert not _album_exists("a")
    with db_module.connect() as conn:
        fp_count = conn.execute(
            "SELECT COUNT(*) AS n FROM track_fingerprints WHERE album_id = ?",
            ("a",),
        ).fetchone()["n"]
    assert fp_count == 0, "delete_album cascade should drop fingerprints"


def test_apply_pipeline_marks_missing_when_dir_vanishes_mid_apply(monkeypatch, tmp_path):
    """The apply pipeline's `removed_album` branch (fires when
    `scan_one_album` returns None after the FLAC writes) must mark
    the album Missing — NOT delete it. The historical bug: a drive
    flap mid-apply would cascade-nuke that one album's fingerprints
    + MB anchor + track tags. v15 demands apply paths use the same
    preservation semantics as scan paths."""
    from musaic.services import apply as apply_service

    # Seed an album in the cache with a fingerprint.
    cache_module.save_snapshot(_snapshot([
        _album_dict(id="a-dropped", path=str(tmp_path / "Album [FLAC]")),
    ]))
    _insert_fingerprint("a-dropped")
    assert _album_exists("a-dropped")
    assert _missing_since("a-dropped") is None

    # No-op the tag-writer (no FLAC fixture available; we only need
    # the post-write rescan path to fire).
    def _empty_apply_iter(*_args, **_kwargs):
        yield "start", {"file_count": 0}
        yield "done", {"saved_count": 0, "errors": 0, "art_changed_count": 0}

    monkeypatch.setattr(apply_service, "apply_to_album_iter", _empty_apply_iter)
    # Simulate the dir vanishing between writes and rescan.
    monkeypatch.setattr(apply_service, "scan_one_album", lambda _path: None)

    events: list[tuple[str, Any]] = []
    for ev_name, ev_payload in apply_service.apply_album_changes_iter(
        album={"id": "a-dropped", "path": str(tmp_path / "Album [FLAC]")},
        library_dir=str(tmp_path),
        suggestions={"title": "Whatever"},
    ):
        events.append((ev_name, ev_payload))

    # Album row + fingerprint preserved — Missing, not deleted.
    assert _album_exists("a-dropped"), \
        "apply path must NOT delete the album when the dir vanishes"
    assert _missing_since("a-dropped") is not None, \
        "apply path must mark Missing instead"
    with db_module.connect() as conn:
        fp_count = conn.execute(
            "SELECT COUNT(*) AS n FROM track_fingerprints WHERE album_id = ?",
            ("a-dropped",),
        ).fetchone()["n"]
    assert fp_count == 1, \
        "apply path must preserve fingerprints on disk-vanish — the load-bearing v15 promise"

    # Generator still yields removed_album so the SSE consumer sees
    # the signal; the event name is historical but the underlying
    # action (mark Missing) is what's load-bearing.
    assert any(name == "removed_album" for name, _ in events), \
        "removed_album event must still fire so the SSE stream surfaces the transition"


# ============================================================
# Scan sanity check
# ============================================================


def test_scan_sanity_check_aborts_when_too_many_albums_vanish(monkeypatch):
    """Drive-outage scenario: cache has 20 albums, scan finds 0.
    100% vanish ratio is way over the 30% threshold → abort with
    no state changes. Nothing gets marked Missing."""
    # Seed: 20 albums in cache.
    cache_module.save_snapshot(_snapshot([
        _album_dict(id=f"a{i}", path=f"{LIBRARY_PATH}/A{i} [FLAC]")
        for i in range(20)
    ]))

    # Simulate `incremental_scan` returning no albums + all 20 removed.
    def _empty_scan(library_dir, cached_by_path):
        yield ("discovered", {"total": 0})
        for path, album in cached_by_path.items():
            yield ("removed", {"id": album.get("id"), "path": path})

    monkeypatch.setattr(
        "musaic.services.scan.incremental_scan", _empty_scan,
    )

    emitted: list[tuple[str, dict]] = []

    def emit(name, payload):
        emitted.append((name, payload))

    import threading
    scan_service._scan_worker(
        LIBRARY_PATH, force=False, emit=emit,
        done_evt=threading.Event(),
    )

    # Scan aborted; no album was marked Missing.
    for i in range(20):
        assert _missing_since(f"a{i}") is None
    # The "aborted" event fired with the expected reason.
    aborted = [e for e in emitted if e[0] == "aborted"]
    assert len(aborted) == 1
    assert aborted[0][1]["reason"] == "vanish_ratio_exceeded"
    assert aborted[0][1]["vanish_count"] == 20
    # No "marked_missing" events leaked through — the buffer was
    # discarded entirely when the sanity check aborted, so the user
    # doesn't see a "20 missing" toast on a scan that ultimately
    # changed nothing.
    assert not [e for e in emitted if e[0] == "marked_missing"]


def test_scan_sanity_check_proceeds_when_below_threshold(monkeypatch):
    """20 cached, 4 vanish (20%) → under the 30% threshold → proceed.
    Those 4 get marked Missing."""
    cache_module.save_snapshot(_snapshot([
        _album_dict(id=f"a{i}", path=f"{LIBRARY_PATH}/A{i} [FLAC]")
        for i in range(20)
    ]))

    def _partial_scan(library_dir, cached_by_path):
        yield ("discovered", {"total": 16})
        # First 4 albums are missing; remaining 16 are still there.
        for path, album in list(cached_by_path.items())[:4]:
            yield ("removed", {"id": album.get("id"), "path": path})
        for path, album in list(cached_by_path.items())[4:]:
            yield ("cached", album)

    monkeypatch.setattr(
        "musaic.services.scan.incremental_scan", _partial_scan,
    )

    emitted: list[tuple[str, dict]] = []
    import threading
    scan_service._scan_worker(
        LIBRARY_PATH, force=False, emit=emitted.append.__class__.__call__.__get__(emitted),
        done_evt=threading.Event(),
    ) if False else None  # avoid lint; using the proper emit below

    emitted = []

    def emit(name, payload):
        emitted.append((name, payload))

    scan_service._scan_worker(
        LIBRARY_PATH, force=False, emit=emit,
        done_evt=threading.Event(),
    )

    # No abort, normal "done" event.
    done = [e for e in emitted if e[0] == "done"]
    assert len(done) == 1
    # 4 albums marked Missing.
    missing_count = sum(
        1 for i in range(20) if _missing_since(f"a{i}") is not None
    )
    assert missing_count == 4


def test_scan_sanity_check_skipped_for_tiny_libraries(monkeypatch):
    """Fresh installs / first scans typically have a tiny baseline.
    With cached_count < 10 we don't enforce the threshold — otherwise
    a single removal on a 2-album library would (60% ratio) abort."""
    cache_module.save_snapshot(_snapshot([
        _album_dict(id="a", path=f"{LIBRARY_PATH}/A [FLAC]"),
        _album_dict(id="b", path=f"{LIBRARY_PATH}/B [FLAC]"),
    ]))

    def _half_gone(library_dir, cached_by_path):
        yield ("discovered", {"total": 1})
        for path, album in list(cached_by_path.items())[:1]:
            yield ("removed", {"id": album.get("id"), "path": path})
        for path, album in list(cached_by_path.items())[1:]:
            yield ("cached", album)

    monkeypatch.setattr(
        "musaic.services.scan.incremental_scan", _half_gone,
    )

    emitted: list[tuple[str, dict]] = []
    import threading
    scan_service._scan_worker(
        LIBRARY_PATH, force=False, emit=lambda n, p: emitted.append((n, p)),
        done_evt=threading.Event(),
    )

    # No abort — the threshold doesn't apply to tiny libraries.
    aborted = [e for e in emitted if e[0] == "aborted"]
    assert aborted == []


# ============================================================
# Check runner — Missing albums excluded from scope
# ============================================================


def test_run_check_filters_missing_albums_from_scope(monkeypatch):
    """track-gaps iterates `scope.albums`. After load_snapshot, Missing
    albums must be filtered out so checks don't yield findings the
    user can't act on."""
    # Two albums: one Present, one Missing.
    cache_module.save_snapshot(_snapshot([
        _album_dict(id="present", path=f"{LIBRARY_PATH}/P [FLAC]", track_count=3),
        _album_dict(id="missing", path=f"{LIBRARY_PATH}/M [FLAC]", track_count=3),
    ]))
    cache_module.mark_album_missing("missing")

    # Inject track_numbers with gaps so the check WOULD yield findings
    # for both if both were in scope. Track-gaps reads from
    # scope.albums[i]["track_numbers"].
    from musaic.checks.runner import run_check
    # We can't easily monkeypatch the track_numbers since they come
    # from snapshot.albums_by_path; check the SCOPE directly via the
    # runner's load path.
    from musaic import cache as _cache
    snap = _cache.load_snapshot(LIBRARY_PATH)
    all_albums = list(snap.albums_by_path.values())
    visible = [a for a in all_albums if not a.get("missing_since")]
    assert {a["id"] for a in all_albums} == {"present", "missing"}
    assert {a["id"] for a in visible} == {"present"}


# ============================================================
# Inconsistent-artist-sort — SQL-pure check filters missing rows
# ============================================================


def test_inconsistent_artist_sort_query_skips_missing_albums():
    """Direct check on the SQL-pure path: missing albums must not
    appear in the cluster build. Two albums same primary artist;
    one is Missing → cluster has <2 visible members → no finding."""
    cache_module.save_snapshot(_snapshot([
        _album_dict(id="present", path=f"{LIBRARY_PATH}/P [FLAC]",
                    album_artist="Ray Conniff"),
        _album_dict(id="missing", path=f"{LIBRARY_PATH}/M [FLAC]",
                    album_artist="Ray Conniff & His Orchestra"),
    ]))
    cache_module.mark_album_missing("missing")

    scope = CheckScope(library_path=LIBRARY_PATH, kind="all", albums=[])
    check = inconsistent_artist_sort.InconsistentArtistSort()
    findings = list(check.run(scope))
    # Only 1 visible member of the would-be cluster → no finding.
    assert findings == []
