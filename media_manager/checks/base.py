"""Base types for the health-check framework.

A `Check` is a Python class that knows how to detect a specific class of
library issue and (optionally) fix it. The runner instantiates the class,
calls `.run(scope)` to enumerate `Finding` objects, persists them via the
upsert/resolve cycle, and (for findings the user clicks Fix on) calls
`.fix(finding)` to either enqueue an AI job or perform a deterministic
mutation.

These types intentionally have no dependencies on `db` or the FastAPI
layer — checks should be testable in isolation by feeding them a `CheckScope`
fixture and consuming the iterator.
"""
from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, ClassVar, Iterable, Literal, TYPE_CHECKING

if TYPE_CHECKING:
    # Ambient type for what a check sees; concretely a list of dicts loaded
    # from `cache.list_albums(library_path)`. Kept loose here to avoid a
    # cycle into media_manager.cache during import.
    AlbumDict = dict[str, Any]


Severity = Literal["info", "warn", "error"]
"""How loudly to surface the finding in the UI. info=neutral, warn=yellow,
error=red. Severity is purely presentational; the runner treats them all
the same."""


Cost = Literal["cheap", "medium", "heavy"]
"""Rough scheduling tier.

- cheap   : finishes in <1s for a typical library; safe to run as a
            post-scan tail every time.
- medium  : seconds-to-minutes; default schedule weekly, also runnable
            on demand.
- heavy   : minutes-to-hours; default schedule monthly; should support
            sharded scope (one shard per scheduled run).
"""


FixKind = Literal["none", "ai-job", "deterministic"]
"""How a finding can be acted on.

- none           : no Fix button rendered.
- ai-job         : Fix enqueues a job through media_manager.jobs and
                   returns the job_id. Frontend then surfaces "Job queued."
- deterministic  : Fix runs synchronously in the request handler. The
                   check's fix() method does the work and returns a
                   FixResult describing what happened.
"""


# ---------- Dataclasses ----------

@dataclass(frozen=True)
class CheckMeta:
    """Static metadata about a check. One instance per check class, attached
    as a class variable. Used by the API listing and the registry."""
    id: str
    label: str
    description: str
    category: Literal["metadata", "cover", "audio", "files", "duplicates", "external"]
    cost: Cost
    runs_during_scan: bool = False
    """When True, this check runs as a bulk pass after every successful
    library scan (post-scan tail, NOT per-album-as-they-stream). Cheap-only
    in practice; the runner will skip scheduling these on a separate
    timer."""
    fix_kind: FixKind = "none"
    bulk_action_label: str | None = None
    """If set, the Health detail page renders a "<label> (N)" button that
    calls try_quick_fix on every open finding via the bulk SSE endpoint.
    Different checks need different verbs — "Try MusicBrainz on all" for
    metadata fills, "Fingerprint all" for AcoustID coverage, etc.
    Leave None to hide the bulk button entirely (the per-finding Fix
    button is always rendered separately)."""


@dataclass
class Finding:
    """A single issue detected by a check.

    The (check_id, album_id, summary) tuple is the natural identity used for
    upsert in `check_findings` — same finding seen on a later run bumps
    last_seen_at instead of inserting a duplicate row.
    """
    summary: str
    album_id: str | None = None
    severity: Severity = "warn"
    detail: dict[str, Any] | None = None


@dataclass
class CheckScope:
    """The set of albums a check should inspect on this run.

    `kind` distinguishes:
      - "all"         : the entire library. The runner WILL auto-resolve
                        findings not yielded by this run.
      - "incremental" : a subset chosen by the runner (e.g. only albums
                        modified since the last scan). The runner WILL NOT
                        auto-resolve.
      - "albums"      : an explicit list (per-album rerun from the UI).
                        WILL NOT auto-resolve.
    """
    library_path: str
    kind: Literal["all", "incremental", "albums"]
    albums: list[AlbumDict] = field(default_factory=list)

    @property
    def can_resolve(self) -> bool:
        """True if findings missing from this run should be auto-resolved."""
        return self.kind == "all"


@dataclass
class FixResult:
    """Returned by Check.fix(). The runner persists/serializes this to the
    Fix-button response."""
    ok: bool
    message: str = ""
    # When the fix enqueued an AI job, surface its id so the UI can deep-link.
    job_id: str | None = None


# ---------- Check ABC ----------

class Check:
    """Abstract base. Subclasses set the `meta` class variable and implement
    at least `run()`."""

    meta: ClassVar[CheckMeta]  # subclasses MUST set this

    def run(self, scope: CheckScope) -> Iterable[Finding]:
        """Yield findings as they're discovered. May be a generator (preferred,
        so the runner can stream them to SSE subscribers) or a regular
        iterable. Should not raise for individual album failures; surface
        those as Findings with severity='error' instead. Raise only for
        truly catastrophic conditions (missing required tool, DB
        unreachable) — those terminate the run."""
        raise NotImplementedError

    def fix(self, finding: Finding, *, user_hint: str = "") -> FixResult:
        """Apply a fix for `finding`. Only invoked when meta.fix_kind != 'none'.

        For 'ai-job' kind: enqueue the appropriate job and return its id.
        For 'deterministic' kind: do the mutation synchronously, return
        ok/message. Raise ValueError for non-actionable findings (the UI
        will not render the Fix button in that case but server-side defense
        in depth doesn't hurt).

        `user_hint` is optional free-form context the user typed in the
        finding-row Fix dialog. AI-job checks should plumb it into the
        enqueued job's `hint` field; deterministic checks can ignore it
        (the hint can't influence a MusicBrainz lookup)."""
        raise NotImplementedError(
            f"Check {self.meta.id} declared fix_kind={self.meta.fix_kind} but did not implement fix()"
        )

    def try_quick_fix(self, finding: Finding) -> FixResult | None:
        """Free / deterministic fix attempt. NEVER enqueues an AI job.

        Returns:
          - FixResult(ok=True)  : finding fully resolved by a free path
                                  (typically a confident MusicBrainz match
                                  applied directly).
          - FixResult(ok=False) : tried but couldn't fully resolve. Caller
                                  should leave the finding for AI / manual.
          - None                : check has no quick-fix path at all.

        Used by the bulk "Try MusicBrainz on all" action — lets the user
        skim cheap fixes off a long list without committing the queue to
        AI work for every one. Default returns None; checks that have a
        deterministic path (MB-first ones) override it."""
        return None
