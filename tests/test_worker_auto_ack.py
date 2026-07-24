"""Tests for worker._is_no_op_confirmation — the auto-acknowledge gate.

The gate decides whether a completed AI job is a "verified no-op"
(MB identified the album, nothing to apply) and should skip the
review queue. The gate must:
  - Return True ONLY when mbid is set AND no album/track/cover changes
  - Return False when mbid is missing (user still needs to decide)
  - Return False when ANY album/track/cover change exists
  - Be defensive against malformed result shapes
"""
from __future__ import annotations

from musaic.services.worker import _is_no_op_confirmation

MBID = "9b03c15f-f0ac-3205-8327-8b7e91a923cf"


def test_auto_ack_when_identified_and_no_changes():
    """The Mannheim Steamroller / Fresh Aire 8 case from the user's
    screenshot — MB identified, synthesizer emitted empty
    suggestions + empty tracks, no cover (metadata-only job)."""
    result = {
        "metadata": {
            "suggestions": {},
            "tracks": [],
            "notes": "Derived from MusicBrainz; no AI call needed.",
            "confidence": "high",
            "mb_release_group_mbid": MBID,
        },
    }
    assert _is_no_op_confirmation(result) is True


def test_no_auto_ack_when_not_identified():
    """No MBID → user still needs to decide whether to retry with a
    hint, fix the dir name, or move on. Surface for review."""
    result = {
        "metadata": {
            "suggestions": {},
            "tracks": [],
            "notes": "No high-confidence corrections.",
            "confidence": "low",
            "mb_release_group_mbid": None,
        },
    }
    assert _is_no_op_confirmation(result) is False


def test_no_auto_ack_when_album_level_changes_exist():
    """Has at least one album-level field to apply → user must
    review/approve."""
    result = {
        "metadata": {
            "suggestions": {"title": "Better Title"},
            "tracks": [],
            "notes": "",
            "confidence": "high",
            "mb_release_group_mbid": MBID,
        },
    }
    assert _is_no_op_confirmation(result) is False


def test_no_auto_ack_when_track_level_changes_exist():
    """Even a single track with a non-empty suggestions dict counts
    as a change requiring review."""
    result = {
        "metadata": {
            "suggestions": {},
            "tracks": [
                {"filename": "01.flac", "suggestions": {}},
                {"filename": "02.flac", "suggestions": {"title": "Cleanup"}},
            ],
            "notes": "",
            "confidence": "high",
            "mb_release_group_mbid": MBID,
        },
    }
    assert _is_no_op_confirmation(result) is False


def test_no_auto_ack_when_cover_change_proposed():
    """Cover-included jobs with a proposed cover_url need user
    decision (the cover may or may not be wanted)."""
    result = {
        "metadata": {
            "suggestions": {},
            "tracks": [],
            "mb_release_group_mbid": MBID,
        },
        "cover": {
            "cover_url": "https://coverartarchive.org/release/.../front",
            "source": "caa",
            "notes": "",
            "confidence": "high",
        },
    }
    assert _is_no_op_confirmation(result) is False


def test_auto_ack_when_cover_lookup_returned_empty():
    """Cover-included job where the cover lookup came back with no
    URL (verified absence) — combined with no metadata changes +
    identified album = nothing for the user to act on."""
    result = {
        "metadata": {
            "suggestions": {},
            "tracks": [],
            "mb_release_group_mbid": MBID,
        },
        "cover": {
            "cover_url": None,
            "source": None,
            "notes": "",
            "confidence": "low",
        },
    }
    assert _is_no_op_confirmation(result) is True


def test_defensive_against_missing_metadata_key():
    """Cover-only jobs don't populate `metadata`. The gate returns
    False (no MBID known on this path) without raising."""
    result = {
        "cover": {
            "cover_url": "https://example.com/cover.jpg",
            "source": "ai",
            "notes": "",
            "confidence": "high",
        },
    }
    assert _is_no_op_confirmation(result) is False


def test_defensive_against_empty_result():
    """Pathological/empty result — don't auto-ack, don't raise."""
    assert _is_no_op_confirmation({}) is False
    assert _is_no_op_confirmation({"metadata": {}}) is False
    assert _is_no_op_confirmation({"metadata": None}) is False  # type: ignore[arg-type]


def test_defensive_against_whitespace_only_mbid():
    """MBID that's only whitespace shouldn't qualify as identified."""
    result = {
        "metadata": {
            "suggestions": {},
            "tracks": [],
            "mb_release_group_mbid": "   ",
        },
    }
    assert _is_no_op_confirmation(result) is False
