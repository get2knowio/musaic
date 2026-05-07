# Media Manager — Frontend

Vite + React 18 + TypeScript + Tailwind v4 + Radix + TanStack Query + React Router.

Design source: [`../design-handoff/README.md`](../design-handoff/README.md).
Implementation guide: [`../design/SPEC.md`](../design/SPEC.md).

## First-time setup

```bash
cd frontend
npm install
```

## Dev server

```bash
npm run dev
```

Opens on <http://localhost:5173>. The Vite config proxies `/api/*` to the
FastAPI server on `:8765`, so you need to be running the backend in another
terminal:

```bash
# in another terminal, from project root
.venv/bin/media-manager
```

> Note: `uv run media-manager` is broken on this Mac (uv-run-specific issue
> we hit earlier); use the direct script.

## Production build

```bash
npm run build
```

Outputs to `frontend/dist/`. The FastAPI server will (eventually) serve this
directly at `/`. Until that wiring is done, the legacy vanilla-JS UI at
`media_manager/static/` is what loads in production.

## Project layout

```
src/
  main.tsx           # entry; mounts <App> with QueryClient + Router
  App.tsx            # top-level grid: titlebar / sidebar / route outlet
  styles/
    index.css        # Tailwind import + design tokens (OKLCH) + base styles
  components/
    Titlebar.tsx
    Sidebar.tsx
    FilterBar.tsx
  pages/
    Library.tsx      # / and /library/:albumId
    Settings.tsx     # /settings
```

Path alias `@/*` resolves to `src/*` (configured in `tsconfig.app.json` +
`vite.config.ts`).
