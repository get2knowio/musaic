"""Check M2 — albums whose artist credits differ from MusicBrainz's
canonical form for the same release.

Why this matters: ad-hoc-tagged libraries collect drift over time
("The Beatles" / "Beatles", "Crosby Stills & Nash" / "Crosby, Stills
& Nash", "Bob Dylan" / "Dylan, Bob") that doesn't match MB's
authoritative form. Plex / Roon use whatever's in the tags, so the
display drift propagates everywhere downstream — sidebars, lyrics
lookup, multi-source playlist building. Normalizing tags to MB's
canonical form before any other rename / playlist work avoids
double-fixing later.

Scope (intentionally narrow per user direction):
  - album_artist (string compare; whitespace+case-insensitive
    equivalence is "good enough", more aggressive normalization
    would mask real drift)
  - per-track ARTIST credits — only when the resolver matched the
    track to a canonical recording (via AcoustID fingerprint vote
    or position+duration alignment). Skips the position-based-match
    false-positive on out-of-order rips because the resolver tracks
    fingerprint provenance.

Out of scope: album title, per-track titles. The user can extend
later if desired.

Fix path is custom: the user reviews the per-field diffs in a
custom Resolve dialog and opts in field-by-field. No bulk
deterministic action — the entire point is to put the human in the
loop because some "diffs" are deliberate (artist using "&" vs MB's
"and", user preferring a regional credit, etc.).

Implementation note: this check used to do its own MusicBrainz
lookups (`mb_lookup.find_release_cached`) per album. As of the
resolver refactor it consumes `services.resolver.AlbumResolution`
instead — same MB cost per album but the resolution memoizes across
checks, so a "Run all checks" pass (which today touches several
checks per album) goes from N×M MB requests to M.
"""
from __future__ import annotations

import logging
from pathlib import Path
from typing import Any, Iterable

from mutagen.flac import FLAC

from .. import db as db_module
from ..services import resolver as resolver_service
from .base import Check, CheckMeta, CheckScope, Finding, FixResult

log = logging.getLogger(__name__)


class MbCanonicalMismatch(Check):
    meta = CheckMeta(
        id="mb-canonical-mismatch",
        label="Tags differ from MusicBrainz",
        description=(
            "Albums whose artist credits don't match MusicBrainz's "
            "canonical form. The Resolve dialog shows a per-field diff "
            "(\"The Beatles\" → \"Beatles, The\" etc.); pick which to "
            "accept. Per-track artist credits on compilations are also "
            "compared, but only for tracks the resolver matched via "
            "AcoustID fingerprint or duration alignment — those are the "
            "files we know map to the right MB recording regardless of "
            "disk order."
        ),
        category="metadata",
        cost="medium",
        # MB API calls are rate-limited (1 req/sec). The resolver
        # memoizes per album so a Run All scan is one MB request per
        # album regardless of how many checks consume the result.
        runs_during_scan=False,
        fix_kind="deterministic",
        bulk_action_label=None,
    )

    def run(self, scope: CheckScope) -> Iterable[Finding]:
        # Only consider albums that already have an MBID anchor — drift
        # detection requires knowing which release we're comparing
        # against. Surfacing unanchored albums is `missing-mbid`'s job.
        with db_module.connect() as conn:
            rows = conn.execute(
                """
                SELECT id, dir_name, album_artist, title, year,
                       is_compilation, mb_release_group_mbid,
                       mb_release_mbid, mb_match_score, path
                FROM albums
                WHERE library_path = ?
                  AND mb_release_group_mbid IS NOT NULL
                """,
                (scope.library_path,),
            ).fetchall()

        for row in rows:
            try:
                finding = self._check_album(dict(row))
            except Exception:
                log.exception(
                    "mb-canonical-mismatch: error checking album %s",
                    row["id"],
                )
                continue
            if finding is not None:
                yield finding

    def _check_album(self, row: dict[str, Any]) -> Finding | None:
        album_id = row["id"]
        album_artist = (row["album_artist"] or "").strip()
        if not album_artist:
            # The missing-core-tags check handles the "no album_artist"
            # case; we only flag drift between two non-empty values.
            return None

        # The resolver does the MB fetch + per-track alignment for us.
        # When this check runs after the album was already identified,
        # Tier 0a + Tier 1 fire — one find_release_cached round trip
        # (release-detail by MBID, no search step), shared with any
        # other check / UI that asks about this album.
        resolution = resolver_service.resolve_album(album_id)
        if resolution.anchor is None:
            # Anchor went away (album rescan invalidated, MBID no
            # longer valid in MB, etc.). Don't flag drift for an album
            # we can't authoritatively compare against.
            return None
        canonical_album_artist = (
            resolution.canonical_album_artist.value.strip()
            if resolution.canonical_album_artist
            else ""
        )
        if not canonical_album_artist:
            return None

        # Album-level diff — case- and whitespace-insensitive
        # equivalence is "good enough". The dialog will preview the
        # exact replacement so the user sees what's actually changing.
        album_diff: dict[str, str] | None = None
        if _normalize_for_compare(album_artist) != _normalize_for_compare(canonical_album_artist):
            album_diff = {
                "current":   album_artist,
                "canonical": canonical_album_artist,
            }

        # Per-track diffs (only for tracks the resolver mapped to a
        # canonical recording).
        track_diffs = self._track_diffs(row, resolution)

        if album_diff is None and not track_diffs:
            return None

        n_album = 1 if album_diff else 0
        n_track = len(track_diffs)
        return Finding(
            album_id=album_id,
            severity="info",
            summary=_summarize(row["dir_name"], n_album, n_track, canonical_album_artist),
            detail={
                "dir_name":          row["dir_name"],
                "is_compilation":    bool(row["is_compilation"]),
                "headline":          canonical_album_artist or row["dir_name"] or album_id,
                "mbid":              resolution.anchor.value,
                # release_mbid lives on the album row when known; resolver
                # doesn't separate it out, but the apply path doesn't need
                # it functionally — it's a diagnostic for the dialog.
                "release_mbid":      row.get("mb_release_mbid"),
                "score":             int((resolution.anchor.confidence or 0) * 100),
                "album_artist_diff": album_diff,
                "track_artist_diffs": track_diffs,
            },
        )

    def _track_diffs(
        self,
        album_row: dict[str, Any],
        resolution: resolver_service.AlbumResolution,
    ) -> list[dict[str, Any]]:
        """Compare each resolver-matched track's ARTIST tag against its
        canonical artist credit.

        Tracks without a `canonical_artist` (no AcoustID match AND no
        duration-unique alignment) are skipped — we can't reliably
        attribute drift on those.
        """
        candidates = [
            t for t in resolution.tracks
            if t.canonical_artist is not None
        ]
        if not candidates:
            return []

        album_path = Path(album_row.get("path") or "")
        if not album_path.is_dir():
            return []

        diffs: list[dict[str, Any]] = []
        for tr in candidates:
            canonical_artist = tr.canonical_artist.value.strip()
            if not canonical_artist:
                continue
            flac_path = album_path / tr.filename
            if not flac_path.is_file():
                continue
            try:
                audio = FLAC(str(flac_path))
                tags = audio.tags or {}
                v = tags.get("artist") or tags.get("ARTIST") or []
                current_artist = (
                    (v[0] if isinstance(v, list) else str(v)).strip()
                    if v else ""
                )
            except Exception:
                continue
            if not current_artist:
                continue
            if _normalize_for_compare(current_artist) == _normalize_for_compare(canonical_artist):
                continue
            diffs.append({
                "filename":      tr.filename,
                "current":       current_artist,
                "canonical":     canonical_artist,
                "recording_mbid": tr.recording_mbid,
                "mb_title":      tr.canonical_title.value if tr.canonical_title else None,
            })
        return diffs

    def fix(self, finding: Finding, *, user_hint: str = "") -> FixResult:
        """Stub. Resolution goes through the dedicated apply endpoint
        because the user opts in per field — that decision can't be
        encoded in a single-click fix."""
        return FixResult(
            ok=False,
            message=(
                "MB-canonical findings are resolved per-field via the "
                "Resolve… dialog; this check has no single-click fix."
            ),
        )


def _normalize_for_compare(s: str) -> str:
    """Whitespace+case-insensitive equivalence. We deliberately don't
    do anything more aggressive (e.g. unicode NFC, smart-quote
    folding) because those normalizations might mask drift the user
    actually wants to see surfaced."""
    return " ".join(s.lower().split())


def _summarize(
    dir_name: str | None,
    n_album: int,
    n_track: int,
    canonical: str,
) -> str:
    label = dir_name or "(unknown album)"
    parts: list[str] = []
    if n_album:
        parts.append("album_artist")
    if n_track:
        parts.append(f"{n_track} track artist{'s' if n_track != 1 else ''}")
    diff_str = " + ".join(parts) or "no diffs"
    return f"{label!r}: {diff_str} differ from MB ({canonical!r})"


def apply_canonical(
    *,
    album_id: str,
    accept_album_artist: bool,
    accept_track_filenames: list[str],
) -> dict[str, Any]:
    """Apply the user-confirmed subset of the canonical diff. Reads
    canonical values off the resolver so they match what the dialog
    showed — the resolver memoizes, so this is free if the dialog was
    just open. force=True ensures we re-derive in the rare case the
    album was apply-edited between the dialog opening and the user
    clicking Apply (which would have invalidated the cached resolution
    anyway via services.apply)."""
    from . import _apply as direct_apply

    resolution = resolver_service.resolve_album(album_id)
    if resolution.anchor is None:
        return {
            "ok": False,
            "message": "Album lost its MusicBrainz anchor; nothing applied.",
            "applied": 0,
        }

    suggestions: dict[str, str] = {}
    if accept_album_artist and resolution.canonical_album_artist:
        canonical = resolution.canonical_album_artist.value.strip()
        if canonical:
            suggestions["album_artist"] = canonical

    track_suggestions: dict[str, dict[str, str]] = {}
    if accept_track_filenames:
        canonical_by_filename = {
            tr.filename: tr.canonical_artist.value.strip()
            for tr in resolution.tracks
            if tr.canonical_artist is not None
        }
        for fn in accept_track_filenames:
            canonical = canonical_by_filename.get(fn) or ""
            if canonical:
                track_suggestions[fn] = {"artist": canonical}

    if not suggestions and not track_suggestions:
        return {
            "ok": False,
            "message": "Nothing to apply (none of the requested fields had a canonical value).",
            "applied": 0,
        }

    ok, msg = direct_apply.apply_direct(
        album_id=album_id,
        suggestions=suggestions or None,
        track_suggestions=track_suggestions or None,
        rerun_checks=True,
    )
    return {
        "ok":      ok,
        "message": msg,
        "applied": (1 if suggestions else 0) + len(track_suggestions),
    }
