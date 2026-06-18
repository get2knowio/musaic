# Musaic desktop (Electron) — Phases 0–6

Tracks issue #8 ("Package musaic as a cross-platform Electron desktop app").
This directory is the Electron shell that wraps the existing FastAPI + React
app as a double-clickable desktop application.

**Status: all phases (0–6) implemented.** The `build/` scripts assemble a
relocatable standalone CPython with musaic + its full dep stack, the built SPA,
**and the native `fpcalc` / `libchromaprint` binaries** under `resources/`;
**electron-builder packages that into per-OS installers** (macOS dmg/zip,
Windows nsis, Linux AppImage) via a GitHub Actions matrix, **signs +
notarizes** them when certs are configured, and ships an **electron-updater**
feed. The shell prefers the bundled interpreter (`python -m musaic.main`),
points fingerprinting at the bundled binaries, augments PATH so user CLIs are
visible to a GUI-launched app, and falls back to the dev loop (`uv run
musaic`) when no bundle is present.

> **Activation note:** signing/notarization (Phase 4) and published auto-update
> releases (Phase 6) are *gated on credentials* — with no certs/token in the
> environment, builds are **unsigned** and nothing is published, but everything
> works locally. Supplying the secrets (below) flips both on with no code change.

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
npm run build:resources    # fetch-python → build-sidecar → fetch-binaries (≈760MB resources/)
npm run validate:sidecar   # boots `python -m musaic.main` headless, no dev env:
                           #   gate 1 imports the heavy stack + runs numba JIT
                           #     + a librosa op (JIT is the real relocation risk)
                           #   gate 2 health-gates /api/health and serves the SPA
npm run validate:binaries  # fingerprints a synthesized FLAC via the bundled
                           #   fpcalc + libchromaprint, with Homebrew hidden
                           #   from PATH (proves "no Homebrew chromaprint")
```

Scripts (all `node build/*.mjs`, cross-platform, paths shared via `_common.mjs`):
- `build:python` → `fetch-python.mjs` — stage relocatable CPython into `resources/python`.
- `build:sidecar` → `build-sidecar.mjs` — `uv pip install .` into it + build/stage the SPA + trim caches.
- `build:binaries` → `fetch-binaries.mjs` — fetch the official static `fpcalc` into `resources/bin/` + best-effort-copy `libchromaprint` into `resources/lib/` + write `NOTICES.txt` (Phase 2).
- `validate:sidecar` → `validate-sidecar.mjs` — the two sidecar acceptance gates above.
- `validate:binaries` → `validate-binaries.mjs` — the Phase 2 native-binary gate.

## Phase 2 — native binaries (`fpcalc` / `libchromaprint`)

Fingerprinting shells out to `fpcalc` (Chromaprint) and, for near-duplicate
similarity, loads `libchromaprint`. The bundle ships both so it works with **no
Homebrew/distro chromaprint**:

- **`fpcalc`** — the official **static** build from acoustid/chromaprint
  releases (ffmpeg + chromaprint linked in, one self-contained file). Homebrew's
  `fpcalc` instead drags in the whole ffmpeg dylib chain, so it's unsuitable.
- **`libchromaprint`** — upstream ships no standalone shared library, so it's
  copied best-effort from the build host (Homebrew / distro); on macOS the copy
  is verified to depend only on system frameworks so it stays relocatable. It
  powers only the duplicate-tracks *slow path* — if it's absent, near-duplicate
  similarity degrades to exact-match-only (byte-identical dupes still flagged),
  the backend's existing graceful degradation. For CI, `brew install chromaprint`
  (macOS) / `apt-get install libchromaprint1` (Linux) on the build host bundles it.

Backend resolution (`musaic/fingerprint.py`): `MUSAIC_FPCALC` (then `FPCALC`,
then `PATH`) selects the binary; `MUSAIC_LIBCHROMAPRINT` is prepended ahead of
the hardcoded Homebrew/distro dylib candidates. The shell injects all three
(only when the files exist) in `resolveSidecarCommand()`.

**Size lever (open, issue #8):** librosa+numba+llvmlite+scipy ≈ 250MB power only
the optional BPM/analyze endpoints. Dropping them (BPM as opt-in) is the biggest
bundle-size win but is a product call — Phase 1 deliberately bundles the full
stack so we've proven it works before deciding to cut it.

## Phase 3 — packaging (electron-builder)

`electron-builder.yml` packages the compiled main process (into `app.asar`)
plus the assembled `resources/` tree (as `extraResources`, unpacked alongside
the asar so the sidecar can exec/dlopen real files) into per-OS installers.

```bash
cd desktop
npm install
npm run build:resources    # assemble resources/ (Phases 1–2), once
npm run pack               # → release/<os>/  unpacked app dir (fast smoke)
npm run dist               # → release/  installers (dmg+zip / nsis / AppImage)
```

Key config decisions:
- **`directories.output: release`** — not `dist/`, which is tsc's outDir for the
  main process.
- **`files: [dist, package.json]`** — only the main process enters the asar; the
  ~760MB runtime ships as `extraResources`, so it isn't double-bundled. The app
  has no production npm deps (main.ts uses only node builtins + electron).
- **`extraResources: resources → .`** lands the tree at `process.resourcesPath`,
  which `main.ts`'s `resourcesRoot()` reads when `app.isPackaged`. The python
  tree's **relative symlinks survive** the copy (verified), so it still relocates.
- **`mac.identity: null`** explicitly skips code signing so unsigned local + CI
  builds succeed; Phase 4 adds Developer ID + notarization (and will need to sign
  the bundled `fpcalc` + `libchromaprint` and the python tree's nested dylibs).

**CI** (`.github/workflows/desktop.yml`): a matrix of macos-14 (arm64),
windows-latest, ubuntu-22.04 — each builds on its own OS (no cross-compile,
since the standalone CPython + native fpcalc are per-OS). It assembles
resources, runs both validation gates, builds the installer, and uploads it as
an artifact. Triggered on `workflow_dispatch` + `v*` tags (the job is heavy).
Not published — releases/auto-update are Phase 6.

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
  sidecar resolution (bundled python → dev fallback), native-binary env
  injection, health gate, window, teardown (SIGTERM → SIGKILL).
- `src/preload.ts` — intentionally empty (renderer needs no IPC bridge).
- `build/*.mjs` — Phase 1–2 bundling scripts (see above).
- `build/entitlements.mac.plist` — Phase 4 hardened-runtime entitlements.
- `electron-builder.config.js` — Phase 3/4/6 packaging config (signing + publish gated on env).
- `../.github/workflows/desktop.yml` — CI matrix (build, sign, publish).
- `package.json`, `tsconfig.json` — build config.

## Phase 4 — signing & notarization (gated on certs)

`electron-builder.config.js` is a JS config (not static yml) precisely so
signing can be conditional: `mac.identity` is `null` (skip) until a cert env
var appears, then it signs, turns on the hardened runtime with
`build/entitlements.mac.plist`, and notarizes when Apple creds are present.
electron-builder's deep-sign pass covers the bundled `fpcalc`,
`libchromaprint`, and the python tree's nested dylibs under the same identity.

The entitlements grant what the bundled stack needs under the hardened runtime:
`allow-jit` + `allow-unsigned-executable-memory` (numba/llvmlite JIT),
`disable-library-validation` (load dylibs not signed by our Team ID), and
`allow-dyld-environment-variables`.

To activate, set these in the CI environment (GitHub repo secrets, consumed by
`.github/workflows/desktop.yml`):

| Secret | Purpose |
|---|---|
| `MAC_CSC_LINK` / `MAC_CSC_KEY_PASSWORD` | Developer ID cert (.p12, base64) + password |
| `APPLE_ID` / `APPLE_APP_SPECIFIC_PASSWORD` / `APPLE_TEAM_ID` | notarization |
| `WIN_CSC_LINK` / `WIN_CSC_KEY_PASSWORD` | Windows Authenticode cert |

## Phase 5 — AI availability gating

AI runs through airframe; the Claude adapter shells out to the `claude` CLI
(github-copilot to `gh`). In a packaged app that CLI may be absent — and a
GUI-launched macOS app doesn't even inherit the shell PATH. Two mitigations:

- **PATH augmentation** (`main.ts`): the shell prepends the common
  user/Homebrew bin dirs that exist, so `claude` / `gh` / `uv` resolve for a
  double-clicked app.
- **Readiness probe** (`musaic/ai.py` `provider_runtime_status`): the
  `/api/ai/providers` endpoint reports whether the selected provider's CLI is
  present; Settings shows an actionable banner when it isn't, and the worker
  fails with that remedy instead of an opaque crash. Either way the
  **deterministic MusicBrainz resolver keeps working** — AI being unavailable
  degrades the app rather than breaking it. Bundling the `claude` CLI is a
  deliberate non-goal for v1; steer users to an API-key provider instead.

## Phase 6 — auto-update (gated on a published feed)

`electron-updater` (a bundled production dep) checks for updates on launch —
guarded to packaged builds, and to a quiet no-op when there's no feed / no
release / offline / unsigned (macOS won't apply an unsigned update). The
`publish: github` config embeds `app-update.yml` into the app and emits the
`latest-*.yml` update manifests. CI publishes to GitHub releases **only on a
`v*` tag** (and only with a token + signing). Note: a full-bundle update is a
large download (~370 MB) — delta/partial updates are a future refinement.

## Still TODO

A custom app icon (builds use the default Electron icon); the librosa-drop
size lever; in-app "an update is ready" UX (currently `checkForUpdatesAndNotify`
uses the OS notification + installs on quit).
