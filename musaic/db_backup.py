"""Backup + restore of the SQLite library DB.

Backup uses SQLite's `VACUUM INTO` to write a clean, self-contained
copy of the database — no WAL / SHM dependencies, no half-finished
transactions, safe to ship as a single file. The output is byte-
identical to a freshly-created DB with the same content.

Restore is a destructive operation: the user uploads a .db file, we
validate it, move the live DB aside as `library.db.bak-{timestamp}`,
move the uploaded file into place, and reset the bootstrap flag so
the next `db.connect()` opens the new file. Existing in-flight
connections keep pointing at the old inode (Unix semantics) until
they close, which lets the swap happen mid-traffic without a server
restart.

We do NOT auto-migrate restored DBs forward. If the upload's
schema_version is older than the current `SCHEMA_VERSION`, restore
runs the same migration path bootstrap() uses, so the restored DB
ends up at the current schema. If it's NEWER (the user is restoring
on an older binary), we refuse rather than silently corrupting.
"""
from __future__ import annotations

import logging
import shutil
import sqlite3
import tempfile
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from . import db as db_module
from .config import CONFIG_DIR

log = logging.getLogger(__name__)

# Tables the validator checks for. Doesn't have to be exhaustive — we
# just want to confirm the upload is OUR DB and not, say, somebody's
# Firefox cookies.db. The runtime bootstrap() can fill in any missing
# table that's been added since the backup.
_REQUIRED_TABLES: frozenset[str] = frozenset({
    "schema_version",
    "libraries",
    "albums",
})


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")


def _now_filename_tag() -> str:
    """Filesystem-safe timestamp for backup / aside filenames."""
    return datetime.now(timezone.utc).strftime("%Y%m%d-%H%M%SZ")


# ---------- Info ----------

def db_info() -> dict[str, Any]:
    """Summary stats for the live DB. Used by the Settings panel to
    show what's currently loaded so the user can sanity-check before
    overwriting it."""
    db_path: Path = db_module.DB_PATH
    info: dict[str, Any] = {
        "path":           str(db_path),
        "exists":         db_path.is_file(),
        "size_bytes":     db_path.stat().st_size if db_path.is_file() else 0,
        "schema_version": None,
        "current_schema": db_module.SCHEMA_VERSION,
        "counts":         {},
        "modified_at":    None,
    }
    if not db_path.is_file():
        return info
    try:
        info["modified_at"] = datetime.fromtimestamp(
            db_path.stat().st_mtime, tz=timezone.utc,
        ).isoformat(timespec="seconds").replace("+00:00", "Z")
    except OSError:
        pass
    db_module.bootstrap()
    with db_module.connect() as conn:
        try:
            row = conn.execute("SELECT version FROM schema_version LIMIT 1").fetchone()
            info["schema_version"] = row["version"] if row else None
        except sqlite3.Error:
            pass
        info["counts"] = _gather_counts(conn)
    return info


def _gather_counts(conn: sqlite3.Connection) -> dict[str, int]:
    """Per-table row counts for the headline tables. Wrapped in
    try/except per query so a missing optional table (older schema)
    just shows zero rather than 500ing the whole panel."""
    out: dict[str, int] = {}
    queries = {
        "albums":              "SELECT COUNT(*) FROM albums",
        "albums_with_mbid":    "SELECT COUNT(*) FROM albums WHERE mb_release_group_mbid IS NOT NULL",
        "track_fingerprints":  "SELECT COUNT(*) FROM track_fingerprints",
        "track_tags":          "SELECT COUNT(*) FROM track_tags",
        "tag_vocabulary":      "SELECT COUNT(*) FROM tag_vocabulary",
        "ai_jobs":             "SELECT COUNT(*) FROM ai_jobs",
        "check_findings_open":
            "SELECT COUNT(*) FROM check_findings "
            "WHERE resolved_at IS NULL AND ignored_at IS NULL",
    }
    for label, sql in queries.items():
        try:
            row = conn.execute(sql).fetchone()
            out[label] = int(row[0]) if row else 0
        except sqlite3.Error:
            out[label] = 0
    return out


# ---------- Backup ----------

def make_backup(target_dir: str | Path | None = None) -> Path:
    """Write a consistent snapshot of the live DB to a temp file (or
    `target_dir` if provided) and return its path. Caller is
    responsible for cleaning up the file after streaming it.

    Uses `VACUUM INTO` so the output is a clean, self-contained DB
    file — no WAL / SHM, no half-finished transactions, no need for
    the writer to be quiesced. SQLite handles the locking internally."""
    db_module.bootstrap()
    if target_dir is None:
        fd, path_str = tempfile.mkstemp(prefix="mm-backup-", suffix=".db")
        # Close the fd; SQLite needs to open the path itself, and
        # mkstemp opened it for us already.
        import os as _os
        _os.close(fd)
        _os.unlink(path_str)  # VACUUM INTO refuses if the file exists
        target_path = Path(path_str)
    else:
        target = Path(target_dir)
        target.mkdir(parents=True, exist_ok=True)
        target_path = target / f"library-{_now_filename_tag()}.db"
        if target_path.exists():
            target_path.unlink()
    # VACUUM INTO doesn't accept parameter binding for the path —
    # we have to interpolate. SQLite uses double single-quote escaping
    # for embedded apostrophes in string literals.
    quoted = "'" + str(target_path).replace("'", "''") + "'"
    with db_module.connect() as conn:
        conn.execute(f"VACUUM INTO {quoted}")
    log.info("DB backup written: %s (%d bytes)", target_path, target_path.stat().st_size)
    return target_path


def suggested_backup_filename() -> str:
    """The filename the download should arrive as. Includes the live
    schema_version so a user staring at multiple backups in a folder
    can tell them apart at a glance."""
    schema = db_module.SCHEMA_VERSION
    return f"musaic-v{schema}-{_now_filename_tag()}.db"


# ---------- Restore ----------

class RestoreError(ValueError):
    """Validation / IO failure during restore. Carries a user-friendly
    message so the API can surface it directly without re-wrapping."""


def validate_upload(upload_path: str | Path) -> dict[str, Any]:
    """Open the uploaded file as a SQLite DB and confirm it's a valid
    Musaic backup. Returns a summary dict with schema_version
    and per-table counts so the API can echo it back to the UI for
    the user's confirmation step.

    Raises RestoreError on any validation failure — bad magic, missing
    tables, schema_version newer than the running binary's, etc."""
    p = Path(upload_path)
    if not p.is_file():
        raise RestoreError(f"Upload path doesn't exist: {p}")
    if p.stat().st_size < 16:
        raise RestoreError("Upload is too small to be a SQLite database.")
    # SQLite files start with "SQLite format 3\0".
    with p.open("rb") as f:
        header = f.read(16)
    if not header.startswith(b"SQLite format 3"):
        raise RestoreError(
            "Upload doesn't look like a SQLite database — wrong file format."
        )
    # Open read-only and inspect.
    try:
        conn = sqlite3.connect(f"file:{p}?mode=ro", uri=True)
        conn.row_factory = sqlite3.Row
    except sqlite3.Error as e:
        raise RestoreError(f"Could not open upload as SQLite: {e}") from e
    try:
        # Confirm it's our schema.
        rows = conn.execute(
            "SELECT name FROM sqlite_master WHERE type='table'"
        ).fetchall()
        tables = {r["name"] for r in rows}
        missing = _REQUIRED_TABLES - tables
        if missing:
            raise RestoreError(
                "Upload is a SQLite DB but doesn't look like a Musaic "
                f"backup — missing tables: {', '.join(sorted(missing))}"
            )
        try:
            row = conn.execute(
                "SELECT version FROM schema_version LIMIT 1"
            ).fetchone()
        except sqlite3.Error as e:
            raise RestoreError(
                f"Could not read schema_version from upload: {e}"
            ) from e
        if row is None:
            raise RestoreError(
                "Upload's schema_version table is empty — refusing to restore."
            )
        upload_schema = int(row["version"])
        if upload_schema > db_module.SCHEMA_VERSION:
            raise RestoreError(
                f"Upload is from a newer build (schema v{upload_schema}); "
                f"this build only knows up to v{db_module.SCHEMA_VERSION}. "
                "Update Musaic before restoring."
            )
        counts = _gather_counts(conn)
    finally:
        conn.close()
    return {
        "schema_version": upload_schema,
        "size_bytes":     p.stat().st_size,
        "counts":         counts,
    }


def restore_from(upload_path: str | Path) -> dict[str, Any]:
    """Replace the live DB with the uploaded file. Returns the post-
    restore info dict so the caller can show the user what landed.

    Behavior:
      - Validates first via validate_upload(); raises RestoreError on
        any problem before touching the live DB.
      - Moves the existing DB aside as library.db.bak-{timestamp}
        so the user can recover if the new one is bad. Same for the
        WAL / SHM files.
      - Atomically moves the upload into place via Path.replace().
      - Resets db._bootstrap_done so the next connect() re-opens
        the fresh file and runs migrations if needed (for backups
        from older builds).
    """
    info = validate_upload(upload_path)
    upload = Path(upload_path)
    live = db_module.DB_PATH
    aside_tag = _now_filename_tag()

    # Make sure the config dir exists (first-run case).
    live.parent.mkdir(parents=True, exist_ok=True)

    # Move existing DB + its companions aside. WAL / SHM files only
    # matter if we don't move them; SQLite won't open the new DB
    # cleanly with stale WAL state from the old one alongside it.
    moved: list[tuple[Path, Path]] = []
    try:
        for ext in ("", "-wal", "-shm"):
            src = Path(str(live) + ext)
            if src.exists():
                dst = src.with_name(src.name + f".bak-{aside_tag}")
                src.replace(dst)
                moved.append((src, dst))
                log.info("Moved aside for restore: %s → %s", src, dst)
        # Move the upload into place. Use copy then unlink rather than
        # rename, because the upload may be on a different filesystem
        # (tempfile dir vs config dir) and Path.replace can fail
        # cross-device.
        shutil.copyfile(upload, live)
    except OSError as e:
        # Roll back the aside-moves so the user isn't left with no DB.
        log.exception("Restore failed mid-swap; rolling back asides")
        for src, dst in reversed(moved):
            try:
                dst.replace(src)
            except OSError:
                log.exception("Rollback failed for %s", src)
        raise RestoreError(f"Couldn't replace live DB: {e}") from e

    # Force the next connect() to re-open the fresh file and run
    # migrations if needed.
    db_module._bootstrap_done = False
    try:
        db_module.bootstrap()
    except Exception as e:
        log.exception("Bootstrap failed after restore; the new DB may be incomplete")
        raise RestoreError(f"Restore wrote the new DB but bootstrap failed: {e}") from e

    info["restored_at"] = _now_iso()
    info["aside_paths"] = [str(d) for _, d in moved]
    return info


# ---------- Snapshots ----------
#
# Periodic automatic backups stored in ~/.musaic/backups/. The
# user picks one in Settings and clicks Restore. This is the "I broke
# something, take me back" path; the manual download/upload above is
# for off-machine backups.

SNAPSHOT_DIR: Path = CONFIG_DIR / "backups"
SNAPSHOT_PREFIX = "library-"
SNAPSHOT_SUFFIX = ".db"


def _ensure_snapshot_dir() -> Path:
    SNAPSHOT_DIR.mkdir(parents=True, exist_ok=True)
    return SNAPSHOT_DIR


def list_snapshots() -> list[dict[str, Any]]:
    """All on-disk snapshots, newest first. Each entry has the
    filename (just the basename — never an absolute path; the API
    builds paths via _resolve_snapshot for safety), size, mtime,
    and schema_version when readable.

    Snapshots that fail to open (corrupted, partially written) still
    appear in the list with schema_version=None so the user can
    delete them from the UI."""
    _ensure_snapshot_dir()
    entries: list[dict[str, Any]] = []
    for p in SNAPSHOT_DIR.glob(f"{SNAPSHOT_PREFIX}*{SNAPSHOT_SUFFIX}"):
        if not p.is_file():
            continue
        try:
            stat = p.stat()
        except OSError:
            continue
        info: dict[str, Any] = {
            "filename":       p.name,
            "size_bytes":     stat.st_size,
            "modified_at":    datetime.fromtimestamp(
                stat.st_mtime, tz=timezone.utc,
            ).isoformat(timespec="seconds").replace("+00:00", "Z"),
            "schema_version": None,
            "valid":          False,
            "counts":         {},
        }
        try:
            conn = sqlite3.connect(f"file:{p}?mode=ro", uri=True)
            conn.row_factory = sqlite3.Row
            try:
                row = conn.execute(
                    "SELECT version FROM schema_version LIMIT 1"
                ).fetchone()
                if row:
                    info["schema_version"] = int(row["version"])
                    info["valid"] = True
                    info["counts"] = _gather_counts(conn)
            finally:
                conn.close()
        except sqlite3.Error:
            pass
        entries.append(info)
    # Sort by (modified_at DESC, filename DESC). Filenames include a
    # timestamp prefix and a numeric same-second tiebreaker suffix, so
    # filename-descending breaks mtime ties in favor of the more
    # recently-created file. Without this, multiple snapshots created
    # in the same second can re-order randomly and a just-created
    # snapshot can land outside the retention window.
    entries.sort(key=lambda e: (e["modified_at"], e["filename"]), reverse=True)
    return entries


def _resolve_snapshot(name: str) -> Path:
    """Resolve a snapshot filename (basename only) to its full path,
    rejecting any attempt to escape SNAPSHOT_DIR via path traversal.
    The API always passes whatever the user picked from list_snapshots,
    but we belt-and-suspenders this anyway."""
    if not name or "/" in name or "\\" in name or name.startswith("."):
        raise RestoreError(f"Invalid snapshot name: {name!r}")
    p = (SNAPSHOT_DIR / name).resolve()
    snap_dir = SNAPSHOT_DIR.resolve()
    try:
        p.relative_to(snap_dir)
    except ValueError as e:
        raise RestoreError(f"Snapshot name escapes backup dir: {name!r}") from e
    if not p.is_file():
        raise RestoreError(f"Snapshot not found: {name}")
    return p


def create_snapshot(*, retention: int = 10) -> dict[str, Any]:
    """Take a new snapshot in SNAPSHOT_DIR and prune older ones to
    the most recent `retention` (including the just-written one).

    Returns metadata for the new snapshot so the caller can show
    "snapshot 2026-05-04 15:00 saved (12 MB)".
    """
    _ensure_snapshot_dir()
    target = SNAPSHOT_DIR / f"{SNAPSHOT_PREFIX}{_now_filename_tag()}-v{db_module.SCHEMA_VERSION}{SNAPSHOT_SUFFIX}"
    # In the unlikely case the same-second filename already exists,
    # bump a numeric suffix.
    n = 0
    base = target
    while target.exists():
        n += 1
        target = base.with_name(f"{base.stem}.{n}{base.suffix}")
    quoted = "'" + str(target).replace("'", "''") + "'"
    db_module.bootstrap()
    with db_module.connect() as conn:
        conn.execute(f"VACUUM INTO {quoted}")
    # Capture metadata BEFORE pruning. Same-second filenames can tie
    # in the modified_at sort, which means our just-created snapshot
    # could end up just past the retention cutoff and get pruned away
    # before we read its size.
    size_bytes = target.stat().st_size
    log.info("Snapshot written: %s (%d bytes)", target, size_bytes)

    pruned = _prune_snapshots(retention)

    return {
        "filename":       target.name,
        "size_bytes":     size_bytes,
        "modified_at":    _now_iso(),
        "schema_version": db_module.SCHEMA_VERSION,
        "pruned":         pruned,
    }


def _prune_snapshots(retention: int) -> list[str]:
    """Keep the `retention` newest snapshots, delete the rest.
    Returns the names of the deleted ones (for logging / UI)."""
    if retention <= 0:
        return []
    snaps = list_snapshots()
    if len(snaps) <= retention:
        return []
    to_delete = snaps[retention:]
    deleted: list[str] = []
    for s in to_delete:
        try:
            p = SNAPSHOT_DIR / s["filename"]
            p.unlink(missing_ok=True)
            deleted.append(s["filename"])
            log.info("Pruned old snapshot: %s", s["filename"])
        except OSError as e:
            log.warning("Couldn't prune snapshot %s: %s", s["filename"], e)
    return deleted


def delete_snapshot(name: str) -> bool:
    """Remove one snapshot by name. Returns True if a file was
    deleted, False if it was already gone."""
    p = _resolve_snapshot(name)
    try:
        p.unlink()
        log.info("Deleted snapshot: %s", name)
        return True
    except FileNotFoundError:
        return False


def restore_from_snapshot(name: str) -> dict[str, Any]:
    """Restore the live DB from a snapshot in SNAPSHOT_DIR. Wraps
    `restore_from` with a path-safety lookup."""
    p = _resolve_snapshot(name)
    return restore_from(p)


def latest_snapshot_age_seconds() -> float | None:
    """How long ago, in seconds, the most recent snapshot was
    written. Returns None if there are no snapshots — used by the
    startup auto-snapshot hook to decide whether to take one now."""
    snaps = list_snapshots()
    if not snaps:
        return None
    newest = snaps[0]
    try:
        ts = datetime.fromisoformat(newest["modified_at"].replace("Z", "+00:00"))
    except (KeyError, ValueError):
        return None
    return (datetime.now(timezone.utc) - ts).total_seconds()


def maybe_auto_snapshot(*, retention: int, min_interval_hours: float) -> dict[str, Any] | None:
    """Take a snapshot if the most recent one is older than
    `min_interval_hours` (or there are no snapshots yet). Returns
    the new snapshot's metadata, or None if no snapshot was taken.

    `min_interval_hours <= 0` disables the interval gate and forces
    a snapshot every call. `retention <= 0` disables snapshotting
    entirely (no-op return).
    """
    if retention <= 0:
        return None
    if min_interval_hours > 0:
        age = latest_snapshot_age_seconds()
        if age is not None and age < min_interval_hours * 3600:
            log.debug(
                "Skipping auto-snapshot — last one was %.1f hours ago (min %.1fh)",
                age / 3600, min_interval_hours,
            )
            return None
    return create_snapshot(retention=retention)
