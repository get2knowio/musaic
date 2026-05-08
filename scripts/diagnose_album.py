"""Single-album diagnostic. Run from the project root:

    uv run python scripts/diagnose_album.py "Cocktail"

The argument is matched as a substring against album dir_name. Prints:
  - Album row(s) matching
  - track_fingerprints rows for each match (filename, score, duration,
    fingerprint length, recording_mbid)
  - Open findings for each match
  - For each pair of fingerprinted tracks: the Chromaprint similarity
    + duration delta + whether the duplicate-tracks check would
    cluster them
"""
from __future__ import annotations

import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(HERE))

from musaic.db import _connect_raw  # type: ignore
from musaic import fingerprint as fingerprint_module  # type: ignore
from musaic.checks.duplicate_tracks import (  # type: ignore
    _MAX_DURATION_DELTA_SEC,
    _MIN_FINGERPRINT_SIMILARITY,
)

if len(sys.argv) < 2:
    print("Usage: diagnose_album.py <substring of dir_name>")
    sys.exit(1)
needle = sys.argv[1]

conn = _connect_raw()
albums = conn.execute(
    "SELECT id, dir_name, path, track_count "
    "FROM albums WHERE dir_name LIKE ? ORDER BY dir_name",
    (f"%{needle}%",),
).fetchall()
if not albums:
    print(f"No albums match {needle!r}")
    sys.exit(0)

print(f"Matched {len(albums)} album(s):\n")
for a in albums:
    aid = a["id"]
    print(f"━━━ {a['dir_name']!r}")
    print(f"    id={aid}  path={a['path']!r}  track_count={a['track_count']}")
    print()

    fps = conn.execute(
        "SELECT filename, duration_s, acoustid_recording_mbid, "
        "       acoustid_score, length(fingerprint) AS fp_len, "
        "       last_computed_at "
        "FROM track_fingerprints WHERE album_id = ? ORDER BY filename",
        (aid,),
    ).fetchall()
    print(f"    track_fingerprints rows: {len(fps)}")
    if not fps:
        print(f"      (none — album has not been fingerprinted yet)")
        print()
        continue

    for r in fps:
        score = "NULL" if r["acoustid_score"] is None else f"{r['acoustid_score']:.3f}"
        mbid = r["acoustid_recording_mbid"] or "(none)"
        print(
            f"      {r['filename']!r:50s}  dur={r['duration_s']:6.1f}  "
            f"score={score:>5}  fp_len={r['fp_len']:>5}  mbid={mbid[:8] + '…' if mbid != '(none)' else mbid}"
        )

    # Pairwise similarity for everything in this album
    print()
    print(f"    Pairwise comparison (threshold: dur_delta<={_MAX_DURATION_DELTA_SEC}s, sim>={_MIN_FINGERPRINT_SIMILARITY:.2f}):")
    full_fps = conn.execute(
        "SELECT filename, duration_s, fingerprint "
        "FROM track_fingerprints WHERE album_id = ? ORDER BY filename",
        (aid,),
    ).fetchall()
    for i, x in enumerate(full_fps):
        for y in full_fps[i + 1:]:
            ddur = abs(x["duration_s"] - y["duration_s"])
            if ddur > _MAX_DURATION_DELTA_SEC:
                continue  # skip cheap-rejected pairs to keep output readable
            sim = fingerprint_module.chromaprint_similarity(
                x["fingerprint"], y["fingerprint"],
            )
            verdict = "DUPLICATE" if sim >= _MIN_FINGERPRINT_SIMILARITY else "different"
            print(
                f"      {x['filename']!r:40s}  vs  {y['filename']!r:40s}  "
                f"dur_delta={ddur:5.2f}s  sim={sim:.4f}  -> {verdict}"
            )
    print()

    # Open findings
    findings = conn.execute(
        "SELECT check_id, summary, severity FROM check_findings "
        "WHERE album_id = ? AND resolved_at IS NULL AND ignored_at IS NULL",
        (aid,),
    ).fetchall()
    print(f"    Open findings ({len(findings)}):")
    for f in findings:
        print(f"      [{f['check_id']}] {f['summary']!r}  (sev={f['severity']})")
    print()
