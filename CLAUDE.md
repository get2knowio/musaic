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
uv run media-manager        # Backend on :8765; serves built frontend
cd frontend && npm run dev  # Frontend dev server on :5173 (proxies /api to :8765)
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
uv run pytest                          # Backend integration tests (hermetic, no FLAC fixtures)
```
Tests redirect `MEDIA_MANAGER_CONFIG_DIR` to a tmp dir via `tests/conftest.py` so the user's real `~/.media_manager` is untouched.

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
- **Backend** (`media_manager/`): FastAPI REST API + SSE streams on `:8765`
- **Persistence** (`~/.media_manager/library.db`): SQLite v12 schema; source of truth for jobs, findings, settings, and metadata cache

### Key Backend Modules

| Module | Role |
|---|---|
| `main.py` | App construction, lifespan, frontend SPA, CLI entry — wires the routers + worker together |
| `api/` | FastAPI routers: `library` (config/scan/albums/cover/tracks), `tags` (track tags + vocabulary), `jobs` (AI queue REST/SSE), `settings` (settings + DB backup/snapshots), `health` (checks framework REST/SSE), `apply` (album apply SSE) |
| `services/` | Cross-router helpers: `albums` (resolve/read/sidecars), `post_scan` (run cheap checks after a scan/apply), `worker` (AI job dispatcher loop), `sse` (event helpers + `jobs_changed` event) |
| `scanner.py` | Filesystem scan + album parsing (directory-name convention) |
| `db.py` | SQLite schema + migrations |
| `ai.py` | Claude SDK integration (system prompts, JSON extraction, MCP support) |
| `checks/` | Health check framework (11 checks; `base.py`, `runner.py`, `__init__.py`) |
| `jobs.py` | Async AI job queue |
| `tags.py` | FLAC mutation pipeline (Vorbis tag writes + renames + cover embed) |
| `mb_lookup.py` | Direct MusicBrainz / Cover Art Archive lookups |
| `settings.py` | Key-value settings store (SQLite-backed) |
| `fingerprint.py` | Chromaprint + AcoustID (optional; app degrades gracefully if missing) |

### SSE Streams

Long-running tasks emit progress via Server-Sent Events:
- `POST /api/scan/stream` — yields albums as scanned
- `GET /api/ai/jobs/stream` — job status updates (queued → running → done/failed)
- `POST /api/checks/stream` — finding discovery during check runs
- `POST /api/albums/{id}/apply/stream` — per-file diff + application progress

The frontend hooks (`useScanStream`, `useApplyStream`, `useChecksStream`) consume these. Streams terminate on client disconnect with no auto-reconnect; the hooks handle this at a higher level.

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

### Health Check Framework

- **Base class**: `media_manager.checks.base.Check` + `CheckMeta` (id, label, description, category, cost, fix_kind)
- **Cost tiers**: `cheap` (post-scan), `medium` (weekly), `heavy` (monthly) — user-overridable in settings
- **Fix kinds**: `none` (report-only), `ai-job` (enqueue Claude task), `deterministic` (sync mutation)
- **Runner**: upserts findings by `(check_id, album_id, summary)`; auto-resolves disappearing findings
- **Add a new check**: Create `media_manager/checks/your_check.py` inheriting `Check`, set `meta`, implement `run()` (and optionally `fix()`), then register in `checks/__init__.py`

### Mutation Pipeline

When the user applies AI suggestions (`tags.py`):
1. Dry-run diff preview
2. Write Vorbis comments via `mutagen` (explicit `.save()` required)
3. Optionally rename files to match track-number suggestions
4. Download + embed cover as JPEG or save as `cover.jpg` sidecar
5. Drop thumbnail cache and rescan the album
6. Persist track-tag sidecars (JSON colocated with FLACs)

Note: `checks/_apply.py` has a separate deterministic fix path with duplicated logic — a known simplification target (see `docs/IMPROVEMENTS.md`).

## Configuration

### Environment Variables
- `CLAUDE_CODE_OAUTH_TOKEN` (preferred) or `ANTHROPIC_API_KEY` — Claude auth
- `ENABLE_PROMPT_CACHING_1H=true` — opt-in 1-hour prompt cache
- `DEBUG=true` — verbose logging

### Runtime Files
- `~/.media_manager/config.json` — selected library directory
- `~/.media_manager/library.db` — SQLite cache (schema v12)
- `~/.media_manager/thumbs/` — pre-extracted JPEG thumbnails
- `.env` at project root or `~/.media_manager/.env` — auth (project root wins)

## Known Issues

1. **Scan lifecycle**: SSE-driven; if the browser disconnects mid-scan, the scan halts and the library cache is left partial
2. **Health detail cache invalidation**: Fixing or ignoring a finding doesn't immediately refresh the detail view
3. **Deterministic vs AI fix paths** have duplicate mutation logic (`checks/_apply.py` vs `tags.py`)

## Debugging Tips

- Inspect schema: `sqlite3 ~/.media_manager/library.db ".schema"`
- SSE issues: Vite dev proxy in `frontend/vite.config.ts` strips `content-length` to enable streaming
- MusicBrainz rate limits: 1-second backoff required between requests
- FLAC mutations: always save covers as JPEG to avoid format bloat; `mutagen` requires explicit `.save()`
