"""Soft-delete helper — moves files into an app-managed trash directory
instead of unlinking them outright.

Why this lives in its own module: every place in the app that wants to
remove a user file (currently just the duplicate-tracks resolve flow,
likely more later — bulk dedupe, "remove from library," etc.) should go
through one consistent path. That makes it easy to swap the backend
later, gives us a single place to log "file moved to trash on behalf of
user action X," and keeps the trash layout / collision rules in one
place.

Trash layout
------------

Root: `~/.musaic/trash/` (override the parent via
`MUSAIC_CONFIG_DIR`).

Files are organized under the trash root by their album's directory
name, preserving the original filename:

    ~/.musaic/trash/<album_dir>/<filename>

On filename collision (e.g. the user deletes two files with the same
name on different days), the trashed copy gets an ISO timestamp suffix
inserted before the extension:

    ~/.musaic/trash/<album_dir>/<stem>__2026-05-05T16-30-45.<ext>

Cross-volume moves
------------------

`shutil.move` falls back to copy+remove when source and destination are
on different filesystems. For typical setups (music library on an
external drive, app config on the boot disk) that means a real copy,
which uses extra disk and is slower than a same-volume rename. We
accept this — deletes are rare and manual; correctness > speed. If it
ever becomes an issue, we can add an option to put the trash dir on the
same volume as the library.
"""
from __future__ import annotations

import logging
import shutil
from datetime import datetime
from pathlib import Path

from .config import CONFIG_DIR

log = logging.getLogger(__name__)


# Where trashed files land. Lives under the app's config dir alongside
# the SQLite DB and the rotating server.log, so the user has one place
# to look for app state. Created lazily on first use.
TRASH_DIR = CONFIG_DIR / "trash"


class TrashError(Exception):
    """Raised when a file couldn't be moved to the trash. Caller decides
    how to surface it — the duplicate-tracks endpoint turns it into a
    500 with the message. The original exception is chained so the
    cause shows up in server logs."""


def trash_dir() -> Path:
    """Public accessor — useful if a UI wants to display "files go to X"
    or build a "show trash" link."""
    return TRASH_DIR


def send_to_trash(path: Path, *, album_dir_name: str | None = None) -> Path:
    """Move `path` into the app trash. Returns the new path on disk.

    `album_dir_name` is the source album's `dir_name`, used as a
    subfolder under the trash root so trashed files keep their context.
    Falls back to the parent directory's name when not supplied.

    On filename collision under that subfolder, appends an ISO timestamp
    before the extension so we never silently overwrite a previously-
    trashed file.

    Raises TrashError on any IO failure. Doesn't fall through to a
    permanent unlink — the user explicitly asked for soft-delete.
    """
    if not path.is_file():
        raise TrashError(f"File doesn't exist (or isn't a regular file): {path}")

    # Subfolder = album dir name. Defensively strip path separators so a
    # weird value can't escape the trash root.
    sub = (album_dir_name or path.parent.name).replace("/", "_").replace("\\", "_")
    sub = sub or "untitled"
    target_dir = TRASH_DIR / sub

    try:
        target_dir.mkdir(parents=True, exist_ok=True)
    except OSError as e:
        raise TrashError(
            f"Couldn't create trash directory {target_dir}: {e}"
        ) from e

    target = target_dir / path.name
    if target.exists():
        # Suffix the stem with an ISO-ish timestamp (no colons — Windows-
        # safe and easier to eyeball) and keep the extension intact.
        stamp = datetime.now().strftime("%Y-%m-%dT%H-%M-%S")
        target = target_dir / f"{path.stem}__{stamp}{path.suffix}"

    try:
        shutil.move(str(path), str(target))
    except OSError as e:
        raise TrashError(
            f"Couldn't move {path.name!r} to trash: {e}"
        ) from e

    log.info("Trashed %s → %s", path, target)
    return target
