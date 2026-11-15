"""Check a generated synthetic library against the committed manifest.

    python3 testdata/synthetic-library/verify_library.py [LIBRARY_DIR]   # from rust/

Prints every file whose SHA-256 differs from library.manifest.json (or that
is missing, not listed, or without the pinned mtime) and exits 1 if there is
any. Standard library only, so it runs where the generator's packages are not
installed.
"""
from __future__ import annotations

import hashlib
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent


def main(argv: list[str]) -> int:
    root = Path(argv[1]) if len(argv) > 1 else HERE / "library"
    manifest = json.loads((HERE / "library.manifest.json").read_text(encoding="utf-8"))
    listed = {f["path"]: f["sha256"] for f in manifest["files"]}
    bad = []
    for rel, want in sorted(listed.items()):
        p = root / rel
        try:
            got = hashlib.sha256(p.read_bytes()).hexdigest()
        except OSError as e:
            bad.append(f"missing  {rel} ({e})")
            continue
        if got != want:
            bad.append(f"differs  {rel}")
    # The scanner records directory mtimes; the generator pins every one.
    fixed = manifest["fixed_mtime"]
    for p in [root, *root.rglob("*")]:
        if int(p.stat().st_mtime) != fixed:
            bad.append(f"mtime    {p.relative_to(root).as_posix() or '.'} (not {fixed})")
    on_disk = {p.relative_to(root).as_posix() for p in root.rglob("*") if p.is_file()}
    bad += [f"unlisted {rel}" for rel in sorted(on_disk - listed.keys())]
    for line in bad:
        print(line)
    print(f"{len(listed)} files listed, {len(bad)} problem(s)")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
