"""Phase 0 desktop-packaging contract (issue #8).

The Electron shell spawns the backend as a localhost sidecar and needs
two injection points that don't exist as CLI plumbing: a frontend-dir
override (the packaged SPA isn't a sibling of the `musaic` package) and a
port override (the shell picks a free port). Both read the environment;
because `FRONTEND_DIR` is resolved at import time, we exercise it in a
fresh interpreter so the env is read before module load.
"""
from __future__ import annotations

import subprocess
import sys
from pathlib import Path


def _run(snippet: str, env_extra: dict[str, str], tmp_path: Path) -> str:
    import os

    env = dict(os.environ)
    # Keep the subprocess off the user's real ~/.musaic.
    env["MUSAIC_CONFIG_DIR"] = str(tmp_path / "cfg")
    env.update(env_extra)
    out = subprocess.run(
        [sys.executable, "-c", snippet],
        env=env, capture_output=True, text=True, check=True,
    )
    return out.stdout.strip()


def test_frontend_dir_env_override(tmp_path):
    """MUSAIC_FRONTEND_DIR repoints FRONTEND_DIR at import time."""
    target = tmp_path / "packaged" / "dist"
    got = _run(
        "import musaic.main as m; print(m.FRONTEND_DIR)",
        {"MUSAIC_FRONTEND_DIR": str(target)},
        tmp_path,
    )
    assert got == str(target)


def test_frontend_dir_default_when_unset(tmp_path):
    """Without the override, FRONTEND_DIR is the repo-relative
    frontend/dist sibling of the package — the dev / `uv run musaic`
    default."""
    got = _run(
        "import musaic.main as m; print(m.FRONTEND_DIR.name, m.FRONTEND_DIR.parent.name)",
        {},
        tmp_path,
    )
    assert got == "dist frontend"


def test_port_env_default(monkeypatch):
    """MUSAIC_PORT becomes the argparse default for run()'s --port, so the
    sidecar boots on the shell-picked port with no CLI flag. Exercises the
    real run() wiring with uvicorn stubbed out."""
    from musaic import main

    monkeypatch.setenv("MUSAIC_PORT", "9123")
    monkeypatch.setenv("MUSAIC_HOST", "0.0.0.0")
    monkeypatch.setattr(sys, "argv", ["musaic"])

    captured = {}

    def fake_run(_app, **kwargs):
        captured.update(kwargs)

    import uvicorn
    monkeypatch.setattr(uvicorn, "run", fake_run)

    main.run()

    assert captured["port"] == 9123
    assert captured["host"] == "0.0.0.0"


def test_port_cli_flag_overrides_env(monkeypatch):
    """An explicit --port still wins over MUSAIC_PORT."""
    from musaic import main

    monkeypatch.setenv("MUSAIC_PORT", "9123")
    monkeypatch.setattr(sys, "argv", ["musaic", "--port", "7000"])

    captured = {}
    import uvicorn
    monkeypatch.setattr(uvicorn, "run", lambda _app, **kw: captured.update(kw))

    main.run()
    assert captured["port"] == 7000
