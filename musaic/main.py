"""FastAPI entry point for Musaic.

Routes are organized as APIRouter modules under `musaic.api`;
shared helpers (album resolution, post-scan checks, the AI worker, SSE
plumbing) live under `musaic.services`. This file just wires
the app together: lifespan + router includes + frontend SPA + CLI.
"""
from __future__ import annotations

import argparse
import asyncio
import contextlib
import logging
from pathlib import Path

from fastapi import FastAPI, HTTPException
from fastapi.responses import FileResponse
from fastapi.staticfiles import StaticFiles

from . import api
from .services.worker import worker_loop

_log = logging.getLogger(__name__)

FRONTEND_DIR = Path(__file__).resolve().parent.parent / "frontend" / "dist"


@contextlib.asynccontextmanager
async def _lifespan(_app: FastAPI):
    """Start the AI job worker on app startup, cancel it on shutdown.
    Also takes an automatic DB snapshot if the configured interval has
    elapsed since the last one."""
    # Snapshot first, before the worker can start mutating state.
    try:
        from . import db_backup, settings as settings_module
        retention = settings_module.snapshot_retention()
        interval = settings_module.snapshot_min_interval_hours()
        snap = db_backup.maybe_auto_snapshot(
            retention=retention, min_interval_hours=interval,
        )
        if snap:
            _log.info(
                "Auto-snapshot taken on startup: %s (%d bytes); pruned %d older",
                snap["filename"], snap["size_bytes"], len(snap.get("pruned") or []),
            )
    except Exception:
        _log.exception("Auto-snapshot on startup failed (continuing)")

    # Register library_events subscribers (Phase 1 of move E from
    # issue #1). Dual-fire alongside existing direct run_post_scan_checks
    # calls — idempotency makes that safe; see
    # docs/library_events_design.md for the migration plan.
    try:
        from .services import event_subscribers
        event_subscribers.register_all()
    except Exception:
        _log.exception("library_events subscriber registration failed")

    task = asyncio.create_task(worker_loop(), name="ai-job-worker")
    try:
        yield
    finally:
        task.cancel()
        with contextlib.suppress(asyncio.CancelledError):
            await task
        # The eager fingerprinting pool is fire-and-forget from the
        # scan service; it might still be running when uvicorn signals
        # shutdown. Cancel it so we exit cleanly instead of leaving
        # an fpcalc subprocess orphaned.
        try:
            from .services import fingerprint_pool
            await fingerprint_pool.stop()
        except Exception:
            _log.exception("fingerprint-pool stop failed during shutdown")


app = FastAPI(title="Musaic", version="0.1.0", lifespan=_lifespan)

# Wire up the routers. Order matters only for overlapping path patterns
# (e.g. `/api/checks/stream` must register before `/api/checks/{check_id}`,
# which is enforced inside health.py via in-file ordering).
app.include_router(api.library.router)
app.include_router(api.tags.router)
app.include_router(api.jobs.router)
app.include_router(api.settings.router)
app.include_router(api.health.router)
app.include_router(api.apply.router)
app.include_router(api.analyze.router)


# ---------- Frontend ----------

# Serve the built React SPA from frontend/dist. Vite emits /assets/* bundles
# referenced by index.html; everything else is a client-side route, so we fall
# back to index.html for any unknown GET that isn't an /api/* path.
if FRONTEND_DIR.exists():
    app.mount(
        "/assets",
        StaticFiles(directory=str(FRONTEND_DIR / "assets")),
        name="assets",
    )


@app.get("/")
def index() -> FileResponse:
    return FileResponse(str(FRONTEND_DIR / "index.html"))


@app.get("/{full_path:path}")
def spa_fallback(full_path: str) -> FileResponse:
    if full_path.startswith("api/"):
        raise HTTPException(status_code=404)
    candidate = FRONTEND_DIR / full_path
    if candidate.is_file():
        return FileResponse(str(candidate))
    return FileResponse(str(FRONTEND_DIR / "index.html"))


# ---------- CLI entry point ----------

def _configure_logging() -> None:
    """Set up our application logger so log.info() / log.warning() messages
    actually appear (Python's default level is WARNING, which is why all our
    INFO-level diagnostics were invisible until now).

    Two handlers:
      - stdout (mirrors what you'd see by tailing the terminal)
      - rotating file at $CONFIG_DIR/server.log (5 MB × 5 generations =
        ~25 MB cap; survives terminal close / restart so we can grep
        debugging output after the fact)

    Level is INFO by default; override with MUSAIC_LOG_LEVEL=DEBUG
    when chasing something specific.
    """
    import logging.handlers
    import os

    from .config import CONFIG_DIR

    # Honor the legacy MEDIA_MANAGER_LOG_LEVEL as a fallback so existing
    # shell envs keep working post-rename.
    level_name = os.environ.get(
        "MUSAIC_LOG_LEVEL",
        os.environ.get("MEDIA_MANAGER_LOG_LEVEL", "INFO"),
    ).upper()
    level = getattr(logging, level_name, logging.INFO)

    fmt = logging.Formatter(
        "%(asctime)s [%(levelname)s] %(name)s: %(message)s",
        datefmt="%Y-%m-%d %H:%M:%S",
    )

    # Application logger — touch only `musaic.*` and below; leave
    # uvicorn's loggers alone so its access-log format stays intact.
    app_log = logging.getLogger("musaic")
    app_log.setLevel(level)
    # Idempotent: don't double-attach handlers if run() is called twice
    # (e.g. under --reload).
    if not any(getattr(h, "_mm_marker", False) for h in app_log.handlers):
        stream = logging.StreamHandler()
        stream.setFormatter(fmt)
        stream._mm_marker = True  # type: ignore[attr-defined]
        app_log.addHandler(stream)

        try:
            CONFIG_DIR.mkdir(parents=True, exist_ok=True)
            file_path = Path(CONFIG_DIR) / "server.log"
            file_handler = logging.handlers.RotatingFileHandler(
                file_path, maxBytes=5 * 1024 * 1024, backupCount=5,
            )
            file_handler.setFormatter(fmt)
            file_handler._mm_marker = True  # type: ignore[attr-defined]
            app_log.addHandler(file_handler)
            print(f"[musaic] Logging to {file_path} (level={level_name})")
        except Exception as e:
            # Never let log-file setup fail the boot.
            print(f"[musaic] Warning: couldn't set up file log ({e})")

    # Don't bubble up to root — root has its own default handler that
    # would duplicate every line.
    app_log.propagate = False


def run() -> None:
    """`musaic` console-script entry point."""
    parser = argparse.ArgumentParser(description="Musaic web UI")
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8765)
    parser.add_argument("--reload", action="store_true", help="Enable hot reload (development)")
    args = parser.parse_args()

    _configure_logging()

    import os
    auth = "CLAUDE_CODE_OAUTH_TOKEN" if os.environ.get("CLAUDE_CODE_OAUTH_TOKEN") else (
        "ANTHROPIC_API_KEY" if os.environ.get("ANTHROPIC_API_KEY") else None
    )
    if auth:
        print(f"[musaic] Claude auth: using {auth} from environment")
    else:
        print("[musaic] Warning: no CLAUDE_CODE_OAUTH_TOKEN or ANTHROPIC_API_KEY found — AI features will fail")

    import uvicorn

    uvicorn.run(
        "musaic.main:app",
        host=args.host,
        port=args.port,
        reload=args.reload,
    )


if __name__ == "__main__":
    run()
