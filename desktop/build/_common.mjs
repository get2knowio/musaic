/**
 * Shared helpers for the Phase 1 sidecar-bundling scripts (issue #8).
 *
 * The three scripts (fetch-python, build-sidecar, validate-sidecar) all agree
 * on where the bundled runtime lives and how to address its interpreter across
 * platforms. Keep that knowledge here so the paths can't drift apart.
 */
import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

/** desktop/ — parent of build/. */
export const DESKTOP_DIR = path.resolve(__dirname, "..");
/** repo root — parent of desktop/. */
export const REPO_ROOT = path.resolve(DESKTOP_DIR, "..");
/** Assembled-at-build resource tree (gitignored). Mirrors what electron-builder
 *  will drop into the packaged app's `resources/` in Phase 3. */
export const RESOURCES_DIR = path.join(DESKTOP_DIR, "resources");
/** The relocatable standalone CPython tree. */
export const PYTHON_DIR = path.join(RESOURCES_DIR, "python");
/** Where the built SPA is staged so the packaged sidecar can find it via
 *  MUSAIC_FRONTEND_DIR (its own package isn't a sibling of frontend/dist). */
export const FRONTEND_DEST = path.join(RESOURCES_DIR, "frontend", "dist");
/** Bundled native binaries (Phase 2): the static fpcalc lives in bin/, the
 *  optional libchromaprint dylib/so in lib/. The shell points the backend at
 *  these via MUSAIC_FPCALC / FPCALC / MUSAIC_LIBCHROMAPRINT. */
export const BIN_DIR = path.join(RESOURCES_DIR, "bin");
export const LIB_DIR = path.join(RESOURCES_DIR, "lib");

/**
 * Upstream Chromaprint release whose *static* fpcalc we bundle (ffmpeg +
 * chromaprint linked in — a single self-contained file, unlike Homebrew's
 * fpcalc which drags in the ffmpeg dylib chain). Bump deliberately.
 */
export const CHROMAPRINT_VERSION = "1.6.0";

/** Path to the bundled fpcalc, per-OS layout. */
export function fpcalcBin(binRoot = BIN_DIR) {
  return path.join(binRoot, process.platform === "win32" ? "fpcalc.exe" : "fpcalc");
}

/**
 * Python series to bundle. airframe (and therefore musaic) floors at 3.12;
 * we pin the minor and let uv resolve the latest patch from
 * python-build-standalone. Bump deliberately — a patch bump re-downloads the
 * interpreter and re-installs the whole dep stack.
 */
export const PYTHON_VERSION = "3.12";

/** Path to the bundled interpreter, addressed the way each OS lays it out. */
export function pythonBin(pythonRoot = PYTHON_DIR) {
  return process.platform === "win32"
    ? path.join(pythonRoot, "python.exe")
    : path.join(pythonRoot, "bin", "python3");
}

export function log(msg) {
  console.log(`\x1b[36m[sidecar-build]\x1b[0m ${msg}`);
}

/** Run a command, inheriting stdio, throwing on non-zero exit. */
export function run(cmd, args, opts = {}) {
  log(`$ ${cmd} ${args.join(" ")}`);
  // On Windows, npm/npx/yarn/pnpm are `.cmd` shims that Node refuses to exec
  // directly (the .cmd spawn restriction), so route those — and only those —
  // through the shell. Binaries addressed by name or absolute path (uv, curl,
  // tar, the bundled python) run without a shell so their args (e.g. a
  // multi-line `python -c` script) aren't re-parsed by cmd.exe.
  const needsShell =
    process.platform === "win32" && /^(npm|npx|yarn|pnpm)$/.test(cmd);
  execFileSync(cmd, args, { stdio: "inherit", shell: needsShell, ...opts });
}

/** Fail fast with a clear message if the bundled python isn't there yet. */
export function assertPythonBundled() {
  const bin = pythonBin();
  if (!existsSync(bin)) {
    throw new Error(
      `No bundled python at ${bin}.\n` +
        `Run \`npm run build:python\` first (it downloads python-build-standalone).`,
    );
  }
  return bin;
}
