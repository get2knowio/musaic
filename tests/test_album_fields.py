"""Tests for the centralized album-field accessors."""
from __future__ import annotations

from musaic import album_fields


# ---------- display_artist ----------


def test_display_artist_prefers_album_artist():
    assert album_fields.display_artist({
        "album_artist": "Henry Mancini",
        "artist": "Various",
        "dir_artist": "Other",
    }) == "Henry Mancini"


def test_display_artist_falls_back_to_artist():
    assert album_fields.display_artist({
        "album_artist": "",
        "artist": "Henry Mancini",
        "dir_artist": "Other",
    }) == "Henry Mancini"


def test_display_artist_default_excludes_dir_artist():
    """Plain display callers should NOT fall back to dir_artist —
    that's a path-rendering concern, not an informational one."""
    assert album_fields.display_artist({
        "album_artist": None,
        "artist": None,
        "dir_artist": "Mancini",
    }) == ""


def test_display_artist_include_dir_artist_for_path_rendering():
    """Mancini regression: the dir-template renderer must fall back to
    dir_artist or it silently drops the artist token from the proposed
    directory name (the original bug that motivated this module)."""
    assert album_fields.display_artist({
        "album_artist": None,
        "artist": None,
        "dir_artist": "Henry Mancini",
    }, include_dir_artist=True) == "Henry Mancini"


def test_display_artist_strips_whitespace():
    assert album_fields.display_artist({"album_artist": "  Henry Mancini  "}) == "Henry Mancini"


def test_display_artist_treats_whitespace_only_as_empty():
    """A whitespace-only tag shouldn't shadow a real fallback value."""
    assert album_fields.display_artist({
        "album_artist": "   ",
        "artist": "Henry Mancini",
    }) == "Henry Mancini"


def test_display_artist_empty_when_no_data():
    assert album_fields.display_artist({}) == ""


# ---------- sort_key ----------


def test_sort_key_prefers_album_artist_sort():
    assert album_fields.sort_key({
        "album_artist_sort": "Mancini, Henry",
        "artist_sort":       "Henry, Mancini",
        "album_artist":      "Henry Mancini",
    }) == "mancini, henry"


def test_sort_key_cascade_through_to_artist():
    """When every sort-tag variant is missing, fall back to the display
    name so albums still group reasonably even without sort tags."""
    assert album_fields.sort_key({
        "album_artist_sort": None,
        "artist_sort":       None,
        "album_artist":      None,
        "artist":            "Henry Mancini",
    }) == "henry mancini"


def test_sort_key_empty_when_no_data():
    assert album_fields.sort_key({}) == ""
