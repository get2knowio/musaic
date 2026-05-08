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
  - per-track ARTIST credits — only when track_fingerprints rows
    exist for the album AND the AcoustID-resolved recording_mbid
    has a corresponding entry in the MB release's tracklist. This
    skips the position-based-match false-positive on out-of-order
    rips (the user explicitly opted out of position matching).

Out of scope: album title, per-track titles. The user can extend
later if desired.

Fix path is custom: the user reviews the per-field diffs in a
custom Resolve dialog and opts in field-by-field. No bulk
deterministic action — the entire point is to put the human in the
loop because some "diffs" are deliberate (artist using "&" vs MB's
"and", user preferring a regional credit, etc.).
"""
from __future__ import annotations

import logging
from typing import Any, Iterable

from .. import db as db_module
from .. import mb_lookup
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
            "compared, but only for tracks with AcoustID fingerprints — "
            "those let us match the file to the right MB recording "
            "regardless of disk order."
        ),
        category="metadata",
        cost="medium",
        # MB API calls are rate-limited (1 req/sec) and we do a few
        # per album. Don't run during scan.
        runs_during_scan=False,
        fix_kind="deterministic",
        bulk_action_label=None,
    )

    def run(self, scope: CheckScope) -> Iterable[Finding]:
        with db_module.connect() as conn:
            rows = conn.execute(
                """
                SELECT id, dir_name, album_artist, title, year,
                       is_compilation, mb_release_group_mbid,
                       mb_release_mbid, mb_match_score
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

        # Use the cached find_release_cached path — for an album that
        # already has an MBID anchor, this is just a release-detail
        # fetch (one MB request). We need fetch_tracklist=True to
        # cross-check per-track artists.
        match = mb_lookup.find_release_cached(
            album={"id": album_id},
            fetch_cover=False,
            fetch_tracklist=True,
        )
        if not match.confident:
            return None
        canonical_album_artist = (match.artist or "").strip()
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

        # Per-track diffs (only for albums with fingerprints whose
        # recording_mbids match the MB tracklist).
        track_diffs = self._track_diffs(album_id, match)

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
                "mbid":              match.mbid,
                "release_mbid":      match.release_mbid,
                "score":             match.score,
                "album_artist_diff": album_diff,
                "track_artist_diffs": track_diffs,
            },
        )

    def _track_diffs(
        self,
        album_id: str,
        match: mb_lookup.MusicBrainzMatch,
    ) -> list[dict[str, Any]]:
        """Compare each fingerprinted track's ARTIST tag against the
        MB-canonical artist credit for the corresponding recording.

        Only tracks where AcoustID resolved to a recording_mbid that
        also appears in the MB release's tracklist are checked —
        position-based matching was explicitly excluded, since it
        false-positives on out-of-order rips.
        """
        if not match.tracklist:
            return []
        # Build recording_mbid → MB track lookup (lower-case for
        # robust matching even though MBIDs are already canonical).
        by_recording: dict[str, mb_lookup.MBTrack] = {
            (t.recording_mbid or "").lower(): t
            for t in match.tracklist
            if t.recording_mbid
        }
        if not by_recording:
            return []

        # Pull per-track tag values + fingerprint match data. We
        # need both the on-disk ARTIST (from the FLAC) AND the
        # AcoustID-resolved recording_mbid (from track_fingerprints).
        from pathlib import Path
        from mutagen.flac import FLAC
        with db_module.connect() as conn:
            album_path_row = conn.execute(
                "SELECT path FROM albums WHERE id = ?", (album_id,),
            ).fetchone()
            fp_rows = conn.execute(
                "SELECT filename, acoustid_recording_mbid "
                "FROM track_fingerprints "
                "WHERE album_id = ? AND acoustid_recording_mbid IS NOT NULL",
                (album_id,),
            ).fetchall()
        if album_path_row is None or not fp_rows:
            return []
        album_path = Path(album_path_row["path"])
        if not album_path.is_dir():
            return []

        diffs: list[dict[str, Any]] = []
        for fp in fp_rows:
            recording_mbid = (fp["acoustid_recording_mbid"] or "").lower()
            mb_track = by_recording.get(recording_mbid)
            if mb_track is None:
                # This fingerprinted track isn't on the MB release we
                # cached as the album anchor. Could be a different
                # release of the same release-group — skip rather
                # than false-flag.
                continue
            canonical_artist = (mb_track.artist or "").strip()
            if not canonical_artist:
                continue
            flac_path = album_path / fp["filename"]
            if not flac_path.is_file():
                continue
            try:
                audio = FLAC(str(flac_path))
                tags = audio.tags or {}
                current_artist = ""
                v = tags.get("artist") or tags.get("ARTIST") or []
                if v:
                    current_artist = (v[0] if isinstance(v, list) else str(v)).strip()
            except Exception:
                continue
            if not current_artist:
                continue
            if _normalize_for_compare(current_artist) == _normalize_for_compare(canonical_artist):
                continue
            diffs.append({
                "filename":      fp["filename"],
                "current":       current_artist,
                "canonical":     canonical_artist,
                "recording_mbid": fp["acoustid_recording_mbid"],
                "mb_title":      mb_track.title,
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
    """Apply the user-confirmed subset of the canonical diff. Re-fetches
    the MB release (cached, so cheap) so we apply the SAME values the
    Resolve dialog showed — guards against MB drift between the
    finding being emitted and the user clicking Apply."""
    from . import _apply as direct_apply
    with db_module.connect() as conn:
        row = conn.execute(
            "SELECT album_artist, mb_release_mbid FROM albums WHERE id = ?",
            (album_id,),
        ).fetchone()
    if row is None:
        return {"ok": False, "message": "Album not found", "applied": 0}

    match = mb_lookup.find_release_cached(
        album={"id": album_id},
        fetch_cover=False,
        fetch_tracklist=bool(accept_track_filenames),
    )
    if not match.confident:
        return {
            "ok": False,
            "message": "MB lookup didn't return a confident match; nothing applied.",
            "applied": 0,
        }

    suggestions: dict[str, str] = {}
    if accept_album_artist:
        canonical = (match.artist or "").strip()
        if canonical:
            suggestions["album_artist"] = canonical

    track_suggestions: dict[str, dict[str, str]] = {}
    if accept_track_filenames:
        by_recording = {
            (t.recording_mbid or "").lower(): t
            for t in match.tracklist
            if t.recording_mbid
        }
        with db_module.connect() as conn:
            fp_rows = conn.execute(
                "SELECT filename, acoustid_recording_mbid FROM track_fingerprints "
                "WHERE album_id = ?",
                (album_id,),
            ).fetchall()
        fp_by_filename = {r["filename"]: r["acoustid_recording_mbid"] for r in fp_rows}
        for fn in accept_track_filenames:
            recording_mbid = (fp_by_filename.get(fn) or "").lower()
            if not recording_mbid:
                continue
            mb_track = by_recording.get(recording_mbid)
            if mb_track is None:
                continue
            canonical = (mb_track.artist or "").strip()
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
