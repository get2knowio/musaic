/**
 * Phase 2 (issue #8) acceptance check: prove fingerprinting works against the
 * BUNDLED native binaries with NO Homebrew/distro chromaprint reachable.
 *
 * We run the bundled interpreter with:
 *   - PATH stripped of the Homebrew/local bin dirs (so a stray `fpcalc` on the
 *     dev box can't mask a bundling bug),
 *   - MUSAIC_FPCALC / FPCALC / MUSAIC_LIBCHROMAPRINT pointed at the bundle,
 * then:
 *   1. assert fingerprint._resolve_fpcalc() picks the bundled binary,
 *   2. actually generate a fingerprint for a synthesized FLAC (this *executes*
 *      the bundled fpcalc — no network, no AcoustID lookup), and
 *   3. load the bundled libchromaprint and decode that fingerprint via the
 *      slow path, proving the dylib is found + callable (when bundled).
 *
 * libchromaprint is optional: if it wasn't bundled on this host, step 3 is
 * skipped and the gate still passes — the backend degrades to exact-match-only
 * similarity, by design.
 */
import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import * as path from "node:path";
import { LIB_DIR, assertPythonBundled, fpcalcBin, log } from "./_common.mjs";

const pyBin = assertPythonBundled();
const fpcalc = fpcalcBin();
if (!existsSync(fpcalc)) {
  throw new Error(`No bundled fpcalc at ${fpcalc}. Run \`npm run build:binaries\` first.`);
}
const libDylib = path.join(LIB_DIR, "libchromaprint.dylib");
const libSo = path.join(LIB_DIR, "libchromaprint.so");
const libchromaprint = existsSync(libDylib)
  ? libDylib
  : existsSync(libSo)
    ? libSo
    : "";

// Hide any Homebrew/local fpcalc so resolution can only succeed via the bundle.
const strippedPath = (process.env.PATH ?? "")
  .split(path.delimiter)
  .filter((d) => !/\/(opt\/homebrew|usr\/local)\/bin\/?$/.test(d))
  .join(path.delimiter);

const configDir = mkdtempSync(path.join(tmpdir(), "musaic-validate-bin-"));
const env = {
  ...process.env,
  PATH: strippedPath,
  MUSAIC_FPCALC: fpcalc,
  FPCALC: fpcalc,
  ...(libchromaprint ? { MUSAIC_LIBCHROMAPRINT: libchromaprint } : {}),
  // Throwaway config dir so the real ~/.musaic is untouched.
  MUSAIC_CONFIG_DIR: configDir,
};

log(`gate: fingerprint via bundled fpcalc${libchromaprint ? " + libchromaprint" : " (no libchromaprint — exact-match-only)"} …`);

const py = [
  "import os, sys, tempfile, wave, math, struct",
  "import numpy as np, soundfile as sf",
  "from musaic import fingerprint as fp",
  "",
  "# 1. resolution picks the bundled binary, not a PATH install",
  "resolved = fp._resolve_fpcalc()",
  "assert resolved == os.environ['MUSAIC_FPCALC'], f'resolved {resolved!r} != bundled'",
  "print('  fpcalc resolved →', resolved)",
  "",
  "# 2. generate a short FLAC and actually fingerprint it (runs fpcalc)",
  "import acoustid",
  "d = tempfile.mkdtemp()",
  "wav = os.path.join(d, 'tone.flac')",
  "sr = 22050",
  "t = np.arange(int(sr * 5)) / sr",
  "y = (0.4 * np.sin(2*np.pi*440*t)).astype('float32')",
  "sf.write(wav, y, sr, format='FLAC')",
  "duration, fpstr = acoustid.fingerprint_file(wav, maxlength=120)",
  "if isinstance(fpstr, bytes): fpstr = fpstr.decode('ascii')",
  "assert fpstr, 'empty fingerprint'",
  "print('  fingerprinted FLAC: duration', round(float(duration),1), 'fp_len', len(fpstr))",
  "",
  "# 3. self-similarity (fast path) is exactly 1.0",
  "assert fp.chromaprint_similarity(fpstr, fpstr) == 1.0, 'self-similarity != 1.0'",
  "print('  fast-path self-similarity = 1.0')",
  "",
  "# 4. libchromaprint slow path (only when bundled)",
  "if os.environ.get('MUSAIC_LIBCHROMAPRINT'):",
  "    lib = fp._load_libchromaprint()",
  "    assert lib is not None, 'bundled libchromaprint failed to load'",
  "    ints = fp._decode_fingerprint(fpstr)",
  "    assert len(ints) > 0, 'decoder returned no subfingerprints'",
  "    print('  slow-path decode via bundled libchromaprint:', len(ints), 'subfingerprints')",
  "else:",
  "    print('  libchromaprint not bundled — slow path skipped (exact-match-only)')",
  "",
  "print('  OK')",
].join("\n");

try {
  // Run the bundled interpreter directly (not via run()) so the long -c
  // script isn't echoed — only python's own stdout/stderr reaches the user.
  execFileSync(pyBin, ["-c", py], { env, stdio: "inherit" });
} finally {
  rmSync(configDir, { recursive: true, force: true });
}

log("PASS — fingerprinting works against the bundled binaries, no Homebrew chromaprint");
