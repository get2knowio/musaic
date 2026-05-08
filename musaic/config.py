"""Persistent application config (currently just the library directory)."""
from __future__ import annotations

import json
import logging
import os
from dataclasses import asdict, dataclass
from pathlib import Path


def _resolve_config_dir() -> Path:
    """Pick the config directory, honoring back-compat with the pre-rename
    `MEDIA_MANAGER_CONFIG_DIR` env var and `~/.media_manager` default.

    Resolution order:
      1. `MUSAIC_CONFIG_DIR` env var (current name)
      2. `MEDIA_MANAGER_CONFIG_DIR` env var (legacy)
      3. `~/.musaic` if it exists
      4. `~/.media_manager` if it exists (one-time silent migration —
         we move it to `~/.musaic` so subsequent runs stop checking)
      5. `~/.musaic` (fresh install)
    """
    if env := os.environ.get("MUSAIC_CONFIG_DIR"):
        return Path(env)
    if env := os.environ.get("MEDIA_MANAGER_CONFIG_DIR"):
        return Path(env)
    musaic_dir = Path.home() / ".musaic"
    legacy_dir = Path.home() / ".media_manager"
    if musaic_dir.exists():
        return musaic_dir
    if legacy_dir.exists():
        try:
            legacy_dir.rename(musaic_dir)
            logging.getLogger(__name__).info(
                "Migrated config dir %s → %s", legacy_dir, musaic_dir,
            )
        except OSError:
            # Permissions or cross-device issue — keep using the legacy
            # path so the user doesn't lose data, just emit a hint.
            logging.getLogger(__name__).warning(
                "Couldn't migrate %s to %s; continuing to use legacy path",
                legacy_dir, musaic_dir,
            )
            return legacy_dir
    return musaic_dir


CONFIG_DIR = _resolve_config_dir()
CONFIG_FILE = CONFIG_DIR / "config.json"


@dataclass
class AppConfig:
    library_dir: str | None = None


def load_config() -> AppConfig:
    if not CONFIG_FILE.is_file():
        return AppConfig()
    try:
        data = json.loads(CONFIG_FILE.read_text(encoding="utf-8"))
    except Exception:
        return AppConfig()
    return AppConfig(library_dir=data.get("library_dir"))


def save_config(config: AppConfig) -> None:
    CONFIG_DIR.mkdir(parents=True, exist_ok=True)
    CONFIG_FILE.write_text(json.dumps(asdict(config), indent=2), encoding="utf-8")


def set_library_dir(path: str) -> AppConfig:
    expanded = str(Path(path).expanduser().resolve())
    config = load_config()
    config.library_dir = expanded
    save_config(config)
    return config
