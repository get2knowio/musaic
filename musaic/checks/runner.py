"""Execution + persistence layer for health checks.

Responsibilities:

  - Insert a check_runs row when a check starts.
  - Iterate the check's findings, upserting each into check_findings
    (UNIQUE(check_id, album_id, summary) handles dedupe — re-detecting a
    known finding bumps last_seen_at, new findings get inserted).
  - When the run's scope.can_resolve is True, mark any findings for this
    check that we did NOT see this run as resolved (resolved_at = now).
  - Update the check_runs row on completion or failure.
  - Notify the SSE stream + any waiters via the module-level
    `_checks_changed` asyncio.Event.

Public surface:
  - run_check(check_id, scope_kind, *, library_path, album_ids=None)
  - list_checks_with_summary()
  - get_check_with_runs(check_id)
  - list_findings(check_id=None, album_id=None, include_ignored=False, include_resolved=False)
  - ignore_finding(finding_id, reason)
  - unignore_finding(finding_id)
  - get_finding(finding_id)
  - notifier — `notify_changed()` and `subscribe()` for SSE consumers
"""
from __future__ import annotations

import asyncio
import json
import logging
import time
from datetime import datetime, timezone
from typing import Any

from .. import db as db_module
from . import REGISTRY
from . import get as _get_check_class
from .base import Check, CheckScope, Finding, FixResult

log = logging.getLogger(__name__)


# ---------- Notification primitive ----------

# Event the SSE endpoint awaits. Set by run_check / mutation helpers; the
# stream wakes, drains the latest snapshot, and yields it. Modeled on the
# existing _jobs_changed pattern in main.py — same semantics.
_changed = asyncio.Event()


def notify_changed() -> None:
    _changed.set()


def changed_event() -> asyncio.Event:
    """Return the module-level event so an SSE handler can `await .wait()`."""
    return _changed


# ---------- Time helper ----------

def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")


# ---------- Row → dict conversion ----------

def _run_row_to_dict(row) -> dict[str, Any]:
    return {
        "id":           row["id"],
        "check_id":     row["check_id"],
        "started_at":   row["started_at"],
        "completed_at": row["completed_at"],
        "status":       row["status"],
        "library_path": row["library_path"],
        "scope":        row["scope"],
        "issue_count":  row["issue_count"],
        "error":        row["error"],
        "duration_ms":  row["duration_ms"],
    }


def _finding_row_to_dict(row) -> dict[str, Any]:
    detail: Any = None
    if row["detail_json"]:
        try:
            detail = json.loads(row["detail_json"])
        except Exception:
            detail = None
    return {
        "id":             row["id"],
        "check_id":       row["check_id"],
        "album_id":       row["album_id"],
        "severity":       row["severity"],
        "summary":        row["summary"],
        "detail":         detail,
        "first_seen_at":  row["first_seen_at"],
        "last_seen_at":   row["last_seen_at"],
        "resolved_at":    row["resolved_at"],
        "ignored_at":     row["ignored_at"],
        "ignored_reason": row["ignored_reason"],
    }


# ---------- Run a check ----------

async def run_check(
    check_id: str,
    *,
    library_path: str,
    scope_kind: str = "all",     # "all" | "incremental" | "albums"
    album_ids: list[str] | None = None,
) -> dict[str, Any]:
    """Execute a check and return its `check_runs` row as a dict.

    Streams findings into the DB as they arrive. Async so the FastAPI
    handler can offload to a worker thread cleanly via asyncio.to_thread
    when the check itself is sync.
    """
    cls = _get_check_class(check_id)
    if cls is None:
        raise ValueError(f"Unknown check_id: {check_id!r}")

    # Build scope. We load the album list here so checks don't have to
    # touch the cache themselves (most won't need it; the SQL-pure ones
    # ignore scope.albums entirely).
    #
    # Missing albums (missing_since IS NOT NULL) are filtered out: the
    # check can't act on them anyway (FS reads would fail), and showing
    # findings for albums the user can't see in the UI is just noise.
    # Existing findings on Missing albums STAY OPEN — the scope-aware
    # auto-resolve in `_resolve_unseen` only resolves findings whose
    # album_id is in this scope, and Missing albums are excluded, so
    # their findings are preserved until the album returns and the
    # next scope-aware run re-evaluates them.
    #
    # SQL-pure checks that bypass scope.albums (inconsistent-artist-sort,
    # tags-out-of-sync, etc.) need to add their own `AND missing_since
    # IS NULL` clause — see those check modules.
    from .. import cache as cache_module
    snapshot = cache_module.load_snapshot(library_path)
    all_albums = [
        a for a in snapshot.albums_by_path.values()
        if not a.get("missing_since")
    ]
    if scope_kind == "albums":
        if not album_ids:
            raise ValueError("scope_kind='albums' requires non-empty album_ids")
        wanted = set(album_ids)
        albums = [a for a in all_albums if a.get("id") in wanted]
    else:
        albums = all_albums
    scope = CheckScope(library_path=library_path, kind=scope_kind, albums=albums)  # type: ignore[arg-type]

    # Insert the run row.
    started_at = _now_iso()
    started_perf = time.perf_counter()
    scope_str = scope_kind if scope_kind != "albums" else json.dumps(album_ids)
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            cur = conn.execute(
                "INSERT INTO check_runs (check_id, started_at, status, library_path, scope, issue_count) "
                "VALUES (?, ?, 'running', ?, ?, 0)",
                (check_id, started_at, library_path, scope_str),
            )
            run_id = cur.lastrowid
    notify_changed()

    seen_finding_ids: set[int] = set()
    issue_count = 0
    check_instance: Check = cls()

    try:
        # The check itself may be sync (most are: SQL or filesystem). Run it
        # in a worker thread to avoid blocking the event loop. The iterator
        # yields Finding objects which we drain on the main loop side via
        # an asyncio.Queue bridge.
        async for finding in _drain_check(check_instance, scope):
            fid = _upsert_finding(check_id, finding)
            seen_finding_ids.add(fid)
            issue_count += 1
            # Don't notify per-finding — a check that yields N findings
            # would emit N SSE state events, each triggering a fresh
            # GET /api/checks/findings refetch on the frontend. For a
            # check like inconsistent-artist-sort yielding 100+ findings
            # this DDoS-es the user's own UI with a wall of refetches.
            # The end-of-run notify_changed() below is sufficient — the
            # SSE consumer reads the latest state on each wake regardless
            # of how many notifies fired during the run.

        # Auto-resolve when scope.can_resolve. Scoped reruns
        # (scope_kind="albums") pass the album_id whitelist so we don't
        # touch findings on unrelated albums.
        if scope.can_resolve:
            _resolve_unseen(
                check_id,
                seen_ids=seen_finding_ids,
                since=started_at,
                scope_album_ids=scope.resolve_scope_album_ids,
            )

        completed_at = _now_iso()
        duration_ms = int((time.perf_counter() - started_perf) * 1000)
        with db_module.connect() as conn:
            with db_module.transaction(conn):
                conn.execute(
                    "UPDATE check_runs SET status='completed', completed_at=?, "
                    "issue_count=?, duration_ms=? WHERE id=?",
                    (completed_at, issue_count, duration_ms, run_id),
                )
        notify_changed()
        log.info(
            "check %s completed in %dms (%d findings, scope=%s)",
            check_id, duration_ms, issue_count, scope_kind,
        )
    except Exception as e:
        completed_at = _now_iso()
        duration_ms = int((time.perf_counter() - started_perf) * 1000)
        log.exception("check %s failed", check_id)
        with db_module.connect() as conn:
            with db_module.transaction(conn):
                conn.execute(
                    "UPDATE check_runs SET status='failed', completed_at=?, "
                    "error=?, duration_ms=? WHERE id=?",
                    (completed_at, str(e), duration_ms, run_id),
                )
        notify_changed()

    # Return the final row.
    return _get_run_dict(run_id)


async def _drain_check(check: Check, scope: CheckScope):
    """Bridge a (possibly sync) check.run() iterator into an async iterator.

    Runs the iteration in a worker thread; uses a queue to ferry findings
    back to the event loop one at a time. Bound queue keeps memory in
    check if the producer is much faster than the upserts."""
    queue: asyncio.Queue[Finding | object] = asyncio.Queue(maxsize=64)
    SENTINEL = object()
    loop = asyncio.get_running_loop()

    def _producer():
        try:
            for f in check.run(scope):
                # Block until consumer takes; preserves backpressure.
                asyncio.run_coroutine_threadsafe(queue.put(f), loop).result()
        finally:
            asyncio.run_coroutine_threadsafe(queue.put(SENTINEL), loop).result()

    task = loop.run_in_executor(None, _producer)
    try:
        while True:
            item = await queue.get()
            if item is SENTINEL:
                break
            yield item  # type: ignore[misc]
    finally:
        # Surface a producer-side exception (run_in_executor wraps it on the
        # future; awaiting it here re-raises into our caller).
        await task


# ---------- Finding upsert / resolve ----------

def _upsert_finding(check_id: str, finding: Finding) -> int:
    """Insert or refresh a finding. Returns its row id.

    Dedup key: (check_id, album_id, summary). On match we bump last_seen_at,
    refresh detail/severity, and clear `resolved_at` (issue is back). We
    deliberately leave `ignored_at`/`ignored_reason` alone — once a user
    ignores a finding it stays ignored across re-detections.
    """
    detail_json = json.dumps(finding.detail) if finding.detail is not None else None
    now = _now_iso()
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            row = conn.execute(
                "SELECT id FROM check_findings "
                "WHERE check_id=? AND album_id IS ? AND summary=?",
                (check_id, finding.album_id, finding.summary),
            ).fetchone()
            if row is not None:
                fid = row["id"]
                conn.execute(
                    "UPDATE check_findings SET last_seen_at=?, severity=?, "
                    "detail_json=?, resolved_at=NULL WHERE id=?",
                    (now, finding.severity, detail_json, fid),
                )
                return fid
            cur = conn.execute(
                "INSERT INTO check_findings (check_id, album_id, severity, summary, "
                "detail_json, first_seen_at, last_seen_at) "
                "VALUES (?, ?, ?, ?, ?, ?, ?)",
                (check_id, finding.album_id, finding.severity, finding.summary,
                 detail_json, now, now),
            )
            return cur.lastrowid


def _resolve_unseen(
    check_id: str,
    *,
    seen_ids: set[int],
    since: str,
    scope_album_ids: list[str] | None = None,
) -> int:
    """Mark findings for `check_id` not in `seen_ids` (and not previously
    resolved or ignored) as resolved. Returns row count.

    `since` is recorded for diagnostics but no longer filters — the
    `id NOT IN seen_ids` clause is authoritative. (We don't run two
    concurrent runs of the same check, so any row not freshly upserted
    here is by definition stale.) The previous `last_seen_at < since`
    filter could miss rows that ticked into the same second as the run's
    start timestamp.

    `scope_album_ids`: when non-None, limits the auto-resolve UPDATE to
    findings whose album_id is in the list. Used for scope="albums"
    reruns (post-fix on a single album) where we've only examined a
    subset of the library and shouldn't sweep-resolve findings on
    untouched albums.

    TODO: the "no two concurrent runs" assumption is currently enforced
    only by the dispatcher in `main._run_post_scan_checks` /
    `_run_post_scan_checks_async`, which iterate the `runs_during_scan`
    checks one-at-a-time (sequential `asyncio.run` / `await`). If those
    are ever parallelized via `asyncio.gather`, two threads could enter
    `run_check(check_id)` overlapping each other and the unseen-resolve
    pass here would mark each other's freshly-upserted rows stale. Add a
    per-check_id asyncio.Lock at the runner level before parallelizing.
    """
    del since  # documented above; intentionally unused
    now = _now_iso()
    where = "check_id=? AND resolved_at IS NULL AND ignored_at IS NULL"
    args: list[Any] = [check_id]
    if seen_ids:
        where += f" AND id NOT IN ({','.join('?' * len(seen_ids))})"
        args.extend(seen_ids)
    # Scope-limited auto-resolve: when the run only examined a subset of
    # albums (scope_kind="albums"), don't resolve findings on unrelated
    # albums — those weren't even checked this pass.
    if scope_album_ids is not None:
        if not scope_album_ids:
            # Empty scope = nothing examined = nothing to resolve.
            return 0
        where += f" AND album_id IN ({','.join('?' * len(scope_album_ids))})"
        args.extend(scope_album_ids)
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            cur = conn.execute(
                f"UPDATE check_findings SET resolved_at=? WHERE {where}",
                (now, *args),
            )
            return cur.rowcount


# ---------- Read helpers ----------

def _get_run_dict(run_id: int) -> dict[str, Any]:
    with db_module.connect() as conn:
        row = conn.execute("SELECT * FROM check_runs WHERE id=?", (run_id,)).fetchone()
        if row is None:
            raise ValueError(f"check_run id {run_id} vanished after insert")
        return _run_row_to_dict(row)


def list_checks_with_summary() -> list[dict[str, Any]]:
    """List every registered check + its last-run summary + open finding count.

    Used by GET /api/checks. Cheap — three queries total regardless of how
    many checks there are.
    """
    out: list[dict[str, Any]] = []
    with db_module.connect() as conn:
        # Last run per check_id.
        last_runs: dict[str, dict[str, Any]] = {}
        for row in conn.execute(
            "SELECT cr.* FROM check_runs cr "
            "INNER JOIN (SELECT check_id, MAX(id) AS max_id FROM check_runs GROUP BY check_id) m "
            "ON cr.id = m.max_id"
        ):
            last_runs[row["check_id"]] = _run_row_to_dict(row)
        # Open finding counts.
        counts: dict[str, int] = {}
        for row in conn.execute(
            "SELECT check_id, COUNT(*) AS n FROM check_findings "
            "WHERE resolved_at IS NULL AND ignored_at IS NULL "
            "GROUP BY check_id"
        ):
            counts[row["check_id"]] = int(row["n"])
    for cls in REGISTRY:
        m = cls.meta
        out.append({
            "meta": {
                "id":                m.id,
                "label":             m.label,
                "description":       m.description,
                "category":          m.category,
                "cost":              m.cost,
                "runs_during_scan":  m.runs_during_scan,
                "fix_kind":          m.fix_kind,
                "bulk_action_label": m.bulk_action_label,
            },
            "open_count":           counts.get(m.id, 0),
            "last_run":             last_runs.get(m.id),
        })
    return out


def get_check_with_runs(check_id: str, *, run_limit: int = 10) -> dict[str, Any] | None:
    """List the last `run_limit` runs + open findings for a single check."""
    cls = _get_check_class(check_id)
    if cls is None:
        return None
    with db_module.connect() as conn:
        runs = [
            _run_row_to_dict(r) for r in conn.execute(
                "SELECT * FROM check_runs WHERE check_id=? ORDER BY id DESC LIMIT ?",
                (check_id, run_limit),
            )
        ]
        findings = [
            _finding_row_to_dict(r) for r in conn.execute(
                "SELECT * FROM check_findings WHERE check_id=? "
                "AND resolved_at IS NULL AND ignored_at IS NULL "
                "ORDER BY last_seen_at DESC",
                (check_id,),
            )
        ]
    m = cls.meta
    return {
        "meta": {
            "id":                m.id,
            "label":             m.label,
            "description":       m.description,
            "category":          m.category,
            "cost":              m.cost,
            "runs_during_scan":  m.runs_during_scan,
            "fix_kind":          m.fix_kind,
            "bulk_action_label": m.bulk_action_label,
        },
        "runs":     runs,
        "findings": findings,
    }


def list_findings(
    *,
    check_id: str | None = None,
    album_id: str | None = None,
    include_ignored: bool = False,
    include_resolved: bool = False,
    limit: int = 10000,
) -> list[dict[str, Any]]:
    # Limit was 500 — too low for libraries with several open checks
    # each emitting hundreds of findings. The Health detail page
    # filters this list client-side by check_id, so a low cap meant
    # one busy check could push another's findings off the back of
    # the page (showing "No open findings" while the index card
    # showed N>0). 10000 is large enough that no realistic library
    # truncates here, while still bounding response size.
    where: list[str] = []
    params: list[Any] = []
    if check_id is not None:
        where.append("check_id = ?")
        params.append(check_id)
    if album_id is not None:
        where.append("album_id = ?")
        params.append(album_id)
    if not include_ignored:
        where.append("ignored_at IS NULL")
    if not include_resolved:
        where.append("resolved_at IS NULL")
    sql = "SELECT * FROM check_findings"
    if where:
        sql += " WHERE " + " AND ".join(where)
    sql += " ORDER BY last_seen_at DESC LIMIT ?"
    params.append(limit)
    with db_module.connect() as conn:
        return [_finding_row_to_dict(r) for r in conn.execute(sql, params)]


def get_finding(finding_id: int) -> dict[str, Any] | None:
    with db_module.connect() as conn:
        row = conn.execute(
            "SELECT * FROM check_findings WHERE id=?", (finding_id,),
        ).fetchone()
        return _finding_row_to_dict(row) if row else None


def ignore_finding(finding_id: int, reason: str | None = None) -> dict[str, Any] | None:
    now = _now_iso()
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            cur = conn.execute(
                "UPDATE check_findings SET ignored_at=?, ignored_reason=? WHERE id=?",
                (now, (reason or "").strip() or None, finding_id),
            )
            if cur.rowcount == 0:
                return None
    notify_changed()
    return get_finding(finding_id)


def unignore_finding(finding_id: int) -> dict[str, Any] | None:
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            cur = conn.execute(
                "UPDATE check_findings SET ignored_at=NULL, ignored_reason=NULL WHERE id=?",
                (finding_id,),
            )
            if cur.rowcount == 0:
                return None
    notify_changed()
    return get_finding(finding_id)


# ---------- Fix dispatch ----------

def fix_finding(finding_id: int, *, user_hint: str = "") -> FixResult:
    """Look up the finding and ask its check to fix it.

    Returns FixResult; the caller is responsible for serializing it to
    HTTP. The check's fix() may itself raise on non-actionable findings —
    let it bubble up so the API surfaces a 400/409 with the message.

    `user_hint` is forwarded to the check's fix() for AI-job checks. A
    hint changes behavior: when present, the check skips its MB
    short-circuit and goes straight to the AI path so the user's input
    actually reaches Claude. Empty string = no hint (default behavior).
    """
    finding_dict = get_finding(finding_id)
    if finding_dict is None:
        raise ValueError(f"finding {finding_id} not found")
    cls = _get_check_class(finding_dict["check_id"])
    if cls is None:
        raise ValueError(f"finding's check_id {finding_dict['check_id']!r} is not registered")
    if cls.meta.fix_kind == "none":
        raise ValueError(f"check {cls.meta.id} has no fix path")
    instance = cls()
    finding_obj = Finding(
        summary=finding_dict["summary"],
        album_id=finding_dict["album_id"],
        severity=finding_dict["severity"],
        detail=finding_dict["detail"],
    )
    try:
        return instance.fix(finding_obj, user_hint=user_hint)
    finally:
        # Always wake `/api/checks/stream` after a fix attempt, even
        # when fix() returned ok=True without mutating anything (no-op
        # fast-path, idempotent fix). The frontend's `useFixFinding`
        # mutation handler dropped its explicit FINDINGS/CHECKS
        # invalidations relying on the bus; without this wake, a
        # satisfied-but-not-yet-resolved finding would linger in the
        # dialog until the next unrelated mutation.
        notify_changed()


def try_quick_fix_finding(finding_id: int) -> FixResult | None:
    """Like fix_finding, but invokes the check's MB-only path. Returns
    None if the check has no quick-fix support at all (caller should
    treat this as 'skipped'). Used by the bulk quick-fix-all endpoint."""
    finding_dict = get_finding(finding_id)
    if finding_dict is None:
        raise ValueError(f"finding {finding_id} not found")
    cls = _get_check_class(finding_dict["check_id"])
    if cls is None:
        raise ValueError(f"finding's check_id {finding_dict['check_id']!r} is not registered")
    instance = cls()
    finding_obj = Finding(
        summary=finding_dict["summary"],
        album_id=finding_dict["album_id"],
        severity=finding_dict["severity"],
        detail=finding_dict["detail"],
    )
    return instance.try_quick_fix(finding_obj)
