"""Maintenance / health-check infrastructure.

Each check is a small Python class that detects a class of issue (missing
tags, no cover art, file integrity, etc.) and yields `Finding` objects.
The runner persists those findings in the `check_findings` table, dedupes
across runs, and auto-resolves findings whose underlying issue has gone
away.

Phase 1 lands the framework with `REGISTRY = []` — the UI shows "no checks
installed yet" and the SSE stream is silent. Phase 2+ append concrete
check classes here.
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
from .mb_canonical_mismatch import MbCanonicalMismatch
from .missing_core_tags import MissingCoreTags
from .missing_fingerprints import MissingFingerprints
from .missing_mbid import MissingMbid
from .missing_sort_tags import MissingSortTags
from .no_cover_art import NoCoverArt
from .track_gaps import TrackGaps

# Concrete check classes in dispatch / display order. Order here doubles
# as the order they appear in the Health index page within each category
# bucket. Foundations first (identifying albums, fingerprinting them) so
# downstream checks have data to work with.
REGISTRY: list[type[Check]] = [
    MissingMbid,             # external — anchor albums to MusicBrainz
    MissingFingerprints,     # audio — populate AcoustID data
    MissingCoreTags,         # metadata
    TrackGaps,               # metadata
    MissingSortTags,         # metadata — article-rotation sort tags
    InconsistentArtistSort,  # metadata — cluster-level sort normalization
    NoCoverArt,              # cover
    DuplicateTracks,         # duplicates (needs fingerprints)
    MbCanonicalMismatch,     # metadata — MB-canonical artist credits
    FilenameMismatch,        # files — filenames vs user template
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
