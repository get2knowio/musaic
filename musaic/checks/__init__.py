"""Maintenance / health-check infrastructure.

Each check is a small Python class that detects a class of issue (missing
tags, no cover art, file integrity, etc.) and yields `Finding` objects.
The runner persists those findings in the `check_findings` table, dedupes
across runs, and auto-resolves findings whose underlying issue has gone
away.
"""
from __future__ import annotations

from .base import (
    Check,
    CheckMeta,
    CheckScope,
    Finding,
    FixResult,
    Severity,
)
from .duplicate_tracks import DuplicateTracks
from .filename_mismatch import FilenameMismatch
from .inconsistent_artist_sort import InconsistentArtistSort
from .no_cover_art import NoCoverArt
from .tags_out_of_sync import TagsOutOfSync
from .track_gaps import TrackGaps
from .unidentified_album import UnidentifiedAlbum

# Concrete check classes in dispatch / display order. Order doubles as
# the order they appear in the Health index page. Foundations first
# (identifying albums) so downstream checks have data to work with.
REGISTRY: list[type[Check]] = [
    UnidentifiedAlbum,        # external — fingerprint + MB anchor in one
    TagsOutOfSync,            # metadata — canonical drift, missing core, sort tags
    TrackGaps,                # metadata — track-number sequencing
    InconsistentArtistSort,   # metadata — cluster-level sort normalization
    NoCoverArt,               # cover
    DuplicateTracks,          # duplicates (needs fingerprints)
    FilenameMismatch,         # files — filenames vs user template
]


def get(check_id: str) -> type[Check] | None:
    """Look up a registered check class by its meta.id."""
    for cls in REGISTRY:
        if cls.meta.id == check_id:
            return cls
    return None


def all_meta() -> list[CheckMeta]:
    """List metadata for every registered check (used by the API listing)."""
    return [cls.meta for cls in REGISTRY]


__all__ = [
    "Check",
    "CheckMeta",
    "CheckScope",
    "Finding",
    "FixResult",
    "REGISTRY",
    "Severity",
    "all_meta",
    "get",
]
