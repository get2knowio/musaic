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
uv run pytest                          # 48 backend tests (hermetic, no FLAC fixtures)
```

Test layout:
- `tests/test_api_smoke.py` — 18 cases exercising every router slice through TestClient (config, settings, vocab, jobs, health, scan, etc.). End-to-end SSE smoke for `/api/scan/stream` against an empty library.
- `tests/test_resolver.py` — 12 cases for the tiered resolver internals (cache hit, dir-search, fingerprint vote, alignment, memoization, synthesizer). Mocks `mb_lookup` + `fingerprint`.
- `tests/test_checks.py` — 15 cases for the resolver-consuming checks (`mb_canonical_mismatch`, `missing_mbid`, `missing_sort_tags`). Inserts album rows directly into the test DB; mocks the resolver to isolate check-level glue.
- `tests/test_fingerprint_pool.py` — 3 control-plane cases for the eager fingerprinting pool (unavailable short-circuit, idempotent re-call, no-op stop).

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
| `services/` | Cross-router helpers: `albums` (resolve/read/sidecars), `apply` (single album mutation generator used by both the streaming endpoint and the deterministic check-fix path), `resolver` (tiered metadata cascade — cache → MB search → fingerprint vote → AI), `scan` (background library-scan task with replayable event buffer), `fingerprint_pool` (eager fpcalc/AcoustID warmer kicked off after each scan), `post_scan` (run cheap checks after a scan/apply), `worker` (AI job dispatcher loop), `sse` (event helpers + `jobs_changed` event) |
| `scanner.py` | Filesystem scan + album parsing (directory-name convention) |
| `db.py` | SQLite schema + migrations |
| `ai.py` | Claude SDK integration (system prompts, JSON extraction, MCP support) |
| `checks/` | Health check framework (11 checks; `base.py`, `runner.py`, `__init__.py`) |
| `jobs.py` | Async AI job queue |
| `tags.py` | FLAC mutation pipeline (Vorbis tag writes + renames + cover embed) |
| `mb_lookup.py` | Direct MusicBrainz / Cover Art Archive lookups |
| `settings.py` | Key-value settings store (SQLite-backed) |
| `fingerprint.py` | Chromaprint + AcoustID (optional; app degrades gracefully if missing) |

### SSE + Status Endpoints

Long-running tasks emit progress via Server-Sent Events:
- `GET /api/scan/stream` — yields albums as scanned (kicks off a background scan if none running; client disconnect leaves the scan running)
- `GET /api/scan/status` — current scan state (poll this to detect an in-flight scan on page load)
- `GET /api/fingerprint-pool/status` — eager-fingerprinting pool state (started after every successful scan; idle/running/done/cancelled/unavailable)
- `GET /api/ai/jobs/stream` — job status updates (queued → running → done/failed)
- `GET /api/checks/stream` — finding discovery during check runs
- `POST /api/albums/{id}/apply/stream` — per-file diff + application progress
- `GET /api/albums/{id}/resolution` — full resolver output for one album (anchor + canonical fields + per-track matches + trace). Not SSE — single GET.

The frontend hooks (`useScanStream`, `useApplyStream`, `useChecksStream`, `useAlbumResolution`, `useFingerprintPoolStatus`) consume these. Streams terminate on client disconnect with no auto-reconnect; the hooks handle this at a higher level.

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

Skipped silently when fpcalc / AcoustID isn't configured (status flips to `unavailable`). Cancelled cleanly during app shutdown via the lifespan handler. UI surfaces progress via the titlebar's `FingerprintPoolPill` (polls `/api/fingerprint-pool/status` every 2s while running).

### Health Check Framework

- **Base class**: `musaic.checks.base.Check` + `CheckMeta` (id, label, description, category, cost, fix_kind)
- **Cost tiers**: `cheap` (post-scan), `medium` (weekly), `heavy` (monthly) — user-overridable in settings
- **Fix kinds**: `none` (report-only), `ai-job` (enqueue Claude task), `deterministic` (sync mutation)
- **Runner**: upserts findings by `(check_id, album_id, summary)`; auto-resolves disappearing findings
- **Add a new check**: Create `musaic/checks/your_check.py` inheriting `Check`, set `meta`, implement `run()` (and optionally `fix()`), then register in `checks/__init__.py`

**Resolver-consuming checks** (refactored to read off `services.resolver` instead of doing their own MB calls):
- `mb_canonical_mismatch` — diffs `current_tags` against `resolution.canonical_*`; the apply path reads canonical values off the same resolution result
- `missing_mbid` — fix tries the resolver first (now covers Tier 0c fingerprint vote, not just dir-search); falls through to AI only when no anchor found AND no hint supplied
- `missing_sort_tags` — for `album_artist_sort`, prefers `resolution.canonical_album_artist_sort` when the album is anchored; falls back to article rotation otherwise

Other checks remain MB-independent (`missing_core_tags`, `missing_fingerprints`, `no_cover_art`, `track_gaps`, `duplicate_tracks`, `inconsistent_artist_sort`, `filename_mismatch`).

### Mutation Pipeline

Single generator at `services/apply.py:apply_album_changes_iter` is the only path that mutates an album:
1. Download cover bytes (if requested) and normalize to JPEG q95
2. Stream `tags.apply_to_album_iter` per FLAC — Vorbis writes via `mutagen` + optional renames
3. Migrate `track_tags` rows on filename rename
4. Save `cover.jpg` sidecar when cover changed
5. Drop the album thumbnail
6. Resync the track-tag sidecar JSON
7. Re-scan + upsert the single album row (don't drop the whole snapshot)
8. Re-run cheap post-scan health checks so satisfied findings auto-resolve

Both consumers — the `/api/albums/{id}/apply/stream` endpoint and the deterministic check-fix path (`checks/_apply.py`) — drive this same generator. The streaming endpoint forwards every yielded `(event, payload)` to the SSE socket; the check-fix path drains the iterator and reports a single ok/message outcome.

## Configuration

### Environment Variables
- `CLAUDE_CODE_OAUTH_TOKEN` (preferred) or `ANTHROPIC_API_KEY` — Claude auth
- `ENABLE_PROMPT_CACHING_1H=true` — opt-in 1-hour prompt cache
- `DEBUG=true` — verbose logging

### Runtime Files
- `~/.musaic/config.json` — selected library directory
- `~/.musaic/library.db` — SQLite cache (schema v12)
- `~/.musaic/thumbs/` — pre-extracted JPEG thumbnails
- `.env` at project root or `~/.musaic/.env` — auth (project root wins)

## Known Issues

_(none currently tracked — see `docs/IMPROVEMENTS.md` for the historical list)_

## Debugging Tips

- Inspect schema: `sqlite3 ~/.musaic/library.db ".schema"`
- SSE issues: Vite dev proxy in `frontend/vite.config.ts` strips `content-length` to enable streaming
- MusicBrainz rate limits: 1-second backoff required between requests
- FLAC mutations: always save covers as JPEG to avoid format bloat; `mutagen` requires explicit `.save()`
