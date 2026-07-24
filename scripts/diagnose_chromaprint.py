"""Why is chromaprint_similarity returning 0.0 for known-identical fingerprints?

Run from the project root:
    uv run python scripts/diagnose_chromaprint.py

Tests, in order:
  1. Can we import the chromaprint module? Where from?
  2. Pull one fingerprint string from track_fingerprints.
  3. Try to decode it via chromaprint.decode_fingerprint with each combination
     of bytes-vs-str input and base64 flag, and surface the actual error.
  4. If decode succeeds, run chromaprint_similarity(fp, fp) — must be 1.0.
"""
from __future__ import annotations

import sys
import traceback
from pathlib import Path

HERE = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(HERE))

# ---- 1. Where is `chromaprint` coming from? ----
print("[1] Importing chromaprint...")
try:
    import chromaprint
    print(f"    module: {chromaprint!r}")
    print(f"    file:   {getattr(chromaprint, '__file__', '(builtin)')}")
    print(f"    has decode_fingerprint? {hasattr(chromaprint, 'decode_fingerprint')}")
    print(f"    dir(): {[n for n in dir(chromaprint) if not n.startswith('_')]}")
except Exception:
    print("    IMPORT FAILED")
    traceback.print_exc()
    sys.exit(1)

# ---- 2. Pull one fingerprint string ----
from musaic.db import _connect_raw  # type: ignore

conn = _connect_raw()
row = conn.execute(
    "SELECT album_id, filename, fingerprint FROM track_fingerprints "
    "WHERE fingerprint IS NOT NULL LIMIT 1"
).fetchone()
if row is None:
    print("\n[2] No fingerprint rows in DB — fingerprint a few albums first")
    sys.exit(0)
fp = row["fingerprint"]
print("\n[2] Sample fingerprint:")
print(f"    album_id: {row['album_id']}")
print(f"    filename: {row['filename']!r}")
print(f"    type:     {type(fp).__name__}")
print(f"    length:   {len(fp)}")
print(f"    repr[:80]: {fp[:80]!r}")

# ---- 3. Try every plausible decode call shape ----
print("\n[3] Decode attempts:")
attempts = [
    ("str + base64=True",   lambda: chromaprint.decode_fingerprint(fp, base64=True)),
    ("bytes + base64=True", lambda: chromaprint.decode_fingerprint(fp.encode("ascii"), base64=True)),
    ("str (positional)",    lambda: chromaprint.decode_fingerprint(fp, True)),
    ("bytes (positional)",  lambda: chromaprint.decode_fingerprint(fp.encode("ascii"), True)),
    ("str, no kwarg",       lambda: chromaprint.decode_fingerprint(fp)),
]
for label, fn in attempts:
    try:
        out = fn()
        # Inspect the return shape
        if isinstance(out, tuple) and len(out) == 2:
            ints, algo = out
            print(f"    {label:25s} -> ({type(ints).__name__}, algo={algo}); len={len(ints) if hasattr(ints, '__len__') else '?'}")
            if hasattr(ints, '__len__') and len(ints):
                print(f"       first few ints: {list(ints[:4])}")
        else:
            print(f"    {label:25s} -> unexpected shape {type(out).__name__}: {out!r}")
    except Exception as e:
        print(f"    {label:25s} -> {type(e).__name__}: {e}")

# ---- 4. Self-similarity sanity ----
print("\n[4] chromaprint_similarity(fp, fp) — should be 1.0:")
from musaic.fingerprint import chromaprint_similarity  # type: ignore

sim = chromaprint_similarity(fp, fp)
print(f"    similarity: {sim}")
if sim != 1.0:
    print("    !!! identical fingerprints should be 1.0; investigate")
