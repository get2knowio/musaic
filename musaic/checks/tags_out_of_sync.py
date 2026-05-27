"""Merged metadata-correctness check.

Replaces the previous trio of `missing-core-tags`, `missing-sort-tags`,
and `mb-canonical-mismatch`. After the resolver simplification all
three were really one operation: "diff this album's tags against the
authoritative truth and apply what's wrong." The album sheet's
`MbPreviewSection` already shows that diff as a single unified UX;
this check is the per-album-detection mirror of it.

Detection rules — a finding fires when an album has at least one of:

  - **blank required field** : `title` / `(album_)artist` / `year` /
    `genre` is null or whitespace-only. Always actionable
    (deterministic when the resolver can fill, AI fallback otherwise).
  - **canonical drift** : an anchored album whose tags don't match
    MusicBrainz's canonical values (album-level title / artist / year,
    plus per-track artist for resolver-aligned tracks). Equivalent
    to the old `mb-canonical-mismatch` check.
  - **missing sort tag** : a primary field starts with "The "/"A "/
    "An " but the corresponding `*_sort` Vorbis tag is empty.
    Deterministic via article rotation (or MB sort-name when anchored).

The finding's `detail` payload carries the structured diff list so the
album-row Fix button (and any future Resolve dialog) can render the
exact changes inline.

Fix path — single click applies all deterministic diffs and falls
through to AI for whatever the resolver couldn't fill:

  1. **Resolver-driven apply** — for anchored albums, write canonical
     values for any field where current ≠ canonical, plus rotated
     sort tags for the article cases. Counts as fully resolved when
     no AI-fillable gaps remain.
  2. **AI fallback** — only when (a) the user supplied a hint
     (always honored), (b) the album is unanchored AND has blank core
     fields the resolver can't fill, or (c) genre is missing on an
     anchored album (MB tags are too noisy to auto-apply).

Bulk action ("Apply MusicBrainz canonical") runs step 1 across every
open finding. Never AI from a bulk pass.
"""
from __future__ import annotations

import logging
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable

from mutagen.flac import FLAC

from .. import db as db_module
from .. import jobs as jobs_module
from ..services import resolver as resolver_service
from . import _apply as direct_apply
from .base import Check, CheckMeta, CheckScope, Finding, FixResult

log = logging.getLogger(__name__)


# Articles whose owning primary tag should have a *_sort variant.
# Lowercased; the rotation preserves the original article's case
# (so "the Beatles" → "Beatles, the", though that's an odd input).
_ARTICLES: tuple[str, ...] = ("the ", "a ", "an ")

# (primary_col, sort_col, label) for the three sort fields we manage.
_SORT_FIELD_PAIRS: tuple[tuple[str, str, str], ...] = (
    ("album_artist", "album_artist_sort", "album artist"),
    ("artist",       "artist_sort",       "artist"),
    ("title",        "album_sort",        "album title"),
)

# Album fields the resolver can authoritatively fill. Genre is excluded:
# MB tags are crowd-sourced and too inconsistent to auto-apply.
_RESOLVER_FILLABLE: frozenset[str] = frozenset({"title", "album_artist", "year"})


class TagsOutOfSync(Check):
    meta = CheckMeta(
        id="tags-out-of-sync",
        label="Tags don't match canonical",
        description=(
            "Albums where Vorbis tags drift from MusicBrainz's canonical "
            "values, are missing a required field (title / artist / year "
            "/ genre), or are missing the *_SORT variants used by Plex "
            "and Roon for sidebar alphabetization. Fix applies all "
            "MusicBrainz-derivable corrections in one click and falls "
            "back to AI only for fields the resolver can't fill (genre, "
            "or anything on an unanchored album)."
        ),
        category="metadata",
        # `medium` because run() walks every anchored album and asks
        # the resolver for a canonical comparison — that's a MusicBrainz
        # fetch per album on a cold cache (1 req/sec rate limit), which
        # adds up to minutes for a moderately-sized library. Not safe to
        # fire in the post-scan tail; users run it explicitly from the
        # Health page when they want drift detection.
        cost="medium",
        runs_during_scan=False,
        # ai-job: per-finding Fix may end with an AI metadata job when
        # the resolver can't fill everything. The bulk action stays
        # deterministic via try_quick_fix.
        fix_kind="ai-job",
        bulk_action_label="Apply MusicBrainz canonical",
    )

    def run(self, scope: CheckScope) -> Iterable[Finding]:
        """Yield one finding per album with at least one fixable diff.

        We deliberately do a slightly broader pre-filter than the three
        original checks combined and then re-validate in Python — the
        SQL alone can't correctly express "differs from canonical" since
        canonical comes from the resolver.
        """
        sql = """
            SELECT
                id, dir_name, path,
                title, artist, album_artist, year, genre,
                album_artist_sort, artist_sort, album_sort,
                is_compilation, mb_release_group_mbid, mb_release_mbid
            FROM albums
            WHERE library_path = ?
        """
        with db_module.connect() as conn:
            rows = conn.execute(sql, (scope.library_path,)).fetchall()

        for row in rows:
            try:
                finding = self._check_album(dict(row))
            except Exception:
                log.exception("tags-out-of-sync: error checking album %s", row["id"])
                continue
            if finding is not None:
                yield finding

    def _check_album(self, album: dict[str, Any]) -> Finding | None:
        diffs = compute_diffs(album)
        if not diffs.has_any:
            return None
        return Finding(
            album_id=album["id"],
            severity="warn" if diffs.missing_core else "info",
            summary=diffs.summary,
            detail={
                "dir_name":            album["dir_name"],
                "is_compilation":      bool(album.get("is_compilation")),
                "headline":            album["dir_name"] or album["id"],
                "has_anchor":          album.get("mb_release_group_mbid") is not None,
                "missing_core":        diffs.missing_core,
                "album_diffs":         [d.to_dict() for d in diffs.album_diffs],
                "track_diffs":         [d.to_dict() for d in diffs.track_diffs],
                "sort_proposals":      diffs.sort_proposals,
                "ai_fallback_fields":  diffs.ai_fallback_fields,
            },
        )

    def fix(self, finding: Finding, *, user_hint: str = "") -> FixResult:
        """Per-finding fix. Applies everything the resolver can; if any
        fields are AI-fillable only (or the user supplied a hint),
        enqueues an AI metadata job for the rest."""
        if finding.album_id is None:
            return FixResult(ok=False, message="No album to fix")
        album = _load_album(finding.album_id)
        if album is None:
            return FixResult(ok=False, message="Album not found in cache")

        # User hint → straight to AI. Skipping the deterministic shortcut
        # honors the user's added context (which the resolver ignores).
        if user_hint:
            return _enqueue_ai(album, finding.album_id, user_hint)

        diffs = compute_diffs(album)
        if not diffs.has_any:
            return FixResult(ok=True, message="Nothing to fix — tags already in sync")

        applied_message = ""
        if diffs.applicable_album or diffs.applicable_tracks or diffs.sort_proposals:
            ok, msg, applied_count = _apply_resolver_diffs(
                finding.album_id, diffs,
            )
            if not ok:
                return FixResult(ok=False, message=msg)
            applied_message = msg
            log.info(
                "tags-out-of-sync applied %d diffs for album=%s",
                applied_count, finding.album_id,
            )

        # Anything left? AI fallback if so.
        if diffs.ai_fallback_fields or (
            diffs.missing_core and not diffs.applicable_album
        ):
            ai_result = _enqueue_ai(album, finding.album_id, user_hint="")
            if ai_result.job_id:
                # Compose the message: deterministic part + AI part.
                msg = ai_result.message
                if applied_message:
                    msg = f"{applied_message}; also asking Claude to fill the rest"
                return FixResult(ok=True, message=msg, job_id=ai_result.job_id)
            return ai_result

        return FixResult(ok=True, message=applied_message or "All canonical diffs applied")

    def try_quick_fix(self, finding: Finding) -> FixResult | None:
        """Bulk-mode entry: deterministic-only. Applies resolver +
        rotation diffs; reports ok=True iff anything landed.

        Returns None when the album row vanished from the cache so the
        bulk dispatcher counts it as 'not-applicable' rather than 'skipped'.
        """
        if finding.album_id is None:
            return None
        album = _load_album(finding.album_id)
        if album is None:
            return None

        diffs = compute_diffs(album)
        if not (diffs.applicable_album or diffs.applicable_tracks or diffs.sort_proposals):
            return FixResult(
                ok=False,
                message="No deterministic fixes available (anchor or canonical missing)",
            )

        ok, msg, _count = _apply_resolver_diffs(finding.album_id, diffs)
        if not ok:
            return FixResult(ok=False, message=msg)
        # Partial fixes still count as 'fixed' for the bulk summary —
        # we made real progress, the post-scan rerun will surface a
        # smaller follow-up finding the user can finish off.
        return FixResult(ok=True, message=msg)


# ---------- Diff computation ---------------------------------------------


@dataclass
class AlbumFieldDiff:
    field: str          # "title" | "album_artist" | "year"
    current: str
    canonical: str
    source: str         # "musicbrainz"

    def to_dict(self) -> dict[str, Any]:
        return {
            "field": self.field,
            "current": self.current,
            "canonical": self.canonical,
            "source": self.source,
        }


@dataclass
class TrackFieldDiff:
    filename: str
    field: str          # "artist" (only field we surface today)
    current: str
    canonical: str

    def to_dict(self) -> dict[str, Any]:
        return {
            "filename": self.filename,
            "field": self.field,
            "current": self.current,
            "canonical": self.canonical,
        }


@dataclass
class AlbumDiffs:
    """Aggregate diff result for a single album."""
    missing_core: list[str]                       # blank required fields
    album_diffs: list[AlbumFieldDiff]             # current ≠ canonical
    track_diffs: list[TrackFieldDiff]             # per-track current ≠ canonical
    sort_proposals: dict[str, str]                # *_sort col → proposed value
    # Subset of `missing_core` the resolver couldn't fill — escalate to AI.
    ai_fallback_fields: list[str]
    # Convenience subsets used by the apply step.
    applicable_album: dict[str, str]              # field → canonical value
    applicable_tracks: dict[str, dict[str, str]]  # filename → {field: value}
    summary: str

    @property
    def has_any(self) -> bool:
        return bool(
            self.album_diffs
            or self.track_diffs
            or self.sort_proposals
            or self.missing_core
        )


def compute_diffs(album: dict[str, Any]) -> AlbumDiffs:
    """Aggregate every kind of tag fix this check cares about into one
    structured result. Pure function — `album` is the cache row dict.

    Always called: blank-field detection + sort-tag rotation proposals
    (no MB needed). Resolver only consulted when the album is anchored.
    """
    # Blank-required detection (mirrors the old missing-core-tags SQL).
    missing_core = _missing_core_fields(album)

    # Resolver result for canonical comparisons + sort fallbacks.
    resolution: resolver_service.AlbumResolution | None = None
    if album.get("mb_release_group_mbid"):
        try:
            resolution = resolver_service.resolve_album(album["id"])
        except Exception:
            log.exception("resolver call failed for tags-out-of-sync %s", album["id"])
            resolution = None

    album_diffs = _album_diffs(album, resolution)
    track_diffs = _track_diffs(album, resolution)

    # Sort-tag proposals (mirrors old missing-sort-tags). Prefers MB
    # canonical sort-name for album_artist_sort when anchored;
    # rotation-only for the other two.
    mb_album_artist_sort = (
        resolution.canonical_album_artist_sort.value.strip()
        if resolution and resolution.canonical_album_artist_sort
        else None
    )
    sort_proposals = _sort_proposals(album, mb_album_artist_sort)

    # What the resolver-driven apply will write.
    applicable_album: dict[str, str] = {}
    for d in album_diffs:
        applicable_album[d.field] = d.canonical
    # Fill blank required fields whose canonical value is known.
    if resolution:
        if "title" in missing_core and resolution.canonical_title:
            applicable_album.setdefault("title", resolution.canonical_title.value.strip())
        if "album_artist" in missing_core and resolution.canonical_album_artist:
            applicable_album.setdefault(
                "album_artist", resolution.canonical_album_artist.value.strip(),
            )
        if "year" in missing_core and resolution.canonical_year:
            applicable_album.setdefault("year", resolution.canonical_year.value.strip())

    applicable_tracks: dict[str, dict[str, str]] = {}
    for td in track_diffs:
        applicable_tracks.setdefault(td.filename, {})[td.field] = td.canonical

    # Fields blank that we still couldn't fill (genre, anything on
    # unanchored albums) → escalate to AI.
    ai_fallback_fields = [
        f for f in missing_core
        if f not in applicable_album and f != "album_artist"  # filled via either col
    ]
    # Special case: album_artist + artist are interchangeable for
    # blank-detection purposes. If we've supplied album_artist
    # canonically, drop "artist" too.
    if applicable_album.get("album_artist") and "artist" in ai_fallback_fields:
        ai_fallback_fields.remove("artist")
    # Genre never lands deterministically — always flagged as AI.
    if "genre" in missing_core and "genre" not in ai_fallback_fields:
        ai_fallback_fields.append("genre")

    summary = _build_summary(
        missing_core=missing_core,
        n_album=len(album_diffs),
        n_track=len(track_diffs),
        sort_proposals=sort_proposals,
        has_anchor=resolution is not None and resolution.anchor is not None,
    )

    return AlbumDiffs(
        missing_core=missing_core,
        album_diffs=album_diffs,
        track_diffs=track_diffs,
        sort_proposals=sort_proposals,
        ai_fallback_fields=ai_fallback_fields,
        applicable_album=applicable_album,
        applicable_tracks=applicable_tracks,
        summary=summary,
    )


def _missing_core_fields(album: dict[str, Any]) -> list[str]:
    """Mirrors the old missing-core-tags SQL filter, applied in Python."""
    out: list[str] = []
    if not (album.get("title") or "").strip():
        out.append("title")
    if not (album.get("album_artist") or album.get("artist") or "").strip():
        out.append("album_artist")
    if not (album.get("year") or "").strip():
        out.append("year")
    if not (album.get("genre") or "").strip():
        out.append("genre")
    return out


def _album_diffs(
    album: dict[str, Any],
    resolution: resolver_service.AlbumResolution | None,
) -> list[AlbumFieldDiff]:
    if resolution is None or resolution.anchor is None:
        return []
    out: list[AlbumFieldDiff] = []

    def _add(field: str, current_val: str | None, fact) -> None:
        if not fact:
            return
        canonical = (fact.value or "").strip()
        cur = (current_val or "").strip()
        if not canonical or not cur:  # blank-fill is handled via applicable_album
            return
        if _normalize(canonical) == _normalize(cur):
            return
        out.append(AlbumFieldDiff(field, cur, canonical, fact.source))

    _add("title",        album.get("title"),        resolution.canonical_title)
    _add("album_artist", album.get("album_artist"), resolution.canonical_album_artist)
    _add("year",         album.get("year"),         resolution.canonical_year)
    return out


def _track_diffs(
    album: dict[str, Any],
    resolution: resolver_service.AlbumResolution | None,
) -> list[TrackFieldDiff]:
    """Per-track artist drift, only for tracks the resolver matched."""
    if resolution is None or resolution.anchor is None:
        return []
    candidates = [t for t in resolution.tracks if t.canonical_artist is not None]
    if not candidates:
        return []
    album_path = Path(album.get("path") or "")
    if not album_path.is_dir():
        return []
    out: list[TrackFieldDiff] = []
    for tr in candidates:
        canonical = tr.canonical_artist.value.strip()
        if not canonical:
            continue
        flac_path = album_path / tr.filename
        if not flac_path.is_file():
            continue
        try:
            audio = FLAC(str(flac_path))
            tags = audio.tags or {}
            v = tags.get("artist") or tags.get("ARTIST") or []
            current = (
                (v[0] if isinstance(v, list) else str(v)).strip()
                if v else ""
            )
        except Exception:
            continue
        if not current or _normalize(current) == _normalize(canonical):
            continue
        out.append(TrackFieldDiff(tr.filename, "artist", current, canonical))
    return out


def _sort_proposals(
    album: dict[str, Any],
    mb_album_artist_sort: str | None,
) -> dict[str, str]:
    """Article-rotation sort tags. Prefers MB sort-name for
    album_artist_sort when the resolver supplied one."""
    out: dict[str, str] = {}
    for pcol, scol, _ in _SORT_FIELD_PAIRS:
        primary = album.get(pcol) or ""
        if not _starts_with_article(primary):
            continue
        if (album.get(scol) or "").strip():
            continue
        if scol == "album_artist_sort" and mb_album_artist_sort:
            out[scol] = mb_album_artist_sort
        else:
            out[scol] = _rotate_article(primary)
    return out


def _starts_with_article(value: str | None) -> bool:
    if not value:
        return False
    lower = value.lstrip().lower()
    return any(lower.startswith(a) for a in _ARTICLES)


def _rotate_article(value: str) -> str:
    """\"The Beatles\" → \"Beatles, The\" (preserves original case)."""
    if not value:
        return value
    stripped = value.lstrip()
    leading_ws = value[: len(value) - len(stripped)]
    lower = stripped.lower()
    for art in _ARTICLES:
        if lower.startswith(art):
            article_len = len(art) - 1
            article_original = stripped[:article_len]
            rest = stripped[article_len + 1:]
            return f"{leading_ws}{rest}, {article_original}"
    return value


def _normalize(s: str) -> str:
    """Whitespace + case-insensitive equivalence for diff comparison.
    Same shape the album-sheet MbPreviewSection uses."""
    return " ".join(s.lower().split())


def _build_summary(
    *,
    missing_core: list[str],
    n_album: int,
    n_track: int,
    sort_proposals: dict[str, str],
    has_anchor: bool,
) -> str:
    """One-line summary for the finding's row in the Health detail page."""
    parts: list[str] = []
    if missing_core:
        parts.append(f"missing: {', '.join(missing_core)}")
    if n_album:
        parts.append(f"{n_album} canonical drift")
    if n_track:
        parts.append(f"{n_track} track artist drift")
    if sort_proposals:
        parts.append(f"{len(sort_proposals)} sort tag")
    if not parts:
        return "Tags need updating"
    detail = " · ".join(parts)
    return detail if has_anchor else f"{detail} (no MB anchor — AI required)"


# ---------- Apply --------------------------------------------------------


def _apply_resolver_diffs(
    album_id: str,
    diffs: AlbumDiffs,
) -> tuple[bool, str, int]:
    """Drive `apply_direct` with the deterministic-fillable diff set.

    Returns (ok, message, count_applied). Count is the total number of
    field-level changes (album fields + per-track fields + sort tags).
    """
    suggestions: dict[str, str] = dict(diffs.applicable_album)
    suggestions.update(diffs.sort_proposals)
    track_suggestions: dict[str, dict[str, str]] = {
        fn: dict(fields) for fn, fields in diffs.applicable_tracks.items()
    }
    if not suggestions and not track_suggestions:
        return True, "No deterministic fixes to apply", 0

    ok, msg = direct_apply.apply_direct(
        album_id=album_id,
        suggestions=suggestions or None,
        track_suggestions=track_suggestions or None,
    )
    if not ok:
        return False, f"Apply failed: {msg}", 0
    count = len(suggestions) + sum(len(v) for v in track_suggestions.values())
    summary = _format_apply_summary(suggestions, track_suggestions)
    return True, summary, count


def _format_apply_summary(
    suggestions: dict[str, str],
    track_suggestions: dict[str, dict[str, str]],
) -> str:
    bits: list[str] = []
    if suggestions:
        bits.append(f"{len(suggestions)} album field(s)")
    if track_suggestions:
        n = sum(len(v) for v in track_suggestions.values())
        bits.append(f"{n} track field(s) on {len(track_suggestions)} track(s)")
    if not bits:
        return "Nothing applied"
    return "Wrote " + " + ".join(bits)


def _enqueue_ai(album: dict[str, Any], album_id: str, user_hint: str) -> FixResult:
    try:
        job = jobs_module.enqueue(
            album_id=album_id,
            album_path=album["path"] or "",
            album_title=album["title"] or album["dir_name"] or "",
            job_type="metadata",
            hint=user_hint or "",
        )
    except jobs_module.JobConflictError as e:
        return FixResult(
            ok=False,
            message=f"Already has an active job ({e.status})",
            job_id=e.existing_job_id,
        )
    return FixResult(
        ok=True,
        message="Asking Claude to fill in the missing tags",
        job_id=job.id,
    )


def _load_album(album_id: str) -> dict | None:
    with db_module.connect() as conn:
        row = conn.execute(
            "SELECT id, path, dir_name, title, artist, album_artist, year, genre, "
            "       album_artist_sort, artist_sort, album_sort, "
            "       dir_artist, dir_album, is_compilation, "
            "       mb_release_group_mbid, mb_release_mbid "
            "FROM albums WHERE id = ?",
            (album_id,),
        ).fetchone()
    return dict(row) if row else None
