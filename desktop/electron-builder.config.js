// electron-builder config (issue #8, Phases 3/4/6). A JS config (not the
// earlier static yml) so signing + notarization can be *gated on credentials*:
// with no certs in the environment the build is unsigned (works locally + in CI
// without secrets); with certs present it signs, hardens, and notarizes.
//
// Run order: `npm run build:resources` (assemble resources/) → `npm run dist`.

// --- signing/notarization gating (Phase 4) ---
// electron-builder's standard cert env vars. Presence of a cert flips signing
// on; absence leaves the build unsigned instead of failing.
const macSigning = !!(process.env.CSC_LINK || process.env.CSC_NAME);
const winSigning = !!(process.env.CSC_LINK || process.env.WIN_CSC_LINK);
// Notarization needs a Team ID plus either an Apple ID app-specific password
// or an App Store Connect API key — all read from env by electron-builder.
const notarizeCreds = !!(
  process.env.APPLE_TEAM_ID &&
  (process.env.APPLE_ID || process.env.APPLE_API_KEY)
);

/** @type {import('electron-builder').Configuration} */
module.exports = {
  appId: "io.get2know.musaic",
  productName: "Musaic",
  copyright: "Copyright © Musaic contributors",

  directories: {
    // NOT `dist/` — that's tsc's outDir for the compiled main process.
    output: "release",
    buildResources: "build",
  },

  // Only the compiled main-process code goes into the asar. The heavy runtime
  // (python, frontend, fpcalc, libchromaprint) ships as extraResources below,
  // unpacked alongside app.asar so the sidecar can exec/dlopen real files. The
  // app has no production node deps — main.ts uses only node builtins + electron.
  files: ["dist/**/*", "package.json"],

  // Assembled-at-build sidecar tree → Contents/Resources/ (== process.resourcesPath).
  // main.ts's resourcesRoot() reads it from there when app.isPackaged.
  extraResources: [{ from: "resources", to: ".", filter: ["**/*"] }],

  // Don't rebuild native node modules — there are none.
  npmRebuild: false,

  mac: {
    category: "public.app-category.music",
    target: ["dmg", "zip"],
    // Sign only when a cert is present; otherwise identity:null explicitly
    // skips signing so unsigned local + CI builds succeed instead of failing
    // on a missing Developer ID.
    identity: macSigning ? undefined : null,
    // The hardened runtime + entitlements are only meaningful (and only
    // accepted by notarization) on a signed build.
    hardenedRuntime: macSigning,
    gatekeeperAssess: false,
    entitlements: macSigning ? "build/entitlements.mac.plist" : undefined,
    entitlementsInherit: macSigning ? "build/entitlements.mac.plist" : undefined,
    // Notarize when signing AND notarization creds are present. The bundled
    // python's nested dylibs + fpcalc + libchromaprint are signed by
    // electron-builder's deep-sign pass under the same identity.
    notarize:
      macSigning && notarizeCreds ? { teamId: process.env.APPLE_TEAM_ID } : false,
  },

  win: {
    target: ["nsis"],
    // Authenticode signing is automatic when WIN_CSC_LINK/CSC_LINK is set.
    signtoolOptions: winSigning ? {} : undefined,
  },

  nsis: {
    oneClick: false,
    allowToChangeInstallationDirectory: true,
    perMachine: false,
  },

  linux: {
    category: "AudioVideo",
    target: ["AppImage"],
  },

  // Auto-update feed (Phase 6). Embeds app-update.yml into the build so
  // electron-updater knows where to look; nothing is uploaded unless the build
  // runs with `--publish always` (CI does that only on tags, with secrets).
  publish: [{ provider: "github", owner: "get2knowio", repo: "musaic" }],
};
