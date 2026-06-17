/**
 * Phase 1 (issue #8): fetch a relocatable standalone CPython into
 * desktop/resources/python.
 *
 * Why standalone CPython (python-build-standalone) over PyInstaller: musaic's
 * dep stack (librosa → numba → llvmlite, scipy, numpy, soundfile/libsndfile,
 * pyacoustid) is exactly the class that breaks frozen-exe hidden-import hooks
 * and works trivially against a real interpreter + real site-packages tree.
 *
 * We let `uv` fetch the build (it manages python-build-standalone releases),
 * then copy the resolved versioned tree out of uv's managed store into our
 * own resources/python so the result is self-contained and relocatable —
 * nothing at runtime depends on uv or on uv's cache location.
 */
import { cpSync, existsSync, mkdirSync, readdirSync, realpathSync, rmSync, statSync } from "node:fs";
import * as path from "node:path";
import { PYTHON_DIR, PYTHON_VERSION, RESOURCES_DIR, log, pythonBin, run } from "./_common.mjs";

const STAGING = path.join(RESOURCES_DIR, ".python-staging");

function findInstalledTree(stagingDir) {
  // uv drops `cpython-<full>-<triple>` dirs plus a floating
  // `cpython-<minor>-<triple>` symlink. Resolve to the real versioned dir.
  const entries = readdirSync(stagingDir)
    .filter((n) => n.startsWith("cpython-"))
    .map((n) => path.join(stagingDir, n))
    .filter((p) => {
      try {
        return statSync(p).isDirectory();
      } catch {
        return false;
      }
    })
    .map((p) => realpathSync(p));
  const unique = [...new Set(entries)];
  if (unique.length === 0) {
    throw new Error(`uv installed no cpython tree under ${stagingDir}`);
  }
  // If multiple patch releases linger, take the lexically-greatest (newest).
  unique.sort();
  return unique[unique.length - 1];
}

function stripExternallyManaged(pythonRoot) {
  // python-build-standalone ships PEP-668 EXTERNALLY-MANAGED. This tree is a
  // private app runtime we fully own, so installing musaic into it is exactly
  // what it's for — drop the marker so `uv pip install` / pip don't refuse.
  const libDir = path.join(pythonRoot, "lib");
  if (!existsSync(libDir)) return;
  for (const name of readdirSync(libDir)) {
    const marker = path.join(libDir, name, "EXTERNALLY-MANAGED");
    if (existsSync(marker)) {
      rmSync(marker);
      log(`removed ${path.relative(pythonRoot, marker)}`);
    }
  }
}

log(`fetching standalone CPython ${PYTHON_VERSION} via uv …`);
rmSync(STAGING, { recursive: true, force: true });
mkdirSync(STAGING, { recursive: true });
run("uv", ["python", "install", "--install-dir", STAGING, PYTHON_VERSION]);

const installed = findInstalledTree(STAGING);
log(`installed tree: ${installed}`);

log(`copying → ${PYTHON_DIR}`);
rmSync(PYTHON_DIR, { recursive: true, force: true });
mkdirSync(path.dirname(PYTHON_DIR), { recursive: true });
// verbatimSymlinks keeps the tree's *relative* internal symlinks
// (bin/python3 -> python3.12) intact. Without it, cpSync rewrites them to
// absolute paths into the staging dir we're about to delete, leaving dangling
// links. python-build-standalone is built to relocate with these relative
// links preserved, so copy it verbatim.
cpSync(installed, PYTHON_DIR, { recursive: true, verbatimSymlinks: true });
rmSync(STAGING, { recursive: true, force: true });

stripExternallyManaged(PYTHON_DIR);

// Confirm the relocated interpreter runs from its new home.
run(pythonBin(), ["-c", "import sys; print('bundled python', sys.version.split()[0])"]);
log("done — bundled python is relocatable and ready for build:sidecar");
