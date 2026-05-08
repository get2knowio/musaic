"""Per-check tests for the resolver-consuming checks
(`mb_canonical_mismatch`, `missing_mbid`, `missing_sort_tags`).

The resolver itself is exercised in test_resolver.py. These tests
mock the resolver so we can isolate the check-level glue: SQL
filters, diff loops, fix paths (deterministic + AI-fallback),
bulk paths, and the apply orchestrator.

Pattern: each test inserts a small set of album rows directly into
the test DB, monkeypatches `services.resolver.resolve_album` to
return whatever AlbumResolution shape we want, then calls the check
method under test.
"""
from __future__ import annotations

import sqlite3

import pytest

from musaic import db as db_module
from musaic.checks import (
    mb_canonical_mismatch,
    missing_mbid,
    missing_sort_tags,
)
from musaic.checks.base import CheckScope, Finding
from musaic.services import resolver as resolver_service
from musaic.services.resolver import (
    AlbumResolution,
    FieldFact,
    TrackResolution,
)


# ---------- Fixtures + helpers ----------


LIBRARY_PATH = "/fake/library"


@pytest.fixture(autouse=True)
def _clean_albums_table():
    """Wipe albums + related tables between tests so state from one
    test doesn't bleed into another. The DB itself was created by
    conftest.py via the env-var redirect to a session-shared tmp dir."""
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            conn.execute("DELETE FROM albums")
            conn.execute("DELETE FROM libraries")
            conn.execute("DELETE FROM track_fingerprints")
            conn.execute("DELETE FROM check_findings")
            conn.execute("DELETE FROM check_runs")
            conn.execute("DELETE FROM ai_jobs")
    yield


def _ensure_library() -> None:
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            conn.execute(
                "INSERT OR IGNORE INTO libraries(path) VALUES (?)",
                (LIBRARY_PATH,),
            )


def _insert_album(
    *,
    id: str,
    dir_name: str = "Test Artist - Test Album [FLAC]",
    album_artist: str = "Test Artist",
    artist: str = "Test Artist",
    title: str = "Test Album",
    year: str = "1979",
    mb_release_group_mbid: str | None = None,
    mb_release_mbid: str | None = None,
    mb_match_score: int | None = None,
    mb_last_checked_at: str | None = None,
    album_artist_sort: str | None = None,
    artist_sort: str | None = None,
    album_sort: str | None = None,
    is_compilation: bool = False,
    track_count: int = 3,
) -> None:
    _ensure_library()
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            try:
                conn.execute(
                    """
                    INSERT INTO albums (
                        id, library_path, path, dir_name,
                        title, artist, album_artist, year,
                        track_count, sample_track,
                        is_compilation,
                        mb_release_group_mbid, mb_release_mbid,
                        mb_match_score, mb_last_checked_at,
                        album_artist_sort, artist_sort, album_sort
                    ) VALUES (
                        ?, ?, ?, ?,
                        ?, ?, ?, ?,
                        ?, ?,
                        ?,
                        ?, ?, ?, ?,
                        ?, ?, ?
                    )
                    """,
                    (
                        id, LIBRARY_PATH, f"/fake/library/{id}", dir_name,
                        title, artist, album_artist, year,
                        track_count, "",
                        1 if is_compilation else 0,
                        mb_release_group_mbid, mb_release_mbid,
                        mb_match_score, mb_last_checked_at,
                        album_artist_sort, artist_sort, album_sort,
                    ),
                )
            except sqlite3.IntegrityError:
                # Reusing same id within a test? Tests should use distinct
                # ids; surface this loudly rather than silently update.
                raise


def _make_resolution(
    *,
    album_id: str = "a1",
    anchor_mbid: str | None = "rg-1",
    anchor_strategy: str = "cache",
    canonical_album_artist: str | None = "Canonical Artist",
    canonical_album_artist_sort: str | None = None,
    track_canonical_artist: str | None = None,
    track_filename: str = "01.flac",
) -> AlbumResolution:
    """Build a minimal AlbumResolution for monkeypatch returns. Only
    populates the fields each check actually reads off the resolver."""
    res = AlbumResolution(
        album_id=album_id,
        anchor=FieldFact(anchor_mbid, "musicbrainz", 0.95) if anchor_mbid else None,
        anchor_strategy=anchor_strategy if anchor_mbid else "none",
    )
    if canonical_album_artist:
        res.canonical_album_artist = FieldFact(canonical_album_artist, "musicbrainz", 0.95)
    if canonical_album_artist_sort:
        res.canonical_album_artist_sort = FieldFact(canonical_album_artist_sort, "musicbrainz", 0.95)
    if track_canonical_artist:
        res.tracks.append(TrackResolution(
            filename=track_filename,
            canonical_artist=FieldFact(track_canonical_artist, "musicbrainz", 0.95),
        ))
    return res


# ---------- mb_canonical_mismatch ----------


def test_mb_canonical_mismatch_run_flags_album_artist_drift(monkeypatch):
    """Album whose current album_artist disagrees with MB's canonical
    form gets a finding."""
    _insert_album(
        id="a1",
        album_artist="The Beatles",
        mb_release_group_mbid="rg-1",
    )
    monkeypatch.setattr(
        resolver_service, "resolve_album",
        lambda _id: _make_resolution(canonical_album_artist="Beatles, The"),
    )

    check = mb_canonical_mismatch.MbCanonicalMismatch()
    findings = list(check.run(CheckScope(library_path=LIBRARY_PATH, kind="all")))

    assert len(findings) == 1
    f = findings[0]
    assert f.album_id == "a1"
    assert f.detail["album_artist_diff"] == {
        "current": "The Beatles",
        "canonical": "Beatles, The",
    }


def test_mb_canonical_mismatch_run_skips_when_canonical_matches(monkeypatch):
    """Whitespace+case-insensitive equivalence — no finding emitted."""
    _insert_album(
        id="a1",
        album_artist="Beatles, The",
        mb_release_group_mbid="rg-1",
    )
    monkeypatch.setattr(
        resolver_service, "resolve_album",
        lambda _id: _make_resolution(canonical_album_artist="Beatles, The"),
    )

    check = mb_canonical_mismatch.MbCanonicalMismatch()
    assert list(check.run(CheckScope(library_path=LIBRARY_PATH, kind="all"))) == []


def test_mb_canonical_mismatch_run_skips_unanchored_albums(monkeypatch):
    """SQL pre-filter excludes albums without an MBID anchor — the
    check shouldn't even try to resolve them."""
    _insert_album(id="a1", album_artist="Anything", mb_release_group_mbid=None)

    # Resolver shouldn't even be called.
    def _explode(_id):
        raise AssertionError("resolver should not be called for unanchored albums")

    monkeypatch.setattr(resolver_service, "resolve_album", _explode)

    check = mb_canonical_mismatch.MbCanonicalMismatch()
    assert list(check.run(CheckScope(library_path=LIBRARY_PATH, kind="all"))) == []


def test_mb_canonical_mismatch_apply_calls_apply_direct_with_resolver_values(monkeypatch):
    """apply_canonical reads canonical fields off the resolver and
    forwards them to apply_direct as suggestions."""
    _insert_album(id="a1", album_artist="The Beatles", mb_release_group_mbid="rg-1")
    monkeypatch.setattr(
        resolver_service, "resolve_album",
        lambda _id: _make_resolution(canonical_album_artist="Beatles, The"),
    )

    captured = {}

    def fake_apply_direct(*, album_id, suggestions, track_suggestions, rerun_checks):
        captured["album_id"] = album_id
        captured["suggestions"] = suggestions
        captured["track_suggestions"] = track_suggestions
        return (True, "Wrote 1 file")

    from musaic.checks import _apply as direct_apply
    monkeypatch.setattr(direct_apply, "apply_direct", fake_apply_direct)

    result = mb_canonical_mismatch.apply_canonical(
        album_id="a1",
        accept_album_artist=True,
        accept_track_filenames=[],
    )

    assert result["ok"] is True
    assert captured["suggestions"] == {"album_artist": "Beatles, The"}
    assert captured["track_suggestions"] is None  # falsy ⇒ omitted


# ---------- missing_mbid ----------


def test_missing_mbid_run_flags_unanchored_albums():
    """Album without an MBID + never previously checked → flagged."""
    _insert_album(id="a1", mb_release_group_mbid=None, mb_last_checked_at=None)
    _insert_album(id="a2", mb_release_group_mbid="rg-x")  # already anchored

    check = missing_mbid.MissingMbid()
    findings = list(check.run(CheckScope(library_path=LIBRARY_PATH, kind="all")))

    assert len(findings) == 1
    assert findings[0].album_id == "a1"


def test_missing_mbid_run_skips_recently_checked_albums():
    """An album we recently tried (within the staleness window) shouldn't
    re-surface — the empty `mb_last_checked_at` row keeps it visible."""
    from datetime import datetime, timezone
    recent = datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")
    _insert_album(id="a1", mb_release_group_mbid=None, mb_last_checked_at=recent)

    check = missing_mbid.MissingMbid()
    assert list(check.run(CheckScope(library_path=LIBRARY_PATH, kind="all"))) == []


def test_missing_mbid_fix_uses_resolver_first(monkeypatch):
    """fix() with no hint runs the resolver; if it anchors, return ok
    without enqueuing AI."""
    _insert_album(id="a1", mb_release_group_mbid=None)
    monkeypatch.setattr(
        resolver_service, "resolve_album",
        lambda _id: _make_resolution(anchor_mbid="rg-1", anchor_strategy="dir-search"),
    )

    # AI enqueue must NOT be called.
    def _explode(*a, **kw):
        raise AssertionError("AI should not be enqueued when resolver succeeded")

    from musaic import jobs as jobs_module
    monkeypatch.setattr(jobs_module, "enqueue", _explode)

    check = missing_mbid.MissingMbid()
    result = check.fix(Finding(album_id="a1", summary="x", severity="info"))

    assert result.ok is True
    assert "rg-1" in result.message
    assert "dir-search" in result.message


def test_missing_mbid_fix_falls_through_to_ai_when_resolver_fails(monkeypatch):
    """When the resolver returns no anchor, fix() enqueues an AI job."""
    _insert_album(id="a1", mb_release_group_mbid=None)
    monkeypatch.setattr(
        resolver_service, "resolve_album",
        lambda _id: _make_resolution(anchor_mbid=None),
    )

    from dataclasses import dataclass

    @dataclass
    class _FakeJob:
        id: str = "job-1"

    enqueue_calls = []

    def fake_enqueue(**kw):
        enqueue_calls.append(kw)
        return _FakeJob()

    from musaic import jobs as jobs_module
    monkeypatch.setattr(jobs_module, "enqueue", fake_enqueue)

    check = missing_mbid.MissingMbid()
    result = check.fix(Finding(album_id="a1", summary="x", severity="info"))

    assert result.ok is True
    assert result.job_id == "job-1"
    assert len(enqueue_calls) == 1
    assert enqueue_calls[0]["job_type"] == "metadata"
    assert enqueue_calls[0]["hint"] == ""


def test_missing_mbid_fix_with_hint_skips_resolver(monkeypatch):
    """A user-supplied hint means we go straight to AI — skipping the
    resolver would discard the user's added context."""
    _insert_album(id="a1", mb_release_group_mbid=None)

    def _explode(_id):
        raise AssertionError("resolver should not be called when a hint was supplied")

    monkeypatch.setattr(resolver_service, "resolve_album", _explode)

    from dataclasses import dataclass

    @dataclass
    class _FakeJob:
        id: str = "job-h"

    captured = {}

    def fake_enqueue(**kw):
        captured.update(kw)
        return _FakeJob()

    from musaic import jobs as jobs_module
    monkeypatch.setattr(jobs_module, "enqueue", fake_enqueue)

    check = missing_mbid.MissingMbid()
    result = check.fix(
        Finding(album_id="a1", summary="x", severity="info"),
        user_hint="1979 original pressing, not the 2012 remaster",
    )

    assert result.ok is True
    assert captured["hint"] == "1979 original pressing, not the 2012 remaster"


def test_missing_mbid_quick_fix_returns_none_for_missing_album():
    """Bulk path: if the album row vanished, return None so the bulk
    dispatcher counts it as 'not-applicable' rather than 'skipped'."""
    check = missing_mbid.MissingMbid()
    result = check.try_quick_fix(Finding(album_id="ghost", summary="x", severity="info"))
    assert result is None


# ---------- missing_sort_tags ----------


def test_missing_sort_tags_run_flags_article_prefix_with_empty_sort():
    _insert_album(
        id="a1", album_artist="The Beatles",
        album_artist_sort=None,
    )

    check = missing_sort_tags.MissingSortTags()
    findings = list(check.run(CheckScope(library_path=LIBRARY_PATH, kind="all")))

    assert len(findings) == 1
    assert findings[0].album_id == "a1"
    proposed = {m["field"]: m["proposed_sort"] for m in findings[0].detail["missing"]}
    assert proposed["album_artist"] == "Beatles, The"


def test_missing_sort_tags_run_skips_when_sort_already_set():
    _insert_album(
        id="a1", album_artist="The Beatles",
        album_artist_sort="Beatles, The",
    )
    check = missing_sort_tags.MissingSortTags()
    assert list(check.run(CheckScope(library_path=LIBRARY_PATH, kind="all"))) == []


def test_missing_sort_tags_run_skips_when_no_article_prefix():
    """Personal-name cases (Bob Dylan → Dylan, Bob) intentionally don't
    fire here — they need MB data and would defeat cheap-cost."""
    _insert_album(id="a1", album_artist="Bob Dylan", album_artist_sort=None)
    check = missing_sort_tags.MissingSortTags()
    assert list(check.run(CheckScope(library_path=LIBRARY_PATH, kind="all"))) == []


def test_missing_sort_tags_fix_prefers_mb_sort_when_anchored(monkeypatch):
    """Anchored album → resolver returns canonical sort → fix uses MB's
    value instead of mechanical article rotation. Useful for credit
    permutations like 'Ray Conniff & His Orchestra' → 'Conniff, Ray',
    which rotation can't produce. We use 'The Beatles' → 'Beatles, the'
    (lowercase article) to verify the mechanical path WASN'T used —
    rotation would produce 'Beatles, The'."""
    _insert_album(
        id="a1",
        album_artist="The Beatles",
        album_artist_sort=None,
        mb_release_group_mbid="rg-1",
    )
    monkeypatch.setattr(
        resolver_service, "resolve_album",
        lambda _id: _make_resolution(canonical_album_artist_sort="Beatles, the"),
    )

    captured = {}

    def fake_apply_direct(*, album_id, suggestions, **kw):
        captured["suggestions"] = suggestions
        return (True, "ok")

    from musaic.checks import _apply as direct_apply
    monkeypatch.setattr(direct_apply, "apply_direct", fake_apply_direct)

    check = missing_sort_tags.MissingSortTags()
    result = check.fix(Finding(album_id="a1", summary="x", severity="info"))

    assert result.ok is True
    # MB's lowercase-article variant survives — we didn't fall through
    # to article rotation (which would have produced "Beatles, The").
    assert captured["suggestions"]["album_artist_sort"] == "Beatles, the"


def test_missing_sort_tags_fix_falls_back_to_rotation_when_no_anchor(monkeypatch):
    """Album without an MBID anchor → resolver isn't called → rotation
    wins. The mb_release_group_mbid IS NULL guard short-circuits."""
    _insert_album(
        id="a1",
        album_artist="The Beatles",
        album_artist_sort=None,
        mb_release_group_mbid=None,
    )

    def _explode(_id):
        raise AssertionError("resolver should not be called for unanchored albums")

    monkeypatch.setattr(resolver_service, "resolve_album", _explode)

    captured = {}

    def fake_apply_direct(*, album_id, suggestions, **kw):
        captured["suggestions"] = suggestions
        return (True, "ok")

    from musaic.checks import _apply as direct_apply
    monkeypatch.setattr(direct_apply, "apply_direct", fake_apply_direct)

    check = missing_sort_tags.MissingSortTags()
    result = check.fix(Finding(album_id="a1", summary="x", severity="info"))

    assert result.ok is True
    assert captured["suggestions"]["album_artist_sort"] == "Beatles, The"
