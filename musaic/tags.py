"""Write Vorbis comment tags + embed cover art across an album's FLAC files.

Lifts the safe-write pattern from flac-jacket's `tools/tag_write.py`: only the
tags explicitly passed in are touched; nothing else is cleared. A `dry_run`
flag computes the diff without saving.
"""
from __future__ import annotations

import io
import logging
import os
import re
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from mutagen.flac import FLAC, Picture

log = logging.getLogger(__name__)


# Map album-level suggestion keys → Vorbis comment keys.
_FIELD_TO_VORBIS = {
    "title": "album",            # album title
    "album_artist": "albumartist",
    "artist": "artist",
    "year": "date",
    "genre": "genre",
    # v8: Vorbis sort variants. Standard convention — Picard, beets,
    # foobar2000 all use the *SORT spelling. Plex / Roon / Symfonium
    # honor them for sidebar alphabetization.
    "album_artist_sort": "albumartistsort",
    "artist_sort":       "artistsort",
    "album_sort":        "albumsort",
}

# Map per-track suggestion keys → Vorbis comment keys.
_TRACK_FIELD_TO_VORBIS = {
    "title": "title",
    "artist": "artist",
    "tracknumber": "tracknumber",
    # v11: per-track ARTISTSORT, used by the inconsistent-artist-sort
    # check when the cluster contains track-level credits (e.g. four
    # different "Ray Conniff" credit variations across compilation
    # tracks). The cluster-apply path writes ARTISTSORT to the FLACs
    # the user selected without touching their displayed ARTIST.
    "artist_sort": "artistsort",
}

# Recognized leading-numeric prefixes in FLAC filenames. Captures the prefix
# (group "prefix") plus any separator before the rest of the name. Examples:
#   "01 - Title.flac"       prefix="01", sep=" - "
#   "01-Title.flac"         prefix="01", sep="-"
#   "1.05 - Title.flac"     prefix="1.05", sep=" - "
#   "1-05 - Title.flac"     prefix="1-05", sep=" - "
#   "Track 01 - Title.flac" no match — leave alone
_FILENAME_PREFIX_RE = re.compile(r"^(?P<prefix>\d+(?:[-.]\d+)?)(?P<sep>[\s\-_.]+)")


@dataclass
class FileChange:
    path: str
    diffs: dict[str, dict[str, str]] = field(default_factory=dict)  # vorbis_key -> {old, new}
    art_changed: bool = False
    saved: bool = False
    error: str | None = None


@dataclass
class ApplyResult:
    album_path: str
    dry_run: bool
    suggestions_applied: dict[str, str]
    files: list[FileChange]
    art_url: str | None = None


def _list_flacs(album_dir: Path) -> list[Path]:
    return sorted(
        p for p in album_dir.iterdir()
        if p.is_file() and p.suffix.lower() == ".flac" and not p.name.startswith("._")
    )


def _current(audio: FLAC, vorbis_key: str) -> str:
    if not audio.tags:
        return ""
    values = audio.tags.get(vorbis_key, [])
    if not values:
        # case-insensitive fallback
        for k, v in audio.tags.as_dict().items():
            if k.lower() == vorbis_key.lower() and v:
                return str(v[0]) if isinstance(v, list) else str(v)
        return ""
    return str(values[0])


def _proposed_filename(current_name: str, new_tracknumber: str) -> str | None:
    """If `current_name` has a recognizable leading numeric prefix, return a
    new filename with that prefix replaced by `new_tracknumber` (zero-padded
    to two digits). Returns None if no prefix is found — the caller should
    leave the filename alone in that case.
    """
    m = _FILENAME_PREFIX_RE.match(current_name)
    if not m:
        return None
    try:
        n = int(new_tracknumber)
    except (ValueError, TypeError):
        return None
    new_prefix = f"{n:02d}"
    return new_prefix + current_name[m.end("prefix"):]


def apply_to_album_iter(
    album_dir: str | Path,
    suggestions: dict[str, str],
    *,
    track_suggestions: dict[str, dict[str, str]] | None = None,
    cover_bytes: bytes | None = None,
    cover_mime: str | None = None,
    dry_run: bool = False,
):
    """Generator over apply events. Yields ("event_name", payload) tuples.

    `suggestions` are album-level (applied to every FLAC).
    `track_suggestions` is keyed by FLAC filename → {title?, artist?, tracknumber?}.
    Per-track suggestions take precedence over album-level for overlapping keys
    (e.g. per-track "artist" wins over album-level "artist" for that one file).

    When a per-track suggestion changes `tracknumber` AND the existing filename
    has a recognizable numeric prefix (e.g. "05 - …"), the file is renamed to
    match the new number. Renames happen after all tag writes via a two-pass
    approach (rename to temp, then to final) so we never collide mid-flight.

    Event sequence:
      ("start",         {album_path, dry_run, file_count, suggestions_applied, has_cover_change, track_change_count})
      ("file_started",  {index, filename, path})
      ("file_done",     FileChange-as-dict, possibly with `renamed_to`)
      … repeat per file …
      ("file_renamed",  {old_path, new_path, old_filename, new_filename})  # 0+ events after the per-file loop
      ("done",          {album_path, dry_run, saved_count, art_changed_count, renamed_count, errors, suggestions_applied})

    Callers wanting a single aggregated result should use `apply_to_album()`.
    """
    album_path = Path(album_dir)
    if not album_path.is_dir():
        raise FileNotFoundError(f"Not a directory: {album_path}")

    track_suggestions = track_suggestions or {}

    # Filter + translate album-level suggestions to Vorbis keys.
    album_applied: dict[str, str] = {
        _FIELD_TO_VORBIS[k]: str(v).strip()
        for k, v in suggestions.items()
        if k in _FIELD_TO_VORBIS and v not in (None, "")
    }
    inv_album = {v: k for k, v in _FIELD_TO_VORBIS.items()}
    user_friendly_applied = {inv_album.get(k, k): v for k, v in album_applied.items()}

    # Pre-translate per-file suggestions and compute target rename mappings.
    # Maps: filename → {vorbis_key: new_value}
    per_file_translated: dict[str, dict[str, str]] = {}
    for filename, raw_sug in track_suggestions.items():
        if not isinstance(raw_sug, dict):
            continue
        translated = {
            _TRACK_FIELD_TO_VORBIS[k]: str(v).strip()
            for k, v in raw_sug.items()
            if k in _TRACK_FIELD_TO_VORBIS and v not in (None, "")
        }
        if translated:
            per_file_translated[filename] = translated

    flacs = _list_flacs(album_path)
    pic: Picture | None = None
    if cover_bytes:
        pic = _build_picture(cover_bytes, cover_mime or "image/jpeg")

    yield "start", {
        "album_path": str(album_path),
        "dry_run": dry_run,
        "file_count": len(flacs),
        "suggestions_applied": user_friendly_applied,
        "has_cover_change": pic is not None,
        "track_change_count": len(per_file_translated),
    }

    if not flacs:
        yield "done", {
            "album_path": str(album_path),
            "dry_run": dry_run,
            "saved_count": 0,
            "art_changed_count": 0,
            "renamed_count": 0,
            "errors": 0,
            "suggestions_applied": user_friendly_applied,
        }
        return

    saved_count = 0
    art_changed_count = 0
    errors = 0

    # Collect rename plan: list of (original Path, target name) — built up
    # during the per-file loop, executed after all tag writes succeed.
    pending_renames: list[tuple[Path, str]] = []

    for index, flac_path in enumerate(flacs):
        yield "file_started", {
            "index": index,
            "filename": flac_path.name,
            "path": str(flac_path),
        }
        change = FileChange(path=str(flac_path))
        try:
            audio = FLAC(str(flac_path))

            # Combine album-level + per-file. Per-file wins on overlap
            # (e.g. compilation per-track artist beats album-level artist).
            file_specific = per_file_translated.get(flac_path.name, {})
            combined = {**album_applied, **file_specific}

            for vkey, new_val in combined.items():
                old_val = _current(audio, vkey)
                if str(old_val) != str(new_val):
                    change.diffs[vkey] = {"old": old_val, "new": new_val}
                    if not dry_run:
                        audio[vkey] = new_val

            if pic is not None:
                change.art_changed = True
                if not dry_run:
                    audio.clear_pictures()
                    audio.add_picture(pic)

            if not dry_run and (change.diffs or change.art_changed):
                audio.save()
                change.saved = True
                saved_count += 1
                if change.art_changed:
                    art_changed_count += 1

            # Plan rename if this file got a new tracknumber AND filename
            # has a recognizable numeric prefix.
            new_tracknum = file_specific.get("tracknumber")
            if new_tracknum:
                proposed = _proposed_filename(flac_path.name, new_tracknum)
                if proposed and proposed != flac_path.name:
                    pending_renames.append((flac_path, proposed))
                    # Surface the planned rename in the file_done event so
                    # the UI can show it inline.
                    setattr(change, "_planned_rename", proposed)
        except Exception as e:
            change.error = str(e)
            errors += 1

        out = change_to_dict(change)
        if hasattr(change, "_planned_rename"):
            out["renamed_to"] = getattr(change, "_planned_rename")
        yield "file_done", out

    # Two-pass rename: stage all to temp names, then move to finals. This
    # prevents collisions when N tracks swap numbers (e.g. 5↔7 mid-album).
    renamed_count = 0
    if pending_renames and not dry_run:
        staged: list[tuple[Path, Path, Path]] = []  # (original, tmp, final)
        try:
            for orig, target_name in pending_renames:
                tmp = orig.with_name(f".{orig.name}.renaming.tmp")
                orig.rename(tmp)
                staged.append((orig, tmp, orig.with_name(target_name)))
            for orig, tmp, final in staged:
                if final.exists():
                    # Final path collides with an unrelated file (not in our
                    # rename batch). Roll this one back rather than overwriting.
                    log.warning("rename target exists, rolling back: %s", final)
                    tmp.rename(orig)
                    errors += 1
                    continue
                tmp.rename(final)
                renamed_count += 1
                yield "file_renamed", {
                    "old_path": str(orig),
                    "new_path": str(final),
                    "old_filename": orig.name,
                    "new_filename": final.name,
                }
        except Exception:
            log.exception("rename batch failed; attempting rollback")
            errors += 1
            for orig, tmp, _final in staged:
                if tmp.exists() and not orig.exists():
                    try:
                        tmp.rename(orig)
                    except OSError:
                        pass

    yield "done", {
        "album_path": str(album_path),
        "dry_run": dry_run,
        "saved_count": saved_count,
        "art_changed_count": art_changed_count,
        "renamed_count": renamed_count,
        "errors": errors,
        "suggestions_applied": user_friendly_applied,
    }


def apply_to_album(
    album_dir: str | Path,
    suggestions: dict[str, str],
    *,
    track_suggestions: dict[str, dict[str, str]] | None = None,
    cover_bytes: bytes | None = None,
    cover_mime: str | None = None,
    dry_run: bool = False,
) -> ApplyResult:
    """Aggregating wrapper around `apply_to_album_iter`. Returns a single
    ApplyResult after all events have been consumed. Used by the legacy
    synchronous JSON endpoint."""
    files: list[FileChange] = []
    final: dict[str, Any] = {}
    user_friendly_applied: dict[str, str] = {}
    album_path = Path(album_dir)

    for event, payload in apply_to_album_iter(
        album_dir, suggestions,
        track_suggestions=track_suggestions,
        cover_bytes=cover_bytes, cover_mime=cover_mime, dry_run=dry_run,
    ):
        if event == "start":
            user_friendly_applied = payload.get("suggestions_applied", {})
        elif event == "file_done":
            change = FileChange(
                path=payload["path"],
                diffs=payload.get("diffs", {}),
                art_changed=payload.get("art_changed", False),
                saved=payload.get("saved", False),
                error=payload.get("error"),
            )
            files.append(change)
        elif event == "done":
            final = payload

    return ApplyResult(
        album_path=str(album_path),
        dry_run=final.get("dry_run", dry_run),
        suggestions_applied=user_friendly_applied,
        files=files,
    )


def _build_picture(data: bytes, mime: str) -> Picture:
    pic = Picture()
    pic.type = 3  # front cover
    pic.mime = mime
    pic.desc = "Front Cover"
    try:
        from PIL import Image
        img = Image.open(io.BytesIO(data))
        pic.width = img.width
        pic.height = img.height
        pic.depth = 8 * len(img.getbands())
    except Exception:
        # If Pillow isn't around or the bytes aren't a real image, ship anyway —
        # mutagen will still embed the bytes; metadata fields just won't be set.
        pass
    pic.data = data
    return pic


def download_cover(url: str, *, timeout: int = 30) -> tuple[bytes, str] | None:
    """Fetch an image URL. Returns (bytes, mime) or None on failure.

    Reads from the in-memory cover cache first — the AI suggestion's
    verification step does a full GET and stashes the bytes there, so a
    "verify suggestion → apply" round-trip doesn't re-hit the source. Falls
    back to a fresh GET on cache miss (server restarted, TTL expired, or
    the apply path was reached without prior verification)."""
    from . import cover_cache
    cached = cover_cache.get(url)
    if cached is not None:
        log.debug("cover bytes served from in-memory cache: %s", url)
        return cached

    try:
        import requests
    except ImportError:
        return None
    try:
        resp = requests.get(url, timeout=timeout, headers={"User-Agent": "musaic/0.1"})
        if resp.status_code == 200:
            mime = resp.headers.get("content-type", "image/jpeg").split(";")[0].strip()
            # Populate cache so any subsequent reference within TTL is a hit
            # (e.g. retry after a transient error mid-apply).
            cover_cache.put(url, resp.content, mime)
            return resp.content, mime
    except Exception as e:
        log.warning("cover download failed: %s", e)
    return None


def normalize_to_jpeg(image_bytes: bytes, *, quality: int = 95) -> bytes:
    """Convert any Pillow-readable image to JPEG bytes at the given quality.

    Always re-encodes (even if input is already JPEG) so output is consistent
    quality-95 regardless of source. Drops alpha channels (RGBA → RGB) since
    JPEG can't carry transparency. Falls back to the original bytes if Pillow
    isn't available or the image can't be opened.
    """
    try:
        from PIL import Image
    except ImportError:
        log.warning("Pillow not installed; cannot normalize cover to JPEG")
        return image_bytes
    try:
        img = Image.open(io.BytesIO(image_bytes))
        if img.mode in ("RGBA", "P", "LA"):
            img = img.convert("RGB")
        out = io.BytesIO()
        img.save(out, format="JPEG", quality=quality, optimize=True, progressive=True)
        return out.getvalue()
    except Exception as e:
        log.warning("cover JPEG normalization failed: %s — keeping original bytes", e)
        return image_bytes


# Other cover-art file names we'd consider "competing" with the cover.jpg we
# write. We delete cover.jpeg / cover.png since they share the base name and
# would just be stale duplicates. We DON'T touch folder.* or front.* — those
# are separate conventions the user may rely on for specific tools.
_COMPETING_COVER_FILES = ("cover.jpeg", "cover.png", "cover.JPG", "cover.JPEG", "cover.PNG")


def save_cover_to_album_dir(album_dir: str | Path, jpeg_bytes: bytes) -> Path | None:
    """Write `cover.jpg` next to the FLAC files. Removes stale cover.{jpeg,png}
    siblings. Returns the path written, or None on failure.

    Uses an atomic write (temp file + rename) so a half-written cover never
    appears on disk. Folder.* and front.* sibling covers are left alone.
    """
    album_path = Path(album_dir)
    if not album_path.is_dir():
        log.warning("save_cover_to_album_dir: not a directory: %s", album_path)
        return None
    target = album_path / "cover.jpg"
    tmp = album_path / ".cover.jpg.tmp"
    try:
        tmp.write_bytes(jpeg_bytes)
        os.replace(str(tmp), str(target))
    except OSError as e:
        log.warning("failed to write %s: %s", target, e)
        try:
            tmp.unlink()
        except OSError:
            pass
        return None

    # Best-effort cleanup of stale duplicates. The same-file check uses
    # inode comparison (`os.path.samefile`) rather than `.resolve()`
    # string equality — on case-insensitive filesystems (macOS APFS by
    # default), `cover.jpg` and `cover.JPG` resolve to different path
    # strings but are the same physical file. The string compare would
    # delete the cover we just wrote.
    for name in _COMPETING_COVER_FILES:
        stale = album_path / name
        try:
            if not stale.is_file():
                continue
            if os.path.samefile(str(stale), str(target)):
                # Same physical file as our target (case-insensitive FS) —
                # leave it alone.
                continue
            stale.unlink()
            log.info("removed stale cover sibling: %s", stale.name)
        except OSError:
            pass
    return target


def change_to_dict(c: FileChange) -> dict[str, Any]:
    return {
        "path": c.path,
        "filename": Path(c.path).name,
        "diffs": c.diffs,
        "art_changed": c.art_changed,
        "saved": c.saved,
        "error": c.error,
    }


def result_to_dict(r: ApplyResult) -> dict[str, Any]:
    return {
        "album_path": r.album_path,
        "dry_run": r.dry_run,
        "suggestions_applied": r.suggestions_applied,
        "files": [change_to_dict(f) for f in r.files],
        "art_url": r.art_url,
    }
