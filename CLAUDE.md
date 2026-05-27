# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What This Is

**Musaic** is a local-first web app for browsing and maintaining a personal FLAC music collection. It combines a React/TypeScript frontend, a FastAPI backend, SQLite persistence, and Claude AI to suggest and apply metadata corrections and cover art.

## Commands

### Setup
```bash
uv sync                     # Python backend (requires Python 3.10+)
cd frontend && npm install  # Frontend
```

### Run
```bash
cd frontend && npm run dev:all   # Both backend (:8765) + Vite (:5173) in one terminal

# Or run them separately:
uv run musaic             # Backend on :8765; serves built frontend
cd frontend && npm run dev       # Frontend dev server on :5173 (proxies /api to :8765)
```

### Build
```bash
cd frontend && npm run build  # Produces dist/ served by FastAPI in production
```

### Type Check
```bash
cd frontend && npm run typecheck
```

### Tests
```bash
uv run pytest                          # 141 backend tests (hermetic, no FLAC fixtures)
```

Test layout:
- `tests/test_api_smoke.py` — 22 cases exercising every router slice through TestClient (config, settings, vocab, jobs, health, scan, etc.). End-to-end SSE smoke for `/api/scan/stream`; route-registration smoke for `/api/library-events/stream` + `/api/fingerprint-pool/stream` (the full stream behaviour is pinned in `test_library_events_relay.py`).
- `tests/test_resolver.py` — 12 cases for the tiered resolver internals (cache hit, dir-search, fingerprint vote, alignment, memoization, synthesizer). Mocks `mb_lookup` + `fingerprint`.
- `tests/test_checks.py` — 27 cases for the merged checks (`unidentified-album`, `tags-out-of-sync`, `inconsistent-artist-sort`). Inserts album rows directly into the test DB; mocks the resolver to isolate check-level glue.
- `tests/test_fingerprint_pool.py` — 4 control-plane cases for the eager fingerprinting pool (unavailable short-circuit, idempotent re-call, no-op stop, changed-event signaled on every state mutation for the SSE wakeup).
- `tests/test_library_events.py` — 13 cases for the event-bus contract: dispatch, ordering, isolation, the 3 load-bearing recursion-guard scenarios (in-flight short-circuit, background contextvar inheritance, concurrent unrelated rerun NOT suppressed). New tests for new events go here.
- `tests/test_library_events_relay.py` — 9 cases for the SSE relay subscriber: serialization of each event type, multi-connection fan-out, unregister stops delivery, bounded-queue drops oldest on overflow, cross-thread publish reaches the queue via `loop.call_soon_threadsafe`, relay runs at priority 400 (after every other subscriber).
- `tests/test_event_subscribers.py` — 17 cases pinning each subscriber's invocation contract via stubbed side-effect targets. New subscriber → new test here.
- `tests/test_post_scan_guard.py` — 6 cases for the recursion guard at the `run_post_scan_checks` level (sync + async variants, nested-call short-circuit, exception resets flag).
- `tests/test_filename_mismatch_plan_preview.py` — 9 cases for `derive_plan` + the `/plan-preview` endpoint (re-derives instead of echoing stored detail, the load-bearing assertion).
- `tests/test_duplicate_cluster_preview.py` — 9 cases for `derive_cluster` + the `/cluster-preview` endpoint (partial deletion, fully-resolved, unrelated-cluster refusal).
- `tests/test_album_fields.py` — 10 cases for the shared `display_artist` + `sort_key` cascades, including the Mancini regression.
- `tests/test_audio_analysis.py`, `tests/test_cover_upload.py`, `tests/test_failure_excerpt.py` — feature-specific smoke (BPM/ReplayGain, manual cover upload, AI job transcript capture).

Tests redirect `MUSAIC_CONFIG_DIR` to a tmp dir via `tests/conftest.py` so the user's real `~/.musaic` is untouched. The env var is set at the very top of conftest.py, before any `musaic.*` import, because `config.CONFIG_DIR` is captured at module-load time.

### Diagnostics
```bash
python diagnose_claude.py [album_id]   # Test Claude AI integration
python diagnose_album.py <album_id>    # Dump album metadata from SQLite
python diagnose_fingerprints.py        # Check AcoustID setup
python diagnose_chromaprint.py         # Check fpcalc binary
```

## Architecture

### Three-Tier Structure

- **Frontend** (`frontend/src/`): Vite + React + TypeScript + Tailwind CSS v4 + Radix UI + React Router + TanStack Query
- **Backend** (`musaic/`): FastAPI REST API + SSE streams on `:8765`
- **Persistence** (`~/.musaic/library.db`): SQLite v12 schema; source of truth for jobs, findings, settings, and metadata cache

### Resolution-Driven UI Affordances (album detail sheet)

Three components surface the resolver's deterministic output so the user can act on free MusicBrainz data without going through the AI job queue:

- **`ResolutionBadge`** (in `AlbumHero`) — chip showing anchor strategy + match ratio (`Anchored • fingerprints (12/12)`, `Anchored • directory match`, `Unanchored`, `Resolving…`). Hover shows the full Tier-by-Tier trace.
- **`MbPreviewSection`** (in the sheet body) — inline diff + Apply button. Visible only when `anchor != null`, real diffs exist, AND no AI job is in flight. Clicking Apply uses the existing `useApplyStream` — same per-file SSE feedback, same mutation pipeline. Backend invalidates the resolver cache after apply, the React Query refetch returns no diffs, the section unmounts.
- **`FingerprintPoolPill`** (in `Titlebar`) — appears while the eager fingerprinting pool is processing; shows running counter `Fingerprinting 47/342`. Auto-hides on every other state.

The "Suggest fixes" / "Find cover art" buttons relabel to "Find MusicBrainz corrections" / "Get cover from MusicBrainz" when the resolver covers everything — they still route through the job queue (synthesizer skips Claude internally), but the label signals "instant + free" to the user.

Per-track tracks with a confident `recording_mbid` from AcoustID get a small fingerprint icon next to the title in `TrackList`, linking to the MB recording page.

### Key Backend Modules

| Module | Role |
|---|---|
| `main.py` | App construction, lifespan, frontend SPA, CLI entry — wires the routers + worker together |
| `api/` | FastAPI routers: `library` (config/scan/albums/cover/tracks), `tags` (track tags + vocabulary), `jobs` (AI queue REST/SSE), `settings` (settings + DB backup/snapshots), `health` (checks framework REST/SSE), `apply` (album apply SSE) |
| `services/` | Cross-router helpers: `albums` (resolve/read/sidecars), `apply` (single album mutation generator used by both the streaming endpoint and the deterministic check-fix path), `resolver` (tiered metadata cascade — cache → MB search → fingerprint vote → AI), `scan` (background library-scan task with replayable event buffer), `fingerprint_pool` (eager fpcalc/AcoustID warmer kicked off after each scan), `post_scan` (low-level worker that runs `runs_during_scan` checks; driven by the bus subscriber — see Architectural patterns below), `event_subscribers` (registers the 5 bus subscribers that fan out cross-cutting concerns), `worker` (AI job dispatcher loop), `sse` (event helpers + `jobs_changed` event) |
| `library_events.py` | In-process pub/sub for state mutations. Every producer publishes typed events; subscribers handle post-scan reruns, resolver invalidation, thumb drops, and findings-stream notifies. See [Architectural patterns](#architectural-patterns). |
| `album_fields.py` | Shared accessors for the album-artist + sort cascades. Use `display_artist(album, include_dir_artist=...)` instead of reinventing `album_artist or artist or ""` at every callsite. |
| `scanner.py` | Filesystem scan + album parsing (directory-name convention) |
| `db.py` | SQLite schema + migrations |
| `ai.py` | Claude SDK integration (system prompts, JSON extraction, MCP support) |
| `checks/` | Health check framework (7 checks; `base.py`, `runner.py`, `__init__.py`) |
| `jobs.py` | Async AI job queue |
| `tags.py` | FLAC mutation pipeline (Vorbis tag writes + renames + cover embed) |
| `mb_lookup.py` | Direct MusicBrainz / Cover Art Archive lookups |
| `settings.py` | Key-value settings store (SQLite-backed) |
| `fingerprint.py` | Chromaprint + AcoustID (optional; app degrades gracefully if missing) |

### SSE + Status Endpoints

Long-running tasks AND cross-cutting cache freshness flow through Server-Sent Events. Three streams are App-level (one connection for the app's lifetime); the rest are per-operation:

App-level:
- `GET /api/library-events/stream` — every `library_events` bus event relayed as a named SSE event. Drives `useLibraryEventsStream`, which owns TanStack Query cache invalidation for albums/tracks/resolution/findings/checks. See [Architectural patterns](#architectural-patterns) #5.
- `GET /api/ai/jobs/stream` — job status updates (queued → running → done/failed). Drives `useJobsStream`.
- `GET /api/checks/stream` — check / finding state changes. Drives `useChecksStream`.
- `GET /api/fingerprint-pool/stream` — eager-fingerprinting pool state (`idle | running | done | cancelled | unavailable`). Drives `useFingerprintPoolStream`. Replaced the old 2s polling.

Per-operation (die at end of operation):
- `GET /api/scan/stream` — yields albums as scanned (kicks off a background scan if none running; client disconnect leaves the scan running)
- `POST /api/albums/{id}/apply/stream` — per-file diff + application progress
- `POST /api/albums/{id}/analyze/stream` — per-file BPM / ReplayGain progress
- `POST /api/checks/{check_id}/quick-fix-all` — per-finding outcome stream for bulk quick-fix runs

Status / lookups (not SSE):
- `GET /api/scan/status` — current scan state (poll this to detect an in-flight scan on page load)
- `GET /api/fingerprint-pool/status` — one-shot pool state (live consumers use the `/stream` channel above)
- `GET /api/albums/{id}/resolution` — full resolver output for one album (anchor + canonical fields + per-track matches + trace).

The frontend hooks (`useScanStream`, `useApplyStream`, `useChecksStream`, `useLibraryEventsStream`, `useJobsStream`, `useFingerprintPoolStream`, `useAlbumResolution`) consume these. Streams terminate on client disconnect; `EventSource`-based hooks auto-reconnect, the `fetch`+`postSse` ones don't (per-operation streams that die anyway).

### Directory Naming Convention (flac-jacket)

Albums are identified by directory name:
```
Artist - Album [FLAC]
Artist - Album (disc 1 of 3) [FLAC]
Compilations - Album [FLAC]
Various Artists - Album [FLAC]
```
`scanner.py` parses these into `dir_artist`, `dir_album`, `is_compilation`, and `disc_number` fields. This is the **authoritative anchor** for album identity and seeds MusicBrainz lookups.

### AI Integration

- System prompt is static and cached (5-min ephemeral TTL; 1-hour with `ENABLE_PROMPT_CACHING_1H=true`)
- Per-call user messages contain only the album payload + optional hint (`<album>`, `<hint>` XML tags)
- Tools: WebSearch + WebFetch (built-in); optional FireCrawl MCP (opt-in, requires `settings.firecrawl_api_key`)
- Opaque "exit code 1" errors are treated as rate-limits; job queue auto-pauses, user resumes manually
- Model: Claude Sonnet 4.6 (more reliable tool use than Haiku)

### Metadata Resolver (`services/resolver.py`)

The single source of "what does the authoritative world say about this album." Lazy by default — runs only when something asks. Memoized per `(album_id, content_hash)` so multiple consumers (UI badge, AI worker, multiple checks) share one resolution.

**Tier 0 — Bootstrap (find the MBID anchor by any free means):**
- 0a. Cache hit (`albums.mb_release_group_mbid` already set)
- 0b. Directory + duration MB search (`mb_lookup.find_release`; user hint biases candidate reranking when supplied)
- 0c. Per-track AcoustID fingerprint vote (each `recording_mbid` → containing release-groups → weighted vote by AcoustID score; share ≥ 80% = high confidence, 50–80% = medium, below = no anchor)

Strategies run cheapest-first; first confident match wins. Anchors discovered by 0b/0c are persisted via `cache.set_album_mb_anchor` so subsequent runs short-circuit at 0a.

**Tier 1 — Refine from anchor:** fetch the canonical tracklist, align FLACs to MB recordings by `(tracknumber, duration ±2s)` with a unique-duration fallback. Populates `canonical_title`, `canonical_album_artist`, `canonical_year`, `canonical_album_artist_sort`, `canonical_genre`, `cover_url` (CAA-verified), and per-track `canonical_title` / `canonical_artist` / `canonical_tracknumber`.

**Tier 2 — User hint** biases candidate selection inside Tier 0b.

**Tier 3 — AI** is NOT invoked here. The AI worker (`services/worker.run_one_job`) calls `resolve_album` first; if `anchor != null` AND `unresolved_track_count == 0`, it synthesizes a `MetadataSuggestion` via `ai.synthesize_metadata_from_resolution` and skips Claude entirely. Otherwise Claude is called with the resolution embedded in the prompt (anchor MBID, per-track fingerprint matches, list of unresolved filenames) so it focuses on gaps rather than re-doing identification.

**Endpoint:** `GET /api/albums/{id}/resolution` returns the full `AlbumResolution.to_dict()` including the trace. The frontend's `useAlbumResolution` hook consumes it.

**Cache invalidation:** `services.apply.apply_album_changes_iter` calls `resolver_service.invalidate(album_id)` after each successful apply (the resolver's own content_hash doesn't catch tag-only edits).

### Eager Fingerprinting (`services/fingerprint_pool.py`)

After every successful scan completes, `services.scan._run_scan` kicks off `fingerprint_pool.warm_library` as a fire-and-forget task. The pool walks albums where `track_count > COUNT(track_fingerprints)`, calls `fingerprint_album` (fpcalc + AcoustID lookup) serially with a 0.5s inter-album sleep. This warms Tier 0c so an album the user opens for the first time anchors instantly instead of paying the per-track fpcalc latency on demand.

Skipped silently when fpcalc / AcoustID isn't configured (status flips to `unavailable`). Cancelled cleanly during app shutdown via the lifespan handler. UI surfaces progress via the titlebar's `FingerprintPoolPill`, which subscribes to `/api/fingerprint-pool/stream` (the pool sets an `asyncio.Event` after every state mutation; the SSE handler awaits and yields).

### Health Check Framework

- **Base class**: `musaic.checks.base.Check` + `CheckMeta` (id, label, description, category, cost, fix_kind)
- **Cost tiers**: `cheap` (post-scan), `medium` (weekly), `heavy` (monthly) — user-overridable in settings
- **Fix kinds**: `none` (report-only), `ai-job` (enqueue Claude task), `deterministic` (sync mutation)
- **Runner**: upserts findings by `(check_id, album_id, summary)`; auto-resolves disappearing findings
- **Scope**: `CheckScope.kind` ∈ `"all"` | `"incremental"` | `"albums"`. The runner's auto-resolve is scope-aware — a per-album rerun only resolves findings for the listed albums, not unrelated ones.
- **Add a new check**: Create `musaic/checks/your_check.py` inheriting `Check`, set `meta`, implement `run()` (and optionally `fix()`), then register in `checks/__init__.py`

**Active checks (7)** after the 10→7 consolidation:
- `unidentified-album` (cheap, runs during scan) — merged `missing-fingerprints` + `missing-mbid`. Fix cascades through resolver Tier 0c (fingerprint vote) → AI escalation only when no anchor found
- `tags-out-of-sync` (medium, on-demand) — merged `missing-core-tags` + `missing-sort-tags` + `mb-canonical-mismatch`. Diffs current Vorbis tags against `resolution.canonical_*`; fix applies what the resolver can fill, falls through to AI for the rest
- `no-cover-art` (cheap, runs during scan) — MB / CAA lookup via apply pipeline; AI escalation otherwise
- `track-gaps` (cheap, runs during scan) — tracknumber gaps; fix tries title-match then fingerprint-renumber against MB tracklist
- `duplicate-tracks` (cheap, runs during scan) — Chromaprint similarity clustering on the album's `track_fingerprints` rows; resolve dialog re-derives the cluster live via the plan-preview endpoint (see [Architectural patterns](#architectural-patterns))
- `inconsistent-artist-sort` (cheap, runs during scan) — clusters albums sharing an artist with conflicting `*_sort` values; bulk-apply via custom endpoint
- `filename-mismatch` (medium, on-demand) — checks each FLAC's filename + album dir against user-configured templates; resolve dialog re-derives the rename plan live (plan-preview pattern, see [Architectural patterns](#architectural-patterns))

### Mutation Pipeline

Single generator at `services/apply.py:apply_album_changes_iter` is the only path that mutates an album:
1. Download cover bytes (if requested) and normalize to JPEG q95
2. Stream `tags.apply_to_album_iter` per FLAC — Vorbis writes via `mutagen` + optional renames
3. Migrate `track_tags` rows on filename rename
4. Save `cover.jpg` sidecar when cover changed
5. Resync the track-tag sidecar JSON
6. Re-scan + upsert the single album row (don't drop the whole snapshot)
7. Publish `AlbumTagsChanged` (+ optional `AlbumCoverChanged`) on the events bus

Steps that used to live inline here — thumb drop, resolver-memo invalidation, post-scan check rerun — now happen via `library_events` subscribers fanning out from step 7. The generator's only job is mutate + publish; the bus handles the rest. See [Architectural patterns](#architectural-patterns) for why.

Both consumers — the `/api/albums/{id}/apply/stream` endpoint and the deterministic check-fix path (`checks/_apply.py`) — drive this same generator. The streaming endpoint forwards every yielded `(event, payload)` to the SSE socket; the check-fix path drains the iterator and reports a single ok/message outcome.

## Architectural patterns

The patterns below are the load-bearing ones. They came out of a retrospective on recurring whack-a-mole bugs (issue #1) — each one retires a class of bug rather than instances. Reach for them when designing new mutations / endpoints / dialogs; the prior anti-pattern is named alongside so the *why* stays findable.

### 1. Event-driven state propagation (publish, never invoke)

**Anti-pattern (retired):** every state-mutating helper expected its caller to remember a fan-out: fire `run_post_scan_checks`, invalidate the resolver memo, drop the thumb, notify the SSE state stream. Different callers remembered different subsets. Five "satisfied finding lingers" bugs landed before we factored it out.

**Pattern:** producers publish a typed event at the moment of mutation. Subscribers in `musaic/services/event_subscribers.py` handle the cross-cutting concerns.

```python
# services/apply.py — end of apply_album_changes_iter
library_events.publish(library_events.AlbumTagsChanged(
    album_id=album_id, fields=tuple(suggestions.keys()),
))
if had_art_change:
    library_events.publish(library_events.AlbumCoverChanged(album_id=album_id))
```

Event types live in `musaic/library_events.py`:
- `AlbumTagsChanged(album_id, fields)` — apply, audio analysis
- `AlbumCoverChanged(album_id)` — apply (when art was touched)
- `AlbumMbAnchorSet(album_id, source)` — mb_lookup, worker AI MBID persist, resolver Tier 0b/0c
- `TrackFingerprinted(album_id, filenames)` — fingerprint.fingerprint_album
- `AlbumRenamed(old_album_id, new_album_id)` — filename_mismatch.execute_renames (old==new signals file-only rename, different ids signal dir rename)
- `AlbumDeleted(album_id)` — cache.delete_album
- `LibraryScanCompleted(album_count)` — services.scan after a full scan
- `BulkBatchCompleted()` — quick-fix-all dispatcher after its loop exits

**Recursion guard:** `library_events.in_post_scan_rerun: ContextVar[bool]` is set by `run_post_scan_checks` for the duration of any rerun. Subscribers consult it and skip when set — this prevents a check writing data that publishes an event that would route back into the runner. Background subscriber threads inherit the flag via `contextvars.copy_context().run(...)` — load-bearing for the guard. Idempotency is the real safety net (Tier 0a cache hits replace Tier 0b/0c side-effect writes on the second pass; nothing new gets published, recursion terminates).

**Bulk-loop suppression:** `checks/_apply.in_bulk_run: ContextVar[bool]` is set by bulk dispatchers (quick-fix-all, inconsistent-artist-sort bulk apply) for the duration of their loop. The `_post_scan_rerun` subscriber checks it and skips. The dispatcher resets the flag and publishes `BulkBatchCompleted` as its last act — one coalesced library-wide rerun instead of N per-album ones.

**When adding a new mutation:** publish the relevant event. Don't import `run_post_scan_checks` / `resolver.invalidate` / `thumbs.delete_thumb` directly — the subscriber chain handles all of them. The only direct callers of `run_post_scan_checks` live in `event_subscribers.py`; anywhere else is a smell.

Design doc: `docs/library_events_design.md` (covers the recursion-guard reasoning, subscriber priorities, what's intentionally out of scope).

### 2. Findings store references, not payloads (re-derive at dialog open)

**Anti-pattern (retired):** findings stored their full detail (`detail.renames` for filename-mismatch, `detail.filenames` for duplicate-tracks) at check-run time. Resolve dialogs read that payload as truth. If anything changed between detection and dialog-open — sibling Resolve click already ran, user edited tags by hand, file deleted via another tool — the dialog showed ghost rows that either failed to apply or re-did work that was already done.

**Pattern:** findings carry stable references (`album_id`, sometimes `filenames` as identity hints). Resolve dialogs call a `*-preview` endpoint on open that re-derives the plan against current disk + DB state.

Implemented for two checks today:
- `GET /api/checks/filename-mismatch/findings/{id}/plan-preview` → drives `FilenameMismatchResolveDialog`
- `GET /api/checks/duplicate-tracks/findings/{id}/cluster-preview` → drives `DuplicateResolveDialog`

Both endpoints return a stable shape including `is_empty: bool` and (for duplicate-tracks) `removed_filenames: list[str]`. The dialog renders loading / empty / error / "X file was already trashed" branches deterministically.

**When adding a new Resolve dialog:** prefer a preview endpoint over reading `finding.detail` for anything load-bearing. Stored detail is fine for display labels / summaries (the data won't change meaningfully); use it as a fallback when the preview fetch is in-flight or errors, never as the authoritative source for what to apply.

### 3. Path identity uses `os.path.samefile`, never `==`

**Anti-pattern (retired):** `Path == Path` compares strings. On case-insensitive APFS, `Path('cover.jpg') != Path('cover.JPG')` even though they're the same inode. On NFC vs NFD Unicode normalization, `Path('café') != Path('café')` even though they're the same dir. Three bugs landed this way (cover.jpg cleanup deleting the cover it just wrote; dir-rename refusing safe renames; cover-upload collision detection).

**Pattern:** when the question is "do these two paths refer to the same file?", use `os.path.samefile(str(a), str(b))`. Wrap in `try/except OSError` because it raises on missing paths.

When the question really is "are these the same string?" (config keys, route params, log identifiers), `==` is correct — but those should be rare in path-handling code. If you find yourself comparing path-shaped values with `==`, ask which question you're actually answering.

### 4. Shared accessors for album-field cascades

**Anti-pattern (retired):** every consumer (file template, dir template, AlbumHero, finding summary, sort key) reinvented its own `album.album_artist or album.artist or ""` cascade. The Mancini bug landed because the dir-template renderer used `album_artist → artist → dir_artist` and the file-template renderer only did `album_artist`; a library carrying only an ARTIST tag (no ALBUMARTIST) silently lost the artist token from the proposed directory name.

**Pattern:** `musaic/album_fields.py` (backend) and `frontend/src/lib/albumFields.ts` (frontend) own the cascades.

```python
# Backend
album_fields.display_artist(album)                          # → "Henry Mancini" or ""
album_fields.display_artist(album, include_dir_artist=True) # for path-rendering callers
album_fields.sort_key(album)                                # → "mancini, henry"
```

```typescript
// Frontend
displayArtist(album)                       // → "Henry Mancini" or ""
displayArtist(album, "Unknown artist")     // → "Henry Mancini" or "Unknown artist"
```

`mb_lookup.find_release` deliberately keeps its own cascade (`dir_artist → album_artist → artist`) because the on-disk name is the search anchor; it's the one documented exception. Otherwise, reach for the shared helper before adding a new fallback chain.

### 5. SSE-driven frontend cache invalidation (subscribe once, invalidate by event)

**Anti-pattern (retired):** every frontend mutation handler had a 3–5 line `qc.invalidateQueries(...)` checklist of which TanStack Query caches to bust — `["albums-snapshot"]`, `["tracks", id]`, `["album-resolution", id]`, `["findings"]`, `["checks"]`. Different handlers remembered different subsets (DetailSheet's apply hit all 5, PropTable's clear-anchor missed `["tracks"]`, cover upload missed nothing but in a different order). 57 invalidations spread across ~10 files; every new endpoint a fresh opportunity to forget.

**Pattern:** publish backend events through the `library_events` bus (pattern #1 above), relay them to a `/api/library-events/stream` SSE channel, and let one App-level `useLibraryEventsStream` subscriber own the invalidation map. Mutation handlers do nothing — they trigger the backend operation, the bus event round-trips back, the right cache keys get invalidated.

```typescript
// frontend/src/hooks/useLibraryEventsStream.ts (excerpt)
es.addEventListener("AlbumTagsChanged", (ev) => {
  const { album_id } = JSON.parse(ev.data);
  qc.invalidateQueries({ queryKey: ["albums-snapshot"] });
  qc.invalidateQueries({ queryKey: ["tracks", album_id] });
  qc.invalidateQueries({ queryKey: ["album-resolution", album_id] });
  qc.invalidateQueries({ queryKey: ["findings"] });
  qc.invalidateQueries({ queryKey: ["checks"] });
});
```

Wired through `musaic/services/library_events_relay.py` — a bus subscriber at priority 400 (last, so SSE consumers see post-mutation state) that fans every event out to per-connection `asyncio.Queue(maxsize=256)` instances. Cross-thread enqueue uses `loop.call_soon_threadsafe` because background subscribers publish from daemon threads. Bounded queue drops oldest on overflow — clients reconnect/refetch; they don't need every event, they need an eventually-correct cache.

**When adding a new mutation endpoint:** publish the relevant bus event (pattern #1). Don't add a frontend `invalidateQueries` call — `useLibraryEventsStream` already handles it. Exceptions: caches the bus doesn't see today (`["jobs"]` via its own SSE channel, `["vocabulary"]` / `["album-tags"]` via `setQueryData`, preview-endpoint caches the dialog manages).

**`staleTime: Infinity` as the default:** caches kept fresh by SSE should use `staleTime: Infinity` so refetch-on-focus doesn't race with the bus. Any non-Infinity stale time in `frontend/src/hooks/` needs an explicit comment explaining why polling/refetch is correct there.

Design doc: `docs/connectivity_design.md` (covers the relay's priority + backpressure reasoning, the per-handler invalidation diff, what's explicitly out of scope).

## Configuration

### Environment Variables
- `CLAUDE_CODE_OAUTH_TOKEN` (preferred) or `ANTHROPIC_API_KEY` — Claude auth
- `ENABLE_PROMPT_CACHING_1H=true` — opt-in 1-hour prompt cache
- `DEBUG=true` — verbose logging

### Runtime Files
- `~/.musaic/config.json` — selected library directory
- `~/.musaic/library.db` — SQLite cache (schema version tracked in `musaic/db.py::SCHEMA_VERSION`; migrations run on startup)
- `~/.musaic/thumbs/` — pre-extracted JPEG thumbnails
- `~/.musaic/trash/` — soft-deleted files (delete-duplicate uses this; restore by moving back)
- `.env` at project root or `~/.musaic/.env` — auth (project root wins)

## Known Issues

_(none currently tracked — see `docs/IMPROVEMENTS.md` for the historical list)_

## Debugging Tips

- Inspect schema: `sqlite3 ~/.musaic/library.db ".schema"`
- SSE issues: Vite dev proxy in `frontend/vite.config.ts` strips `content-length` to enable streaming
- MusicBrainz rate limits: 1-second backoff required between requests
- FLAC mutations: always save covers as JPEG to avoid format bloat; `mutagen` requires explicit `.save()`
