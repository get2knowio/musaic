"""Persistent scan cache, backed by SQLite.

This module's public API is the same it was when backed by JSON:

    - `LibrarySnapshot`  : in-memory shape callers consume
    - `load_snapshot()`  : read the cached snapshot for a library
    - `save_snapshot()`  : replace the cached snapshot for a library
    - `drop_snapshot()`  : forget the cached snapshot
    - `get_dir_mtime()`  : helper unrelated to caching, kept for compatibility

Switching the backend is one module deep — `main.py` and the rest of the app
don't see SQLite.

On first import of any library that has an existing JSON cache file (from the
previous JSON-based version), `load_snapshot()` transparently imports the
contents into SQLite and renames the JSON to `library_cache.json.imported`
as a one-shot backup. Idempotent — the second call just reads SQLite.
"""
from __future__ import annotations

import json
import logging
import os
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from . import db as db_module
from .config import CONFIG_DIR

# Kept for back-compat with anything importing this constant.
CACHE_FILE = db_module.DB_PATH
CACHE_VERSION = db_module.SCHEMA_VERSION

# Legacy JSON file from the pre-SQLite version. Imported on first read,
# then renamed to <name>.imported so we never re-import.
_LEGACY_JSON = CONFIG_DIR / "library_cache.json"

log = logging.getLogger(__name__)


@dataclass
class LibrarySnapshot:
    library_dir: str
    scanned_at: str = ""
    albums_by_path: dict[str, dict[str, Any]] = field(default_factory=dict)

    @property
    def albums(self) -> list[dict[str, Any]]:
        return list(self.albums_by_path.values())


# ---------- Row <-> dict ----------

def _row_to_album(row) -> dict[str, Any]:
    """Convert a sqlite3.Row from the albums table into the album dict shape
    callers expect (matches `scanner.album_to_dict`)."""
    return {
        "id": row["id"],
        "path": row["path"],
        "dir_name": row["dir_name"],
        "title": row["title"],
        "artist": row["artist"],
        "album_artist": row["album_artist"],
        "year": row["year"],
        "genre": row["genre"],
        "track_count": row["track_count"] or 0,
        "total_seconds": row["total_seconds"] or 0.0,
        "has_embedded_cover": bool(row["has_embedded_cover"]),
        "cover_file": row["cover_file"],
        "sample_track": row["sample_track"] or "",
        "dir_mtime": row["dir_mtime"] or 0.0,
        # v2 — directory-name parsed fields
        "dir_artist": row["dir_artist"] if "dir_artist" in row.keys() else None,
        "dir_album": row["dir_album"] if "dir_album" in row.keys() else None,
        "is_compilation": bool(row["is_compilation"]) if "is_compilation" in row.keys() else False,
        "disc_number": row["disc_number"] if "disc_number" in row.keys() else None,
        "disc_total": row["disc_total"] if "disc_total" in row.keys() else None,
        # v3 — audio format
        "sample_rate": row["sample_rate"] if "sample_rate" in row.keys() else None,
        "bits_per_sample": row["bits_per_sample"] if "bits_per_sample" in row.keys() else None,
        "channels": row["channels"] if "channels" in row.keys() else None,
        # v6 — per-track track number sequence (stored as JSON; deserialized here)
        "track_numbers": _parse_track_numbers_json(
            row["track_numbers_json"] if "track_numbers_json" in row.keys() else None
        ),
        # v7 — cached MusicBrainz anchors (None until the album has been
        # MB-identified by mb_lookup.find_release).
        "mb_release_group_mbid": row["mb_release_group_mbid"] if "mb_release_group_mbid" in row.keys() else None,
        "mb_release_mbid":       row["mb_release_mbid"] if "mb_release_mbid" in row.keys() else None,
        "mb_match_score":        row["mb_match_score"] if "mb_match_score" in row.keys() else None,
        "mb_last_checked_at":    row["mb_last_checked_at"] if "mb_last_checked_at" in row.keys() else None,
        # v8 — Vorbis sort variants (None when the source FLAC didn't carry them).
        "album_artist_sort": row["album_artist_sort"] if "album_artist_sort" in row.keys() else None,
        "artist_sort":       row["artist_sort"] if "artist_sort" in row.keys() else None,
        "album_sort":        row["album_sort"] if "album_sort" in row.keys() else None,
        # v11 — per-track artist credits (empty list when row pre-dates v11
        # or the album had no per-track ARTIST/ARTISTSORT tags).
        "track_artists": _parse_track_artists_json(
            row["track_artists_json"] if "track_artists_json" in row.keys() else None
        ),
        # v12 — timestamp of most recent AI metadata job acted_on.
        # NULL when the album has never been AI-reviewed (the most
        # common case until the user starts running metadata jobs).
        "metadata_ai_acted_on_at":
            row["metadata_ai_acted_on_at"]
            if "metadata_ai_acted_on_at" in row.keys() else None,
    }


def _parse_track_artists_json(raw: str | None) -> list[dict[str, str]]:
    """Decode the album row's track_artists_json column. Tolerant of
    nulls, malformed JSON, and pre-v11 rows. Always returns a list of
    {filename, artist, artist_sort} dicts; missing keys default to ''."""
    if not raw:
        return []
    try:
        loaded = json.loads(raw)
    except Exception:
        return []
    if not isinstance(loaded, list):
        return []
    out: list[dict[str, str]] = []
    for item in loaded:
        if not isinstance(item, dict):
            continue
        filename = str(item.get("filename") or "")
        if not filename:
            continue
        out.append({
            "filename":    filename,
            "artist":      str(item.get("artist") or ""),
            "artist_sort": str(item.get("artist_sort") or ""),
        })
    return out


def _parse_track_numbers_json(raw: str | None) -> list[int | None]:
    """Decode the album row's track_numbers_json column. Tolerant of nulls,
    malformed JSON, and pre-v6 rows that have no value yet — all degrade
    to an empty list, which the track-gaps check treats as 'skip me'."""
    if not raw:
        return []
    try:
        loaded = json.loads(raw)
    except Exception:
        return []
    if not isinstance(loaded, list):
        return []
    return [int(x) if isinstance(x, (int, float)) else None for x in loaded]


def _album_to_row(album: dict[str, Any], library_path: str) -> tuple:
    """Build a tuple matching `db.ALBUM_COLUMNS` for INSERT."""
    return (
        album.get("id"),
        library_path,
        album.get("path"),
        album.get("dir_name"),
        album.get("title"),
        album.get("artist"),
        album.get("album_artist"),
        album.get("year"),
        album.get("genre"),
        int(album.get("track_count") or 0),
        float(album.get("total_seconds") or 0.0),
        1 if album.get("has_embedded_cover") else 0,
        album.get("cover_file"),
        album.get("sample_track") or "",
        float(album.get("dir_mtime") or 0.0),
        # v2
        album.get("dir_artist"),
        album.get("dir_album"),
        1 if album.get("is_compilation") else 0,
        album.get("disc_number"),
        album.get("disc_total"),
        # v3
        album.get("sample_rate"),
        album.get("bits_per_sample"),
        album.get("channels"),
        # v6 — track_numbers (Python list) → JSON. None / missing → NULL so a
        # row left over from before this column was scanned can be told apart
        # from "scanned but truly empty" (which would be []).
        json.dumps(album.get("track_numbers")) if album.get("track_numbers") is not None else None,
        # v7 — cached MusicBrainz anchors. The scanner doesn't supply these;
        # they're written by mb_lookup. Pass them through if the caller has
        # a value; the upsert SQL won't actually overwrite them on conflict
        # (only scanner-owned columns are listed in the DO UPDATE clause).
        album.get("mb_release_group_mbid"),
        album.get("mb_release_mbid"),
        album.get("mb_match_score"),
        album.get("mb_last_checked_at"),
        # v8 — Vorbis sort variants. Read from the FLAC's
        # ALBUMARTISTSORT / ARTISTSORT / ALBUMSORT tags. NULL when the
        # source files don't carry them.
        album.get("album_artist_sort"),
        album.get("artist_sort"),
        album.get("album_sort"),
        # v11 — per-track artist credits. JSON-encode the list; NULL
        # when the scanner produced no per-track credits (single-artist
        # albums where no track has an explicit artist tag).
        json.dumps(album.get("track_artists")) if album.get("track_artists") else None,
        # v12 — AI-review timestamp. Pass through whatever the caller
        # provided (the worker maintains this; the scanner doesn't).
        # The on-conflict-do-update clause in save_snapshot deliberately
        # excludes this column so it survives re-scans.
        album.get("metadata_ai_acted_on_at"),
    )


# ---------- Legacy JSON migration ----------

def _maybe_import_legacy_json() -> None:
    """One-shot: if the old JSON cache exists, copy its contents into SQLite
    and rename the file so we never re-import it."""
    if not _LEGACY_JSON.is_file():
        return
    try:
        data = json.loads(_LEGACY_JSON.read_text(encoding="utf-8"))
    except Exception as e:
        log.warning("Could not read legacy cache JSON %s: %s", _LEGACY_JSON, e)
        _move_aside(_LEGACY_JSON)
        return

    libraries = data.get("libraries", {}) or {}
    imported_libs = 0
    imported_albums = 0
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            for library_path, payload in libraries.items():
                resolved = str(Path(library_path).expanduser().resolve())
                # Skip if SQLite already has rows for this library.
                exists = conn.execute(
                    "SELECT 1 FROM albums WHERE library_path = ? LIMIT 1", (resolved,)
                ).fetchone()
                if exists:
                    continue
                conn.execute(
                    "INSERT OR REPLACE INTO libraries(path, scanned_at) VALUES (?, ?)",
                    (resolved, payload.get("scanned_at", "")),
                )
                albums = payload.get("albums", []) or []
                rows = [_album_to_row(a, resolved) for a in albums if a.get("id") and a.get("path")]
                if rows:
                    placeholders = ", ".join("?" for _ in db_module.ALBUM_COLUMNS)
                    cols = ", ".join(db_module.ALBUM_COLUMNS)
                    conn.executemany(
                        f"INSERT OR REPLACE INTO albums ({cols}) VALUES ({placeholders})",
                        rows,
                    )
                imported_libs += 1
                imported_albums += len(rows)

    log.info("Imported %d album(s) from %d librari(es) out of %s into SQLite",
             imported_albums, imported_libs, _LEGACY_JSON)
    _move_aside(_LEGACY_JSON)


def _move_aside(path: Path) -> None:
    """Rename `path` to `path.imported` (or `.imported.N` if that exists)."""
    target = path.with_name(path.name + ".imported")
    n = 1
    while target.exists():
        target = path.with_name(f"{path.name}.imported.{n}")
        n += 1
    try:
        path.rename(target)
    except OSError as e:
        log.warning("Could not move %s aside: %s", path, e)


# ---------- Public API ----------

def load_snapshot(library_dir: str) -> LibrarySnapshot:
    """Return the cached snapshot for `library_dir`, or an empty one."""
    _maybe_import_legacy_json()
    library_dir = str(Path(library_dir).expanduser().resolve())
    snap = LibrarySnapshot(library_dir=library_dir)
    with db_module.connect() as conn:
        row = conn.execute(
            "SELECT scanned_at FROM libraries WHERE path = ?", (library_dir,)
        ).fetchone()
        if row is None:
            return snap
        snap.scanned_at = row["scanned_at"] or ""
        for r in conn.execute(
            "SELECT * FROM albums WHERE library_path = ?", (library_dir,)
        ):
            album = _row_to_album(r)
            snap.albums_by_path[album["path"]] = album
    return snap


def save_snapshot(snapshot: LibrarySnapshot) -> None:
    """Persist a snapshot incrementally — INSERT-or-UPDATE every album in
    the snapshot, then DELETE only albums whose ids dropped out.

    Why incremental matters: track_fingerprints and several scanner-adjacent
    columns on `albums` (mb_release_group_mbid, mb_match_score, …) are
    written by other subsystems (fingerprint.py, mb_lookup.py). The naive
    "DELETE all then re-INSERT" pattern that lived here previously
    cascade-wiped every fingerprint row on every full scan via the FK on
    track_fingerprints, AND clobbered every cached MB anchor by re-inserting
    raw scanner columns. Bug surfaced as "I just fingerprinted X albums
    and the check still flags every one."

    The fix: same on-conflict-do-update-only-scanner-columns pattern that
    upsert_album() uses, scaled to the whole snapshot in one transaction.
    Albums no longer present in the snapshot still get DELETEd (handles
    directories that disappeared since the last scan); their fingerprint
    rows correctly cascade away in that case, which is desired behavior.
    """
    _maybe_import_legacy_json()
    library_dir = str(Path(snapshot.library_dir).expanduser().resolve())
    scanned_at = snapshot.scanned_at or _now_iso()
    cols = ", ".join(db_module.ALBUM_COLUMNS)
    placeholders = ", ".join("?" for _ in db_module.ALBUM_COLUMNS)
    update_clause = ", ".join(
        f"{c} = excluded.{c}" for c in _SCANNER_OWNED_COLUMNS
    )
    rows = [_album_to_row(a, library_dir) for a in snapshot.albums_by_path.values()
            if a.get("id") and a.get("path")]
    new_ids = {r[0] for r in rows}  # ALBUM_COLUMNS[0] == "id"

    with db_module.connect() as conn:
        with db_module.transaction(conn):
            conn.execute(
                "INSERT INTO libraries(path, scanned_at) VALUES (?, ?) "
                "ON CONFLICT(path) DO UPDATE SET scanned_at = excluded.scanned_at",
                (library_dir, scanned_at),
            )
            if rows:
                conn.executemany(
                    f"INSERT INTO albums ({cols}) VALUES ({placeholders}) "
                    f"ON CONFLICT(id) DO UPDATE SET {update_clause}",
                    rows,
                )
            # Drop any albums that used to live under this library_path but
            # didn't appear in this snapshot. Keeps the cache tidy and
            # cascades fingerprint cleanup for genuinely-removed albums.
            existing = {
                r["id"] for r in conn.execute(
                    "SELECT id FROM albums WHERE library_path = ?",
                    (library_dir,),
                )
            }
            stale = existing - new_ids
            if stale:
                conn.executemany(
                    "DELETE FROM albums WHERE id = ?",
                    [(aid,) for aid in stale],
                )


def drop_snapshot(library_dir: str) -> None:
    """Invalidate the cached snapshot for `library_dir` so the next
    scan re-reads every album from disk. NON-DESTRUCTIVE: preserves
    out-of-band data that other subsystems wrote to album rows.

    Why we can't just DELETE the library row (the obvious fix):
    `albums.library_path` has FK ON DELETE CASCADE, and several
    out-of-band columns / tables hang off the album row:
      - mb_release_group_mbid + friends (paid AI/MB lookup output)
      - track_fingerprints (Chromaprint compute, FK CASCADE)
      - track_tags (user-curated tag taxonomy, FK CASCADE — the
        sidecar JSON survives, but the DB rows would still flap)
    A naive DELETE chained through the cascade and silently nuked
    all of it. Recovery for MBIDs / fingerprints means re-running
    expensive AI / fingerprint passes; recovery for tags means
    re-importing every sidecar (and possibly losing recently-applied
    tags that hadn't yet been written to disk).

    Instead we zero out `dir_mtime` on every album row in the
    library. The scanner's incremental cache check compares
    `cached.dir_mtime` against the live filesystem mtime and treats
    any mismatch as "needs fresh re-read" — bumping every cached
    row to 0 forces every album to scan-from-disk on the next pass.
    save_snapshot's INSERT-ON-CONFLICT clause only updates
    scanner-owned columns, so MBIDs / fingerprints / tags survive
    the rebuild.
    """
    _maybe_import_legacy_json()
    library_dir = str(Path(library_dir).expanduser().resolve())
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            # Reset scanned_at so the library list shows "never scanned"
            # if the user lands on the page before the next scan starts.
            conn.execute(
                "UPDATE libraries SET scanned_at = '' WHERE path = ?",
                (library_dir,),
            )
            # Zero every album's mtime so the cache check fails and
            # the scanner re-reads. Scanner-owned columns get
            # overwritten in-place via upsert; out-of-band columns
            # (MBIDs, etc.) and FK-linked tables (fingerprints,
            # track_tags) are left intact.
            conn.execute(
                "UPDATE albums SET dir_mtime = 0 WHERE library_path = ?",
                (library_dir,),
            )


# Columns that the *scanner* knows how to populate. Anything outside this
# set (e.g. cached MBIDs from mb_lookup, AcoustID hits) is owned by other
# subsystems and must be preserved across re-scans. The upsert below uses
# this list as the only fields to UPDATE on conflict — everything else
# stays at whatever value its respective owner last wrote.
_SCANNER_OWNED_COLUMNS = (
    "library_path", "path", "dir_name", "title", "artist", "album_artist",
    "year", "genre", "track_count", "total_seconds", "has_embedded_cover",
    "cover_file", "sample_track", "dir_mtime",
    "dir_artist", "dir_album", "is_compilation", "disc_number", "disc_total",
    "sample_rate", "bits_per_sample", "channels", "track_numbers_json",
    # v8 sort variants. Same lifecycle as title/artist: the scanner reads
    # them from FLAC tags, the apply pipeline writes them to FLAC tags,
    # and the next scan reflects whatever's currently on disk. So they
    # belong with the rest of the scanner-owned set.
    "album_artist_sort", "artist_sort", "album_sort",
    # v11 per-track credits. Same scanner-owned lifecycle.
    "track_artists_json",
)


def upsert_album(library_dir: str, album: dict[str, Any]) -> None:
    """Insert or update a single album row.

    Used after a single-album mutation (e.g. metadata apply) to refresh just
    that row instead of dropping the whole library snapshot. This is what
    fixes the "library re-scans after every apply" flicker bug.

    Important: only updates scanner-owned columns on conflict. Cached
    MusicBrainz anchors (mb_release_group_mbid, etc.) and any other
    out-of-band data live on the same row but were written by a different
    subsystem; clobbering them on every scan would defeat the cache.
    """
    _maybe_import_legacy_json()
    library_dir = str(Path(library_dir).expanduser().resolve())
    cols = ", ".join(db_module.ALBUM_COLUMNS)
    placeholders = ", ".join("?" for _ in db_module.ALBUM_COLUMNS)
    update_clause = ", ".join(
        f"{c} = excluded.{c}" for c in _SCANNER_OWNED_COLUMNS
    )
    row = _album_to_row(album, library_dir)
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            # Make sure the parent library row exists (FK requirement).
            conn.execute(
                "INSERT INTO libraries(path, scanned_at) VALUES (?, ?) "
                "ON CONFLICT(path) DO NOTHING",
                (library_dir, _now_iso()),
            )
            conn.execute(
                f"INSERT INTO albums ({cols}) VALUES ({placeholders}) "
                f"ON CONFLICT(id) DO UPDATE SET {update_clause}",
                row,
            )


def get_album_by_id(album_id: str) -> dict[str, Any] | None:
    """Return the merged album dict (scanner-owned + cached MB fields)
    for a single album. None when the row isn't in the cache.

    Use after `upsert_album` when the caller needs the post-upsert
    state — the upsert only writes scanner-owned columns, so the
    in-hand dict won't reflect cached fields like
    `mb_release_group_mbid` set by other subsystems. This helper goes
    back to the DB and gives you the merged truth.
    """
    with db_module.connect() as conn:
        row = conn.execute(
            f"SELECT {', '.join(db_module.ALBUM_COLUMNS)} FROM albums WHERE id = ?",
            (album_id,),
        ).fetchone()
    return _row_to_album(row) if row else None


def delete_album(album_id: str) -> None:
    """Remove a single album row by id (e.g. after the directory disappears)."""
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            conn.execute("DELETE FROM albums WHERE id = ?", (album_id,))


def rename_album_directory(
    *,
    old_id: str,
    new_id: str,
    new_path: str,
    new_dir_name: str,
) -> dict[str, int]:
    """Atomically migrate an album row to a new id + path + dir_name,
    AND update every other table that references the album_id.

    The album's id is sha1(album_path), so renaming the directory on
    disk implies a new id. Several tables reference album_id:

      - track_fingerprints (FK CASCADE on album_id)
      - track_tags (FK CASCADE on album_id)
      - check_findings (album_id column, no FK)
      - ai_jobs (album_id column, no FK)

    We update them all inside a single transaction with
    `defer_foreign_keys = TRUE` so the FK constraints are checked at
    commit rather than per-statement — that lets us update the
    parent (`albums.id`) before the children, in one transaction,
    without violating intermediate state.

    Returns a dict of per-table row counts so the caller can log /
    surface what moved. Raises sqlite3.Error on failure (caller is
    responsible for catching + rolling back the on-disk rename if
    the DB transaction fails).
    """
    out: dict[str, int] = {}
    with db_module.connect() as conn:
        # `defer_foreign_keys` is per-transaction; turn it on inside
        # the transaction we're about to begin.
        with db_module.transaction(conn):
            conn.execute("PRAGMA defer_foreign_keys = ON")
            # Update the parent first. The deferred FK lets the
            # children below catch up before commit.
            r = conn.execute(
                "UPDATE albums SET id = ?, path = ?, dir_name = ? "
                "WHERE id = ?",
                (new_id, new_path, new_dir_name, old_id),
            )
            out["albums"] = r.rowcount
            r = conn.execute(
                "UPDATE track_fingerprints SET album_id = ? WHERE album_id = ?",
                (new_id, old_id),
            )
            out["track_fingerprints"] = r.rowcount
            r = conn.execute(
                "UPDATE track_tags SET album_id = ? WHERE album_id = ?",
                (new_id, old_id),
            )
            out["track_tags"] = r.rowcount
            # check_findings + ai_jobs reference album_id without FK,
            # so they'd just keep pointing at the old id (which now
            # has no row) without this migration. Update them so
            # findings stay attached to the right album.
            r = conn.execute(
                "UPDATE check_findings SET album_id = ? WHERE album_id = ?",
                (new_id, old_id),
            )
            out["check_findings"] = r.rowcount
            r = conn.execute(
                "UPDATE ai_jobs SET album_id = ? WHERE album_id = ?",
                (new_id, old_id),
            )
            out["ai_jobs"] = r.rowcount
    return out


# ---------- MusicBrainz anchor cache (v7) ----------

def get_album_mb_anchor(album_id: str) -> dict[str, Any] | None:
    """Return the cached MB anchor for `album_id`, or None if nothing's
    been written yet. Shape: {release_group_mbid, release_mbid, score,
    last_checked_at}.

    Used by mb_lookup.find_release_cached() to short-circuit a fresh MB
    search when we already know the album's identity."""
    with db_module.connect() as conn:
        row = conn.execute(
            "SELECT mb_release_group_mbid, mb_release_mbid, mb_match_score, "
            "       mb_last_checked_at "
            "FROM albums WHERE id = ?",
            (album_id,),
        ).fetchone()
    if row is None or row["mb_release_group_mbid"] is None:
        return None
    return {
        "release_group_mbid": row["mb_release_group_mbid"],
        "release_mbid":       row["mb_release_mbid"],
        "score":              row["mb_match_score"],
        "last_checked_at":    row["mb_last_checked_at"],
    }


def set_album_mb_anchor(
    album_id: str,
    *,
    release_group_mbid: str,
    release_mbid: str | None,
    score: int,
) -> None:
    """Persist the MB match for an album. Bumps last_checked_at to now.
    Called by mb_lookup after a confident find_release() result."""
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            conn.execute(
                "UPDATE albums SET mb_release_group_mbid = ?, mb_release_mbid = ?, "
                "mb_match_score = ?, mb_last_checked_at = ? WHERE id = ?",
                (release_group_mbid, release_mbid, int(score), _now_iso(), album_id),
            )


def add_blocked_mb_release_group(album_id: str, release_group_mbid: str) -> None:
    """Add an MB release-group MBID to this album's blocklist.

    The resolver's Tier 0b (dir search) and Tier 0c (fingerprint vote)
    skip blocked MBIDs, and AI-MBID persistence refuses them. Used by
    the "Wrong match" UI affordance — without it, fingerprint vote
    would re-derive the same wrong release-group from cached AcoustID
    matches the moment the user cleared the anchor, putting them right
    back in the same loop.
    """
    if not release_group_mbid:
        return
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            conn.execute(
                "INSERT OR IGNORE INTO album_blocked_mb_release_groups "
                "(album_id, release_group_mbid, blocked_at) VALUES (?, ?, ?)",
                (album_id, release_group_mbid, _now_iso()),
            )


def get_blocked_mb_release_groups(album_id: str) -> set[str]:
    """Return the set of MB release-group MBIDs blocked for this album.

    Empty set when the album has no blocks (the common case). Cheap —
    indexed lookup on a small per-album partition.
    """
    with db_module.connect() as conn:
        rows = conn.execute(
            "SELECT release_group_mbid FROM album_blocked_mb_release_groups "
            "WHERE album_id = ?",
            (album_id,),
        ).fetchall()
    return {r["release_group_mbid"] for r in rows}


def clear_album_mb_anchor(album_id: str) -> None:
    """Wipe the cached MusicBrainz anchor for an album.

    Used by the "wrong match" escape hatch in the UI: the resolver
    short-circuits on cache hit (Tier 0a), so a wrong anchor can lock
    the user out of the AI re-search path until the cache is cleared.
    Resets every column the anchor populates so the resolver starts
    completely fresh on the next call.

    Caller is responsible for invalidating the resolver's in-process
    memoization (resolver_service.invalidate(album_id)).
    """
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            conn.execute(
                "UPDATE albums SET mb_release_group_mbid = NULL, "
                "mb_release_mbid = NULL, mb_match_score = NULL, "
                "mb_last_checked_at = NULL WHERE id = ?",
                (album_id,),
            )


def record_mb_lookup_failure(album_id: str) -> None:
    """Mark an album as "MB lookup attempted but didn't yield an MBID."

    Used by the AI MBID flow when AI reports it couldn't identify the
    album on MusicBrainz. Bumps mb_last_checked_at without setting
    mb_release_group_mbid — the (mbid IS NULL, last_checked_at IS NOT
    NULL) shape becomes the "tried, no match" sentinel.

    The missing-mbid check skips albums in this state until the
    staleness window expires, so the user doesn't keep re-running AI
    on the same un-identifiable albums every time the check runs.
    """
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            conn.execute(
                "UPDATE albums SET mb_last_checked_at = ? "
                "WHERE id = ? AND mb_release_group_mbid IS NULL",
                (_now_iso(), album_id),
            )


def mark_metadata_ai_acted_on(album_id: str) -> None:
    """Bump metadata_ai_acted_on_at to now() for `album_id`. Called
    when a metadata or metadata+cover AI job transitions to acted_on
    (either applied or discarded — both count as the user having
    reviewed AI's suggestions for this album).

    Idempotent: re-running just bumps the timestamp forward."""
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            conn.execute(
                "UPDATE albums SET metadata_ai_acted_on_at = ? WHERE id = ?",
                (_now_iso(), album_id),
            )


# ---------- Per-track fingerprint cache (v7) ----------

def get_track_fingerprints(album_id: str) -> list[dict[str, Any]]:
    """Return all cached fingerprints for an album, ordered by filename.
    Empty list when the album hasn't been fingerprinted yet."""
    with db_module.connect() as conn:
        rows = conn.execute(
            "SELECT filename, fingerprint, duration_s, "
            "       acoustid_recording_mbid, acoustid_score, last_computed_at "
            "FROM track_fingerprints WHERE album_id = ? "
            "ORDER BY filename",
            (album_id,),
        ).fetchall()
    return [
        {
            "filename":                row["filename"],
            "fingerprint":             row["fingerprint"],
            "duration_s":              row["duration_s"],
            "acoustid_recording_mbid": row["acoustid_recording_mbid"],
            "acoustid_score":          row["acoustid_score"],
            "last_computed_at":        row["last_computed_at"],
        }
        for row in rows
    ]


def upsert_track_fingerprint(
    album_id: str,
    *,
    filename: str,
    fingerprint: str,
    duration_s: float,
    acoustid_recording_mbid: str | None,
    acoustid_score: float | None,
) -> None:
    """Insert or refresh a per-track fingerprint row. Called per-file by
    fingerprint.fingerprint_album() — usually inside a single thread, so
    we don't bother batching."""
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            conn.execute(
                "INSERT INTO track_fingerprints "
                "(album_id, filename, fingerprint, duration_s, "
                " acoustid_recording_mbid, acoustid_score, last_computed_at) "
                "VALUES (?, ?, ?, ?, ?, ?, ?) "
                "ON CONFLICT(album_id, filename) DO UPDATE SET "
                " fingerprint = excluded.fingerprint, "
                " duration_s = excluded.duration_s, "
                " acoustid_recording_mbid = excluded.acoustid_recording_mbid, "
                " acoustid_score = excluded.acoustid_score, "
                " last_computed_at = excluded.last_computed_at",
                (album_id, filename, fingerprint, float(duration_s),
                 acoustid_recording_mbid,
                 float(acoustid_score) if acoustid_score is not None else None,
                 _now_iso()),
            )


def delete_track_fingerprints(album_id: str) -> int:
    """Drop all fingerprint rows for an album. Returns rows removed."""
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            cur = conn.execute(
                "DELETE FROM track_fingerprints WHERE album_id = ?",
                (album_id,),
            )
            return cur.rowcount


def delete_track_fingerprint(album_id: str, filename: str) -> int:
    """Drop a single fingerprint row for an album/filename pair. Returns
    rows removed (0 or 1).

    Called from the duplicate-tracks delete-file endpoint after the FLAC
    is unlinked from disk — keeps the duplicate-tracks check from
    re-flagging the same cluster on the next run."""
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            cur = conn.execute(
                "DELETE FROM track_fingerprints WHERE album_id = ? AND filename = ?",
                (album_id, filename),
            )
            return cur.rowcount


# ---------- Per-track user tags (v9) ----------
#
# These are the user's *own* taxonomy ("Cocktail Hour", "Christmas",
# "Instrumental") — not the FLAC GENRE field. Stored per-track so the
# user can mark just one song on an album (e.g. "Greensleeves" on a
# non-Christmas record). Album-level UI computes the intersection: a
# tag shows up at the album level only when every track on the album
# carries it.
#
# Source of truth: this DB. The album-dir sidecar JSON
# (.media-manager-tags.json) is a persistence layer — written on every
# mutation, read on first scan when the DB has no rows for the album.
# Once a session has touched the DB, DB wins.

def add_track_tag(album_id: str, filename: str, tag: str) -> bool:
    """Apply `tag` to a single track. Returns True if a new row was
    inserted, False if the row already existed (idempotent). The tag
    string is stored as-is (caller is responsible for casing /
    canonicalization decisions); uniqueness is case-sensitive."""
    tag = (tag or "").strip()
    if not tag:
        return False
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            cur = conn.execute(
                "INSERT OR IGNORE INTO track_tags "
                "(album_id, filename, tag, created_at) VALUES (?, ?, ?, ?)",
                (album_id, filename, tag, _now_iso()),
            )
            return cur.rowcount > 0


def remove_track_tag(album_id: str, filename: str, tag: str) -> bool:
    """Remove `tag` from a single track. Returns True if a row was
    deleted, False if the row wasn't there to begin with."""
    tag = (tag or "").strip()
    if not tag:
        return False
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            cur = conn.execute(
                "DELETE FROM track_tags WHERE album_id = ? AND filename = ? AND tag = ?",
                (album_id, filename, tag),
            )
            return cur.rowcount > 0


def get_track_tags(album_id: str, filename: str) -> list[str]:
    """All tags applied to a single track, sorted alphabetically.
    Empty list when the track hasn't been tagged."""
    with db_module.connect() as conn:
        rows = conn.execute(
            "SELECT tag FROM track_tags WHERE album_id = ? AND filename = ? "
            "ORDER BY tag",
            (album_id, filename),
        ).fetchall()
    return [r["tag"] for r in rows]


def get_album_track_tags(album_id: str) -> dict[str, list[str]]:
    """All tags grouped by filename. Returns
    `{filename: [tag, ...], ...}` for tracks that have at least one
    tag; tracks with no tags are absent (callers can default to []).

    Used by the per-album tag editor and by sidecar serialization."""
    with db_module.connect() as conn:
        rows = conn.execute(
            "SELECT filename, tag FROM track_tags WHERE album_id = ? "
            "ORDER BY filename, tag",
            (album_id,),
        ).fetchall()
    out: dict[str, list[str]] = {}
    for r in rows:
        out.setdefault(r["filename"], []).append(r["tag"])
    return out


def get_album_tag_intersection(album_id: str, total_track_count: int) -> list[str]:
    """Tags that appear on *every* track of the album — the album-level
    chip set. Caller passes `total_track_count` (typically the album
    row's track_count) because we don't trust the tag table alone to
    know how many files exist on disk: a tag carried by every row in
    track_tags but only present on 3 out of 12 tracks shouldn't be
    treated as an album-wide tag.

    Tags are returned sorted alphabetically. Empty when the album has
    no tagged tracks or no fully-shared tag."""
    if total_track_count <= 0:
        return []
    with db_module.connect() as conn:
        rows = conn.execute(
            "SELECT tag, COUNT(DISTINCT filename) AS n "
            "FROM track_tags WHERE album_id = ? "
            "GROUP BY tag "
            "HAVING n >= ? "
            "ORDER BY tag",
            (album_id, total_track_count),
        ).fetchall()
    return [r["tag"] for r in rows]


def find_tracks_with_tag(tag: str) -> list[dict[str, Any]]:
    """All (album_id, filename) pairs carrying `tag`. The playlist-
    construction query — every track tagged "Cocktail Hour" forms the
    candidate pool for that playlist."""
    tag = (tag or "").strip()
    if not tag:
        return []
    with db_module.connect() as conn:
        rows = conn.execute(
            "SELECT album_id, filename FROM track_tags WHERE tag = ? "
            "ORDER BY album_id, filename",
            (tag,),
        ).fetchall()
    return [{"album_id": r["album_id"], "filename": r["filename"]} for r in rows]


def list_all_tags() -> list[dict[str, Any]]:
    """Every tag the user has ever applied, with usage counts. Powers
    the tag-suggestion autocomplete in the UI. Sorted by descending
    usage so frequently-used tags come first."""
    with db_module.connect() as conn:
        rows = conn.execute(
            "SELECT tag, COUNT(*) AS n FROM track_tags "
            "GROUP BY tag ORDER BY n DESC, tag ASC"
        ).fetchall()
    return [{"tag": r["tag"], "count": r["n"]} for r in rows]


def add_album_tag(album_id: str, tag: str, filenames: list[str]) -> int:
    """Apply `tag` to every track in `filenames`. Returns the number of
    new rows inserted (existing rows count 0 — already-tagged tracks
    are silently skipped). Caller supplies the filename list because
    the cache module doesn't own the album/track relationship."""
    tag = (tag or "").strip()
    if not tag or not filenames:
        return 0
    now = _now_iso()
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            inserted = 0
            for fn in filenames:
                cur = conn.execute(
                    "INSERT OR IGNORE INTO track_tags "
                    "(album_id, filename, tag, created_at) VALUES (?, ?, ?, ?)",
                    (album_id, fn, tag, now),
                )
                inserted += cur.rowcount
            return inserted


def remove_album_tag(album_id: str, tag: str) -> int:
    """Strip `tag` from every track on the album. Returns rows deleted."""
    tag = (tag or "").strip()
    if not tag:
        return 0
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            cur = conn.execute(
                "DELETE FROM track_tags WHERE album_id = ? AND tag = ?",
                (album_id, tag),
            )
            return cur.rowcount


def rename_track_filename(album_id: str, old_filename: str, new_filename: str) -> int:
    """Migrate tag rows when a track's filename changes (e.g. an
    apply-metadata step renames `01 - foo.flac` to `01 Foo.flac`).
    Returns rows updated. Idempotent: safe to call when there are no
    tag rows for `old_filename`.

    Conflict handling: if rows already exist at the destination (the
    new filename already had tags), we OR the sets together using
    INSERT OR IGNORE then DELETE the old rows. That way the rename
    never crashes and never silently drops tags."""
    if not old_filename or not new_filename or old_filename == new_filename:
        return 0
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            # Copy any tags from old → new that aren't already on new.
            conn.execute(
                "INSERT OR IGNORE INTO track_tags "
                "(album_id, filename, tag, created_at) "
                "SELECT album_id, ?, tag, created_at "
                "FROM track_tags "
                "WHERE album_id = ? AND filename = ?",
                (new_filename, album_id, old_filename),
            )
            cur = conn.execute(
                "DELETE FROM track_tags WHERE album_id = ? AND filename = ?",
                (album_id, old_filename),
            )
            return cur.rowcount


def album_has_track_tags(album_id: str) -> bool:
    """True if any tag rows exist for the album. Used by the sidecar
    importer to decide whether to import (no rows) or trust DB (rows
    present)."""
    with db_module.connect() as conn:
        row = conn.execute(
            "SELECT 1 FROM track_tags WHERE album_id = ? LIMIT 1", (album_id,)
        ).fetchone()
    return row is not None


def replace_album_track_tags(
    album_id: str,
    track_tags: dict[str, list[str]],
) -> None:
    """Replace the album's tag rows wholesale with the given
    `{filename: [tag, ...]}` mapping. Used by the sidecar importer.

    Empty filenames or tag strings are skipped. created_at on every
    new row is set to now; the sidecar doesn't preserve creation
    timestamps."""
    now = _now_iso()
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            conn.execute("DELETE FROM track_tags WHERE album_id = ?", (album_id,))
            for filename, tags in track_tags.items():
                if not filename:
                    continue
                for tag in tags or []:
                    tag = (tag or "").strip()
                    if not tag:
                        continue
                    conn.execute(
                        "INSERT OR IGNORE INTO track_tags "
                        "(album_id, filename, tag, created_at) "
                        "VALUES (?, ?, ?, ?)",
                        (album_id, filename, tag, now),
                    )


# ---------- User-curated tag vocabulary (v10) ----------
#
# Approved list of tags the user picks from in the album/track tag
# editors. The picker UI is fed exclusively from this table; track_tags
# rows that don't have a matching vocabulary entry (e.g. legacy
# free-typed tags, sidecar imports from before this feature) surface
# as "orphans" so the user can promote, rename, or strip them.
#
# The two tables intentionally aren't FK-linked. Reasons:
#   - Sidecar import on first scan must succeed even when the
#     vocabulary is empty.
#   - Orphan detection is a useful product state, not an error.
#   - Vocabulary edits (rename / merge / delete) need explicit
#     cascade semantics, not the implicit ON DELETE CASCADE behavior
#     a FK would force.

def list_vocabulary() -> list[dict[str, Any]]:
    """All vocabulary entries with usage counts. Sorted by `sort_order`
    then `tag`. The count is computed from track_tags via a
    case-insensitive match on the tag string — vocabulary is COLLATE
    NOCASE, track_tags is BINARY, so we lower() in Python rather than
    rely on cross-collation joins."""
    with db_module.connect() as conn:
        vocab_rows = conn.execute(
            "SELECT tag, description, color, sort_order, created_at "
            "FROM tag_vocabulary ORDER BY sort_order ASC, tag ASC"
        ).fetchall()
        usage_rows = conn.execute(
            "SELECT tag, COUNT(*) AS n FROM track_tags GROUP BY tag"
        ).fetchall()
    counts: dict[str, int] = {}
    for r in usage_rows:
        # Match case-insensitively to the vocabulary's casing.
        counts[(r["tag"] or "").lower()] = counts.get((r["tag"] or "").lower(), 0) + r["n"]
    return [
        {
            "tag":         r["tag"],
            "description": r["description"] or "",
            "color":       r["color"],
            "sort_order":  r["sort_order"],
            "created_at":  r["created_at"],
            "count":       counts.get((r["tag"] or "").lower(), 0),
        }
        for r in vocab_rows
    ]


def add_vocabulary_entry(
    tag: str,
    *,
    description: str = "",
    color: str | None = None,
    sort_order: int | None = None,
) -> bool:
    """Insert a vocabulary entry. Returns True if a new row landed,
    False if a row already existed (case-insensitive). Caller should
    treat False as "tag is already approved" — not an error.

    `sort_order` defaults to (max+1) so newly-added tags land at the
    end of the picker list."""
    tag = (tag or "").strip()
    if not tag:
        return False
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            if sort_order is None:
                row = conn.execute(
                    "SELECT COALESCE(MAX(sort_order), -1) + 1 AS n FROM tag_vocabulary"
                ).fetchone()
                sort_order = int(row["n"])
            cur = conn.execute(
                "INSERT OR IGNORE INTO tag_vocabulary "
                "(tag, description, color, sort_order, created_at) "
                "VALUES (?, ?, ?, ?, ?)",
                (tag, description or "", color, int(sort_order), _now_iso()),
            )
            return cur.rowcount > 0


def update_vocabulary_entry(
    tag: str,
    *,
    description: str | None = None,
    color: str | None = None,
    sort_order: int | None = None,
) -> bool:
    """Patch one or more fields. Tag identity stays put — use
    `rename_vocabulary_entry` to change the tag string. Returns True
    if a row was updated."""
    sets: list[str] = []
    args: list[Any] = []
    if description is not None:
        sets.append("description = ?")
        args.append(description)
    if color is not None:
        # Empty string clears.
        sets.append("color = ?")
        args.append(color or None)
    if sort_order is not None:
        sets.append("sort_order = ?")
        args.append(int(sort_order))
    if not sets:
        return False
    args.append(tag)
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            cur = conn.execute(
                f"UPDATE tag_vocabulary SET {', '.join(sets)} WHERE tag = ?",
                args,
            )
            return cur.rowcount > 0


def rename_vocabulary_entry(old_tag: str, new_tag: str) -> dict[str, Any]:
    """Rename a vocabulary entry AND every track_tags row carrying it.
    Returns `{"vocab": True/False, "tag_rows": int, "albums": [ids]}`
    so the caller can resync sidecars for the affected albums.

    If `new_tag` already exists in the vocabulary (case-insensitive),
    the rename is rejected — the caller should use `merge_vocabulary_entries`
    instead. If new_tag exists in track_tags rows of the same album as
    a row being renamed, the conflict is unioned (DISTINCT in the
    PK protects against duplicates)."""
    old_tag = (old_tag or "").strip()
    new_tag = (new_tag or "").strip()
    if not old_tag or not new_tag:
        raise ValueError("old_tag and new_tag are required")
    if old_tag.lower() == new_tag.lower():
        # Pure case change — let it through.
        pass
    affected_albums: set[str] = set()
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            # Reject if new_tag already exists as a different vocabulary
            # entry (case-insensitive). Pure-case rename of the same tag
            # is allowed.
            existing = conn.execute(
                "SELECT tag FROM tag_vocabulary WHERE tag = ? AND tag != ?",
                (new_tag, old_tag),
            ).fetchone()
            if existing is not None:
                raise ValueError(
                    f"vocabulary entry {new_tag!r} already exists; use merge instead"
                )
            # Collect affected albums BEFORE we mutate, so the caller
            # has the right list to resync sidecars from. Match
            # case-insensitively because the vocabulary uses COLLATE
            # NOCASE — "Cocktail Hour" and "cocktail hour" represent
            # the same tag conceptually, even if free-typed track
            # rows carry different casing.
            for r in conn.execute(
                "SELECT DISTINCT album_id FROM track_tags "
                "WHERE LOWER(tag) = LOWER(?)",
                (old_tag,),
            ):
                affected_albums.add(r["album_id"])
            # Insert union-style first (so destinations that already
            # have new_tag don't blow up on PK conflict), then delete
            # the old rows. Both sides match LOWER(tag) so legacy
            # case-variant rows ("cocktail hour") get renamed too.
            conn.execute(
                "INSERT OR IGNORE INTO track_tags "
                "(album_id, filename, tag, created_at) "
                "SELECT album_id, filename, ?, created_at "
                "FROM track_tags WHERE LOWER(tag) = LOWER(?)",
                (new_tag, old_tag),
            )
            tag_rows = conn.execute(
                "DELETE FROM track_tags WHERE LOWER(tag) = LOWER(?)",
                (old_tag,),
            ).rowcount
            # Vocabulary row: UPDATE in place (preserves description /
            # color / sort_order / created_at).
            vocab_updated = conn.execute(
                "UPDATE tag_vocabulary SET tag = ? WHERE tag = ?",
                (new_tag, old_tag),
            ).rowcount > 0
    return {
        "vocab": vocab_updated,
        "tag_rows": tag_rows,
        "albums": sorted(affected_albums),
    }


def delete_vocabulary_entry(tag: str, *, strip_from_tracks: bool = False) -> dict[str, Any]:
    """Remove a vocabulary entry. By default leaves track_tags rows
    intact — they'll surface as orphans. With `strip_from_tracks=True`,
    also deletes every track_tags row carrying that tag.

    Returns `{"vocab": bool, "tag_rows": int, "albums": [ids]}`."""
    tag = (tag or "").strip()
    if not tag:
        raise ValueError("tag is required")
    affected_albums: set[str] = set()
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            if strip_from_tracks:
                # Strip every case variant of the tag — vocabulary
                # is NOCASE, so "Christmas" and "christmas" should
                # both be cleared when the user deletes-with-strip.
                for r in conn.execute(
                    "SELECT DISTINCT album_id FROM track_tags "
                    "WHERE LOWER(tag) = LOWER(?)",
                    (tag,),
                ):
                    affected_albums.add(r["album_id"])
                tag_rows = conn.execute(
                    "DELETE FROM track_tags WHERE LOWER(tag) = LOWER(?)", (tag,),
                ).rowcount
            else:
                tag_rows = 0
            vocab_deleted = conn.execute(
                "DELETE FROM tag_vocabulary WHERE tag = ?", (tag,),
            ).rowcount > 0
    return {
        "vocab": vocab_deleted,
        "tag_rows": tag_rows,
        "albums": sorted(affected_albums),
    }


def merge_vocabulary_entries(from_tag: str, into_tag: str) -> dict[str, Any]:
    """Move every track_tags row from `from_tag` onto `into_tag`,
    then delete the `from_tag` vocabulary entry. The destination
    vocabulary entry must already exist.

    Returns `{"moved": int, "albums": [ids]}` — the number of track
    rows actually inserted at the destination (excludes union-merges
    where the destination already had the row), and the albums whose
    sidecars need resyncing."""
    from_tag = (from_tag or "").strip()
    into_tag = (into_tag or "").strip()
    if not from_tag or not into_tag:
        raise ValueError("from_tag and into_tag are required")
    if from_tag.lower() == into_tag.lower():
        raise ValueError("from_tag and into_tag must differ")
    affected_albums: set[str] = set()
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            into_exists = conn.execute(
                "SELECT 1 FROM tag_vocabulary WHERE tag = ?", (into_tag,),
            ).fetchone()
            if into_exists is None:
                raise ValueError(
                    f"merge target {into_tag!r} does not exist in vocabulary",
                )
            # Match case-insensitively for the same reason the rename
            # path does — "cocktail hour" should follow "Cocktail Hour"
            # when merging, regardless of casing on the track row.
            for r in conn.execute(
                "SELECT DISTINCT album_id FROM track_tags "
                "WHERE LOWER(tag) = LOWER(?)",
                (from_tag,),
            ):
                affected_albums.add(r["album_id"])
            cur = conn.execute(
                "INSERT OR IGNORE INTO track_tags "
                "(album_id, filename, tag, created_at) "
                "SELECT album_id, filename, ?, created_at "
                "FROM track_tags WHERE LOWER(tag) = LOWER(?)",
                (into_tag, from_tag),
            )
            moved = cur.rowcount
            conn.execute(
                "DELETE FROM track_tags WHERE LOWER(tag) = LOWER(?)", (from_tag,),
            )
            conn.execute("DELETE FROM tag_vocabulary WHERE tag = ?", (from_tag,))
    return {
        "moved": moved,
        "albums": sorted(affected_albums),
    }


def find_orphan_tags() -> list[dict[str, Any]]:
    """Tags that exist in track_tags but aren't in tag_vocabulary
    (case-insensitive comparison, since vocabulary uses NOCASE).
    Sorted by descending usage. Empty list when every tag is
    represented in the vocabulary.

    The Settings page renders these as a "tags found in your library
    that aren't approved yet" subsection so the user can promote /
    rename / strip them in bulk."""
    with db_module.connect() as conn:
        vocab = {
            r["tag"].lower()
            for r in conn.execute("SELECT tag FROM tag_vocabulary")
        }
        rows = conn.execute(
            "SELECT tag, COUNT(*) AS n FROM track_tags GROUP BY tag"
        ).fetchall()
    orphans: list[dict[str, Any]] = []
    for r in rows:
        if (r["tag"] or "").lower() not in vocab:
            orphans.append({"tag": r["tag"], "count": r["n"]})
    orphans.sort(key=lambda x: (-x["count"], x["tag"].lower()))
    return orphans


def get_albums_for_resync(album_ids: list[str]) -> list[dict[str, str]]:
    """Look up `(id, path)` pairs for a list of album_ids — used by
    the API after a vocabulary mutation to drive sidecar resyncs.
    Caller iterates and writes each album's current track_tags out
    to its sidecar via track_tags_sidecar.write_sidecar."""
    if not album_ids:
        return []
    with db_module.connect() as conn:
        placeholders = ", ".join("?" for _ in album_ids)
        rows = conn.execute(
            f"SELECT id, path FROM albums WHERE id IN ({placeholders})",
            album_ids,
        ).fetchall()
    return [{"id": r["id"], "path": r["path"]} for r in rows]


def import_track_tag_sidecars(snapshot: LibrarySnapshot) -> int:
    """Import per-album tag sidecars for any album in the snapshot that
    doesn't yet have track_tags rows in the DB. Returns the number of
    albums whose sidecar was imported (not the number of tag rows).

    The DB-empty gate is what makes this safe to call on every scan:
    once any session has touched the DB, this is a no-op for that
    album. The sidecar is the source of truth only on first scan or
    after a DB reset.

    Best-effort: any per-album error is logged and skipped — the rest
    of the snapshot still gets imported. Don't let one bad sidecar
    block the whole scan.
    """
    # Imported lazily so the rest of cache.py stays free of the sidecar
    # module's dependencies.
    from . import track_tags_sidecar
    imported = 0
    for album in snapshot.albums_by_path.values():
        album_id = album.get("id")
        album_path = album.get("path")
        if not album_id or not album_path:
            continue
        try:
            if album_has_track_tags(album_id):
                continue
            sidecar = track_tags_sidecar.read_sidecar(album_path)
            if not sidecar:
                continue
            replace_album_track_tags(album_id, sidecar)
            imported += 1
            log.info(
                "Imported %d track tag entr(ies) from sidecar for album %s",
                len(sidecar), album_id,
            )
        except Exception:
            log.exception(
                "Failed to import tag sidecar for album %s", album_id,
            )
    return imported


def get_dir_mtime(path: str) -> float:
    """Return the directory's mtime, or 0.0 if it can't be stat'd. Kept here
    for back-compat — used by the scanner."""
    try:
        return os.stat(path).st_mtime
    except OSError:
        return 0.0


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")
