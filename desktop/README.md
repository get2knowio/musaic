# Musaic desktop (Electron) — Phases 0–1

Tracks issue #8 ("Package musaic as a cross-platform Electron desktop app").
This directory is the Electron shell that wraps the existing FastAPI + React
app as a double-clickable desktop application.

**Status: Phase 1 (Python bundling) done.** The `build/` scripts assemble a
relocatable standalone CPython with musaic + its full dep stack installed,
plus the built SPA, under `resources/`. The shell prefers that bundled
interpreter (`python -m musaic.main`) and only falls back to the dev loop
(`uv run musaic`) when no bundle is present. Native binaries (Phase 2),
electron-builder packaging/signing (Phases 3–4), AI gating (Phase 5), and
auto-update (Phase 6) are not done yet.

## Phase 1 — bundled sidecar (`build/`)

The backend ships as a **standalone CPython + installed site-packages** tree
(python-build-standalone via `uv`), not a PyInstaller freeze: musaic's stack
(librosa→numba→llvmlite, scipy, numpy, soundfile, pyacoustid) is exactly the
class that breaks frozen-exe hooks and works trivially against a real
interpreter. The tree is copied verbatim (relative internal symlinks intact)
so it relocates; the PEP-668 externally-managed marker is dropped because this
is a private app runtime we own.

```bash
cd desktop
npm install
npm run build:resources    # fetch-python → build-sidecar (≈760MB resources/)
npm run validate:sidecar   # boots `python -m musaic.main` headless, no dev env:
                           #   gate 1 imports the heavy stack + runs numba JIT
                           #     + a librosa op (JIT is the real relocation risk)
                           #   gate 2 health-gates /api/health and serves the SPA
```

Scripts (all `node build/*.mjs`, cross-platform, paths shared via `_common.mjs`):
- `build:python` → `fetch-python.mjs` — stage relocatable CPython into `resources/python`.
- `build:sidecar` → `build-sidecar.mjs` — `uv pip install .` into it + build/stage the SPA + trim caches.
- `validate:sidecar` → `validate-sidecar.mjs` — the two acceptance gates above.

**Size lever (open, issue #8):** librosa+numba+llvmlite+scipy ≈ 250MB power only
the optional BPM/analyze endpoints. Dropping them (BPM as opt-in) is the biggest
bundle-size win but is a product call — Phase 1 deliberately bundles the full
stack so we've proven it works before deciding to cut it.

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
- `MUSAIC_FRONTEND_DIR` — overrides where FastAPI serves the SPA from. The
  bundled `musaic` package isn't a sibling of `frontend/dist`, so the shell
  points this at the staged SPA under `resources/`.

## Run it

```bash
cd desktop
npm install
npm run build:resources    # assemble the bundled sidecar (Phase 1), once
npm start                  # tsc build + electron .
```

With `resources/python` present the shell launches the **bundled** interpreter
(no `uv`/dev env needed). Without it (skip `build:resources`), the shell falls
back to `uv run musaic` from the repo root — the Phase 0 dev loop, which needs
a synced `uv sync` env and `uv` on PATH.

## Files

- `src/main.ts` — main process: single-instance lock, free-port pick,
  sidecar resolution (bundled python → dev fallback), health gate, window,
  teardown (SIGTERM → SIGKILL).
- `src/preload.ts` — intentionally empty (renderer needs no IPC bridge).
- `build/*.mjs` — Phase 1 bundling scripts (see above).
- `package.json`, `tsconfig.json` — build config.

## Not yet present (later phases)

`electron-builder.yml`, `entitlements.mac.plist`, `build/fetch-binaries.*`
(native fpcalc/libchromaprint, Phase 2), and the per-OS packaging/signing
pipeline (Phases 3–4).
