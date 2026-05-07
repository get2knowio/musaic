"""One-off diagnostic for the "fingerprinting completed but findings persist" bug.

Run this from the project root:
    uv run python scripts/diagnose_fingerprints.py

Now extended to actively reproduce the persistence path:
  1. Counts rows + flagged albums (as before).
  2. Picks a real album_id from the cache and tries the exact write path
     used by fingerprint_album: cache_module.upsert_track_fingerprint(...).
  3. Reads it back to confirm visibility.
  4. Cleans up the synthetic row so we don't pollute prod.
"""
from __future__ import annotations

import os
import sqlite3
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(HERE))

from media_manager.db import DB_PATH, _connect_raw  # type: ignore
from media_manager import cache as cache_module  # type: ignore

print(f"DB:   {DB_PATH}")
print(f"size: {DB_PATH.stat().st_size:,} bytes")
print()

# ----- 0. Schema-version check -----
conn = _connect_raw()
v = conn.execute("SELECT version FROM schema_version LIMIT 1").fetchone()
print(f"schema_version: {v[0] if v else 'MISSING'}")
print()

# ----- 1. Row counts -----
total_fp = conn.execute("SELECT COUNT(*) AS n FROM track_fingerprints").fetchone()["n"]
total_albums = conn.execute("SELECT COUNT(*) AS n FROM albums").fetchone()["n"]
flagged = conn.execute(
    """
    SELECT a.id, a.dir_name, a.path FROM albums a
    WHERE a.track_count > 0
      AND NOT EXISTS (SELECT 1 FROM track_fingerprints tf WHERE tf.album_id = a.id)
    LIMIT 1
    """
).fetchone()
print(f"albums total={total_albums}  fingerprint rows={total_fp}")
print()
if flagged is None:
    print("No flagged albums; nothing to reproduce against.")
    sys.exit(0)
album_id = flagged["id"]
album_path = flagged["path"]
print(f"Reproducer album: {album_id} dir_name={flagged['dir_name']!r}")
print(f"                  path={album_path!r}")
print()

# ----- 2. Write a synthetic row via cache_module (the same path
#         fingerprint_album uses) -----
synthetic_filename = "__diagnostic_fingerprint_test__.flac"
print(f"Calling cache_module.upsert_track_fingerprint(...) with synthetic filename {synthetic_filename!r}")
try:
    cache_module.upsert_track_fingerprint(
        album_id,
        filename=synthetic_filename,
        fingerprint="DIAG-FAKE-FINGERPRINT",
        duration_s=1.0,
        acoustid_recording_mbid=None,
        acoustid_score=None,
    )
    print("  upsert returned without raising")
except Exception as e:
    print(f"  upsert RAISED: {type(e).__name__}: {e}")
    sys.exit(1)

# ----- 3. Read it back via a fresh connection -----
print("Reading back via a NEW connection ...")
conn2 = _connect_raw()
row = conn2.execute(
    "SELECT album_id, filename, fingerprint, duration_s, last_computed_at "
    "FROM track_fingerprints WHERE album_id=? AND filename=?",
    (album_id, synthetic_filename),
).fetchone()
if row is None:
    print("  ROW NOT FOUND. Writes are silently failing. Possible causes:")
    print("    - autocommit / BEGIN / COMMIT interaction not working as expected")
    print("    - a different DB file being used by the running app")
    print("    - SQLite extension hooking and rolling back")
else:
    print(f"  found: album_id={row['album_id']}  filename={row['filename']!r}  "
          f"last_computed_at={row['last_computed_at']}")
    # Cleanup the synthetic row.
    conn2.execute(
        "DELETE FROM track_fingerprints WHERE album_id=? AND filename=?",
        (album_id, synthetic_filename),
    )
    print("  cleaned up synthetic row")

# ----- 4. Final tally -----
n_after = conn2.execute("SELECT COUNT(*) AS n FROM track_fingerprints").fetchone()["n"]
print(f"\nfinal track_fingerprints row count: {n_after}")
print("\nDone.")
