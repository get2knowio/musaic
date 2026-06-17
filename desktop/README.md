# Musaic desktop (Electron) — Phase 0 spike

Tracks issue #8 ("Package musaic as a cross-platform Electron desktop app").
This directory is the Electron shell that wraps the existing FastAPI + React
app as a double-clickable desktop application.

**Status: Phase 0 spike.** The shell spawns the *dev* backend (`uv run
musaic`) as a localhost HTTP sidecar, health-gates on `/api/health`, and
points a window at the sidecar origin. Python bundling (Phase 1), native
binaries (Phase 2), packaging/signing (Phases 3–4), AI gating (Phase 5),
and auto-update (Phase 6) are not done yet.

## Architecture (why HTTP sidecar, not IPC)

The frontend uses only **relative** `/api` paths + `EventSource` SSE, and
FastAPI already self-serves the built SPA in production. So the lowest-risk
model is: Electron main picks a free port, spawns the backend bound to
`127.0.0.1:<port>`, waits for `/api/health`, then
`win.loadURL("http://127.0.0.1:<port>")`. Same-origin → relative `/api`
calls and SSE work with **zero frontend changes**. IPC would mean rewriting
the streaming + fetch layers for no gain. Full rationale in issue #8.

## Backend contract (already in `musaic/`)

Phase 0 added the three injection points the shell relies on:

- `GET /api/health` — cheap readiness probe (no DB/scan/checks). The shell
  backoff-polls this before `loadURL`.
- `MUSAIC_PORT` / `MUSAIC_HOST` — honored by `musaic.main.run()`; the shell
  injects the free port it picked.
- `MUSAIC_FRONTEND_DIR` — overrides where FastAPI serves the SPA from (for
  Phase 1+, when the packaged `frontend/dist` isn't a sibling of the
  `musaic` package).

## Run the spike (requires the dev backend to work)

```bash
cd desktop
npm install
npm start          # tsc build + electron .
```

The shell runs `uv run musaic` from the repo root, so the Python env must
be synced (`uv sync`) and `uv` on PATH. This is the dev-loop spike, not the
shippable bundle.

## Files

- `src/main.ts` — main process: single-instance lock, free-port pick,
  sidecar spawn, health gate, window, teardown (SIGTERM → SIGKILL).
- `src/preload.ts` — intentionally empty (renderer needs no IPC bridge).
- `package.json`, `tsconfig.json` — build config.

## Not yet present (later phases)

`electron-builder.yml`, `entitlements.mac.plist`, `build/fetch-python.*`,
`build/build-sidecar.*`, `build/fetch-binaries.*`, and the assembled
`resources/` tree all arrive in Phases 1–4.
