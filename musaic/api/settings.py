"""App settings + DB backup / restore + on-disk snapshots.

The library DB at ~/.musaic/library.db carries data that's
expensive to recover after a wipe (paid AI-driven MBID anchors, paid
AcoustID fingerprints, the user's tag taxonomy). Snapshots are
auto-taken on app startup if the configured interval has elapsed;
retention is enforced at create time.
"""
from __future__ import annotations

import os
import tempfile
from typing import Any

from fastapi import APIRouter, File, HTTPException, UploadFile
from fastapi.responses import FileResponse
from pydantic import BaseModel
from starlette.background import BackgroundTask

from ..services.sse import notify_jobs_changed

router = APIRouter()


class SettingsPatchPayload(BaseModel):
    ai_max_parallel: int | None = None
    ai_queue_paused: bool | None = None
    # Agent backend selection. ai_provider_id is an airframe provider id; it's
    # validated against the *installed* providers in the handler. ai_model_id
    # is the vendor model id (free-form). ai_api_key is write-only (same
    # semantics as the other keys) — injected into the adapter constructor
    # only when that provider accepts one. Empty string clears each; None
    # leaves it alone.
    ai_provider_id: str | None = None
    ai_model_id: str | None = None
    ai_api_key: str | None = None
    # AcoustID API key (write-only — never echoed back). Empty string clears
    # the key. None means "leave it alone" (as with all PATCH fields).
    acoustid_api_key: str | None = None
    # FireCrawl API key — see settings.firecrawl_api_key for behavior.
    # Same write-only semantics as the AcoustID key.
    firecrawl_api_key: str | None = None
    # Snapshot policy — how many automatic backups to keep, and the
    # minimum interval between auto-snapshots in hours. See db_backup.py.
    snapshot_retention_count: int | None = None
    snapshot_min_interval_hours: float | None = None
    # Filename templates used by the filename-mismatch health check
    # and its rename apply path. Empty string clears (falls back to
    # default); None means leave alone.
    filename_template_single_artist: str | None = None
    filename_template_compilation: str | None = None
    # Album-directory templates — same rationale as filename
    # templates but for the album folder names.
    dir_template_single_artist: str | None = None
    dir_template_compilation: str | None = None


@router.get("/api/settings")
def get_settings() -> dict[str, Any]:
    """All persistent settings, with defaults filled in."""
    from .. import settings as settings_module
    return settings_module.all_settings()


@router.patch("/api/settings")
def patch_settings(payload: SettingsPatchPayload) -> dict[str, Any]:
    """Update one or more settings. Only the fields present in the payload
    are written; absent fields are left alone."""
    from .. import settings as settings_module
    if payload.ai_max_parallel is not None:
        if payload.ai_max_parallel < 1 or payload.ai_max_parallel > 8:
            raise HTTPException(
                status_code=400,
                detail="ai_max_parallel must be between 1 and 8",
            )
        settings_module.set_ai_max_parallel(payload.ai_max_parallel)
    if payload.ai_queue_paused is not None:
        settings_module.set_ai_queue_paused(payload.ai_queue_paused)
    if payload.ai_provider_id is not None:
        pid = payload.ai_provider_id.strip()
        # Empty clears back to the default ("claude"); a non-empty value must
        # name a provider whose airframe adapter is actually installed,
        # otherwise the worker would fail at dispatch with an opaque error.
        if pid:
            from airframe import list_providers

            installed = list_providers(installed_only=True)
            if pid not in installed:
                raise HTTPException(
                    status_code=400,
                    detail=(
                        f"Unknown or not-installed provider {pid!r}. Installed: "
                        f"{', '.join(installed)}. Install its airframe extra to add more."
                    ),
                )
        settings_module.set_ai_provider_id(pid)
    if payload.ai_model_id is not None:
        settings_module.set_ai_model_id(payload.ai_model_id.strip())
    if payload.ai_api_key is not None:
        settings_module.set_ai_api_key(payload.ai_api_key)
    if payload.acoustid_api_key is not None:
        settings_module.set_acoustid_api_key(payload.acoustid_api_key)
    if payload.firecrawl_api_key is not None:
        settings_module.set_firecrawl_api_key(payload.firecrawl_api_key)
    if payload.snapshot_retention_count is not None:
        if payload.snapshot_retention_count < 0 or payload.snapshot_retention_count > 100:
            raise HTTPException(
                status_code=400,
                detail="snapshot_retention_count must be between 0 and 100",
            )
        settings_module.set_snapshot_retention(payload.snapshot_retention_count)
    if payload.snapshot_min_interval_hours is not None:
        if payload.snapshot_min_interval_hours < 0:
            raise HTTPException(
                status_code=400,
                detail="snapshot_min_interval_hours must be >= 0",
            )
        settings_module.set_snapshot_min_interval_hours(payload.snapshot_min_interval_hours)
    if payload.filename_template_single_artist is not None:
        settings_module.set_filename_template_single_artist(
            payload.filename_template_single_artist,
        )
    if payload.filename_template_compilation is not None:
        settings_module.set_filename_template_compilation(
            payload.filename_template_compilation,
        )
    if payload.dir_template_single_artist is not None:
        settings_module.set_dir_template_single_artist(
            payload.dir_template_single_artist,
        )
    if payload.dir_template_compilation is not None:
        settings_module.set_dir_template_compilation(
            payload.dir_template_compilation,
        )
    # Wake the worker so a max_parallel bump (or unpause) takes effect now.
    notify_jobs_changed()
    return settings_module.all_settings()


# ---------- AI provider discovery ----------

# Human-readable auth instructions for the providers that DON'T take a pasted
# api_key (they resolve credentials from their own env / CLI chain). Keyed by
# airframe provider id; a generic fallback covers anything not listed. The
# key-based providers don't need an entry — the UI shows a key field for them.
_AUTH_HINTS: dict[str, str] = {
    "github-copilot": "Authenticate with the GitHub CLI (`gh auth login`) or set GITHUB_TOKEN — no key needed here.",
    "bedrock": "Uses the AWS credential chain (env vars / AWS_PROFILE / IAM role) plus AWS_REGION — no key needed here.",
    "opencode": "Talks to a local OpenCode server over loopback (or set OPENCODE_SERVER_PASSWORD for a remote one) — no key needed here.",
}
_AUTH_HINT_FALLBACK = "This provider resolves credentials from its own environment / CLI chain — see its airframe adapter docs."


@router.get("/api/ai/providers")
def ai_providers() -> dict[str, Any]:
    """Discoverable airframe providers for the settings UI.

    `providers` are the *installed* adapters — the selectable set. Each is
    fully introspected: whether it takes a pasted API key (vs an env/CLI auth
    chain) and, for the chain-based ones, a short auth hint. `uninstalled` is
    the names of providers airframe knows about but whose extra isn't installed
    here — informational, the user adds them with `pip install
    airframe-agents[<extra>]`. (airframe guards `runtime_for` on uninstalled
    adapters, so we can't introspect those without importing their SDK.) Plus
    the current selection.
    """
    from airframe import list_providers

    from .. import ai as ai_module
    from .. import settings as settings_module

    installed = list_providers(installed_only=True)
    known = list_providers(installed_only=False)

    providers: list[dict[str, Any]] = []
    for pid in installed:
        accepts_key = ai_module.runtime_accepts_api_key(pid)
        # Phase 5 (issue #8): whether the provider's external prerequisite (a
        # CLI it shells out to, e.g. `claude` / `gh`) is present. Drives the
        # Settings "AI unavailable" banner; key/SDK-chain providers report
        # ready=True (their auth is verified at call time).
        status = ai_module.provider_runtime_status(pid)
        providers.append(
            {
                "id": pid,
                "accepts_api_key": accepts_key,
                # Key-based providers get no hint — the UI renders a key field
                # instead. Chain-based ones get an actionable instruction.
                "auth_hint": None if accepts_key else _AUTH_HINTS.get(pid, _AUTH_HINT_FALLBACK),
                "ready": status["ready"],
                "requirement": status["requirement"],
                "remedy": status["remedy"],
            }
        )

    return {
        "selected": {
            "provider_id": settings_module.ai_provider_id(),
            "model_id": settings_module.ai_model_id(),
            "api_key_set": bool(settings_module.ai_api_key()),
        },
        "providers": providers,
        "uninstalled": [pid for pid in known if pid not in set(installed)],
    }


# ---------- DB backup / restore + snapshots ----------


@router.get("/api/db/info")
def db_info() -> dict[str, Any]:
    from .. import db_backup
    return db_backup.db_info()


@router.get("/api/db/backup")
def db_backup_download() -> FileResponse:
    """Stream a consistent snapshot of the live DB as a download. Used
    for off-machine backups (e.g. dropping the file in iCloud / Time
    Machine / a USB stick). Doesn't touch the snapshot dir."""
    from .. import db_backup
    path = db_backup.make_backup()
    filename = db_backup.suggested_backup_filename()
    return FileResponse(
        str(path),
        media_type="application/octet-stream",
        filename=filename,
        background=BackgroundTask(lambda: path.unlink(missing_ok=True)),
    )


@router.post("/api/db/restore")
async def db_restore(file: UploadFile = File(...)) -> dict[str, Any]:
    """Restore the live DB from an uploaded backup file. Destructive:
    the existing DB is moved aside as library.db.bak-{timestamp} but
    the in-memory state of any in-flight requests will be stale until
    a refresh."""
    from .. import db_backup
    fd, tmp_path = tempfile.mkstemp(prefix="mm-restore-", suffix=".db")
    try:
        try:
            while True:
                chunk = await file.read(1024 * 1024)
                if not chunk:
                    break
                os.write(fd, chunk)
        finally:
            os.close(fd)
        try:
            result = db_backup.restore_from(tmp_path)
        except db_backup.RestoreError as e:
            raise HTTPException(status_code=400, detail=str(e))
        return result
    finally:
        try:
            os.unlink(tmp_path)
        except OSError:
            pass


@router.get("/api/db/snapshots")
def list_snapshots() -> dict[str, Any]:
    from .. import db_backup
    return {"snapshots": db_backup.list_snapshots()}


@router.post("/api/db/snapshots")
def create_snapshot() -> dict[str, Any]:
    """Take a manual snapshot now, bypassing the interval gate. Retention
    is still enforced (older snapshots may get pruned)."""
    from .. import db_backup, settings as settings_module
    retention = settings_module.snapshot_retention()
    if retention <= 0:
        raise HTTPException(
            status_code=409,
            detail="Snapshotting is disabled (retention count is 0). "
                   "Set snapshot_retention_count > 0 in Settings first.",
        )
    return db_backup.create_snapshot(retention=retention)


@router.delete("/api/db/snapshots/{name}")
def delete_snapshot(name: str) -> dict[str, Any]:
    from .. import db_backup
    try:
        deleted = db_backup.delete_snapshot(name)
    except db_backup.RestoreError as e:
        raise HTTPException(status_code=400, detail=str(e))
    return {"deleted": deleted, "name": name}


@router.post("/api/db/snapshots/{name}/restore")
def restore_snapshot(name: str) -> dict[str, Any]:
    from .. import db_backup
    try:
        return db_backup.restore_from_snapshot(name)
    except db_backup.RestoreError as e:
        raise HTTPException(status_code=400, detail=str(e))
