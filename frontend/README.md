# Musaic — Frontend

Vite + React + TypeScript + Tailwind v4 + Radix + TanStack Query + React Router.

The UI talks to the Musaic backend over plain HTTP (`fetch`, `EventSource`,
`<img>`) on the same origin. In the desktop app that backend runs inside the
app; for UI development you run the standalone API server (`musaic-api`,
from the Rust workspace in `../rust`) next to Vite.

## First-time setup

```bash
cd frontend
npm ci
```

## Dev server

```bash
npm run dev:all
```

runs both processes in one terminal: the API server
(`cargo run --manifest-path ../rust/Cargo.toml -p musaic-api -- --config-dir ../work/dev-config --verification-memory-secrets`,
on `127.0.0.1:8765`, keys kept in memory, config in `../work/dev-config`)
and Vite on <http://localhost:5173>, which proxies `/api/*` to `:8765`.

To run them separately:

```bash
# terminal 1, from the repository root
cd rust && cargo run -p musaic-api -- --config-dir "$(mktemp -d)/config" --verification-memory-secrets --network offline

# terminal 2
cd frontend && npm run dev
```

Drop `--network offline` for live MusicBrainz / AcoustID / Cover Art Archive
lookups. Point the app at a copy of some FLACs, never at your real
collection: Musaic writes tags and renames files.

## Checks

```bash
npm run lint
npm run typecheck          # app sources
npm run typecheck:test     # the *.test.ts(x) files (tsconfig.vitest.json)
npm test                   # vitest (jsdom)
```

## Production build

```bash
npm run build
```

Outputs to `frontend/dist/`. The desktop app bundles it, and the API server
serves it at `/`.

## Project layout

```
src/
  main.tsx           # entry; mounts <App> with QueryClient + Router
  App.tsx            # top-level grid: titlebar / sidebar / route outlet
  styles/            # Tailwind import + design tokens + base styles
  components/        # titlebar, sidebar, album sheet, dialogs, ...
  hooks/             # data hooks; App-level SSE streams own cache invalidation
  lib/               # API client, types, pure helpers
  pages/             # Library, Health, Jobs, Settings
  test/              # Vitest setup and fixtures
```

Path alias `@/*` resolves to `src/*` (configured in `tsconfig.app.json` +
`vite.config.ts`).
