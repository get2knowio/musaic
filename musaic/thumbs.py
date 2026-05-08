"""Cover-art thumbnails: pre-extract small JPEGs to a cache directory.

Thumbnails are 384px on the long edge, JPEG quality 82, stored at
`~/.musaic/thumbs/{album_id}.jpg`. The cover endpoint serves these
directly so tile-view doesn't re-open FLACs or push multi-megabyte original
artwork over the wire for every visible album.

Source-image preference order (cheapest → most expensive):
  1. cover.jpg / folder.jpg / front.jpg (and .png variants) on disk — fastest,
     a kernel file read
  2. Embedded picture in the first FLAC — slower, requires opening + parsing
     the FLAC

This module is `Album`-aware (works on dataclass) and dict-aware (works on the
cached snapshot), since both shapes are passed around.
"""
from __future__ import annotations

import io
import logging
import os
from pathlib import Path
from typing import Any

from .config import CONFIG_DIR
from .scanner import get_embedded_cover

THUMBS_DIR = CONFIG_DIR / "thumbs"
THUMB_SIZE = 384  # px on the long edge
THUMB_QUALITY = 82

log = logging.getLogger(__name__)


def thumb_path(album_id: str) -> Path:
    return THUMBS_DIR / f"{album_id}.jpg"


def _album_field(album: Any, key: str, default=None):
    """Read a field from either an Album dataclass or a dict."""
    if isinstance(album, dict):
        return album.get(key, default)
    return getattr(album, key, default)


def get_disk_cover_path(album: Any) -> Path | None:
    """Return absolute path to the on-disk cover file, or None."""
    cover_file = _album_field(album, "cover_file")
    album_path = _album_field(album, "path")
    if not cover_file or not album_path:
        return None
    p = Path(album_path) / cover_file
    return p if p.is_file() else None


def get_source_image(album: Any) -> tuple[bytes, str] | None:
    """Return (bytes, mime) for the album's best available source image.

    Prefers on-disk cover file (cheap kernel read) over embedded FLAC picture
    (requires opening + parsing the FLAC). Returns None if no cover exists.
    """
    disk = get_disk_cover_path(album)
    if disk is not None:
        try:
            data = disk.read_bytes()
            mime = "image/png" if disk.suffix.lower() == ".png" else "image/jpeg"
            return data, mime
        except OSError as e:
            log.warning("failed to read on-disk cover %s: %s", disk, e)

    if _album_field(album, "has_embedded_cover"):
        sample = _album_field(album, "sample_track")
        if sample:
            result = get_embedded_cover(sample)
            if result:
                return result

    return None


def make_thumbnail(image_bytes: bytes, max_size: int = THUMB_SIZE) -> bytes | None:
    """Resize to `max_size` on the long edge and return JPEG bytes. None on failure."""
    try:
        from PIL import Image
    except ImportError:
        log.warning("Pillow not installed; thumbnails disabled")
        return None
    try:
        img = Image.open(io.BytesIO(image_bytes))
        # Convert palette / RGBA to RGB so JPEG save works.
        if img.mode in ("RGBA", "P", "LA"):
            img = img.convert("RGB")
        img.thumbnail((max_size, max_size))
        out = io.BytesIO()
        img.save(out, format="JPEG", quality=THUMB_QUALITY, optimize=True, progressive=True)
        return out.getvalue()
    except Exception as e:
        log.warning("thumbnail generation failed: %s", e)
        return None


def write_thumb(album_id: str, jpeg_bytes: bytes) -> Path:
    THUMBS_DIR.mkdir(parents=True, exist_ok=True)
    path = thumb_path(album_id)
    # Atomic write: temp file then rename.
    tmp = path.with_suffix(".jpg.tmp")
    tmp.write_bytes(jpeg_bytes)
    os.replace(str(tmp), str(path))
    return path


def ensure_thumb(album: Any, *, force: bool = False) -> Path | None:
    """Generate the thumbnail file if missing (or `force=True`).

    Returns the thumb path on success, None if no source image was available
    or generation failed. Idempotent — safe to call repeatedly.
    """
    album_id = _album_field(album, "id")
    if not album_id:
        return None
    path = thumb_path(album_id)
    if path.is_file() and not force:
        return path
    src = get_source_image(album)
    if src is None:
        return None
    jpeg = make_thumbnail(src[0])
    if jpeg is None:
        return None
    return write_thumb(album_id, jpeg)


def delete_thumb(album_id: str) -> bool:
    """Remove the cached thumbnail. Returns True if a file was deleted."""
    path = thumb_path(album_id)
    try:
        path.unlink()
        return True
    except FileNotFoundError:
        return False
    except OSError as e:
        log.warning("delete_thumb(%s) failed: %s", album_id, e)
        return False


def thumb_exists(album_id: str) -> bool:
    return thumb_path(album_id).is_file()


def thumb_etag(album_id: str) -> str | None:
    """A weak ETag derived from album_id + thumb mtime. None if no thumb."""
    path = thumb_path(album_id)
    try:
        st = path.stat()
    except OSError:
        return None
    # Quote the value per RFC 7232.
    return f'"{album_id}-{int(st.st_mtime)}"'
