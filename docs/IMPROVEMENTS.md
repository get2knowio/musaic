# Improvements

This document captures the main implementation and product issues found during review, along with recommendations for simplifying the health-check model and making the repo easier to drive with autonomous coding agents.

## 1. Mental Model

The application is fundamentally a local-first music library manager structured in three tiers:
- **Frontend UI:** The user-facing application for browsing, queueing tasks, and adjusting settings.
- **FastAPI Backend (`musaic/main.py`):** The orchestration layer that provides REST endpoints and Server-Sent Events (SSE) streams for long-running background tasks.
- **Persistence:** A SQLite database acting as a fast caching index and state store (for jobs, findings, and settings), while the user's media volume (FLAC files, `cover.jpg` sidecars) remains the ultimate source of truth.

A major architectural split currently exists: the FastAPI backend serves a legacy static UI out of `musaic/static`, while the intended product UI is a modern React SPA located in `frontend/src`. The React app heavily leverages SSE streams to reflect backend state (scans, jobs, health checks) reactively, but since it is not the default shipped surface, the intended and shipped user experiences are diverged.

## 2. Flow Review

### 1. First-run setup / choosing the library directory
- **Intended:** User configures a library directory, backend validates it, cache is cleared, and an initial scan begins.
- **Actual Path:** In `Settings.tsx`, `useSetLibraryDir` sends a `POST /api/config`. The backend validates the path and clears the SQLite snapshot via `_invalidate_cache()`. The global `useScanStream` hook detects the configuration change and initiates the SSE scan stream.
- **Validation:** Works end-to-end, but relies heavily on the React component lifecycle (mounting the scan stream) rather than a persistent backend job kickoff.

### 2. Initial scan and rescan behavior
- **Intended:** Rescanning wipes old data and streams progress as it reads the filesystem.
- **Actual Path:** `useScanStream.rescan()` triggers a `POST /api/scan/rescan` (dropping cache), then opens `/api/scan/stream?force=true`. The backend's synchronous generator reads the filesystem and yields progress to the SSE socket.
- **Validation:** Fragile. If the user navigates away or disconnects the browser stream mid-scan, the generator raises `asyncio.CancelledError` and terminates. Because the cache was dropped preemptively, the library is left in an empty or partial state until another stream completes.

### 3. Browsing the library and opening album detail
- **Intended:** Quick grid browsing with non-disruptive album drill-downs.
- **Actual Path:** `Library.tsx` maps over sorted albums via `useScanStream` or `useAlbumsSnapshot`. Clicking an album triggers a nested `<Route path=":albumId">`, which renders the `DetailSheet` while preserving the underlying grid's scroll position.
- **Validation:** Works very well end-to-end. The UI properly isolates the detail overlay from the list state.

### 4. Viewing tracks, tags, cover art, and findings
- **Intended:** Album detail exposes read-only data, cover thumbnails, and open health/tagging findings.
- **Actual Path:** The `DetailSheet` consumes `/api/albums/:id/tracks`, fetches thumbnails via `/api/albums/:id/cover?size=thumb` (relying on robust ETag caching), and queries open findings.
- **Validation:** Works end-to-end. Thumbnails are generated efficiently via a background thread pool.

### 5. Queueing AI metadata and cover jobs
- **Intended:** Users queue albums for AI processing; tasks run asynchronously and stream state to the UI.
- **Actual Path:** `POST /api/ai/jobs` enqueues jobs in SQLite. The background worker in `main.py` dispatches them. The UI subscribes to `/api/ai/jobs/stream` via `useJobsStream` Context.
- **Validation:** Works securely and reliably. The global context appropriately updates `JobStatusBadge` across different pages.

### 6. Reviewing job results and applying changes
- **Intended:** Users can preview AI suggestions and confidently apply tags and renames.
- **Actual Path:** `useApplyStream.ts` calls `POST /api/albums/:albumId/apply/stream`. The backend loops over FLACs, updates tags, performs necessary file renames, invalidates thumbnails, updates track-tag sidecars, and triggers a localized rescan of the album.
- **Validation:** Works end-to-end securely, providing real-time feedback per file modified.

### 7. Health checks: listing, detail page, fixing, ignoring, unignoring
- **Intended:** Users browse categorization of issues, drill into details, and perform deterministic fixes.
- **Actual Path:** `Health.tsx` queries `/api/checks` for summaries. `HealthDetail.tsx` fetches specific open findings via `/api/checks/:checkId`. Actions trigger `POST /api/checks/.../fix` or `/ignore`.
- **Validation:** Broken cache invalidation. While the global SSE stream (`useChecksStream`) updates the summary counts, it does not invalidate the local findings array fetched by `HealthDetail.tsx`. Consequently, fixing or ignoring an item leaves it visible on screen.

### 8. Settings flows, including filename templates / backups / library reconfiguration
- **Intended:** Unified configuration for integrations, behaviors, and backups.
- **Actual Path:** Centralized in `Settings.tsx`, dispatching PATCH requests to `/api/settings` and snapshot commands to `/api/db/snapshots`. 
- **Validation:** Works reliably. Decoupled appropriately from actual execution logic.

### 9. Cross-page flows
- **Intended:** Job states and library filters persist smoothly during navigation.
- **Actual Path:** `useRememberedLibraryUrl` stashes the last library filter state in `sessionStorage`. `useJobsStream` Context provides queue status everywhere.
- **Validation:** Works cleanly. Provides a seamless single-page application experience.

## 3. Findings

### 1. The shipped UI and the intended UI have diverged
The FastAPI app still serves the legacy static frontend from `musaic/static/`, while the React app under `frontend/` contains the newer flows.
- **Implications:** The default app entrypoint does not expose the React routes, and the legacy frontend still calls older AI endpoints that no longer exist.

### 2. Scan and rescan behavior is more fragile than the UI suggests
The scan lifecycle is split across multiple frontend mechanisms (`useScanStream`, `useAlbumsSnapshot`, `rescanController`, session flags).
- **Implications:** Changing the library directory or clicking rescan away from the Library page can leave the UI showing stale or empty data because an actual scan depends on the client keeping an active connection to `/api/scan/stream`.

### 3. Health detail state is not consistently refreshed
The Health detail page reads from a per-check detail query, but the SSE refresh and several mutations only refresh the summary-level health caches.
- **Implications:** Fixing, ignoring, or unignoring a finding can leave the currently open Health detail page stale.

### 4. Deterministic health-check fixes use a second apply pipeline
The deterministic fix helpers in `musaic/checks/_apply.py` do not go through the same full mutation path used by `/api/albums/{id}/apply/stream`.
- **Implications:** File rename handling, track-tag sidecar migration, and cover normalization are not fully aligned, producing subtly different results.

### 5. The frontend is not currently in a clean buildable state
`npm run typecheck` does not currently pass.
- **Implications:** The frontend is not at a reliable handoff point for autonomous coding.

### 6. There is little automated flow validation
There is no integration coverage for the core end-to-end flows.
- **Implications:** High risk of regression during major refactors.

## 4. Simplification Opportunities

1. **Use one frontend.**
   - React app <-> FastAPI <-> SQLite <-> media volume is a coherent model. Remove the legacy static frontend.
2. **Use one mutation pipeline for album writes.**
   - Renames, tag writes, cover writes, sidecar maintenance, thumbnail invalidation, and targeted rescans should all happen in one unified service path instead of duplicating logic in deterministic health check fixes.
3. **Reduce persistence concepts.**
   - SQLite should be the authoritative app-state store for settings, findings, jobs, scans, and cached metadata. The media volume should remain authoritative for FLAC contents and on-disk covers.
4. **Split `musaic/main.py` by concern.**
   - It currently carries too many responsibilities. Recommended slices: `api/library.py`, `api/jobs.py`, `api/health.py`, `api/settings.py`, `services/scan.py`, `services/apply.py`.
5. **Decouple Scan Execution from SSE Streams.**
   - Make “start scan” a first-class backend job action rather than an indirect cache-clear-plus-SSE attachment sequence. The SSE stream should just observe a scan running in the background.

## 5. Recommended Next Steps

1. **Put the repo under Git.**
   - Initialize a real Git repository to allow agents to rely on diffs, branches, and commit workflows.
2. **Add `AGENTS.md`.**
   - Add a root-level guide outlining setup commands, verification commands, and architectural sources of truth.
3. **Remove transitional ambiguity.**
   - Make the React app the real frontend and delete the static legacy files.
4. **Fix frontend build.**
   - Make `npm run typecheck` pass.
5. **Add Integration Tests.**
   - Add one backend test path and one end-to-end smoke path for core flows (e.g., scanning, applying tags).
6. **Refactor `musaic/main.py`.**
   - Split the routing and service logic so that ownership is obvious to both humans and agents.