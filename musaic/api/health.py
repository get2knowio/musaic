"""Health-check endpoints. Routes are sequenced so static prefixes
(`/stream`, `/findings`) win over parameterized `/{check_id}` paths —
FastAPI matches in registration order."""
from __future__ import annotations

import asyncio
import contextvars
import logging
from pathlib import Path
from typing import Any

from fastapi import APIRouter, HTTPException, Query
from fastapi.responses import StreamingResponse
from pydantic import BaseModel

from .. import cache as cache_module
from .. import checks as checks_module
from .. import config as config_module
from .. import mb_lookup
from ..checks import runner as checks_runner
from ..scanner import album_to_dict, scan_one_album
from ..services.albums import cached_album_dict
from ..services.post_scan import (
    library_path_for_checks,
    run_post_scan_checks,
    run_post_scan_checks_async,
)
from ..services.sse import sse_event

_log = logging.getLogger(__name__)
router = APIRouter()


# ---------- Payloads ----------


class RunCheckPayload(BaseModel):
    scope: str = "all"  # "all" | "incremental" | "albums"
    album_ids: list[str] | None = None


class IgnoreFindingPayload(BaseModel):
    reason: str | None = None


class FixFindingPayload(BaseModel):
    """Optional body for the fix-finding endpoint. The hint is free-form
    text the user typed in the finding-row Fix dialog (mirrors the
    existing detail-sheet AI hint dialog). Forwarded to the check's
    fix() and, for AI-job checks, plumbed into the enqueued job's
    `hint` field. Absent body = no hint = pre-existing default behavior."""
    hint: str = ""


class DeleteDuplicateFilePayload(BaseModel):
    filename: str


class ApplyClusterSortPayload(BaseModel):
    """Sort string the user picked from the inconsistent-artist-sort
    Resolve dialog. Validated minimally — must be non-empty, max 200
    chars (room for unusually-long band names without letting an
    accidental whole-paragraph paste through). The check itself
    decides which albums get the value via the finding's stored
    cluster member list."""
    sort_value: str


class FilenameRenameEntry(BaseModel):
    """One file in the rename plan. `old` and `new` are basenames
    (no directory component) — the apply path joins them onto the
    album dir."""
    old: str
    new: str


class DirRenameEntry(BaseModel):
    """Optional album-directory rename. When present, runs FIRST so
    file renames execute inside the new dir."""
    old: str
    new: str


class FilenameRenamePayload(BaseModel):
    plan: list[FilenameRenameEntry]
    dir_rename: DirRenameEntry | None = None


# ---------- Endpoints ----------


@router.get("/api/checks/stream")
async def checks_stream() -> StreamingResponse:
    """SSE stream of check / finding state changes. Emits a single 'state'
    event per change, carrying the full check-summary list. The frontend
    cache is small enough that diffing isn't worth the complexity."""
    async def event_gen():
        yield sse_event("state", {"checks": checks_runner.list_checks_with_summary()})
        ev = checks_runner.changed_event()
        try:
            while True:
                try:
                    await asyncio.wait_for(ev.wait(), timeout=30.0)
                    ev.clear()
                    yield sse_event("state", {"checks": checks_runner.list_checks_with_summary()})
                except asyncio.TimeoutError:
                    yield ": ping\n\n"
        except asyncio.CancelledError:
            return

    return StreamingResponse(
        event_gen(),
        media_type="text/event-stream",
        headers={
            "Cache-Control": "no-cache, no-transform",
            "X-Accel-Buffering": "no",
            "Connection": "keep-alive",
        },
    )


@router.get("/api/checks/findings")
def list_all_findings(
    album_id: str | None = Query(None, description="filter to one album"),
    check_id: str | None = Query(None, description="filter to one check"),
    include_ignored: bool = Query(False),
    include_resolved: bool = Query(False),
) -> dict[str, Any]:
    """Open findings across checks. The detail-sheet 'Findings' section
    calls this with ?album_id= ; the Health detail page calls it with
    ?check_id= ."""
    return {
        "findings": checks_runner.list_findings(
            check_id=check_id,
            album_id=album_id,
            include_ignored=include_ignored,
            include_resolved=include_resolved,
        ),
    }


@router.get("/api/checks")
def list_checks() -> dict[str, Any]:
    """All registered checks + last-run summary + open finding counts.
    Drives the Health page's index-card grid."""
    return {"checks": checks_runner.list_checks_with_summary()}


@router.get("/api/checks/{check_id}")
def get_check(check_id: str) -> dict[str, Any]:
    out = checks_runner.get_check_with_runs(check_id)
    if out is None:
        raise HTTPException(status_code=404, detail=f"Unknown check_id: {check_id!r}")
    return out


@router.post("/api/checks/{check_id}/run")
async def run_check_now(check_id: str, payload: RunCheckPayload) -> dict[str, Any]:
    """Kick off a check. Returns the final run row when the check finishes.

    For the cheap checks Phase 2 ships, runs complete in well under a second
    so this is fine as a sync request. Once the heavy A1 (FLAC integrity)
    check lands, we'll likely change this to fire-and-forget + SSE updates.
    """
    if payload.scope not in ("all", "incremental", "albums"):
        raise HTTPException(status_code=400, detail="scope must be 'all'|'incremental'|'albums'")
    library_path = library_path_for_checks()
    try:
        run = await checks_runner.run_check(
            check_id,
            library_path=library_path,
            scope_kind=payload.scope,
            album_ids=payload.album_ids,
        )
    except ValueError as e:
        raise HTTPException(status_code=400, detail=str(e))
    return run


@router.post("/api/checks/{check_id}/findings/{finding_id}/ignore")
def ignore_finding(check_id: str, finding_id: int, payload: IgnoreFindingPayload) -> dict[str, Any]:
    finding = checks_runner.get_finding(finding_id)
    if finding is None or finding["check_id"] != check_id:
        raise HTTPException(status_code=404, detail="Finding not found")
    updated = checks_runner.ignore_finding(finding_id, reason=payload.reason)
    return updated  # type: ignore[return-value]


@router.post("/api/checks/{check_id}/findings/{finding_id}/unignore")
def unignore_finding(check_id: str, finding_id: int) -> dict[str, Any]:
    finding = checks_runner.get_finding(finding_id)
    if finding is None or finding["check_id"] != check_id:
        raise HTTPException(status_code=404, detail="Finding not found")
    updated = checks_runner.unignore_finding(finding_id)
    return updated  # type: ignore[return-value]


@router.post("/api/checks/{check_id}/findings/{finding_id}/fix")
def fix_finding(
    check_id: str,
    finding_id: int,
    payload: FixFindingPayload | None = None,
) -> dict[str, Any]:
    finding = checks_runner.get_finding(finding_id)
    if finding is None or finding["check_id"] != check_id:
        raise HTTPException(status_code=404, detail="Finding not found")
    user_hint = (payload.hint if payload else "") or ""
    try:
        result = checks_runner.fix_finding(finding_id, user_hint=user_hint)
    except ValueError as e:
        raise HTTPException(status_code=400, detail=str(e))

    # Synchronous deterministic fix attempts may mutate cache state
    # even when the high-level result reports ok=False — `mb_lookup.
    # find_release` writes an anchor as a side effect of any confident
    # MB hit (regardless of whether the fix consumed it), and
    # `fingerprint.fingerprint_album` writes track_fingerprints rows
    # regardless of whether the caller could map them. Without the
    # post_scan rerun, those side-effected mutations leave findings
    # on OTHER checks (e.g. `unidentified-album`) stale until the
    # next library scan. Fire unconditionally for any sync deterministic
    # attempt; AI-job fixes don't need this — the worker reruns
    # post-scan when it persists results.
    if result.job_id is None:
        try:
            cfg = config_module.load_config()
            if cfg.library_dir:
                run_post_scan_checks(cfg.library_dir)
        except Exception:
            _log.exception("post-fix check rerun failed (continuing)")

    return {
        "ok":      result.ok,
        "message": result.message,
        "job_id":  result.job_id,
    }


@router.get("/api/checks/duplicate-tracks/findings/{finding_id}/canonical")
def duplicate_finding_canonical(finding_id: int) -> dict[str, Any]:
    """Look up MusicBrainz's canonical title + artist for a duplicate
    cluster's recording_mbid. Powers the resolve dialog's "MB says: …"
    line, which lets the user disambiguate which mistagged file is which.

    Lazy-loaded (this endpoint hits MB live, the check itself doesn't) so
    the duplicate-tracks check stays fast — most clusters never have
    their dialog opened. Recordings are immutable, so a single lookup
    per MBID per server lifetime is sufficient (cached via
    mb_lookup.get_recording_canonical's lru_cache).

    Returns:
      {title, artist}                         — MB found it
      {title: null, artist: null, reason: …}  — no MBID, MB miss, or HTTP error
    """
    finding = checks_runner.get_finding(finding_id)
    if finding is None or finding["check_id"] != "duplicate-tracks":
        raise HTTPException(status_code=404, detail="Finding not found")
    detail = finding.get("detail") or {}
    mbid = detail.get("recording_mbid")
    if not mbid:
        return {
            "title": None,
            "artist": None,
            "mbid": None,
            "reason": "no recording MBID for this cluster",
        }
    canonical = mb_lookup.get_recording_canonical(mbid)
    if canonical is None:
        return {
            "title": None,
            "artist": None,
            "mbid": mbid,
            "reason": "MusicBrainz didn't return a recording for that MBID",
        }
    return {
        "title": canonical["title"],
        "artist": canonical["artist"],
        "mbid": mbid,
        "reason": None,
    }


@router.post("/api/checks/inconsistent-artist-sort/findings/{finding_id}/apply-sort")
def apply_inconsistent_artist_sort(
    finding_id: int,
    payload: ApplyClusterSortPayload,
) -> dict[str, Any]:
    """Custom action for the inconsistent-artist-sort check: bulk-write
    the user's chosen sort value to album_artist_sort across every
    album in the finding's cluster. Goes through apply_direct so the
    actual FLAC tags get rewritten, not just our cache."""
    from ..checks import inconsistent_artist_sort
    finding = checks_runner.get_finding(finding_id)
    if finding is None or finding["check_id"] != "inconsistent-artist-sort":
        raise HTTPException(status_code=404, detail="Finding not found")
    sort_value = (payload.sort_value or "").strip()
    if not sort_value:
        raise HTTPException(status_code=400, detail="sort_value is required")
    if len(sort_value) > 200:
        raise HTTPException(status_code=400, detail="sort_value too long (max 200 chars)")
    detail = finding.get("detail") or {}
    members = detail.get("members") or []
    if not members:
        raise HTTPException(status_code=400, detail="Finding has no cluster members")

    # Members may be in the v11+ shape (with "kind") or the legacy v10
    # shape (album-only, fields named "id"/"album_artist_sort"). Adapt
    # legacy entries on the fly so we don't have to re-run the check
    # before existing findings can be resolved.
    normalized = [_normalize_cluster_member(m) for m in members]
    normalized = [m for m in normalized if m and m.get("album_id")]
    if not normalized:
        raise HTTPException(status_code=400, detail="Cluster members missing album_id")

    applied = inconsistent_artist_sort.apply_cluster_sort(
        members=normalized, sort_value=sort_value,
    )
    distinct_albums = len({m["album_id"] for m in normalized})
    return {
        "ok":             applied > 0,
        "applied":        applied,
        "cluster_size":   len(normalized),
        "distinct_albums": distinct_albums,
        "sort_value":     sort_value,
        "message":        (
            f"Wrote sort={sort_value!r} to {applied} of "
            f"{distinct_albums} album(s) in the cluster."
        ),
    }


def _normalize_cluster_member(m: dict) -> dict | None:
    """Coerce a cluster-member dict into the v11+ shape regardless of
    whether it came from a fresh run (already correct) or a legacy
    finding stored under the v10 schema. Returns None for entries
    that are too malformed to act on."""
    if not isinstance(m, dict):
        return None
    if "kind" in m:
        if m.get("kind") == "track":
            fns = m.get("filenames") or []
            if not isinstance(fns, list):
                fns = [fns] if fns else []
            return {**m, "filenames": [str(x) for x in fns if x]}
        return m
    # Legacy v10: album-kind entry with field names "id" /
    # "album_artist" / "album_artist_sort".
    album_id = m.get("id") or m.get("album_id")
    if not album_id:
        return None
    return {
        "kind":         "album",
        "album_id":     album_id,
        "dir_name":     m.get("dir_name"),
        "album_artist": m.get("album_artist"),
        "sort":         m.get("album_artist_sort") or "",
    }


@router.post("/api/checks/filename-mismatch/findings/{finding_id}/apply")
def apply_filename_rename(
    finding_id: int,
    payload: FilenameRenamePayload,
) -> dict[str, Any]:
    """Custom action for the filename-mismatch check: execute the
    user-confirmed rename plan. Validates the finding belongs to the
    right check, then runs the renames through filename_mismatch.
    execute_renames (two-pass + track-tag migration + post-rename
    rescan). When `dir_rename` is supplied, the album directory is
    renamed first and FK references migrate to the new id."""
    from ..checks import filename_mismatch
    finding = checks_runner.get_finding(finding_id)
    if finding is None or finding["check_id"] != "filename-mismatch":
        raise HTTPException(status_code=404, detail="Finding not found")
    album_id = finding.get("album_id")
    if not album_id:
        raise HTTPException(status_code=400, detail="Finding has no album")
    if not payload.plan and payload.dir_rename is None:
        raise HTTPException(status_code=400, detail="plan or dir_rename is required")
    plan_dicts = [{"old": e.old, "new": e.new} for e in payload.plan]
    dir_rename_dict = (
        {"old": payload.dir_rename.old, "new": payload.dir_rename.new}
        if payload.dir_rename else None
    )
    result = filename_mismatch.execute_renames(
        album_id=album_id, plan=plan_dicts, dir_rename=dir_rename_dict,
    )
    if not result.get("ok") and result.get("renamed", 0) == 0 and not result.get("dir_renamed"):
        raise HTTPException(
            status_code=409,
            detail=result.get("message") or "Rename failed",
        )
    # Post-apply check rerun so the finding clears.
    #
    # Scope-limited to JUST the album we touched: filename-mismatch
    # reads FLAC tags per file and a whole-library rerun takes minutes
    # on slow storage (SMB). A per-album scope reads ~12 files →
    # sub-second. The runner's scope-aware auto-resolve fires for
    # findings on the listed album only — perfect for the single-album
    # post-fix case where unrelated findings shouldn't be touched.
    #
    # `effective_id` is the post-dir-rename album_id when the apply
    # also renamed the directory; otherwise the original. Use the new
    # id so the rerun's scope hits the now-canonical album row.
    effective_id = (result.get("new_album_id") or album_id) if result.get("ok") else album_id
    try:
        cfg = config_module.load_config()
        if cfg.library_dir:
            library_path = str(Path(cfg.library_dir).expanduser().resolve())
            try:
                asyncio.run(checks_runner.run_check(
                    "filename-mismatch",
                    library_path=library_path,
                    scope_kind="albums",
                    album_ids=[effective_id],
                ))
            except Exception:
                _log.exception(
                    "post-rename filename-mismatch rerun failed (continuing)",
                )
    except Exception:
        _log.exception("post-rename check rerun failed (continuing)")
    return result


@router.post("/api/checks/duplicate-tracks/findings/{finding_id}/delete-file")
def delete_duplicate_file(finding_id: int, payload: DeleteDuplicateFilePayload) -> dict[str, Any]:
    """Custom action for the duplicate-tracks check: delete one of the
    duplicate FLACs that the user has chosen to remove.

    Validates the filename is one of the cluster's listed duplicates (so a
    misdirected request can't accidentally nuke an unrelated file), deletes
    the file from disk, drops the matching track_fingerprints row, and
    triggers a post-scan rerun so the finding auto-resolves once the
    cluster is down to 1 file.
    """
    finding = checks_runner.get_finding(finding_id)
    if finding is None or finding["check_id"] != "duplicate-tracks":
        raise HTTPException(status_code=404, detail="Finding not found")
    detail = finding.get("detail") or {}
    listed_filenames = list(detail.get("filenames") or [])
    if payload.filename not in listed_filenames:
        raise HTTPException(
            status_code=400,
            detail=f"Filename {payload.filename!r} is not one of this finding's duplicates",
        )

    album_id = finding.get("album_id")
    if not album_id:
        raise HTTPException(status_code=400, detail="Finding has no album to act on")

    album = cached_album_dict(album_id)
    if album is None:
        raise HTTPException(status_code=404, detail=f"Album {album_id!r} not in cache")
    album_path = Path(album.get("path") or "")
    target = album_path / payload.filename
    # Sanity: refuse to delete anything outside the album dir.
    try:
        target_resolved = target.resolve(strict=True)
        album_resolved = album_path.resolve(strict=True)
        file_already_gone = False
    except FileNotFoundError:
        # Idempotent recovery: the file is missing on disk, but the
        # finding's detail still references it (and the fingerprint row
        # may still be present). Treat the click as "finish the cleanup."
        target_resolved = None
        album_resolved = album_path.resolve()
        file_already_gone = True
    if not file_already_gone and album_resolved not in target_resolved.parents:
        raise HTTPException(status_code=400, detail="Refusing to delete file outside album directory")

    # Soft-delete: move the file into the app's trash directory rather
    # than unlinking it permanently. The user can restore from
    # ~/.musaic/trash/<album_dir>/<filename> if they change
    # their mind.
    trashed_path: Path | None = None
    if not file_already_gone:
        from .. import trash as trash_module
        try:
            trashed_path = trash_module.send_to_trash(
                target_resolved,
                album_dir_name=album.get("dir_name"),
            )
        except trash_module.TrashError as e:
            raise HTTPException(status_code=500, detail=str(e))

    cache_module.delete_track_fingerprint(album_id, payload.filename)

    # Re-scan THIS ONE album so the cache reflects the deleted file.
    try:
        fresh = scan_one_album(album_path)
        if fresh is None:
            cache_module.delete_album(album_id)
        else:
            cfg = config_module.load_config()
            if not cfg.library_dir:
                raise RuntimeError("No library directory configured for post-delete rescan")
            library_dir = str(Path(cfg.library_dir).expanduser().resolve())
            cache_module.upsert_album(library_dir, album_to_dict(fresh))
    except Exception:
        _log.exception("post-delete rescan failed for %s (continuing)", album_id)

    # Run the post-scan tail so the duplicate-tracks finding (and any
    # related findings — track-gaps may also clear) auto-resolves.
    try:
        cfg = config_module.load_config()
        if cfg.library_dir:
            run_post_scan_checks(str(Path(cfg.library_dir).expanduser().resolve()))
    except Exception:
        _log.exception("post-delete check rerun failed (continuing)")

    if file_already_gone:
        return {
            "ok": True,
            "message": (
                f"{payload.filename} was already gone from disk — cleaned up "
                f"the cache so this duplicate finding will clear."
            ),
            "trashed_to": None,
        }
    return {
        "ok": True,
        "message": f"Moved {payload.filename} to trash",
        "trashed_to": str(trashed_path) if trashed_path else None,
    }


@router.post("/api/checks/{check_id}/quick-fix-all")
async def quick_fix_all(check_id: str) -> StreamingResponse:
    """SSE-streamed bulk run of try_quick_fix() across every open finding
    for this check. Lets the user trim the deterministic-fix portion of a
    long list without committing AI work for the rest.

    Pacing: one second between MB calls (MusicBrainz allows 1 req/s for
    unauthenticated traffic). For 80 findings that's ~80+ seconds; we
    stream per-finding outcomes so the UI can show progress.

    Events emitted (event name → payload):
      - `start`    : { total }
      - `progress` : { finding_id, outcome, message }
                       outcome ∈ "fixed" | "skipped" | "error" | "not-applicable"
      - `done`     : { total, fixed, skipped, errors, not_applicable }
      - `error`    : { detail }   (only on a fatal mid-run failure)
    """
    cls = checks_module.get(check_id)
    if cls is None:
        raise HTTPException(status_code=404, detail=f"Unknown check_id: {check_id!r}")

    findings = checks_runner.list_findings(check_id=check_id)
    finding_ids = [f["id"] for f in findings]

    async def event_gen():
        # Flip the bulk-mode contextvar so apply_direct() skips its
        # per-apply post-scan rerun. We do one batch rerun at the very
        # end, which is N times faster than running it per-finding.
        from ..checks._apply import in_bulk_run
        token = in_bulk_run.set(True)

        yield sse_event("start", {"total": len(finding_ids)})
        counts = {"fixed": 0, "skipped": 0, "errors": 0, "not_applicable": 0}
        try:
            for i, fid in enumerate(finding_ids):
                # MB rate limit: 1 req/s. Skip the sleep on the first one.
                if i > 0:
                    await asyncio.sleep(1.0)
                try:
                    # try_quick_fix is sync (calls requests.* + apply); offload
                    # so the event loop stays responsive for SSE flushes.
                    # Use copy_context() so the in_bulk_run flag follows the
                    # call into the worker thread.
                    ctx = contextvars.copy_context()
                    result = await asyncio.to_thread(
                        ctx.run, checks_runner.try_quick_fix_finding, fid,
                    )
                except Exception as e:
                    counts["errors"] += 1
                    yield sse_event("progress", {
                        "finding_id": fid,
                        "outcome": "error",
                        "message": f"{type(e).__name__}: {e}",
                    })
                    continue
                if result is None:
                    counts["not_applicable"] += 1
                    yield sse_event("progress", {
                        "finding_id": fid,
                        "outcome": "not-applicable",
                        "message": "Check has no quick-fix path",
                    })
                elif result.ok:
                    counts["fixed"] += 1
                    yield sse_event("progress", {
                        "finding_id": fid,
                        "outcome": "fixed",
                        "message": result.message,
                    })
                else:
                    counts["skipped"] += 1
                    yield sse_event("progress", {
                        "finding_id": fid,
                        "outcome": "skipped",
                        "message": result.message,
                    })

            # Single post-scan rerun for the whole batch. MUST use the async
            # variant — we're inside an async coroutine; the sync variant
            # internally calls asyncio.run() which would raise here.
            #
            # Fire even when nothing was "fixed" from the dispatcher's POV:
            # try_quick_fix paths (no_cover_art, track_gaps, unidentified-
            # album) routinely side-effect cache state (MB anchor backfill,
            # track_fingerprints rows) via mb_lookup/fingerprint even when
            # the high-level result is skipped — those writes can satisfy
            # findings on OTHER checks that would otherwise stay stale.
            if len(finding_ids) > 0:
                try:
                    library_path = library_path_for_checks()
                    await run_post_scan_checks_async(library_path)
                except Exception:
                    _log.exception("post-bulk check rerun failed; continuing")

            yield sse_event("done", {"total": len(finding_ids), **counts})
        except asyncio.CancelledError:
            return
        except Exception as e:
            yield sse_event("error", {"detail": f"{type(e).__name__}: {e}"})
        finally:
            in_bulk_run.reset(token)

    return StreamingResponse(
        event_gen(),
        media_type="text/event-stream",
        headers={
            "Cache-Control": "no-cache, no-transform",
            "X-Accel-Buffering": "no",
            "Connection": "keep-alive",
        },
    )
