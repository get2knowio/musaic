# Maintenance / Health Checks — Implementation Spec

This doc designs the "shell" — schema, registry, runner, API, UI — for periodic library health checks, then specs five concrete checks that exercise different edges of the framework. The goal is something you could sit down and start implementing from.

Companion to `maintenance-scripts.md` (the longer catalog of *what* checks we'd want); this doc focuses on *how* to build them.

## Goals & non-goals

**Goals.**

- One framework that hosts many checks. Adding a new check is a single Python file plus a registry entry.
- Check results are persistent and queryable. The UI shouldn't re-run a check just to render its findings.
- A check can either *report* (read-only, surface findings) or *suggest a fix* (link a finding to an action — usually enqueueing an AI job, sometimes a deterministic auto-fix).
- Heavy checks (FLAC integrity) run on a rotating schedule in the background; cheap checks run as part of the normal scan or on-demand.
- The user can manually run any check, ignore individual findings, and see "fixed since last run" history.

**Non-goals.**

- We are not building a generic rule engine. Each check is hand-written Python with as much or as little logic as it needs.
- No multi-user, no per-finding comments, no ticket tracker. This is a personal tool; lightweight wins.
- No automatic destructive actions without user confirmation. Even auto-fixes show a diff first or run dry-run by default.

---

## The shell

### Data model

One new table. v6 migration.

```sql
CREATE TABLE IF NOT EXISTS check_runs (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    check_id        TEXT NOT NULL,           -- "missing-core-tags", "track-gaps", …
    started_at      TEXT NOT NULL,
    completed_at    TEXT,                    -- NULL while running
    status          TEXT NOT NULL,           -- "running" | "completed" | "failed" | "cancelled"
    library_path    TEXT NOT NULL,           -- denormalized for filtering history per library
    scope           TEXT NOT NULL,           -- "all" | "incremental" | JSON list of album_ids
    issue_count     INTEGER NOT NULL DEFAULT 0,
    error           TEXT,                    -- on status='failed'
    duration_ms     INTEGER
);

CREATE INDEX IF NOT EXISTS idx_check_runs_check  ON check_runs(check_id, started_at DESC);

CREATE TABLE IF NOT EXISTS check_findings (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    check_id        TEXT NOT NULL,
    album_id        TEXT,                    -- NULL = library-level finding (rare; e.g. orphaned dirs)
    severity        TEXT NOT NULL DEFAULT 'warn',  -- "info" | "warn" | "error"
    summary         TEXT NOT NULL,           -- short, one-line, the row label in the UI
    detail_json     TEXT,                    -- check-specific structured data
    first_seen_at   TEXT NOT NULL,
    last_seen_at    TEXT NOT NULL,
    resolved_at     TEXT,                    -- non-NULL once the issue is gone (auto or user)
    ignored_at      TEXT,                    -- non-NULL if user dismissed it
    ignored_reason  TEXT,
    UNIQUE(check_id, album_id, summary)      -- dedupe across runs
);

CREATE INDEX IF NOT EXISTS idx_findings_check   ON check_findings(check_id, resolved_at, ignored_at);
CREATE INDEX IF NOT EXISTS idx_findings_album   ON check_findings(album_id);
```

Key design choices:

- **Findings are upserted, not replaced per run.** When a check runs again and re-detects an existing finding, we bump `last_seen_at`. When a known finding is no longer detected, we set `resolved_at`. This gives us "fixed since last week" history for free.
- **`UNIQUE(check_id, album_id, summary)`.** The `summary` is the natural identity of a finding — "Missing year tag" on album X is the same finding every time we detect it. Two distinct findings against the same album get distinct summaries.
- **`detail_json`** holds whatever structured data the UI / fix flow needs. E.g. for the year-format check, `{"current": "2003-05-12", "suggested": "2003"}`.
- **Findings outlive runs.** Deleting old `check_runs` rows for log hygiene doesn't touch findings.

### Per-check Python contract

```python
# musaic/checks/base.py
from dataclasses import dataclass
from typing import Iterable, Literal

Severity = Literal["info", "warn", "error"]

@dataclass
class Finding:
    album_id: str | None        # None = library-level
    summary: str                # row label
    severity: Severity = "warn"
    detail: dict | None = None  # serialized to detail_json

@dataclass
class CheckMeta:
    id: str                     # "missing-core-tags"
    label: str                  # human title for the UI: "Missing core tags"
    description: str            # one-sentence what-and-why
    category: str               # "metadata" | "cover" | "audio" | "files" | "duplicates" | "external"
    cost: Literal["cheap", "medium", "heavy"]    # affects scheduling defaults
    runs_during_scan: bool = False   # true = also runs as part of the regular scan loop
    # If a finding can be auto-acted on:
    fix_kind: Literal["none", "ai-job", "deterministic"] = "none"

class Check:
    meta: CheckMeta

    def run(self, scope: "CheckScope") -> Iterable[Finding]:
        """Yield findings as they're discovered. The runner persists them
        (upsert into check_findings) and updates the check_run row's count."""
        raise NotImplementedError

    def fix(self, finding: Finding) -> "FixResult":
        """Optional. Either enqueue an AI job (returns its id) or perform a
        deterministic mutation. Only called when meta.fix_kind != 'none'."""
        raise NotImplementedError
```

`CheckScope` is the resolved set of albums to inspect (helpful for incremental runs and the per-album "rerun checks for this album" button). It exposes:

```python
@dataclass
class CheckScope:
    albums: list[Album]                  # already loaded from cache
    library_path: str
    all: bool                            # True if scope was "the whole library"
```

The runner is responsible for:

1. Inserting the `check_runs` row at status='running'.
2. Calling `check.run(scope)`, streaming findings.
3. Upserting each finding (UNIQUE-key match → bump `last_seen_at`; new → insert with `first_seen_at = last_seen_at = now`).
4. After the iterator drains, marking `resolved_at = now` on any rows for this check_id whose `last_seen_at` is strictly older than the run's `started_at` AND whose scope was "all". (Incremental runs can't resolve findings outside their scope.)
5. Setting `check_runs.completed_at`, `status`, `issue_count`, `duration_ms`.

### Registry & discovery

```python
# musaic/checks/__init__.py
from .missing_core_tags import MissingCoreTags
from .track_gaps import TrackGaps
from .no_cover_art import NoCoverArt
from .orphaned_files import OrphanedFiles
from .flac_integrity import FlacIntegrity

REGISTRY: list[type[Check]] = [
    MissingCoreTags,
    TrackGaps,
    NoCoverArt,
    OrphanedFiles,
    FlacIntegrity,
]
```

Adding a new check = file + registry line. No magic auto-discovery — the explicit list keeps the registration order deterministic and makes "what checks exist" a single place to look.

### Scheduling model

Three triggers:

1. **On every scan.** Checks with `runs_during_scan=True` run inline as part of the existing scan loop, on each album as it's processed. Cheap-only — must finish in O(microseconds) per album. Examples: `MissingCoreTags`, `TrackGaps`, `NoCoverArt`.
2. **On demand.** Any check, any time, via the Health Checks UI ("Run now" button) or `POST /api/checks/{id}/run`.
3. **Periodic.** A small cron-like state in `settings`: `checks.schedule = {"flac-integrity": "weekly", "mb-stale-tags": "monthly"}`. The worker that already runs the AI job loop also wakes once an hour and starts any check whose last completed_at is older than its interval. Heavy checks run with rate-limiting (one album every few seconds) so they don't trash I/O while you're using the app.

Scheduling defaults derive from `meta.cost`:

- `cheap` → `runs_during_scan=True` (effectively continuous)
- `medium` → `weekly`
- `heavy` → `monthly`

User can override per check in Settings.

### Backend API

```
GET    /api/checks                      # list registered checks + last-run summary + open finding counts
GET    /api/checks/{id}                 # detail: meta + last 10 runs + open findings (paginated)
GET    /api/checks/findings             # all open findings across checks; optional ?album_id=… filter for the detail sheet
POST   /api/checks/{id}/run             # body: { scope: "all" | "incremental" | { album_ids: [...] } }
POST   /api/checks/{id}/findings/{fid}/ignore       # body: { reason?: string }
POST   /api/checks/{id}/findings/{fid}/unignore
POST   /api/checks/{id}/findings/{fid}/fix          # if check.fix_kind != 'none'
GET    /api/checks/stream               # SSE: emits {"type":"run", run} on start, {"type":"finding", finding} as they stream, {"type":"complete", run} on done
```

All endpoints follow the existing patterns (FastAPI handlers in `main.py`, SSE shape mirrors `/api/scan/stream`).

### Frontend UI

New route: `/health`. New sidebar entry "Health" between "AI Jobs" and "Settings".

**Index page (`/health`)**: one card per registered check.

```
┌─────────────────────────────────────────────────┐
│ Missing core tags                          ⚠ 12 │
│ Albums missing title, artist, year, or genre.   │
│ Last run 3h ago · runs with every scan          │
│                                       [View →] │
└─────────────────────────────────────────────────┘
┌─────────────────────────────────────────────────┐
│ FLAC integrity                              ✓ 0 │
│ Verifies STREAMINFO MD5 against decoded audio.  │
│ Last run 2d ago · 47/450 albums                 │
│                            [Run now] [View →] │
└─────────────────────────────────────────────────┘
```

**Detail page (`/health/{id}`)**: drilldown — table of open findings, severity icon, summary, album link (if scoped), per-finding "Fix" (when supported) and "Ignore" actions, plus a "Run now" button at the top and a small "Recent runs" sidebar showing duration/issue counts for the last 5 runs.

The cards on the index card stream live: as a check runs, the count updates and a small spinner appears. Same SSE pattern we already have for the AI jobs page.

### Integration with the existing AI jobs system

When a finding's `fix_kind == "ai-job"`, the Fix button on a finding row enqueues a job through the existing `jobs.enqueue()` path. The finding row stores the resulting job_id in its `detail_json`, and the UI surfaces "AI job in progress" inline (links to the AI Jobs page). When the job completes and applies, the next run of the originating check resolves the finding automatically.

This means health checks become a structured *entry point* into the AI job queue — instead of opening albums one at a time and clicking "Suggest fixes", the user can go to Health, see "12 albums missing year", and bulk-fix them.

### Settings additions

Three new keys (using the existing `settings` KV table, no schema change):

- `checks.schedule.<check_id>`: `"never" | "weekly" | "monthly"` — overrides the default.
- `checks.flac_integrity.albums_per_run`: int, default `30`. Heavy checks shard their work; this caps how many albums per scheduled run.
- `checks.auto_fix.<check_id>`: `bool`, default `false`. Allow-list for deterministic fixes that the runner can apply without confirmation. Off by default.

---

## Concrete check specs

Five checks chosen to exercise different parts of the framework: cheap inline (M1), needs per-track data (M3), reuses existing column data (C1), filesystem cross-check (F5), heavy I/O-bound (A1).

### 1. `missing-core-tags` (M1) — cheap, inline

**Detect.** Albums where any of `title`, `album_artist`/`artist`, `year`, `genre` is null/empty/whitespace.

**Implementation.**

```python
def run(self, scope):
    rows = db.connect().execute("""
        SELECT id, title, album_artist, artist, year, genre, dir_name
        FROM albums
        WHERE library_path = ?
        AND (
            COALESCE(NULLIF(TRIM(title), ''), '') = ''
            OR COALESCE(NULLIF(TRIM(year), ''), '') = ''
            OR COALESCE(NULLIF(TRIM(genre), ''), '') = ''
            OR (
                COALESCE(NULLIF(TRIM(album_artist), ''), '') = ''
                AND COALESCE(NULLIF(TRIM(artist), ''), '') = ''
            )
        )
    """, (scope.library_path,)).fetchall()
    for row in rows:
        missing = []
        if not (row["title"] or "").strip(): missing.append("title")
        if not ((row["album_artist"] or row["artist"] or "").strip()): missing.append("artist")
        if not (row["year"] or "").strip(): missing.append("year")
        if not (row["genre"] or "").strip(): missing.append("genre")
        yield Finding(
            album_id=row["id"],
            summary=f"Missing: {', '.join(missing)}",
            severity="warn",
            detail={"missing_fields": missing, "dir_name": row["dir_name"]},
        )
```

**Meta:** `cost="cheap"`, `runs_during_scan=True`, `fix_kind="ai-job"`.

**Fix:** enqueues a `metadata` AI job for the album.

**Why this exercises the framework.** Pure SQL, finishes instantly, runs implicitly on every scan. Tests the "runs_during_scan" path and the upsert/resolve cycle (resolves automatically when a fix lands and the next scan re-runs).

---

### 2. `track-gaps` (M3) — cheap, inline, needs per-track data

**Detect.** Albums whose track-number sequence has gaps (1, 2, 4, 5) or duplicates (1, 2, 2, 3).

**Implementation.** Scan walks each FLAC anyway. Either expose track-number data via a new `albums.track_numbers_json` column (cheap denormalization), or do a small filesystem read per album during the check. Recommend the former; one-time v6 migration adds the column and `scanner.py` populates it.

```python
def run(self, scope):
    for album in scope.albums:
        nums = json.loads(album.track_numbers_json or "[]")  # list of int|None
        if not nums:
            continue
        # Strip nulls (we'll catch them via the M2 "missing per-track tags" check)
        filtered = sorted(n for n in nums if n is not None)
        if len(filtered) <= 1:
            continue
        gaps = [
            (filtered[i], filtered[i] + 1, filtered[i + 1])
            for i in range(len(filtered) - 1)
            if filtered[i + 1] - filtered[i] > 1
        ]
        dups = sorted({n for n in filtered if filtered.count(n) > 1})
        if gaps:
            yield Finding(
                album_id=album.id,
                summary=f"Track-number gaps: missing {', '.join(str(g[1]) for g in gaps)}",
                severity="warn",
                detail={"present": filtered, "gaps": gaps, "expected_total": album.track_count},
            )
        if dups:
            yield Finding(
                album_id=album.id,
                summary=f"Duplicate track numbers: {', '.join(map(str, dups))}",
                severity="error",
                detail={"present": filtered, "duplicates": dups},
            )
```

**Meta:** `cost="cheap"`, `runs_during_scan=True`, `fix_kind="ai-job"`.

**Fix:** enqueues a `metadata` AI job — track renumbering is exactly what the existing per-track suggestion path is for.

**Schema dependency.** v6 migration adds `albums.track_numbers_json TEXT`. `scanner.py` populates it from the existing FLAC read. Backward-compatible: missing column = empty list = check skips that album cleanly.

---

### 3. `no-cover-art` (C1) — cheap, already partially built

**Detect.** Albums where `has_embedded_cover = 0 AND cover_file IS NULL`.

**Why we need a dedicated check** (we already have a sidebar filter chip). The filter chip is a transient view over the live cache — useful for browsing. A check is a *durable record* of "we noticed this album has no cover at time T, here's how to fix it." That distinction matters because:

- It lets the user mark an album as "ignore — I know this album has no cover anywhere" so it stops showing in the count.
- It feeds the periodic-run model: every weekly run, we get a snapshot of how many albums still have no cover.
- It gives a single "Find covers for all of these" bulk action (enqueue cover lookups for all open findings).

**Implementation.**

```python
def run(self, scope):
    rows = db.connect().execute("""
        SELECT id, dir_name, dir_artist, dir_album
        FROM albums
        WHERE library_path = ? AND has_embedded_cover = 0 AND cover_file IS NULL
    """, (scope.library_path,)).fetchall()
    for row in rows:
        yield Finding(
            album_id=row["id"],
            summary="No cover art (embedded or sidecar)",
            severity="info",
            detail={"dir_name": row["dir_name"], "dir_artist": row["dir_artist"], "dir_album": row["dir_album"]},
        )
```

**Meta:** `cost="cheap"`, `runs_during_scan=True`, `fix_kind="ai-job"`.

**Fix:** enqueues a `cover` AI job (which already falls back to MusicBrainz/CAA when AI errors).

**Bulk fix.** First check we'd want a "Fix all" button for. Implementation: enqueue one job per finding, respecting the parallel-jobs setting. Surface the resulting batch in the AI Jobs page.

**Coordination with existing prior-attempts surface.** Findings should be auto-ignored when there's an unresolved prior failed cover attempt and `created_at` of that attempt is recent (say < 30 days). Otherwise re-running the check after we just gave up on a hard album would just re-create the finding. Logic: when computing the open-finding set, exclude albums that have a recent failed cover job. The user can still hit `/health/{id}` and explicitly retry. (Concretely: the SQL grows a `NOT EXISTS` subquery against `ai_jobs` filtering on the conditions described in `usePriorCoverAttempts`.)

---

### 4. `orphaned-rows-and-files` (F5) — filesystem cross-check

**Detect.** Two related issues:

- Cache rows pointing to a path that no longer exists on disk.
- FLAC files on disk inside the configured library that aren't in any cache row (a scan glitch, a manual rename, a directory that doesn't fit our album shape).

**Implementation.**

```python
def run(self, scope):
    # 1. cache rows whose path is missing from disk
    rows = db.connect().execute("SELECT id, path FROM albums WHERE library_path = ?", (scope.library_path,)).fetchall()
    for row in rows:
        if not Path(row["path"]).is_dir():
            yield Finding(
                album_id=row["id"],
                summary="Cache row points to a directory that no longer exists",
                severity="error",
                detail={"path": row["path"]},
            )
    # 2. FLACs on disk in directories we don't have an album row for
    library_root = Path(scope.library_path)
    known_dirs = {Path(r["path"]) for r in rows}
    for flac in library_root.rglob("*.flac"):
        if flac.parent in known_dirs:
            continue
        # parent dir is unknown to the cache. Could be a one-off track,
        # a misnamed compilation, or a brand-new album we haven't scanned.
        yield Finding(
            album_id=None,                       # library-level finding
            summary=f"FLAC file outside any cached album: {flac.parent.name}",
            severity="warn",
            detail={"path": str(flac.parent), "filename": flac.name},
        )
```

**Meta:** `cost="medium"` (full library walk), `runs_during_scan=False` (already runs on every full scan implicitly via the cleanup path, but as a check we want the *finding history*), `fix_kind="deterministic"` for the missing-row case (delete the cache row).

**Fix.**

- For "cache row points to deleted dir": auto-fix by deleting the row. Gated by `checks.auto_fix.orphaned-rows-and-files` (default false).
- For "FLAC outside any cached album": no auto-fix; the user investigates.

**Why this exercises the framework.** First check that operates on filesystem state independently of the album cache, first deterministic fix path, first library-level (album_id=None) finding.

---

### 5. `flac-integrity` (A1) — heavy, scheduled, slow-rotating

**Detect.** Per-FLAC: decode the file, verify against the STREAMINFO MD5. Implemented as `flac --test --silent <path>`; non-zero exit = bitrot or corruption.

**Implementation strategy.** This is the only "heavy" check in the first batch. Single full pass would take hours on a large library. Two design choices to handle this:

1. **Sharded incremental runs.** Each scheduled run handles `albums_per_run` (default 30) albums, picked as the 30 with the *oldest* `last_checked_at` (a new column on `check_findings` for this check, or on `albums` itself if we want it more broadly). After ~15 runs the whole library is covered; then the rotation starts over.
2. **Cancellable.** The runner records progress per-album so a server restart resumes cleanly, and the user can hit "Cancel" mid-run.

```python
def run(self, scope):
    last_checked = self._read_last_checked()  # dict[album_id, iso_ts]
    candidates = sorted(scope.albums, key=lambda a: last_checked.get(a.id, ""))
    n = settings.get_int("checks.flac_integrity.albums_per_run", default=30)
    for album in candidates[:n]:
        for flac in self._flacs_in(album.path):
            result = subprocess.run(
                ["flac", "--test", "--silent", str(flac)],
                capture_output=True, timeout=120,
            )
            if result.returncode != 0:
                yield Finding(
                    album_id=album.id,
                    summary=f"FLAC decode failed: {flac.name}",
                    severity="error",
                    detail={
                        "filename": flac.name,
                        "path": str(flac),
                        "stderr_excerpt": result.stderr.decode("utf-8", "replace")[:500],
                    },
                )
        self._mark_checked(album.id)
```

**Meta:** `cost="heavy"`, `runs_during_scan=False`, default schedule `monthly`, `fix_kind="none"` (a corrupted FLAC needs the user's attention; no automated fix).

**Why this exercises the framework.**

- First scheduled check.
- First check with shardable scope (the runner can't just say "scope is everything"; the check itself decides what subset to do per run).
- First check with a proper cancellation story.
- First check that shells out to a binary (`flac`) — informs how we surface "missing tool" errors.

**Operational concerns.**

- We need `flac` on `$PATH`. If absent, the check itself yields a single library-level finding: "FLAC integrity check: `flac` binary not found." Better failure mode than a cryptic FileNotFoundError.
- Long-running shell-outs need a hard timeout per file (120 s above) so a wedged file doesn't park the worker.
- I/O-heavy. Default schedule is monthly; user can crank it via settings.

---

## Migration & rollout

### Phase 1 — framework

1. v6 migration: `check_runs` and `check_findings` tables; add `albums.track_numbers_json` (used by check #2).
2. `musaic/checks/base.py` — dataclasses + Check ABC.
3. `musaic/checks/runner.py` — runs a check, manages upsert/resolve, emits SSE events.
4. `musaic/checks/__init__.py` — REGISTRY (initially empty).
5. REST endpoints in `main.py` (list, detail, all-findings, run, ignore, unignore, fix, stream).
6. Frontend foundation:
   - `lib/api.ts` — `listChecks`, `getCheck`, `listFindings`, `runCheck`, `ignoreFinding`, `unignoreFinding`, `fixFinding`.
   - `hooks/useChecks.ts` — `useChecks`, `useCheck(id)`, `useFindings`, `useFindingsForAlbum(id)`, `useFindingsForCheck(id)`, plus mutations.
   - `hooks/useChecksStream.ts` — App-level SSE subscriber (parallel to `useJobsStream`) writing to the checks/findings query caches.
7. Frontend pages + components:
   - `/health` route + sidebar entry "Health" between "AI Jobs" and "Settings."
   - `pages/Health.tsx` — index card grid.
   - `pages/HealthDetail.tsx` — drilldown for a single check.
   - `components/DetailSheet/FindingsSection.tsx` — collapsible findings list rendered inside the album sheet.

The framework should land with **zero registered checks** and a passing UI that just says "no checks installed yet". The detail sheet's Findings section also collapses to nothing in that state. This lets us merge it incrementally and test in isolation.

### Phase 2 — first three checks (cheap)

`missing-core-tags`, `track-gaps`, `no-cover-art`. All `runs_during_scan=True`, no scheduler interaction needed. Each is one Python file.

### Phase 3 — orphaned files

Adds the deterministic-fix path and library-level findings. One Python file plus extending the runner to handle deterministic fixes.

### Phase 4 — FLAC integrity

Adds the scheduler, sharded-scope handling, cancellation, and the "tool missing" failure mode. The biggest single phase.

After Phase 4 we have the full shell shaken out and can add subsequent checks (X1 MB anchoring, M5 album_artist drift, etc.) one at a time without further framework changes.

---

## Decisions made

These were open questions in the first draft; locked in here so future iterations don't re-debate them.

1. **Inline-during-scan latency budget.** A "cheap" check needs to finish per-album in microseconds. M1 and C1 are pure SQL after the scan, not per-album work. M3 needs per-track data.

   **Decision:** "Cheap" checks run as a single bulk pass *after* the scan completes, inside the same async lifespan that just finished scanning. Simpler than per-album hooks, same user-visible latency, and lets the SQL operate on the freshly-written data in one query rather than 100 micro-queries. The `runs_during_scan` flag on `CheckMeta` keeps its name but its semantics are "runs as a post-scan tail," not "runs per-album-as-they-stream."

2. **Upsert dedupe granularity.**

   **Decision:** Use `UNIQUE(check_id, album_id, summary)` for now. Simpler, lower per-check discipline burden. We accept that re-wording a finding's summary creates a one-time duplicate cycle (old finding orphans + auto-resolves on next full run; new finding appears with current wording). If that gets painful in practice, switch to an explicit `detail.key` then.

3. **Resolution semantics for incremental runs.**

   **Decision:** Only `scope="all"` runs resolve findings. Incremental and shard runs can add new findings but never remove stale ones. The next full run cleans up. Documented in the runner so check authors don't expect partial-scope auto-resolution.

4. **Bulk-fix UI affordance.**

   **Decision:** No soft cap. The existing queue auto-pause-on-first-error already protects against runaway batches — if 200 cover lookups start and the 5th one trips a rate limit, the queue pauses and the user sees the banner. The Fix-all button just enqueues; the user manages from there.

5. **Where do findings live in the album detail sheet?**

   **Decision:** Yes — add a "Findings" section to the detail sheet, stacked between "Album info" and "Audio analysis." Each finding renders a row with severity icon, summary, and the same Fix / Ignore actions as the Health page. When the section is empty (no open findings), it collapses entirely to avoid bloating the sheet for healthy albums. Per-check details (the originating check label, last-seen-at) live in a small mono-spaced subtext under each row.

   **UI sketch.**

   ```
   ┌─ Findings (3) ─────────────────────────────┐
   │ ⚠ Missing: year, genre                     │
   │   missing-core-tags · 2h ago               │
   │                          [Fix] [Ignore ▾] │
   │ ⚠ Track-number gaps: missing 7             │
   │   track-gaps · 2h ago                      │
   │                          [Fix] [Ignore ▾] │
   │ ⓘ No cover art                             │
   │   no-cover-art · 2h ago                    │
   │                          [Fix] [Ignore ▾] │
   └────────────────────────────────────────────┘
   ```

## Detail sheet — "Findings" section

A new component, `DetailSheet/FindingsSection.tsx`, mirroring the existing `Section.tsx` pattern (collapsible header + body). Lives in the sheet between `<PropTable />` and `<AudioAnalysis />`.

Data: a new hook `useFindingsForAlbum(albumId)` that reads the cached `findings` query (kept fresh by the same SSE stream as the Health page, see below). The hook returns open findings only — resolved/ignored ones are filtered out by default, with an "Show ignored" toggle in the section header for completeness.

Actions per finding:
- **Fix** — same dispatcher as the Health page. For `fix_kind="ai-job"` checks, enqueues the appropriate job (and the row immediately swaps to a "Job queued" inline pill that links into the AI Jobs page). For `fix_kind="deterministic"` checks, prompts a confirm modal then runs the fix synchronously.
- **Ignore ▾** — dropdown with "Ignore once" and "Ignore with reason…" The latter opens a small dialog that takes a reason string; both call `POST /api/checks/{id}/findings/{fid}/ignore`.

Cache wiring: a single `findings` TanStack Query keyed by `["findings"]` (parallel to `["jobs"]`), populated by `/api/checks/findings` on first read and kept fresh by the new `/api/checks/stream` SSE. Hooks: `useFindings()` for the full list, `useFindingsForAlbum(id)` and `useFindingsForCheck(id)` for the two filtered views.
