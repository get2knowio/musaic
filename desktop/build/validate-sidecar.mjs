/**
 * Phase 1 (issue #8) acceptance check: prove the bundled sidecar boots and
 * runs the heavy stack with NO dev environment on PATH.
 *
 * Two gates:
 *   1. Heavy-import + JIT smoke — import the fragile stack in the relocated
 *      interpreter and actually execute numba/llvmlite JIT + a librosa op.
 *      Imports succeeding isn't enough: numba compiles native code at call
 *      time, which is the real thing that breaks under relocation.
 *   2. Boot the sidecar exactly as Electron will (`python -m musaic.main`
 *      with MUSAIC_PORT/HOST/FRONTEND_DIR), health-gate /api/health, and
 *      confirm it serves the staged SPA at /.
 *
 * Uses a throwaway MUSAIC_CONFIG_DIR so the real ~/.musaic is untouched.
 */
import { spawn } from "node:child_process";
import { get as httpGet } from "node:http";
import { createServer } from "node:net";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import * as path from "node:path";
import { FRONTEND_DEST, assertPythonBundled, log, run } from "./_common.mjs";

const HOST = "127.0.0.1";
const pyBin = assertPythonBundled();

if (!existsSync(FRONTEND_DEST)) {
  throw new Error(`No staged frontend at ${FRONTEND_DEST}. Run \`npm run build:sidecar\` first.`);
}

// ---- Gate 1: heavy-import + JIT smoke ----
log("gate 1: heavy-stack import + numba JIT + librosa op …");
run(pyBin, [
  "-c",
  [
    "import numpy as np, scipy, soundfile, librosa, musaic",
    "from numba import njit",
    "@njit",
    "def _double(x):\n    return x * 2",
    "assert _double(21) == 42  # forces llvmlite JIT in the relocated interpreter",
    "y = np.sin(2*np.pi*220*np.arange(22050)/22050).astype('float32')",
    "tempo, _ = librosa.beat.beat_track(y=y, sr=22050)",
    "print('  heavy stack ok: librosa', librosa.__version__, 'scipy', scipy.__version__, 'tempo', round(float(np.atleast_1d(tempo)[0]), 1))",
  ].join("\n"),
]);

// ---- Gate 2: boot the sidecar headless ----
function pickFreePort() {
  return new Promise((resolve, reject) => {
    const srv = createServer();
    srv.once("error", reject);
    srv.listen(0, HOST, () => {
      const { port } = srv.address();
      srv.close(() => resolve(port));
    });
  });
}

function httpStatus(url) {
  return new Promise((resolve) => {
    const req = httpGet(url, (res) => {
      const chunks = [];
      res.on("data", (c) => chunks.push(c));
      res.on("end", () => resolve({ status: res.statusCode, body: Buffer.concat(chunks).toString("utf8") }));
    });
    req.on("error", () => resolve({ status: 0, body: "" }));
    req.setTimeout(1000, () => req.destroy());
  });
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const port = await pickFreePort();
const configDir = mkdtempSync(path.join(tmpdir(), "musaic-validate-"));
log(`gate 2: booting bundled sidecar on ${HOST}:${port} (config=${configDir}) …`);

const child = spawn(pyBin, ["-m", "musaic.main"], {
  env: {
    ...process.env,
    MUSAIC_HOST: HOST,
    MUSAIC_PORT: String(port),
    MUSAIC_FRONTEND_DIR: FRONTEND_DEST,
    MUSAIC_CONFIG_DIR: configDir,
  },
  stdio: ["ignore", "inherit", "inherit"],
});

let failed = null;
try {
  const deadline = Date.now() + 30_000;
  let healthy = false;
  while (Date.now() < deadline) {
    const { status } = await httpStatus(`http://${HOST}:${port}/api/health`);
    if (status === 200) {
      healthy = true;
      break;
    }
    await sleep(300);
  }
  if (!healthy) throw new Error("/api/health never returned 200 within 30s");
  log("  /api/health → 200");

  const index = await httpStatus(`http://${HOST}:${port}/`);
  if (index.status !== 200 || !/<div id="root">|<!doctype html>/i.test(index.body)) {
    throw new Error(`/ did not serve the SPA (status=${index.status})`);
  }
  log("  / serves the staged SPA");
} catch (err) {
  failed = err;
} finally {
  child.kill("SIGTERM");
  await sleep(500);
  if (!child.killed) child.kill("SIGKILL");
  rmSync(configDir, { recursive: true, force: true });
}

if (failed) {
  console.error(`\x1b[31m[sidecar-build] validation FAILED:\x1b[0m ${failed.message}`);
  process.exit(1);
}
log("PASS — bundled sidecar boots and serves with no dev environment");
