# Contributing

## Setup

```bash
# Backend (Python 3.10+)
uv sync

# Frontend
cd frontend && npm install
```

## Running in development

The backend and frontend run separately:

```bash
# Terminal 1 — backend on :8765
uv run media-manager

# Terminal 2 — frontend dev server on :5173 (proxies /api/* to :8765)
cd frontend && npm run dev
```

Open <http://localhost:5173> for the live-reloading frontend.

## Building for production

```bash
cd frontend && npm run build
```

FastAPI serves the resulting `frontend/dist/` at the root. Run `uv run media-manager` and open <http://localhost:8765>.

## Type checking

```bash
cd frontend && npm run typecheck
```

**Note:** Currently broken — known issue, not blocking day-to-day work.

## Diagnostics

Developer scripts live in `scripts/`. Run them from the project root:

```bash
uv run python scripts/diagnose_claude.py [album_id]   # verify AI integration
uv run python scripts/diagnose_album.py <substring>   # inspect album + fingerprints from DB
uv run python scripts/diagnose_fingerprints.py        # debug fingerprint persistence
uv run python scripts/diagnose_chromaprint.py         # debug chromaprint decode
```

## Adding a health check

1. Create `media_manager/checks/your_check.py` inheriting from `Check` (see `base.py`).
2. Set the `meta` class variable (`CheckMeta` with id, label, description, category, cost, fix_kind).
3. Implement `run(self, album, conn) -> list[Finding]`, and optionally `fix(self, finding, conn)`.
4. Register it in `media_manager/checks/__init__.py`.

Cost tiers: `cheap` (runs after every scan), `medium` (weekly), `heavy` (monthly). Fix kinds: `none`, `ai-job`, `deterministic`.

## Architecture notes

See [CLAUDE.md](CLAUDE.md) for module map, SSE stream details, and known issues.
