/**
 * Phase 1 (issue #8): install musaic + its deps into the bundled CPython and
 * stage the built SPA, producing a self-contained sidecar under
 * desktop/resources/ that boots with no dev environment.
 *
 * Steps:
 *   1. uv pip install the project (musaic + all runtime deps) into the
 *      bundled interpreter's own site-packages — one relocatable tree, no
 *      venv whose pyvenv.cfg would pin an absolute base-python path.
 *   2. Build the frontend and copy frontend/dist into resources/ so the
 *      packaged sidecar finds it via MUSAIC_FRONTEND_DIR.
 *   3. Trim build cruft (__pycache__, *.pyc) to shave the bundle.
 *
 * The heavy lever on size is librosa+numba+llvmlite+scipy (~250MB, BPM only);
 * dropping it is a product call tracked in issue #8, deliberately NOT done
 * here so Phase 1 validates the full stack works bundled.
 */
import { cpSync, mkdirSync, readdirSync, rmSync, statSync } from "node:fs";
import * as path from "node:path";
import {
  FRONTEND_DEST,
  PYTHON_DIR,
  REPO_ROOT,
  RESOURCES_DIR,
  assertPythonBundled,
  log,
  run,
} from "./_common.mjs";

const pyBin = assertPythonBundled();

// 1. Install musaic + deps into the bundled interpreter.
log("installing musaic + deps into the bundled interpreter (uv pip install) …");
run("uv", ["pip", "install", "--python", pyBin, "--no-cache", REPO_ROOT]);

// 2. Build + stage the frontend.
log("building frontend (npm run build) …");
run("npm", ["run", "build"], { cwd: path.join(REPO_ROOT, "frontend") });
const frontendSrc = path.join(REPO_ROOT, "frontend", "dist");
log(`staging frontend → ${FRONTEND_DEST}`);
rmSync(FRONTEND_DEST, { recursive: true, force: true });
mkdirSync(path.dirname(FRONTEND_DEST), { recursive: true });
cpSync(frontendSrc, FRONTEND_DEST, { recursive: true });

// 3. Trim caches.
log("trimming __pycache__ / *.pyc …");
let trimmed = 0;
function trim(dir) {
  for (const name of readdirSync(dir)) {
    const p = path.join(dir, name);
    let st;
    try {
      st = statSync(p);
    } catch {
      continue;
    }
    if (st.isDirectory()) {
      if (name === "__pycache__") {
        rmSync(p, { recursive: true, force: true });
        trimmed++;
      } else {
        trim(p);
      }
    } else if (name.endsWith(".pyc")) {
      rmSync(p, { force: true });
      trimmed++;
    }
  }
}
trim(PYTHON_DIR);
log(`trimmed ${trimmed} cache entries`);

// Report the assembled size so the bundle budget stays visible.
function du(dir) {
  let total = 0;
  for (const name of readdirSync(dir)) {
    const p = path.join(dir, name);
    let st;
    try {
      st = statSync(p);
    } catch {
      continue;
    }
    total += st.isDirectory() ? du(p) : st.size;
  }
  return total;
}
const mb = (n) => (n / 1024 / 1024).toFixed(0);
log(`resources assembled: python=${mb(du(PYTHON_DIR))}MB frontend=${mb(du(FRONTEND_DEST))}MB`);
log(`total resources/=${mb(du(RESOURCES_DIR))}MB`);
log("done — run `npm run validate:sidecar` to boot it headless");
