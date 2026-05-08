"""Tiny no-dependency .env loader.

Mirrors the pattern used in flac-jacket's plex-playlists update script:
  - Real env vars always win over the file (loader skips keys already in os.environ).
  - Supports `KEY=VALUE` lines, blank lines, and `#` comments.
  - Strips a single pair of matching surrounding quotes on the value.
  - Silent no-op if the file doesn't exist.

Lookup order applied by `bootstrap_env()`:
    real env vars  >  $PWD/.env  >  ~/.musaic/.env

The first writer wins, so a real shell-exported value cannot be clobbered by a
file, and a project-local `.env` overrides the user-level one.
"""
from __future__ import annotations

import os
from pathlib import Path


def load_env_file(path: Path) -> int:
    """Load KEY=VALUE pairs from `path` into os.environ (only keys not already set).

    Returns the number of keys loaded. Returns 0 on missing file or read errors.
    """
    if not path.is_file():
        return 0
    loaded = 0
    try:
        with path.open("r", encoding="utf-8") as fh:
            for raw in fh:
                line = raw.strip()
                if not line or line.startswith("#"):
                    continue
                if "=" not in line:
                    continue
                key, _, value = line.partition("=")
                key = key.strip()
                value = value.strip()
                # Strip a single pair of matching surrounding quotes.
                if len(value) >= 2 and value[0] == value[-1] and value[0] in ("'", '"'):
                    value = value[1:-1]
                if not key or key in os.environ:
                    continue
                os.environ[key] = value
                loaded += 1
    except OSError:
        return 0
    return loaded


def bootstrap_env() -> dict[str, int]:
    """Load .env files from conventional locations. Returns {path: keys_loaded}."""
    candidates = [
        Path.cwd() / ".env",
        Path.home() / ".musaic" / ".env",
    ]
    return {str(p): load_env_file(p) for p in candidates}
