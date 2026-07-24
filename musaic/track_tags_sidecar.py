"""Per-album sidecar for user-defined track tags.

The user's custom tagging taxonomy ("Cocktail Hour", "Christmas",
"Instrumental", etc.) lives in two places:

  - the SQLite cache (table `track_tags`), authoritative for the running
    app session; and
  - a sidecar JSON file in each album's directory (`.media-manager-tags.json`),
    which survives DB resets, app reinstalls, and library moves.

The dot-prefix keeps the file hidden in Plex / Roon / Finder, which is
the whole point — Plex shouldn't see this file. The sidecar is a
straightforward `{filename: [tag, ...]}` map; we don't carry creation
timestamps because they aren't meaningful across machines.

Lifecycle:

  - On scan, if the DB has no track_tags rows for the album, the
    scanner imports the sidecar (if present). After that the DB is
    authoritative for the session.
  - On every mutation (add/remove tag), the API writes the sidecar
    back so the on-disk copy stays current. Best-effort: if the
    sidecar can't be written (permissions, network share down), we
    log and continue — the DB still has the truth.

This module deliberately stays small: read, write, parse, validate.
The cache layer (`cache.py`) decides when to use it.
"""
from __future__ import annotations

import json
import logging
from pathlib import Path

SIDECAR_FILENAME = ".media-manager-tags.json"

log = logging.getLogger(__name__)


def sidecar_path(album_dir: str | Path) -> Path:
    """Path to the sidecar for an album directory."""
    return Path(album_dir) / SIDECAR_FILENAME


def read_sidecar(album_dir: str | Path) -> dict[str, list[str]]:
    """Load the sidecar JSON for `album_dir`.

    Returns `{filename: [tag, ...]}`. Returns an empty dict when the
    sidecar is absent, malformed, or has the wrong shape — the
    importer treats "no usable sidecar" the same as "no tags yet".

    Tolerant of:
      - missing file (common case: album never tagged)
      - empty file
      - file with non-dict root (older sidecar shape, garbage)
      - per-entry shape errors (skipped individually)
    """
    p = sidecar_path(album_dir)
    if not p.is_file():
        return {}
    try:
        raw = p.read_text(encoding="utf-8")
    except OSError as e:
        log.warning("Could not read tag sidecar %s: %s", p, e)
        return {}
    if not raw.strip():
        return {}
    try:
        loaded = json.loads(raw)
    except json.JSONDecodeError as e:
        log.warning("Malformed tag sidecar %s: %s", p, e)
        return {}
    if not isinstance(loaded, dict):
        log.warning("Tag sidecar %s has wrong root shape (expected object)", p)
        return {}
    # Some older flac-jacket exports wrapped the map under a top-level key;
    # accept either shape.
    if "tracks" in loaded and isinstance(loaded["tracks"], dict):
        loaded = loaded["tracks"]
    out: dict[str, list[str]] = {}
    for filename, tags in loaded.items():
        if not isinstance(filename, str) or not filename:
            continue
        if not isinstance(tags, list):
            continue
        cleaned = [
            t.strip() for t in tags
            if isinstance(t, str) and t.strip()
        ]
        # De-dup case-insensitively but preserve the casing of the first
        # occurrence — matches the GENRE union policy in scanner.py.
        seen: set[str] = set()
        unique: list[str] = []
        for t in cleaned:
            k = t.lower()
            if k in seen:
                continue
            seen.add(k)
            unique.append(t)
        if unique:
            out[filename] = unique
    return out


def write_sidecar(album_dir: str | Path, track_tags: dict[str, list[str]]) -> bool:
    """Write the sidecar JSON for `album_dir`. Returns True on success.

    Behavior:
      - Empty `track_tags` (no tagged tracks at all) deletes the
        sidecar rather than leaving a `{}` file lying around.
      - Atomic-ish write: tmp file + rename. SMB / network shares
        sometimes don't honor rename atomicity, but it's the best
        we can do without backend-specific code.
      - Any write failure is logged at WARNING and returns False —
        the DB layer is the source of truth for the current session,
        so a sidecar write miss is not catastrophic.
    """
    p = sidecar_path(album_dir)
    # Filter to non-empty entries so the sidecar is tidy.
    cleaned = {
        fn: list(tags) for fn, tags in track_tags.items()
        if fn and tags
    }
    try:
        if not cleaned:
            if p.is_file():
                p.unlink()
            return True
        tmp = p.with_suffix(p.suffix + ".tmp")
        tmp.write_text(
            json.dumps(cleaned, indent=2, sort_keys=True, ensure_ascii=False),
            encoding="utf-8",
        )
        tmp.replace(p)
        return True
    except OSError as e:
        log.warning("Could not write tag sidecar %s: %s", p, e)
        return False


def remove_sidecar(album_dir: str | Path) -> bool:
    """Best-effort delete of the sidecar (e.g. when the album is
    removed from the library). Returns True if removed or already
    absent."""
    p = sidecar_path(album_dir)
    try:
        p.unlink(missing_ok=True)
        return True
    except OSError as e:
        log.warning("Could not remove tag sidecar %s: %s", p, e)
        return False
