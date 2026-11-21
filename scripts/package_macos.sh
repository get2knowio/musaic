#!/usr/bin/env bash
# Build an UNSIGNED macOS Musaic.app + .dmg for this Mac's architecture
# (docs/desktop.md "Packaging (macOS)"). No signing identity, no
# notarisation, no updater artefacts.
#
#   scripts/package_macos.sh          # npm ci if needed, SPA, NOTICES, bundle
#
# Output (gitignored, under rust/target/release/bundle/):
#   macos/Musaic.app
#   dmg/Musaic_<version>_<arch>.dmg   (<arch>: aarch64 or x64)
#
# Needs: cargo-tauri and cargo-about in ~/.cargo/bin or on PATH
#   cargo install tauri-cli --version "^2" --locked
#   cargo install cargo-about --locked --features cli
# Network: crates.io / npm registry fetches only (cargo fetch, npm ci).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export PATH="$HOME/.cargo/bin:$PATH"

# 1. The SPA (frontend/dist), from the committed package-lock.json.
if [ ! -d "$ROOT/frontend/node_modules" ]; then
  (cd "$ROOT/frontend" && npm ci)
fi
(cd "$ROOT/frontend" && npm run build)

# 2. NOTICES.txt (third-party licences) -> rust/target/desktop-notices/NOTICES.txt,
# for the host target (the one `cargo tauri build` builds without --target).
HOST_TARGET="$(cd "$ROOT/rust" && rustc -vV | sed -n 's/^host: //p')"
(cd "$ROOT/rust" && cargo fetch --locked)
python3 "$ROOT/scripts/desktop_notices.py" --target "$HOST_TARGET"

# 2b. The agent CLI, bundled beside the app as a Tauri externalBin
# (Musaic.app/Contents/MacOS/musaic-cli). tauri.bundle.conf.json names it, so
# only a bundling build needs this file; `cargo build` / `cargo test` do not.
(cd "$ROOT/rust" && cargo build --release --locked -p musaic-cli --target "$HOST_TARGET")
mkdir -p "$ROOT/rust/crates/musaic-desktop/binaries"
cp "$ROOT/rust/target/$HOST_TARGET/release/musaic-cli" \
   "$ROOT/rust/crates/musaic-desktop/binaries/musaic-cli-$HOST_TARGET"

# 3. Release build + bundle. tauri.bundle.conf.json adds NOTICES.txt to the
# resources; CI=true makes the dmg step skip its Finder AppleScript
# (no window layout, no Automation prompt). Unsigned: tauri.conf.json has
# signingIdentity null; APPLE_* / TAURI_SIGNING_* are cleared so nothing
# from the caller's environment can turn signing on.
# `cargo tauri build` rewrites the crate's `tauri-build` dependency line
# (to `{ version = "2", features = [] }`, same meaning); put the committed
# Cargo.toml back afterwards so a build leaves no diff.
cd "$ROOT/rust/crates/musaic-desktop"
SAVED_MANIFEST="$(mktemp)"
cp Cargo.toml "$SAVED_MANIFEST"
trap 'cp "$SAVED_MANIFEST" "$ROOT/rust/crates/musaic-desktop/Cargo.toml"; rm -f "$SAVED_MANIFEST"' EXIT
env -u APPLE_CERTIFICATE -u APPLE_SIGNING_IDENTITY -u APPLE_ID -u APPLE_PASSWORD \
    -u APPLE_API_KEY -u APPLE_API_ISSUER -u TAURI_SIGNING_PRIVATE_KEY \
    -u TAURI_SIGNING_PRIVATE_KEY_PASSWORD \
    CI=true cargo tauri build --ci --bundles app,dmg --config tauri.bundle.conf.json

test -x "$ROOT"/rust/target/release/bundle/macos/Musaic.app/Contents/MacOS/musaic-cli \
  || { echo "the bundled musaic-cli is missing from Musaic.app" >&2; exit 1; }
ls -d "$ROOT"/rust/target/release/bundle/macos/*.app "$ROOT"/rust/target/release/bundle/dmg/*.dmg
