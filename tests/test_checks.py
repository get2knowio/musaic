"""Per-check tests for the merged identification + tag-sync checks.

After the 10 → 7 consolidation:
  - `unidentified-album` replaces `missing-fingerprints` + `missing-mbid`.
  - `tags-out-of-sync` replaces `missing-core-tags` + `missing-sort-tags`
    + `mb-canonical-mismatch`.

These tests insert album rows directly, monkeypatch the resolver so the
fixture controls anchor + canonical values, and exercise the run/fix/
try_quick_fix paths in isolation.
"""
from __future__ import annotations

import sqlite3

import pytest

from musaic import db as db_module
from musaic.checks import (
    inconsistent_artist_sort,
    tags_out_of_sync,
    unidentified_album,
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
    test doesn't bleed into another."""
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
    genre: str = "Rock",
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
                        title, artist, album_artist, year, genre,
                        track_count, sample_track,
                        is_compilation,
                        mb_release_group_mbid, mb_release_mbid,
                        mb_match_score, mb_last_checked_at,
                        album_artist_sort, artist_sort, album_sort
                    ) VALUES (
                        ?, ?, ?, ?,
                        ?, ?, ?, ?, ?,
                        ?, ?,
                        ?,
                        ?, ?, ?, ?,
                        ?, ?, ?
                    )
                    """,
                    (
                        id, LIBRARY_PATH, f"/fake/library/{id}", dir_name,
                        title, artist, album_artist, year, genre,
                        track_count, "",
                        1 if is_compilation else 0,
                        mb_release_group_mbid, mb_release_mbid,
                        mb_match_score, mb_last_checked_at,
                        album_artist_sort, artist_sort, album_sort,
                    ),
                )
            except sqlite3.IntegrityError:
                raise


def _insert_fingerprint(album_id: str, filename: str = "01.flac") -> None:
    """Insert one track_fingerprints row so an album counts as 'fingerprinted'."""
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            conn.execute(
                """
                INSERT INTO track_fingerprints (
                    album_id, filename, fingerprint, duration_s,
                    last_computed_at
                ) VALUES (?, ?, ?, ?, ?)
                """,
                (album_id, filename, "fp-bytes", 200.0, "2026-01-01T00:00:00Z"),
            )


def _make_resolution(
    *,
    album_id: str = "a1",
    anchor_mbid: str | None = "rg-1",
    anchor_strategy: str = "cache",
    canonical_title: str | None = None,
    canonical_album_artist: str | None = None,
    canonical_year: str | None = None,
    canonical_album_artist_sort: str | None = None,
    track_canonical_artist: str | None = None,
    track_filename: str = "01.flac",
) -> AlbumResolution:
    res = AlbumResolution(
        album_id=album_id,
        anchor=FieldFact(anchor_mbid, "musicbrainz", 0.95) if anchor_mbid else None,
        anchor_strategy=anchor_strategy if anchor_mbid else "none",
    )
    if canonical_title:
        res.canonical_title = FieldFact(canonical_title, "musicbrainz", 0.95)
    if canonical_album_artist:
        res.canonical_album_artist = FieldFact(canonical_album_artist, "musicbrainz", 0.95)
    if canonical_year:
        res.canonical_year = FieldFact(canonical_year, "musicbrainz", 0.95)
    if canonical_album_artist_sort:
        res.canonical_album_artist_sort = FieldFact(canonical_album_artist_sort, "musicbrainz", 0.95)
    if track_canonical_artist:
        res.tracks.append(TrackResolution(
            filename=track_filename,
            canonical_artist=FieldFact(track_canonical_artist, "musicbrainz", 0.95),
        ))
    return res


# ============================================================
# unidentified-album (replaces missing-fingerprints + missing-mbid)
# ============================================================


def test_unidentified_run_flags_album_missing_both_signals():
    """Album with no anchor AND no fingerprints → flagged with both
    prerequisites listed."""
    _insert_album(id="a1", mb_release_group_mbid=None)

    check = unidentified_album.UnidentifiedAlbum()
    findings = list(check.run(CheckScope(library_path=LIBRARY_PATH, kind="all")))

    assert len(findings) == 1
    f = findings[0]
    assert f.album_id == "a1"
    assert f.detail["needs_fingerprints"] is True
    assert f.detail["needs_anchor"] is True
    assert "Not fingerprinted" in f.summary or "fingerprint" in f.summary.lower()


def test_unidentified_run_flags_anchored_but_unfingerprinted():
    """Has anchor but no fingerprints → still flagged (fingerprints are
    a separate prerequisite for AcoustID-driven checks)."""
    _insert_album(id="a1", mb_release_group_mbid="rg-x")

    check = unidentified_album.UnidentifiedAlbum()
    findings = list(check.run(CheckScope(library_path=LIBRARY_PATH, kind="all")))

    assert len(findings) == 1
    assert findings[0].detail["needs_fingerprints"] is True
    assert findings[0].detail["needs_anchor"] is False


def test_unidentified_run_flags_fingerprinted_but_unanchored():
    """Has fingerprints but no anchor → flagged for the anchor only."""
    _insert_album(id="a1", mb_release_group_mbid=None)
    _insert_fingerprint("a1")

    check = unidentified_album.UnidentifiedAlbum()
    findings = list(check.run(CheckScope(library_path=LIBRARY_PATH, kind="all")))

    assert len(findings) == 1
    assert findings[0].detail["needs_fingerprints"] is False
    assert findings[0].detail["needs_anchor"] is True


def test_unidentified_run_skips_fully_identified():
    """Anchor + fingerprints + within staleness window → no finding."""
    _insert_album(id="a1", mb_release_group_mbid="rg-x")
    _insert_fingerprint("a1")

    check = unidentified_album.UnidentifiedAlbum()
    findings = list(check.run(CheckScope(library_path=LIBRARY_PATH, kind="all")))
    assert findings == []


def test_unidentified_run_respects_staleness_window():
    """Recent failed lookup (mb_last_checked_at within window) shouldn't
    re-surface."""
    from datetime import datetime, timezone
    recent = datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")
    _insert_album(id="a1", mb_release_group_mbid=None, mb_last_checked_at=recent)
    _insert_fingerprint("a1")  # fingerprints already done

    check = unidentified_album.UnidentifiedAlbum()
    findings = list(check.run(CheckScope(library_path=LIBRARY_PATH, kind="all")))
    assert findings == []


def test_unidentified_fix_uses_resolver_first(monkeypatch):
    """When fingerprints already exist and no hint given: resolver-first;
    success → no AI job."""
    _insert_album(id="a1", mb_release_group_mbid=None)
    _insert_fingerprint("a1")

    monkeypatch.setattr(
        resolver_service, "resolve_album",
        lambda _id: _make_resolution(anchor_mbid="rg-1", anchor_strategy="dir-search"),
    )

    def _explode_enqueue(*a, **kw):
        raise AssertionError("AI must not be enqueued when resolver succeeded")

    from musaic import jobs as jobs_module
    monkeypatch.setattr(jobs_module, "enqueue", _explode_enqueue)

    check = unidentified_album.UnidentifiedAlbum()
    result = check.fix(Finding(album_id="a1", summary="x", severity="info"))

    assert result.ok is True
    assert "rg-1" in result.message


def test_unidentified_fix_falls_through_to_ai_when_resolver_fails(monkeypatch):
    _insert_album(id="a1", mb_release_group_mbid=None)
    _insert_fingerprint("a1")

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

    check = unidentified_album.UnidentifiedAlbum()
    result = check.fix(Finding(album_id="a1", summary="x", severity="info"))

    assert result.ok is True
    assert result.job_id == "job-1"
    assert enqueue_calls[0]["job_type"] == "metadata"


def test_unidentified_fix_with_hint_skips_resolver(monkeypatch):
    """Hint → straight to AI (resolver can't use the user's context)."""
    _insert_album(id="a1", mb_release_group_mbid=None)
    _insert_fingerprint("a1")

    def _explode(_id):
        raise AssertionError("resolver should not be called when a hint is supplied")

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

    check = unidentified_album.UnidentifiedAlbum()
    result = check.fix(
        Finding(album_id="a1", summary="x", severity="info"),
        user_hint="1979 original pressing",
    )

    assert result.ok is True
    assert captured["hint"] == "1979 original pressing"


def test_unidentified_fingerprints_step_runs_when_missing(monkeypatch):
    """Fix on an unfingerprinted album triggers fingerprint_album first,
    then runs the resolver. Verifies both calls happen in order."""
    _insert_album(id="a1", mb_release_group_mbid=None)

    fingerprint_calls = []

    def fake_fingerprint_album(album_id, album_path, **kw):
        fingerprint_calls.append((album_id, album_path))
        # Simulate fingerprinting succeeding so the album gets some rows.
        _insert_fingerprint(album_id)
        return [type("R", (), {"acoustid_recording_mbid": "rec-1"})()]

    from musaic import fingerprint as fingerprint_module
    monkeypatch.setattr(fingerprint_module, "fingerprint_album", fake_fingerprint_album)

    monkeypatch.setattr(
        resolver_service, "resolve_album",
        lambda _id: _make_resolution(anchor_mbid="rg-1", anchor_strategy="fingerprint-vote"),
    )

    check = unidentified_album.UnidentifiedAlbum()
    result = check.fix(Finding(album_id="a1", summary="x", severity="info"))

    assert result.ok is True
    assert len(fingerprint_calls) == 1
    assert "fingerprinted 1 files" in result.message
    assert "rg-1" in result.message


def test_unidentified_quick_fix_returns_none_for_missing_album():
    check = unidentified_album.UnidentifiedAlbum()
    result = check.try_quick_fix(Finding(album_id="ghost", summary="x", severity="info"))
    assert result is None


# ============================================================
# tags-out-of-sync (replaces missing-core-tags + missing-sort-tags
#                   + mb-canonical-mismatch)
# ============================================================


def test_tags_out_of_sync_run_flags_canonical_drift(monkeypatch):
    """Anchored album whose album_artist disagrees with MB canonical."""
    _insert_album(
        id="a1",
        album_artist="The Beatles",
        mb_release_group_mbid="rg-1",
    )
    monkeypatch.setattr(
        resolver_service, "resolve_album",
        lambda _id: _make_resolution(canonical_album_artist="Beatles, The"),
    )

    check = tags_out_of_sync.TagsOutOfSync()
    findings = list(check.run(CheckScope(library_path=LIBRARY_PATH, kind="all")))

    assert len(findings) == 1
    f = findings[0]
    diffs = f.detail["album_diffs"]
    assert any(
        d["field"] == "album_artist"
        and d["current"] == "The Beatles"
        and d["canonical"] == "Beatles, The"
        for d in diffs
    )


def test_tags_out_of_sync_run_flags_missing_core_field():
    """Album with blank year (no anchor needed) → finding with
    missing_core populated."""
    _insert_album(id="a1", year="", mb_release_group_mbid=None)

    check = tags_out_of_sync.TagsOutOfSync()
    findings = list(check.run(CheckScope(library_path=LIBRARY_PATH, kind="all")))

    assert len(findings) == 1
    assert "year" in findings[0].detail["missing_core"]


def test_tags_out_of_sync_run_flags_missing_sort_tag(monkeypatch):
    """The Beatles + no album_artist_sort → finding with sort proposal."""
    _insert_album(
        id="a1", album_artist="The Beatles",
        album_artist_sort=None,
        mb_release_group_mbid=None,
    )

    check = tags_out_of_sync.TagsOutOfSync()
    findings = list(check.run(CheckScope(library_path=LIBRARY_PATH, kind="all")))

    assert len(findings) == 1
    assert findings[0].detail["sort_proposals"] == {"album_artist_sort": "Beatles, The"}


def test_tags_out_of_sync_run_clean_album_no_finding(monkeypatch):
    """Anchored, canonical-matching, all required fields present → no finding."""
    _insert_album(
        id="a1",
        album_artist="Beatles, The",
        title="Abbey Road",
        year="1969",
        genre="Rock",
        album_artist_sort=None,  # no article prefix on "Beatles, The" → fine
        mb_release_group_mbid="rg-1",
    )
    monkeypatch.setattr(
        resolver_service, "resolve_album",
        lambda _id: _make_resolution(
            canonical_album_artist="Beatles, The",
            canonical_title="Abbey Road",
            canonical_year="1969",
        ),
    )

    check = tags_out_of_sync.TagsOutOfSync()
    findings = list(check.run(CheckScope(library_path=LIBRARY_PATH, kind="all")))
    assert findings == []


def test_tags_out_of_sync_fix_applies_canonical_when_anchored(monkeypatch):
    """Anchored album with drift → resolver-driven apply, no AI."""
    _insert_album(
        id="a1", album_artist="The Beatles",
        mb_release_group_mbid="rg-1",
    )
    monkeypatch.setattr(
        resolver_service, "resolve_album",
        lambda _id: _make_resolution(canonical_album_artist="Beatles, The"),
    )

    captured = {}

    def fake_apply_direct(*, album_id, suggestions, track_suggestions, **kw):
        captured["suggestions"] = suggestions
        captured["track_suggestions"] = track_suggestions
        return (True, "Wrote 1 file")

    monkeypatch.setattr(
        tags_out_of_sync.direct_apply, "apply_direct", fake_apply_direct,
    )

    def _explode(*a, **kw):
        raise AssertionError("AI should not be enqueued when canonical apply suffices")

    from musaic import jobs as jobs_module
    monkeypatch.setattr(jobs_module, "enqueue", _explode)

    check = tags_out_of_sync.TagsOutOfSync()
    result = check.fix(Finding(album_id="a1", summary="x", severity="info"))

    assert result.ok is True
    assert captured["suggestions"]["album_artist"] == "Beatles, The"


def test_tags_out_of_sync_fix_falls_back_to_ai_for_missing_genre(monkeypatch):
    """Missing genre on an anchored album → genre is AI-only,
    so we should also enqueue an AI job after the canonical apply."""
    _insert_album(
        id="a1",
        album_artist="The Beatles",
        title="Abbey Road",
        year="1969",
        genre="",  # missing — AI fallback only
        mb_release_group_mbid="rg-1",
    )
    monkeypatch.setattr(
        resolver_service, "resolve_album",
        lambda _id: _make_resolution(
            canonical_album_artist="Beatles, The",
            canonical_title="Abbey Road",
            canonical_year="1969",
        ),
    )

    apply_calls = []

    def fake_apply_direct(**kw):
        apply_calls.append(kw)
        return (True, "ok")

    monkeypatch.setattr(
        tags_out_of_sync.direct_apply, "apply_direct", fake_apply_direct,
    )

    from dataclasses import dataclass

    @dataclass
    class _FakeJob:
        id: str = "job-genre"

    enqueue_calls = []

    def fake_enqueue(**kw):
        enqueue_calls.append(kw)
        return _FakeJob()

    from musaic import jobs as jobs_module
    monkeypatch.setattr(jobs_module, "enqueue", fake_enqueue)

    check = tags_out_of_sync.TagsOutOfSync()
    result = check.fix(Finding(album_id="a1", summary="x", severity="info"))

    assert result.ok is True
    assert result.job_id == "job-genre"
    assert len(apply_calls) == 1  # canonical write happened
    assert len(enqueue_calls) == 1  # AI follow-up for genre


def test_tags_out_of_sync_fix_unanchored_blank_core_goes_to_ai(monkeypatch):
    """Unanchored album missing title → resolver can't help → AI job."""
    _insert_album(id="a1", title="", mb_release_group_mbid=None)

    from dataclasses import dataclass

    @dataclass
    class _FakeJob:
        id: str = "job-ai"

    enqueue_calls = []

    def fake_enqueue(**kw):
        enqueue_calls.append(kw)
        return _FakeJob()

    from musaic import jobs as jobs_module
    monkeypatch.setattr(jobs_module, "enqueue", fake_enqueue)

    check = tags_out_of_sync.TagsOutOfSync()
    result = check.fix(Finding(album_id="a1", summary="x", severity="info"))

    assert result.ok is True
    assert result.job_id == "job-ai"


def test_tags_out_of_sync_fix_uses_mb_sort_when_available(monkeypatch):
    """Anchored album → resolver provides canonical sort → fix prefers
    MB sort-name over rotation."""
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

    monkeypatch.setattr(
        tags_out_of_sync.direct_apply, "apply_direct", fake_apply_direct,
    )

    check = tags_out_of_sync.TagsOutOfSync()
    result = check.fix(Finding(album_id="a1", summary="x", severity="info"))

    assert result.ok is True
    # MB lowercase variant survives — proves we didn't fall through to rotation.
    assert captured["suggestions"]["album_artist_sort"] == "Beatles, the"


def test_tags_out_of_sync_fix_falls_back_to_rotation_when_unanchored(monkeypatch):
    """Unanchored album with article prefix → article rotation."""
    _insert_album(
        id="a1",
        album_artist="The Beatles",
        album_artist_sort=None,
        mb_release_group_mbid=None,
    )

    captured = {}

    def fake_apply_direct(*, album_id, suggestions, **kw):
        captured["suggestions"] = suggestions
        return (True, "ok")

    monkeypatch.setattr(
        tags_out_of_sync.direct_apply, "apply_direct", fake_apply_direct,
    )

    check = tags_out_of_sync.TagsOutOfSync()
    result = check.fix(Finding(album_id="a1", summary="x", severity="info"))

    assert result.ok is True
    assert captured["suggestions"]["album_artist_sort"] == "Beatles, The"


def test_tags_out_of_sync_quick_fix_skips_when_nothing_deterministic(monkeypatch):
    """Bulk path on an unanchored album with only AI-fillable fields →
    soft fail (the per-finding fix handles AI escalation)."""
    _insert_album(id="a1", title="", mb_release_group_mbid=None)

    check = tags_out_of_sync.TagsOutOfSync()
    result = check.try_quick_fix(Finding(album_id="a1", summary="x", severity="info"))

    assert result is not None
    assert result.ok is False


# ============================================================
# inconsistent-artist-sort — _sort_to_primary_token edge cases
# ============================================================


def test_sort_to_primary_token_handles_band_suffix_in_sort_form():
    """MB-convention sort names for bands include the credit suffix:
    'Fred, John & His Playboy Band'. Naive un-rotation followed by
    suffix-stripping would leave just 'john' — making the cluster look
    untrustworthy forever. Stripping the suffix FIRST gives the correct
    'john fred' token so the apply round-trips cleanly."""
    token = inconsistent_artist_sort._sort_to_primary_token(
        "Fred, John & His Playboy Band"
    )
    assert token == "john fred"


def test_sort_to_primary_token_basic_last_first():
    assert inconsistent_artist_sort._sort_to_primary_token("Conniff, Ray") == "ray conniff"


def test_sort_to_primary_token_article_form():
    assert inconsistent_artist_sort._sort_to_primary_token("Beatles, The") == "beatles"


def test_sort_to_primary_token_single_name():
    assert inconsistent_artist_sort._sort_to_primary_token("Madonna") == "madonna"


def test_classify_cluster_consistent_with_band_suffix_sort():
    """End-to-end: a cluster of two tracks credited 'John Fred & His
    Playboy Band' with identical sort 'Fred, John & His Playboy Band'
    should classify as `consistent` — same sort, primary tokens match
    after the suffix-stripping fix."""
    members = [
        {
            "kind": "track",
            "album_id": "a1",
            "filenames": ["02 - foo.flac"],
            "artist": "John Fred & His Playboy Band",
            "sort": "Fred, John & His Playboy Band",
        },
        {
            "kind": "track",
            "album_id": "a2",
            "filenames": ["15 - bar.flac"],
            "artist": "John Fred & His Playboy Band",
            "sort": "Fred, John & His Playboy Band",
        },
    ]
    state, value = inconsistent_artist_sort._classify_cluster(
        members, cluster_token="john fred",
    )
    assert state == "consistent"
    assert value == "Fred, John & His Playboy Band"


def test_sort_to_primary_token_handles_amp_the_suffix():
    """`& the X` (Buddy Holly & the Crickets, Tommy James & the
    Shondells, Diana Ross & the Supremes) is just as common as the
    `& His X` form. The regex used to only match `& His/Her X` plus
    `and the X` — stranding the `&`-form sort values as untrustworthy."""
    assert (
        inconsistent_artist_sort._sort_to_primary_token("James, Tommy & the Shondells")
        == "tommy james"
    )
    assert (
        inconsistent_artist_sort._sort_to_primary_token("Holly, Buddy & the Crickets")
        == "buddy holly"
    )


def test_classify_cluster_consistent_with_amp_the_suffix():
    """End-to-end: three tracks on three different compilations credited
    'Tommy James & the Shondells' with identical sort
    'James, Tommy & the Shondells' classify as consistent — no finding."""
    members = [
        {
            "kind": "track", "album_id": "a1",
            "filenames": ["30 - hanky.flac"],
            "artist": "Tommy James & the Shondells",
            "sort": "James, Tommy & the Shondells",
        },
        {
            "kind": "track", "album_id": "a2",
            "filenames": ["04 - hanky.flac"],
            "artist": "Tommy James & the Shondells",
            "sort": "James, Tommy & the Shondells",
        },
        {
            "kind": "track", "album_id": "a3",
            "filenames": ["08 - mony.flac"],
            "artist": "Tommy James & the Shondells",
            "sort": "James, Tommy & the Shondells",
        },
    ]
    state, value = inconsistent_artist_sort._classify_cluster(
        members, cluster_token="tommy james",
    )
    assert state == "consistent"
    assert value == "James, Tommy & the Shondells"
