"""Shared accessors for album-level display fields.

Every consumer that wants to show "the album's artist" or "the album's
sort key" has historically reinvented its own fallback cascade. The
fallbacks looked similar but differed in subtle ways — the Mancini bug
landed because the dir-template renderer cascaded
`album_artist → artist → dir_artist` while the file-template renderer
only did `albumartist_tag or row.album_artist`. The dir-template path
silently produced empty artist tokens for libraries that carry only the
ARTIST tag.

Centralizing here so the cascade is defined once. Consumers that need
*different* cascades (e.g. `mb_lookup.find_release` deliberately leads
with `dir_artist` because the on-disk name is the search anchor) keep
their own — but they should reach for these helpers first and only
diverge with a comment explaining why.
"""
from __future__ import annotations

from typing import Any


def display_artist(album: dict[str, Any], *, include_dir_artist: bool = False) -> str:
    """Return the best human-readable artist string for `album`.

    Cascade: `album_artist` → `artist` → (optional) `dir_artist` → "".

    `include_dir_artist=True` is the right call when the consumer is
    *rendering a path or tag* and would otherwise produce something
    destructive (an empty token in a filename template, an empty
    {albumartist} placeholder). The dir-name is the literal surface
    form the user already accepted, so falling back to it keeps the
    output safe even when the library has no tag-level artist data.

    Default (`include_dir_artist=False`) is for purely informational
    display (UI labels, finding summaries, debug logs) where falling
    back to a derived field would be misleading.
    """
    aa = (album.get("album_artist") or "").strip()
    if aa:
        return aa
    a = (album.get("artist") or "").strip()
    if a:
        return a
    if include_dir_artist:
        da = (album.get("dir_artist") or "").strip()
        if da:
            return da
    return ""


def sort_key(album: dict[str, Any]) -> str:
    """Lowercase string suitable for alphabetical ordering of albums by
    artist. Cascade: `album_artist_sort` → `artist_sort` →
    `album_artist` → `artist` → "" — then lowercased.

    Vorbis SORT tags rotate articles ("Beatles, The") so they cluster
    properly; when absent we use the display name as a best-effort
    fallback. Matches the scanner's pre-existing precedence (kept
    aligned with `scanner.scan_albums_list`) so the API order matches
    what the UI shows.
    """
    for field in ("album_artist_sort", "artist_sort", "album_artist", "artist"):
        v = (album.get(field) or "").strip()
        if v:
            return v.lower()
    return ""
