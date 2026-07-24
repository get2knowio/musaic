"""Unit tests for `musaic.services.resolver`.

The cascade has many branches (cache hit, dir-search win,
fingerprint-vote win, vote-share thresholds, alignment fallback) and
no easy way to exercise them end-to-end without real MB / fpcalc
calls. These tests stub out every external dependency so the strategy
selection + math + alignment logic can be exercised hermetically.

Pattern: each test uses a fresh `_resolver_state` fixture that wipes
the in-process cache, then `monkeypatch.setattr` to swap in the
stubbed network/disk callouts. The album dict + track list shapes
mirror what the real `services.albums` helpers return.
"""
from __future__ import annotations

from dataclasses import dataclass

import pytest

from musaic import mb_lookup
from musaic.services import resolver

# ---------- Fixtures ----------


@pytest.fixture(autouse=True)
def _reset_resolver_state():
    """Drop the in-process resolution cache so test order doesn't leak."""
    resolver.invalidate()
    yield
    resolver.invalidate()


@pytest.fixture
def fake_album():
    """A minimal album dict matching the shape `services.albums.cached_album_dict`
    returns. Five generic instrumental tracks — close to the kind of album
    where AcoustID fingerprinting is the only deterministic anchor."""
    return {
        "id": "abc-123",
        "path": "/tmp/fake-album",
        "dir_name": "Test Artist - Test Album [FLAC]",
        "dir_artist": "Test Artist",
        "dir_album": "Test Album",
        "is_compilation": False,
        "track_count": 3,
        "total_seconds": 600.0,
        "title": "Test Album",
        "album_artist": "Test Artist",
        "artist": "Test Artist",
        "year": "1979",
        "genre": "Easy Listening",
    }


@pytest.fixture
def fake_tracks():
    """FLAC-on-disk track records: filename, tracknumber, title, artist,
    duration. Returned by `services.albums.read_tracks`."""
    return [
        {"filename": "01.flac", "tracknumber": "1", "title": "Old Title 1", "artist": "Test Artist", "duration": 200.0},
        {"filename": "02.flac", "tracknumber": "2", "title": "Old Title 2", "artist": "Test Artist", "duration": 195.0},
        {"filename": "03.flac", "tracknumber": "3", "title": "Old Title 3", "artist": "Test Artist", "duration": 205.0},
    ]


def _stub_resolver_inputs(monkeypatch, album, tracks, *, fingerprints=None, anchor_row=None):
    """Pin the album-side inputs to the resolver. Each parameter:
      - `fingerprints`: rows as cache_module.get_track_fingerprints would return
      - `anchor_row`: dict matching cache_module.get_album_mb_anchor return shape
    """
    monkeypatch.setattr(resolver, "cached_album_dict", lambda _id: album)
    monkeypatch.setattr(resolver, "list_album_filenames", lambda _p: [t["filename"] for t in tracks])
    monkeypatch.setattr(resolver, "read_tracks", lambda _p: tracks)
    monkeypatch.setattr(resolver.cache_module, "get_track_fingerprints", lambda _id: fingerprints or [])
    monkeypatch.setattr(resolver.cache_module, "get_album_mb_anchor", lambda _id: anchor_row)
    # Anchor writes go to the cache as a Tier 0c side-effect; nullify in tests.
    monkeypatch.setattr(resolver.cache_module, "set_album_mb_anchor", lambda *a, **kw: None)


def _make_mb_match(*, mbid="rg-mbid", score=95, with_tracklist=False):
    """Build a MusicBrainzMatch for stubbing find_release / find_release_cached."""
    tracks = []
    if with_tracklist:
        tracks = [
            mb_lookup.MBTrack(position=1, title="Canonical Title 1", length_ms=200_000, recording_mbid="rec-1"),
            mb_lookup.MBTrack(position=2, title="Canonical Title 2", length_ms=195_000, recording_mbid="rec-2"),
            mb_lookup.MBTrack(position=3, title="Canonical Title 3", length_ms=205_000, recording_mbid="rec-3"),
        ]
    return mb_lookup.MusicBrainzMatch(
        confident=score >= mb_lookup.HIGH_CONFIDENCE_SCORE,
        score=score,
        confidence="high" if score >= mb_lookup.HIGH_CONFIDENCE_SCORE else "medium",
        mbid=mbid,
        title="Canonical Album",
        artist="Canonical Artist",
        year="1979",
        primary_genre="Easy Listening",
        cover_url="https://coverartarchive.org/release-group/rg-mbid/front",
        cover_url_verified=True,
        tracklist=tracks,
        notes=f"Matched score {score}",
    )


# ---------- Tier 0a: cache hit ----------


def test_tier_0a_cache_hit_short_circuits_other_strategies(monkeypatch, fake_album, fake_tracks):
    """When the album already has an mb_release_group_mbid stamped on
    its row, dir-search and fingerprint-vote MUST NOT run."""
    anchor_row = {
        "release_group_mbid": "cached-mbid",
        "release_mbid": None,
        "score": 92,
        "last_checked_at": "2026-05-01T00:00:00Z",
    }
    _stub_resolver_inputs(monkeypatch, fake_album, fake_tracks, anchor_row=anchor_row)

    # Anyone calling these strategies is a bug.
    def _explode(*a, **kw):
        raise AssertionError("Tier 0b/0c should not run when cache is fresh")

    monkeypatch.setattr(mb_lookup, "find_release", _explode)
    monkeypatch.setattr(resolver.fp_module, "is_available", _explode)

    # Tier 1 still fires off the cached anchor — stub the tracklist call.
    monkeypatch.setattr(mb_lookup, "find_release_cached", lambda *a, **kw: _make_mb_match(mbid="cached-mbid", with_tracklist=True))

    result = resolver.resolve_album(fake_album["id"])

    assert result.anchor is not None
    assert result.anchor.value == "cached-mbid"
    assert result.anchor.source == "cache"
    assert result.anchor_strategy == "cache"


# ---------- Tier 0b: dir-search ----------


def test_tier_0b_dir_search_anchors_album(monkeypatch, fake_album, fake_tracks):
    _stub_resolver_inputs(monkeypatch, fake_album, fake_tracks)

    monkeypatch.setattr(mb_lookup, "find_release", lambda *a, **kw: _make_mb_match(mbid="dir-mbid", score=95))
    monkeypatch.setattr(mb_lookup, "find_release_cached", lambda *a, **kw: _make_mb_match(mbid="dir-mbid", score=95, with_tracklist=True))

    result = resolver.resolve_album(fake_album["id"])

    assert result.anchor is not None
    assert result.anchor.value == "dir-mbid"
    assert result.anchor_strategy == "dir-search"
    assert "Tier 0b" in " ".join(result.notes)


def test_tier_0b_low_score_falls_through_to_fingerprint(monkeypatch, fake_album, fake_tracks):
    """Below MEDIUM_CONFIDENCE_SCORE, Tier 0b doesn't anchor."""
    _stub_resolver_inputs(monkeypatch, fake_album, fake_tracks)

    monkeypatch.setattr(
        mb_lookup, "find_release",
        lambda *a, **kw: _make_mb_match(mbid="weak-mbid", score=mb_lookup.MEDIUM_CONFIDENCE_SCORE - 1),
    )
    # Force fp unavailable so we cleanly land at unanchored.
    monkeypatch.setattr(resolver.fp_module, "is_available", lambda: (False, "no fpcalc"))

    result = resolver.resolve_album(fake_album["id"])

    assert result.anchor is None
    assert result.anchor_strategy == "none"


# ---------- Tier 0c: fingerprint vote ----------


@dataclass
class _FakeTF:
    filename: str
    fingerprint: str = "fp"
    duration_s: float = 200.0
    acoustid_recording_mbid: str | None = None
    acoustid_score: float | None = None


def test_tier_0c_unanimous_fingerprint_vote(monkeypatch, fake_album, fake_tracks):
    """All three tracks fingerprint to recordings on the same release-group
    → high-confidence anchor via Tier 0c."""
    _stub_resolver_inputs(monkeypatch, fake_album, fake_tracks)

    # Tier 0b: no match.
    monkeypatch.setattr(mb_lookup, "find_release", lambda *a, **kw: _make_mb_match(mbid=None, score=0))
    # Fingerprinting available + returns 3 strong matches.
    monkeypatch.setattr(resolver.fp_module, "is_available", lambda: (True, "ok"))
    monkeypatch.setattr(
        resolver.fp_module, "fingerprint_album",
        lambda *a, **kw: [
            _FakeTF("01.flac", acoustid_recording_mbid="rec-1", acoustid_score=0.95),
            _FakeTF("02.flac", acoustid_recording_mbid="rec-2", acoustid_score=0.92),
            _FakeTF("03.flac", acoustid_recording_mbid="rec-3", acoustid_score=0.88),
        ],
    )
    # Every recording is on release-group "fp-rg".
    monkeypatch.setattr(
        mb_lookup, "get_recording_release_groups",
        lambda _mbid, **kw: [{"release_group_mbid": "fp-rg", "title": "FP Album"}],
    )
    # Tier 1 fetches the canonical tracklist.
    monkeypatch.setattr(mb_lookup, "find_release_cached", lambda *a, **kw: _make_mb_match(mbid="fp-rg", with_tracklist=True))

    result = resolver.resolve_album(fake_album["id"])

    assert result.anchor is not None
    assert result.anchor.value == "fp-rg"
    assert result.anchor_strategy == "fingerprint-vote"
    assert result.anchor.confidence >= 0.80
    # The cached_fps stub in the resolver re-reads after Tier 0c, so the
    # tracks should carry their per-track recording_mbids — but the stub
    # for get_track_fingerprints returned [] before Tier 0c ran. After
    # Tier 0c the resolver re-reads — verify the post-vote behavior in a
    # dedicated test below.


def test_tier_0c_split_vote_below_threshold_no_anchor(monkeypatch, fake_album, fake_tracks):
    """When the winning release-group claims only 50% of weighted votes,
    we land at MEDIUM_CONFIDENCE_SCORE; below that threshold (under 50%)
    we don't anchor at all."""
    _stub_resolver_inputs(monkeypatch, fake_album, fake_tracks)
    monkeypatch.setattr(mb_lookup, "find_release", lambda *a, **kw: _make_mb_match(mbid=None, score=0))
    monkeypatch.setattr(resolver.fp_module, "is_available", lambda: (True, "ok"))
    monkeypatch.setattr(
        resolver.fp_module, "fingerprint_album",
        lambda *a, **kw: [
            _FakeTF("01.flac", acoustid_recording_mbid="rec-1", acoustid_score=0.5),
            _FakeTF("02.flac", acoustid_recording_mbid="rec-2", acoustid_score=0.5),
            _FakeTF("03.flac", acoustid_recording_mbid="rec-3", acoustid_score=0.5),
        ],
    )

    # Each recording lives on a different release-group → 33% share each.
    rg_lookup = {
        "rec-1": [{"release_group_mbid": "rg-A", "title": "A"}],
        "rec-2": [{"release_group_mbid": "rg-B", "title": "B"}],
        "rec-3": [{"release_group_mbid": "rg-C", "title": "C"}],
    }
    monkeypatch.setattr(mb_lookup, "get_recording_release_groups", lambda mbid, **kw: rg_lookup.get(mbid))

    result = resolver.resolve_album(fake_album["id"])
    assert result.anchor is None


def test_tier_0c_skipped_when_fingerprinting_unavailable(monkeypatch, fake_album, fake_tracks):
    _stub_resolver_inputs(monkeypatch, fake_album, fake_tracks)
    monkeypatch.setattr(mb_lookup, "find_release", lambda *a, **kw: _make_mb_match(mbid=None, score=0))
    monkeypatch.setattr(resolver.fp_module, "is_available", lambda: (False, "fpcalc missing"))

    result = resolver.resolve_album(fake_album["id"])
    assert result.anchor is None
    assert result.unresolved_track_count == len(fake_tracks)


# ---------- Tier 1: alignment ----------


def test_tier_1_aligns_tracks_by_position_and_duration(monkeypatch, fake_album, fake_tracks):
    """When tracknumber + duration agree, every track gets canonical_*
    populated."""
    _stub_resolver_inputs(monkeypatch, fake_album, fake_tracks)
    monkeypatch.setattr(mb_lookup, "find_release", lambda *a, **kw: _make_mb_match(mbid="rg-1", score=95))
    monkeypatch.setattr(mb_lookup, "find_release_cached", lambda *a, **kw: _make_mb_match(mbid="rg-1", score=95, with_tracklist=True))

    result = resolver.resolve_album(fake_album["id"])

    assert result.unresolved_track_count == 0
    assert all(t.canonical_title is not None for t in result.tracks)
    assert result.tracks[0].canonical_title.value == "Canonical Title 1"
    assert result.tracks[0].canonical_tracknumber.value == "1"
    # length_s property correctly converts ms → s; alignment shouldn't reject these.
    assert result.tracks[0].canonical_duration_s == pytest.approx(200.0)


def test_tier_1_misaligned_duration_marks_track_unresolved(monkeypatch, fake_album):
    """When the FLAC's duration disagrees with MB by > 2s AND we can't
    duration-match unambiguously elsewhere, the track stays unresolved."""
    tracks = [
        {"filename": "01.flac", "tracknumber": "1", "title": "x", "artist": "x", "duration": 200.0},
        # Track 2 on disk is 30s longer than MB's track 2 — won't position-match,
        # AND there's no other MB track with a 225s length, so won't duration-match either.
        {"filename": "02.flac", "tracknumber": "2", "title": "x", "artist": "x", "duration": 225.0},
        {"filename": "03.flac", "tracknumber": "3", "title": "x", "artist": "x", "duration": 205.0},
    ]
    _stub_resolver_inputs(monkeypatch, fake_album, tracks)
    monkeypatch.setattr(mb_lookup, "find_release", lambda *a, **kw: _make_mb_match(mbid="rg-1", score=95))
    monkeypatch.setattr(mb_lookup, "find_release_cached", lambda *a, **kw: _make_mb_match(mbid="rg-1", score=95, with_tracklist=True))

    result = resolver.resolve_album(fake_album["id"])
    assert result.unresolved_track_count == 1
    # Track 2 is the unaligned one.
    by_filename = {t.filename: t for t in result.tracks}
    assert by_filename["02.flac"].canonical_title is None
    assert by_filename["01.flac"].canonical_title is not None
    assert by_filename["03.flac"].canonical_title is not None


# ---------- Memoization ----------


def test_memoization_avoids_repeat_strategy_calls(monkeypatch, fake_album, fake_tracks):
    """Calling resolve_album twice with the same album shouldn't re-run
    the strategies — content_hash should match and the cached result
    returns instantly."""
    _stub_resolver_inputs(monkeypatch, fake_album, fake_tracks)
    call_count = {"find_release": 0, "tracklist": 0}

    def counting_find_release(*a, **kw):
        call_count["find_release"] += 1
        return _make_mb_match(mbid="rg-1", score=95)

    def counting_tracklist(*a, **kw):
        call_count["tracklist"] += 1
        return _make_mb_match(mbid="rg-1", score=95, with_tracklist=True)

    monkeypatch.setattr(mb_lookup, "find_release", counting_find_release)
    monkeypatch.setattr(mb_lookup, "find_release_cached", counting_tracklist)

    r1 = resolver.resolve_album(fake_album["id"])
    r2 = resolver.resolve_album(fake_album["id"])

    assert r1 is r2  # Same object — served from in-process cache
    assert call_count["find_release"] == 1
    assert call_count["tracklist"] == 1


def test_force_bypasses_cache(monkeypatch, fake_album, fake_tracks):
    _stub_resolver_inputs(monkeypatch, fake_album, fake_tracks)
    call_count = {"find_release": 0}

    def counting_find_release(*a, **kw):
        call_count["find_release"] += 1
        return _make_mb_match(mbid="rg-1", score=95)

    monkeypatch.setattr(mb_lookup, "find_release", counting_find_release)
    monkeypatch.setattr(mb_lookup, "find_release_cached", lambda *a, **kw: _make_mb_match(mbid="rg-1", score=95, with_tracklist=True))

    resolver.resolve_album(fake_album["id"])
    resolver.resolve_album(fake_album["id"], force=True)

    assert call_count["find_release"] == 2


# ---------- Synthesizer ----------


def test_synthesizer_diffs_canonical_against_current(monkeypatch, fake_album, fake_tracks):
    """The synthesizer should propose ONLY the fields where canonical
    disagrees with current_tags — fields that already match get omitted."""
    from musaic import ai

    _stub_resolver_inputs(monkeypatch, fake_album, fake_tracks)
    monkeypatch.setattr(mb_lookup, "find_release", lambda *a, **kw: _make_mb_match(mbid="rg-1", score=95))
    monkeypatch.setattr(mb_lookup, "find_release_cached", lambda *a, **kw: _make_mb_match(mbid="rg-1", score=95, with_tracklist=True))

    resolution = resolver.resolve_album(fake_album["id"])
    suggestion = ai.synthesize_metadata_from_resolution(fake_album, fake_tracks, resolution)

    # `year` and `genre` already match the canonical — should not appear in the diff.
    assert "year" not in suggestion.suggestions
    assert "genre" not in suggestion.suggestions
    # `title` and `album_artist` differ — should appear.
    assert suggestion.suggestions["title"] == "Canonical Album"
    assert suggestion.suggestions["album_artist"] == "Canonical Artist"
    # Per-track titles all differ — every track should have a suggestion.
    assert len(suggestion.tracks) == 3
    assert suggestion.tracks[0].suggestions["title"] == "Canonical Title 1"
    # MBID propagated for the worker's persist step.
    assert suggestion.mb_release_group_mbid == "rg-1"


def test_synthesizer_omits_no_change_albums():
    """When current_tags already match canonical for every field, the
    synthesized suggestion should be empty (no-op apply)."""
    from musaic import ai
    from musaic.services.resolver import AlbumResolution, FieldFact, TrackResolution

    album = {
        "id": "a", "path": "/x", "title": "X", "album_artist": "Y",
        "artist": "Y", "year": "1980", "genre": "Pop",
    }
    resolution = AlbumResolution(
        album_id="a",
        anchor=FieldFact("rg-X", "musicbrainz", 0.95),
        anchor_strategy="cache",
        canonical_title=FieldFact("X", "musicbrainz", 0.95),
        canonical_album_artist=FieldFact("Y", "musicbrainz", 0.95),
        canonical_year=FieldFact("1980", "musicbrainz", 0.95),
        tracks=[
            TrackResolution(
                filename="01.flac",
                canonical_title=FieldFact("Same", "musicbrainz", 0.95),
                canonical_artist=FieldFact("Y", "musicbrainz", 0.95),
                canonical_tracknumber=FieldFact("1", "musicbrainz", 0.95),
            ),
        ],
    )
    flac_tracks = [{"filename": "01.flac", "tracknumber": "1", "title": "Same", "artist": "Y"}]

    suggestion = ai.synthesize_metadata_from_resolution(album, flac_tracks, resolution)
    assert suggestion.suggestions == {}
    assert suggestion.tracks == []
