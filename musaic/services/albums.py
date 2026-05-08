"""Album resolution + per-track helpers shared across routers and the
AI worker. Anything here should be pure of route knowledge — these are
the building blocks the API layer composes."""
from __future__ import annotations

import logging
from pathlib import Path
from typing import Any

from fastapi import HTTPException
from mutagen.flac import FLAC

from .. import cache as cache_module
from .. import config as config_module
from ..scanner import album_to_dict, find_album_by_id

_log = logging.getLogger(__name__)


def sort_albums(albums: list[dict]) -> list[dict]:
    return sorted(
        albums,
        key=lambda a: (
            (a.get("album_artist") or a.get("artist") or "").lower(),
            (a.get("year") or ""),
            (a.get("title") or "").lower(),
        ),
    )


def albums_for(library_dir: str, *, refresh: bool = False) -> list[dict]:
    """Return the cached album list. If `refresh`, drop the cache first
    (a fresh scan will be triggered by the next /api/scan/stream call).
    """
    if refresh:
        cache_module.drop_snapshot(library_dir)
    snap = cache_module.load_snapshot(library_dir)
    return sort_albums(snap.albums)


def invalidate_cache() -> None:
    """Remove cache for the currently-configured library."""
    cfg = config_module.load_config()
    if cfg.library_dir:
        cache_module.drop_snapshot(cfg.library_dir)


def cached_album_dict(album_id: str) -> dict[str, Any] | None:
    """Look up an album by id from the persistent cache. Returns None if
    not found. Does NOT walk the disk."""
    cfg = config_module.load_config()
    if not cfg.library_dir:
        return None
    snap = cache_module.load_snapshot(cfg.library_dir)
    for album in snap.albums:
        if album.get("id") == album_id:
            return album
    return None


def resolve_album_dict(album_id: str) -> dict[str, Any]:
    """Return the album dict by id. Raises 409 if no library configured,
    404 if not in cache (caller may need to scan first)."""
    cfg = config_module.load_config()
    if not cfg.library_dir:
        raise HTTPException(status_code=409, detail="No library directory configured")
    album = cached_album_dict(album_id)
    if album is None:
        # Try a one-shot live lookup as a fallback (cache may be stale or empty)
        live = find_album_by_id(cfg.library_dir, album_id)
        if live is None:
            raise HTTPException(status_code=404, detail="Album not found (try rescanning the library)")
        return album_to_dict(live)
    return album


def read_tracks(album_dir: str) -> list[dict[str, Any]]:
    """Read per-track Vorbis tags for every FLAC in the album dir.

    Includes the standard editing fields (title/artist/etc.) plus the
    audio-analysis fields (bpm, replaygain_*) that the detail panel
    surfaces. Missing fields come back as empty strings or None.
    """
    tracks: list[dict[str, Any]] = []
    album_path = Path(album_dir)
    if not album_path.is_dir():
        return tracks

    def first(tags_lookup: dict, key: str) -> str:
        # Vorbis comment keys are case-insensitive; mutagen lowercases them.
        v = tags_lookup.get(key) or tags_lookup.get(key.lower()) or []
        if not v:
            return ""
        val = v[0] if isinstance(v, list) else v
        return str(val)

    for entry in sorted(album_path.iterdir()):
        if not entry.is_file() or entry.suffix.lower() != ".flac" or entry.name.startswith("._"):
            continue
        try:
            audio = FLAC(str(entry))
            tags = audio.tags.as_dict() if audio.tags else {}
            bpm_str = first(tags, "bpm")
            try:
                bpm_val: float | None = float(bpm_str) if bpm_str else None
            except ValueError:
                bpm_val = None
            tracks.append({
                "filename": entry.name,
                "tracknumber": first(tags, "tracknumber"),
                "title": first(tags, "title"),
                "artist": first(tags, "artist"),
                "duration": round(audio.info.length, 1) if audio.info else None,
                # Audio analysis (None / "" if not yet computed)
                "bpm": bpm_val,
                "replaygain_track_gain": first(tags, "replaygain_track_gain") or None,
                "replaygain_track_peak": first(tags, "replaygain_track_peak") or None,
                "replaygain_album_gain": first(tags, "replaygain_album_gain") or None,
                "replaygain_album_peak": first(tags, "replaygain_album_peak") or None,
            })
        except Exception as e:
            tracks.append({"filename": entry.name, "error": str(e)})
    return tracks


def list_album_filenames(album_dir: str) -> list[str]:
    """Filenames of FLAC tracks in the album dir, sorted. Used to fan
    out album-level tag operations across every track."""
    p = Path(album_dir)
    if not p.is_dir():
        return []
    return sorted(
        e.name for e in p.iterdir()
        if e.is_file() and e.suffix.lower() == ".flac" and not e.name.startswith("._")
    )


def track_count_for(album: dict[str, Any]) -> int:
    """Album track count for the intersection helper. Prefers the
    cached value but falls back to a directory listing for fresh
    albums that haven't been re-saved yet."""
    n = int(album.get("track_count") or 0)
    if n > 0:
        return n
    return len(list_album_filenames(album["path"]))


def persist_track_tags_sidecar(album_id: str, album_path: str) -> None:
    """Re-serialize the album's tags to the dot-prefixed sidecar JSON.
    Best-effort: failures are logged but never raised, since the DB is
    authoritative for the running session."""
    try:
        from .. import track_tags_sidecar as _sidecar
        track_tag_map = cache_module.get_album_track_tags(album_id)
        _sidecar.write_sidecar(album_path, track_tag_map)
    except Exception:
        _log.exception(
            "Failed to write track-tag sidecar for album %s", album_id
        )


def resync_sidecars_for_albums(album_ids: list[str]) -> None:
    """After a vocabulary mutation that touched track_tags, rewrite the
    on-disk sidecar JSON for each affected album so the file reflects
    the new tag set. Best-effort; failures are logged."""
    if not album_ids:
        return
    pairs = cache_module.get_albums_for_resync(album_ids)
    if not pairs:
        return
    from .. import track_tags_sidecar as _sidecar
    for entry in pairs:
        try:
            tag_map = cache_module.get_album_track_tags(entry["id"])
            _sidecar.write_sidecar(entry["path"], tag_map)
        except Exception:
            _log.exception(
                "Vocabulary mutation: failed to resync sidecar for album %s",
                entry["id"],
            )
