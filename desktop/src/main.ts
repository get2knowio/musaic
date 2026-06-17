/**
 * Musaic desktop shell — Electron main process (issue #8, Phase 0 spike).
 *
 * Model: keep the existing FastAPI app as a localhost HTTP "sidecar" and
 * point a BrowserWindow at its origin. Because the frontend uses only
 * relative `/api` paths + EventSource SSE, and FastAPI self-serves the
 * built SPA, same-origin `loadURL("http://127.0.0.1:<port>")` reuses the
 * whole stack verbatim — zero frontend changes. See the issue for why
 * HTTP-sidecar beats IPC and `loadURL` beats `file://`.
 *
 * Phase 0 spawns the dev backend (`uv run musaic`). Phase 1 swaps the
 * command for a bundled standalone-CPython interpreter via the same
 * MUSAIC_PORT / MUSAIC_FRONTEND_DIR contract this file already uses.
 */
import { app, BrowserWindow } from "electron";
import { spawn, ChildProcess } from "node:child_process";
import { createServer } from "node:net";
import { get as httpGet } from "node:http";
import * as path from "node:path";

const HOST = "127.0.0.1";
const HEALTH_TIMEOUT_MS = 30_000;
const HEALTH_POLL_MS = 300;

let sidecar: ChildProcess | null = null;
let mainWindow: BrowserWindow | null = null;
let isQuitting = false;

/** Ask the OS for a free TCP port by binding to :0 and reading it back. */
function pickFreePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const srv = createServer();
    srv.once("error", reject);
    srv.listen(0, HOST, () => {
      const addr = srv.address();
      if (addr && typeof addr === "object") {
        const { port } = addr;
        srv.close(() => resolve(port));
      } else {
        srv.close(() => reject(new Error("could not determine free port")));
      }
    });
  });
}

/**
 * Spawn the backend. Phase 0: dev mode via `uv run musaic`, cwd = repo root
 * (parent of this `desktop/` dir). MUSAIC_PORT/MUSAIC_HOST tell run() where
 * to bind without a CLI flag; process.env is passed through so auth tokens
 * in the launching env still reach the AI worker.
 */
function spawnSidecar(port: number): ChildProcess {
  const repoRoot = path.resolve(app.getAppPath(), "..");
  const child = spawn("uv", ["run", "musaic"], {
    cwd: repoRoot,
    env: {
      ...process.env,
      MUSAIC_HOST: HOST,
      MUSAIC_PORT: String(port),
    },
    stdio: ["ignore", "inherit", "inherit"],
  });
  child.on("exit", (code, signal) => {
    sidecar = null;
    // If the backend dies unexpectedly while the app is up, there's no
    // useful window to show — exit so the user isn't staring at a dead UI.
    if (!isQuitting) {
      console.error(`[musaic] sidecar exited (code=${code}, signal=${signal})`);
      app.quit();
    }
  });
  return child;
}

/** Poll /api/health until it returns 200 or we hit the ceiling. */
function waitForHealth(port: number): Promise<void> {
  const deadline = Date.now() + HEALTH_TIMEOUT_MS;
  const url = `http://${HOST}:${port}/api/health`;
  return new Promise((resolve, reject) => {
    const attempt = () => {
      const req = httpGet(url, (res) => {
        res.resume(); // drain
        if (res.statusCode === 200) {
          resolve();
        } else {
          retry();
        }
      });
      req.on("error", retry);
      req.setTimeout(HEALTH_POLL_MS, () => req.destroy());
    };
    const retry = () => {
      if (Date.now() > deadline) {
        reject(new Error(`backend not ready after ${HEALTH_TIMEOUT_MS}ms`));
      } else {
        setTimeout(attempt, HEALTH_POLL_MS);
      }
    };
    attempt();
  });
}

function createWindow(): BrowserWindow {
  const win = new BrowserWindow({
    width: 1280,
    height: 860,
    show: false,
    webPreferences: {
      preload: path.join(__dirname, "preload.js"),
      contextIsolation: true,
      nodeIntegration: false,
    },
  });
  win.once("ready-to-show", () => win.show());
  return win;
}

async function boot(): Promise<void> {
  const port = await pickFreePort();
  sidecar = spawnSidecar(port);

  mainWindow = createWindow();

  try {
    await waitForHealth(port);
    await mainWindow.loadURL(`http://${HOST}:${port}`);
  } catch (err) {
    console.error("[musaic] failed to start backend:", err);
    // Minimal failure surface for the spike; a real splash/error window
    // comes later in Phase 3.
    await mainWindow.loadURL(
      "data:text/html," +
        encodeURIComponent(
          `<h1>Musaic failed to start</h1><pre>${String(err)}</pre>`,
        ),
    );
  }
}

/** SIGTERM the child (its FastAPI lifespan cancels the AI worker +
 *  fingerprint pool), then SIGKILL as a fallback if it lingers. */
function stopSidecar(): void {
  if (!sidecar || sidecar.killed) return;
  const child = sidecar;
  child.kill("SIGTERM");
  setTimeout(() => {
    if (!child.killed) child.kill("SIGKILL");
  }, 5_000);
}

// Single-instance lock: two sidecars would contend on the SQLite WAL DB.
const gotLock = app.requestSingleInstanceLock();
if (!gotLock) {
  app.quit();
} else {
  app.on("second-instance", () => {
    if (mainWindow) {
      if (mainWindow.isMinimized()) mainWindow.restore();
      mainWindow.focus();
    }
  });

  app.whenReady().then(boot);

  app.on("window-all-closed", () => {
    // macOS convention is to stay alive, but a single-window utility app
    // with a heavy sidecar should fully exit.
    app.quit();
  });

  app.on("before-quit", () => {
    isQuitting = true;
    stopSidecar();
  });
}
