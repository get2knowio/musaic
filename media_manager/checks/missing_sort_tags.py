"""Check S1 — albums missing Vorbis sort variants when their primary
tag starts with "The "/"A "/"An ".

Why this matters: Plex, Roon, Symfonium, and most other modern players
use the *SORT fields (ALBUMARTISTSORT / ARTISTSORT / ALBUMSORT) for
sidebar / library alphabetization. Without them, "The Beatles" sorts
under "T" instead of "B" — annoying once the library gets big.

What we check, per album:
  - album_artist starts with one of the leading articles → expect
    album_artist_sort populated as the rotated form ("X, The")
  - artist (the per-album artist tag) → same, expect artist_sort
  - title (the album name) → same, expect album_sort

We DON'T flag personal-name cases ("Bob Dylan" → "Dylan, Bob") here.
Detecting them reliably needs MusicBrainz data, which the
missing-core-tags / track-gaps AI flow already handles for free
when the user runs it. Sticking to the article-rotation rule keeps
this check fully deterministic.

Fix path: deterministic. Generates the rotated form for each missing
sort field, calls apply_direct to write it back to the FLAC tags. No
AI quota burned. The bulk action sweeps the whole library at once;
articles are unambiguous so the bulk path is safe.
"""
from __future__ import annotations

import logging
from typing import Iterable

from .. import db as db_module
from . import _apply as direct_apply
from .base import Check, CheckMeta, CheckScope, Finding, FixResult

log = logging.getLogger(__name__)


# Articles we'll rotate to the end of the string. Lowercased here so we
# match case-insensitively; the rotation preserves the original case
# (e.g. "the Beatles" → "Beatles, the", though that's an odd input).
_ARTICLES: tuple[str, ...] = ("the ", "a ", "an ")


# Fields we examine in the album row, paired with their sort-variant
# column. Order matters — when a finding lists which fields are missing,
# we report them in this order for stable summaries.
_FIELD_PAIRS: tuple[tuple[str, str, str], ...] = (
    # (primary_col, sort_col, label_for_summary)
    ("album_artist", "album_artist_sort", "album artist"),
    ("artist",       "artist_sort",       "artist"),
    ("title",        "album_sort",        "album title"),
)


class MissingSortTags(Check):
    meta = CheckMeta(
        id="missing-sort-tags",
        label="Missing sort tags",
        description=(
            "Albums whose artist or title starts with \"The\" / \"A\" / \"An\" "
            "but doesn't have the matching ALBUMARTISTSORT / ARTISTSORT / "
            "ALBUMSORT Vorbis tag set. Plex / Roon / most modern players "
            "use these for sidebar alphabetization — without them, \"The "
            "Beatles\" sorts under \"T\". Fix rotates the article to the "
            "end (\"Beatles, The\") and writes it back."
        ),
        category="metadata",
        cost="cheap",
        runs_during_scan=True,
        # Pure deterministic — article rotation is a fixed string
        # transformation, no AI judgment required.
        fix_kind="deterministic",
        bulk_action_label="Add sort tags",
    )

    def run(self, scope: CheckScope) -> Iterable[Finding]:
        # SQL filter: any of the three primary fields starts with an
        # article AND its sort variant is null/blank. Use LOWER() so the
        # match is case-insensitive. The OR cascade lets us pull one
        # row per album rather than three.
        sql = """
            SELECT
                id, dir_name,
                album_artist, album_artist_sort,
                artist,       artist_sort,
                title,        album_sort
            FROM albums
            WHERE library_path = ?
              AND (
                (LOWER(COALESCE(album_artist, '')) LIKE 'the %'
                  OR LOWER(COALESCE(album_artist, '')) LIKE 'a %'
                  OR LOWER(COALESCE(album_artist, '')) LIKE 'an %')
                AND COALESCE(NULLIF(TRIM(album_artist_sort), ''), '') = ''
                OR
                (LOWER(COALESCE(artist, '')) LIKE 'the %'
                  OR LOWER(COALESCE(artist, '')) LIKE 'a %'
                  OR LOWER(COALESCE(artist, '')) LIKE 'an %')
                AND COALESCE(NULLIF(TRIM(artist_sort), ''), '') = ''
                OR
                (LOWER(COALESCE(title, '')) LIKE 'the %'
                  OR LOWER(COALESCE(title, '')) LIKE 'a %'
                  OR LOWER(COALESCE(title, '')) LIKE 'an %')
                AND COALESCE(NULLIF(TRIM(album_sort), ''), '') = ''
              )
        """
        with db_module.connect() as conn:
            rows = conn.execute(sql, (scope.library_path,)).fetchall()

        for row in rows:
            missing = _missing_sort_fields(dict(row))
            if not missing:
                # Defensive: SQL filter said this row qualified but the
                # Python re-check disagrees. Skip rather than emit a
                # nonsensical finding.
                continue
            human_fields = ", ".join(label for _, _, label in missing)
            yield Finding(
                album_id=row["id"],
                summary=f"Missing sort tag for: {human_fields}",
                severity="info",
                detail={
                    "dir_name":    row["dir_name"],
                    "missing":     [
                        {
                            "field":           pcol,
                            "primary_value":   row[pcol],
                            "proposed_sort":   _rotate_article(row[pcol] or ""),
                        }
                        for pcol, _, _ in missing
                    ],
                },
            )

    def fix(self, finding: Finding, *, user_hint: str = "") -> FixResult:
        # user_hint is ignored — article rotation is a fixed transform,
        # no user context can influence it. Accept the kwarg to match
        # the base class signature.
        if finding.album_id is None:
            return FixResult(ok=False, message="No album to fix")
        album = _load_album(finding.album_id)
        if album is None:
            return FixResult(ok=False, message="Album not found in cache")

        proposals = _build_proposals(album)
        if not proposals:
            return FixResult(
                ok=False,
                message="Nothing to do — sort tags already present or no article prefix",
            )

        ok, message = direct_apply.apply_direct(
            album_id=finding.album_id,
            suggestions=proposals,
        )
        if not ok:
            return FixResult(ok=False, message=f"Apply failed: {message}")

        applied = ", ".join(f"{k}={v!r}" for k, v in proposals.items())
        log.info("missing-sort-tags applied %s for album=%s", applied, finding.album_id)
        return FixResult(
            ok=True,
            message=f"Wrote {len(proposals)} sort tag{'s' if len(proposals) != 1 else ''}",
        )

    def try_quick_fix(self, finding: Finding) -> FixResult | None:
        # Bulk-mode is identical to per-finding fix — both do the same
        # deterministic article rotation. No AI fallback to speak of.
        return self.fix(finding)


# ---- Helpers --------------------------------------------------------

def _load_album(album_id: str) -> dict | None:
    with db_module.connect() as conn:
        row = conn.execute(
            "SELECT id, path, dir_name, title, artist, album_artist, "
            "       album_artist_sort, artist_sort, album_sort "
            "FROM albums WHERE id = ?",
            (album_id,),
        ).fetchone()
    return dict(row) if row else None


def _starts_with_article(value: str | None) -> bool:
    if not value:
        return False
    lower = value.lstrip().lower()
    return any(lower.startswith(a) for a in _ARTICLES)


def _is_blank(value: str | None) -> bool:
    return not (value or "").strip()


def _missing_sort_fields(album: dict) -> list[tuple[str, str, str]]:
    """Return the (primary_col, sort_col, label) triples whose primary
    field has an article prefix AND whose sort field is empty."""
    out: list[tuple[str, str, str]] = []
    for pcol, scol, label in _FIELD_PAIRS:
        if _starts_with_article(album.get(pcol)) and _is_blank(album.get(scol)):
            out.append((pcol, scol, label))
    return out


def _rotate_article(value: str) -> str:
    """\"The Beatles\" → \"Beatles, The\". Preserves the original article's
    case (so \"the X\" → \"X, the\"). Returns the input unchanged when it
    doesn't start with a recognized article — callers should gate on
    `_starts_with_article` first."""
    if not value:
        return value
    stripped = value.lstrip()
    leading_ws = value[: len(value) - len(stripped)]
    lower = stripped.lower()
    for art in _ARTICLES:
        if lower.startswith(art):
            article_len = len(art) - 1  # exclude the trailing space
            article_original = stripped[:article_len]
            rest = stripped[article_len + 1:]  # +1 to skip the space
            return f"{leading_ws}{rest}, {article_original}"
    return value


def _build_proposals(album: dict) -> dict[str, str]:
    """For each missing sort field whose primary has an article prefix,
    propose the rotated form. Returns a dict keyed by suggestion-field
    name (matching the AI / apply pipeline's key conventions)."""
    proposals: dict[str, str] = {}
    for pcol, scol, _ in _FIELD_PAIRS:
        primary = album.get(pcol)
        if _starts_with_article(primary) and _is_blank(album.get(scol)):
            proposals[scol] = _rotate_article(primary)
    return proposals
