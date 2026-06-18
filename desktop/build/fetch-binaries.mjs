/**
 * Phase 2 (issue #8): bundle the native binaries musaic shells out to —
 * `fpcalc` (Chromaprint's fingerprint generator) and, best-effort,
 * `libchromaprint` (the slow-path similarity decoder) — under
 * desktop/resources/ so fingerprinting works with NO Homebrew/distro install.
 *
 * fpcalc — we fetch the **official static** build from acoustid/chromaprint
 * releases. It links ffmpeg + chromaprint in, so it's one self-contained file
 * depending only on system libraries. (Homebrew's fpcalc, by contrast, pulls
 * in the whole ffmpeg dylib chain — unsuitable for bundling.)
 *
 * libchromaprint — upstream ships NO standalone shared library, so there's no
 * reproducible URL to fetch. It powers only the duplicate-tracks *slow path*
 * (near-duplicate decode); the byte-equality fast path and all of fingerprint
 * generation work without it. We therefore copy it best-effort from the build
 * host (Homebrew / distro), verifying on macOS that it depends only on system
 * frameworks so it stays relocatable. Absent → we log and skip; the backend's
 * existing graceful degradation (exact-match-only similarity) covers it.
 */
import { execFileSync } from "node:child_process";
import {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  realpathSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import * as path from "node:path";
import {
  BIN_DIR,
  CHROMAPRINT_VERSION,
  LIB_DIR,
  RESOURCES_DIR,
  fpcalcBin,
  log,
  run,
} from "./_common.mjs";

// ---- platform → upstream asset ----

function fpcalcAsset() {
  const v = CHROMAPRINT_VERSION;
  const { platform, arch } = process;
  const triples = {
    "darwin:arm64": `macos-arm64`,
    "darwin:x64": `macos-x86_64`,
    "linux:x64": `linux-x86_64`,
    "linux:arm64": `linux-arm64`,
    "win32:x64": `windows-x86_64`,
  };
  const key = `${platform}:${arch}`;
  const triple = triples[key];
  if (!triple) {
    throw new Error(
      `No upstream fpcalc build for ${key}. Supported: ${Object.keys(triples).join(", ")}.`,
    );
  }
  const ext = platform === "win32" ? "zip" : "tar.gz";
  const name = `chromaprint-fpcalc-${v}-${triple}.${ext}`;
  return {
    name,
    url: `https://github.com/acoustid/chromaprint/releases/download/v${v}/${name}`,
    // Inside the archive: `chromaprint-fpcalc-<v>-<triple>/fpcalc[.exe]`.
    innerDir: `chromaprint-fpcalc-${v}-${triple}`,
    ext,
  };
}

// ---- fpcalc: download the official static binary ----

function fetchFpcalc() {
  const asset = fpcalcAsset();
  const staging = path.join(RESOURCES_DIR, ".binaries-staging");
  rmSync(staging, { recursive: true, force: true });
  mkdirSync(staging, { recursive: true });
  const archive = path.join(staging, asset.name);

  log(`downloading ${asset.name} …`);
  run("curl", ["-fSL", "-o", archive, asset.url]);

  log("extracting fpcalc …");
  // bsdtar (macOS/Linux GNU tar / Windows 10+) reads both .tar.gz and .zip.
  run("tar", ["-xf", archive, "-C", staging]);

  const exe = process.platform === "win32" ? "fpcalc.exe" : "fpcalc";
  const extracted = path.join(staging, asset.innerDir, exe);
  if (!existsSync(extracted)) {
    throw new Error(`fpcalc not found in archive at ${extracted}`);
  }

  mkdirSync(BIN_DIR, { recursive: true });
  const dest = fpcalcBin();
  copyFileSync(extracted, dest);
  if (process.platform !== "win32") chmodSync(dest, 0o755);
  rmSync(staging, { recursive: true, force: true });

  // Confirm the bundled binary actually runs (version string).
  const out = execFileSync(dest, ["-version"], { encoding: "utf8" }).trim();
  log(`bundled fpcalc → ${dest}  (${out.split("\n")[0]})`);
}

// ---- libchromaprint: best-effort copy from the build host ----

const HOST_LIBCHROMAPRINT = {
  darwin: [
    "/opt/homebrew/lib/libchromaprint.dylib",
    "/usr/local/lib/libchromaprint.dylib",
  ],
  linux: [
    "/usr/lib/x86_64-linux-gnu/libchromaprint.so.1",
    "/usr/lib/aarch64-linux-gnu/libchromaprint.so.1",
    "/usr/lib/libchromaprint.so.1",
    "/usr/lib64/libchromaprint.so.1",
  ],
  win32: [],
};

/** On macOS, confirm the dylib only needs system libraries (so copying it
 *  alone keeps it loadable — no FFT/ffmpeg dylib chain to drag along). */
function macDepsAreSystemOnly(dylib) {
  const out = execFileSync("otool", ["-L", dylib], { encoding: "utf8" });
  const deps = out
    .split("\n")
    .slice(1) // line 0 is the "<file>:" header
    .map((l) => l.trim().split(" ")[0])
    .filter(Boolean)
    // Drop self-references: otool lists the dylib's own LC_ID_DYLIB install
    // name (e.g. libchromaprint.1.dylib) as the first entry — that's the lib
    // identifying itself, not an external dependency.
    .filter((d) => !path.basename(d).startsWith("libchromaprint"));
  const ok = (d) =>
    d.startsWith("/usr/lib/") || d.startsWith("/System/Library/");
  const foreign = deps.filter((d) => !ok(d));
  return { ok: foreign.length === 0, foreign };
}

function fetchLibchromaprint() {
  const candidates = HOST_LIBCHROMAPRINT[process.platform] ?? [];
  const found = candidates.find((p) => existsSync(p));
  if (!found) {
    log(
      `libchromaprint not found on this host (looked in: ${
        candidates.join(", ") || "(none for this platform)"
      }).`,
    );
    log(
      "  → skipping. Near-duplicate similarity degrades to exact-match only " +
        "(byte-identical dupes still flagged); install chromaprint on the " +
        "build host to bundle it.",
    );
    return;
  }

  const real = realpathSync(found); // resolve the versioned target behind symlinks
  if (process.platform === "darwin") {
    const { ok, foreign } = macDepsAreSystemOnly(real);
    if (!ok) {
      log(
        `WARNING: ${real} has non-system deps (${foreign.join(", ")}); it would ` +
          "not be relocatable. Skipping libchromaprint bundle.",
      );
      return;
    }
  }

  mkdirSync(LIB_DIR, { recursive: true });
  const ext = process.platform === "darwin" ? "dylib" : "so";
  const dest = path.join(LIB_DIR, `libchromaprint.${ext}`);
  copyFileSync(real, dest);
  if (process.platform !== "win32") chmodSync(dest, 0o755);
  log(`bundled libchromaprint → ${dest}  (from ${found})`);
}

// ---- NOTICES (licensing for the bundled third-party binaries) ----

function writeNotices() {
  const notices = `Third-party native binaries bundled with Musaic (issue #8, Phase 2)
================================================================

fpcalc (Chromaprint ${CHROMAPRINT_VERSION})
  Source: https://github.com/acoustid/chromaprint/releases (official static build)
  License: GPLv2 (the fpcalc command-line tool)
  The unmodified upstream binary is redistributed verbatim.

libchromaprint (when present)
  Source: the build host's Chromaprint install (Homebrew / distro package)
  License: LGPL-2.1
  The unmodified shared library is redistributed verbatim.

Chromaprint is Copyright (C) Lukas Lalinsky and contributors.
See https://github.com/acoustid/chromaprint for full license texts.
`;
  mkdirSync(RESOURCES_DIR, { recursive: true });
  const dest = path.join(RESOURCES_DIR, "NOTICES.txt");
  writeFileSync(dest, notices);
  log(`wrote ${dest}`);
}

// ---- run ----

log(`fetching native binaries for ${process.platform}/${process.arch} …`);
fetchFpcalc();
fetchLibchromaprint();
writeNotices();
log("done — run `npm run validate:binaries` to verify fingerprinting with no dev env");
