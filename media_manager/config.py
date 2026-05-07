"""Persistent application config (currently just the library directory)."""
from __future__ import annotations

import json
import os
from dataclasses import asdict, dataclass
from pathlib import Path


CONFIG_DIR = Path(os.environ.get("MEDIA_MANAGER_CONFIG_DIR", Path.home() / ".media_manager"))
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
