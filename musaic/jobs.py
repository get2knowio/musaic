"""AI job queue.

Persistent queue of AI suggestion requests so the user can fire many of them
without sitting and waiting on each. The background worker (in main.py)
picks queued jobs one at a time and runs them serially.

Job lifecycle:
    queued → running → completed → acted_on        (Apply or Discard from UI)
                     ↘ failed                       (with error stored)

Job types:
    "metadata"        — just `ai.suggest_metadata`
    "cover"           — just `ai.suggest_cover`
    "metadata+cover"  — both, results merged. Used when "Suggest Fixes" is
                        clicked on an album with no cover art.

Dedupe rule (one job per album): `enqueue()` rejects if there's already a
job for this album in any of [queued, running, completed]. That is, the
user has to either Apply, Discard, or explicitly delete the existing job
before queuing a new one.
"""
from __future__ import annotations

import json
import secrets
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Any, Iterable

from . import db as db_module


# ---------- Constants ----------

JOB_TYPES = ("metadata", "cover", "metadata+cover")
STATUSES = ("queued", "running", "completed", "failed", "acted_on")
# A job in any of these states is "active" — counts against the one-per-album
# dedupe rule. Failed jobs do NOT block re-enqueue (user can retry).
ACTIVE_STATUSES = ("queued", "running", "completed")


class JobConflictError(Exception):
    """Raised by `enqueue` when an active job already exists for the album."""
    def __init__(self, existing_job_id: str, status: str):
        super().__init__(f"Active job {existing_job_id} ({status}) already exists for this album")
        self.existing_job_id = existing_job_id
        self.status = status


@dataclass
class Job:
    id: str
    album_id: str
    album_path: str
    album_title: str
    job_type: str
    hint: str
    status: str
    result_json: str | None = None      # JSON-encoded {metadata?, cover?}
    error: str | None = None
    acted_on_action: str | None = None  # "applied" | "discarded"
    created_at: str = ""
    started_at: str | None = None
    completed_at: str | None = None
    acted_on_at: str | None = None


# ---------- Helpers ----------

def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")


def _new_id() -> str:
    return secrets.token_hex(8)  # 16-char hex


def _row_to_job(row) -> Job:
    return Job(
        id=row["id"],
        album_id=row["album_id"],
        album_path=row["album_path"],
        album_title=row["album_title"] or "",
        job_type=row["job_type"],
        hint=row["hint"] or "",
        status=row["status"],
        result_json=row["result_json"],
        error=row["error"],
        acted_on_action=row["acted_on_action"],
        created_at=row["created_at"],
        started_at=row["started_at"],
        completed_at=row["completed_at"],
        acted_on_at=row["acted_on_at"],
    )


def job_to_dict(job: Job) -> dict[str, Any]:
    """API-shaped job dict. `result` is parsed JSON if present."""
    out: dict[str, Any] = {
        "id": job.id,
        "album_id": job.album_id,
        "album_path": job.album_path,
        "album_title": job.album_title,
        "job_type": job.job_type,
        "hint": job.hint,
        "status": job.status,
        "error": job.error,
        "acted_on_action": job.acted_on_action,
        "created_at": job.created_at,
        "started_at": job.started_at,
        "completed_at": job.completed_at,
        "acted_on_at": job.acted_on_at,
    }
    if job.result_json:
        try:
            out["result"] = json.loads(job.result_json)
        except Exception:
            out["result"] = None
    else:
        out["result"] = None
    return out


# ---------- Public API ----------

def enqueue(*, album_id: str, album_path: str, album_title: str,
            job_type: str, hint: str = "") -> Job:
    """Create a new queued job. Raises JobConflictError if an active job
    already exists for `album_id`."""
    if job_type not in JOB_TYPES:
        raise ValueError(f"Unknown job_type {job_type!r}; expected one of {JOB_TYPES}")

    with db_module.connect() as conn:
        existing = conn.execute(
            f"SELECT id, status FROM ai_jobs WHERE album_id = ? "
            f"AND status IN ({','.join('?' * len(ACTIVE_STATUSES))}) LIMIT 1",
            (album_id, *ACTIVE_STATUSES),
        ).fetchone()
        if existing is not None:
            raise JobConflictError(existing["id"], existing["status"])

        job = Job(
            id=_new_id(),
            album_id=album_id,
            album_path=album_path,
            album_title=album_title,
            job_type=job_type,
            hint=hint or "",
            status="queued",
            created_at=_now_iso(),
        )
        with db_module.transaction(conn):
            conn.execute(
                """
                INSERT INTO ai_jobs (id, album_id, album_path, album_title, job_type,
                                     hint, status, created_at)
                VALUES (?, ?, ?, ?, ?, ?, 'queued', ?)
                """,
                (job.id, job.album_id, job.album_path, job.album_title,
                 job.job_type, job.hint, job.created_at),
            )
        return job


def list_jobs(*, statuses: Iterable[str] | None = None,
              limit: int = 200) -> list[Job]:
    """Return jobs matching `statuses` (default: all), newest first."""
    sql = "SELECT * FROM ai_jobs"
    params: list[Any] = []
    if statuses is not None:
        statuses = list(statuses)
        if not statuses:
            return []
        sql += f" WHERE status IN ({','.join('?' * len(statuses))})"
        params.extend(statuses)
    sql += " ORDER BY created_at DESC LIMIT ?"
    params.append(limit)
    with db_module.connect() as conn:
        return [_row_to_job(r) for r in conn.execute(sql, params)]


def get_job(job_id: str) -> Job | None:
    with db_module.connect() as conn:
        row = conn.execute("SELECT * FROM ai_jobs WHERE id = ?", (job_id,)).fetchone()
        return _row_to_job(row) if row else None


def get_active_job_for_album(album_id: str) -> Job | None:
    """Return the most recent active (queued/running/completed-not-acted-on)
    job for an album, or None. Used by the detail sheet to pick up a job's
    result when an album is opened."""
    with db_module.connect() as conn:
        row = conn.execute(
            f"SELECT * FROM ai_jobs WHERE album_id = ? "
            f"AND status IN ({','.join('?' * len(ACTIVE_STATUSES))}) "
            f"ORDER BY created_at DESC LIMIT 1",
            (album_id, *ACTIVE_STATUSES),
        ).fetchone()
        return _row_to_job(row) if row else None


def get_next_queued() -> Job | None:
    """Worker call — return the oldest queued job, or None if queue empty."""
    with db_module.connect() as conn:
        row = conn.execute(
            "SELECT * FROM ai_jobs WHERE status = 'queued' "
            "ORDER BY created_at ASC LIMIT 1"
        ).fetchone()
        return _row_to_job(row) if row else None


def get_next_queued_excluding(exclude_ids: Iterable[str]) -> Job | None:
    """Like `get_next_queued`, but skips IDs in `exclude_ids`. Used by the
    concurrent worker so already-dispatched jobs (still flipping from
    queued→running asynchronously) don't get picked up twice."""
    excluded = list(exclude_ids)
    if not excluded:
        return get_next_queued()
    placeholders = ",".join("?" * len(excluded))
    with db_module.connect() as conn:
        row = conn.execute(
            f"SELECT * FROM ai_jobs WHERE status = 'queued' "
            f"AND id NOT IN ({placeholders}) "
            f"ORDER BY created_at ASC LIMIT 1",
            tuple(excluded),
        ).fetchone()
        return _row_to_job(row) if row else None


def count_running() -> int:
    """Number of jobs currently in 'running' state."""
    with db_module.connect() as conn:
        row = conn.execute(
            "SELECT COUNT(*) AS n FROM ai_jobs WHERE status = 'running'"
        ).fetchone()
        return int(row["n"]) if row else 0


def count_queued() -> int:
    """Number of jobs in 'queued' state, awaiting dispatch."""
    with db_module.connect() as conn:
        row = conn.execute(
            "SELECT COUNT(*) AS n FROM ai_jobs WHERE status = 'queued'"
        ).fetchone()
        return int(row["n"]) if row else 0


def reset_stuck_running() -> int:
    """Move any 'running' jobs back to 'queued' on worker startup.

    The only legitimate way for a job to be 'running' is if the worker is
    currently processing it. If we're starting fresh (or recovering from a
    crash / unclean shutdown), nothing is actually running — those rows are
    leftover state and would otherwise be invisible (worker only picks up
    'queued' jobs). Returns the number of rows reset."""
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            cur = conn.execute(
                "UPDATE ai_jobs SET status = 'queued', started_at = NULL "
                "WHERE status = 'running'"
            )
            return cur.rowcount


def mark_running(job_id: str) -> None:
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            conn.execute(
                "UPDATE ai_jobs SET status = 'running', started_at = ? "
                "WHERE id = ? AND status = 'queued'",
                (_now_iso(), job_id),
            )


def mark_completed(job_id: str, result: dict[str, Any]) -> None:
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            conn.execute(
                "UPDATE ai_jobs SET status = 'completed', "
                "result_json = ?, completed_at = ? WHERE id = ?",
                (json.dumps(result), _now_iso(), job_id),
            )


def mark_failed(
    job_id: str,
    error: str,
    *,
    failure_excerpt: str | None = None,
) -> None:
    """Mark a job failed with the SDK error message.

    `failure_excerpt` is the captured Claude transcript leading up to
    the failure — surfaced in the Jobs UI so the user can see what
    Claude said (or didn't say) before the call blew up. Stored under
    `result_json.failure_excerpt` so we don't need a schema column for
    a debugging-only field. Set to None or empty string to skip.
    """
    result_json: str | None = None
    if failure_excerpt:
        result_json = json.dumps({"failure_excerpt": failure_excerpt})
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            conn.execute(
                "UPDATE ai_jobs SET status = 'failed', error = ?, "
                "result_json = ?, completed_at = ? WHERE id = ?",
                (error, result_json, _now_iso(), job_id),
            )


def mark_acted_on(job_id: str, action: str) -> None:
    """Mark a completed job as acted on. `action` should be 'applied' or 'discarded'."""
    if action not in ("applied", "discarded"):
        raise ValueError(f"action must be 'applied' or 'discarded', got {action!r}")
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            conn.execute(
                "UPDATE ai_jobs SET status = 'acted_on', acted_on_action = ?, "
                "acted_on_at = ? WHERE id = ?",
                (action, _now_iso(), job_id),
            )


def retry_failed(job_id: str) -> Job | None:
    """Reset a failed job back to 'queued' so the worker picks it up again.
    Returns the updated job, or None if the job doesn't exist or isn't failed.

    We mutate in place rather than spawning a fresh row so the user sees the
    original entry resume — no row proliferation, no hunt for "which one is
    the latest". `created_at` is bumped to now so the new attempt sorts at
    the bottom of the queue (most recent attempt last).
    """
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            job = get_job(job_id)
            if job is None or job.status != "failed":
                return None
            now = _now_iso()
            conn.execute(
                "UPDATE ai_jobs SET status = 'queued', error = NULL, "
                "started_at = NULL, completed_at = NULL, created_at = ? "
                "WHERE id = ?",
                (now, job_id),
            )
        return get_job(job_id)


def delete_job(job_id: str) -> bool:
    """Delete a single job. Returns True if a row was removed."""
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            cur = conn.execute("DELETE FROM ai_jobs WHERE id = ?", (job_id,))
            return cur.rowcount > 0


def delete_jobs_by_status(statuses: Iterable[str]) -> int:
    """Bulk delete (e.g. clear all `acted_on` history). Returns rows deleted."""
    statuses = list(statuses)
    if not statuses:
        return 0
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            cur = conn.execute(
                f"DELETE FROM ai_jobs WHERE status IN ({','.join('?' * len(statuses))})",
                tuple(statuses),
            )
            return cur.rowcount
