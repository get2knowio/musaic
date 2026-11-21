#!/usr/bin/env bash
# Build the agent CLI for one target triple and stage it where Tauri's
# `externalBin` (tauri.bundle.conf.json: "binaries/musaic-cli") looks for it:
#
#   rust/crates/musaic-desktop/binaries/musaic-cli-<triple>[.exe]   (gitignored)
#
#   scripts/stage_cli_sidecar.sh <target-triple>
#
# Used by scripts/package_macos.sh and by the desktop-bundle action that the
# release and bundle-check workflows share, so every installer build stages
# the CLI the same way. Only a bundling build needs the file; plain
# `cargo build` / `cargo test` do not. Runs under bash on macOS, Linux and
# Windows (Git Bash on the hosted runners).
set -euo pipefail

if [ "$#" -ne 1 ] || [ -z "$1" ]; then
  echo "usage: $0 <target-triple>" >&2
  exit 64
fi
TARGET="$1"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/rust/target}"

ext=""
case "$TARGET" in
  *-windows-*) ext=".exe" ;;
esac

(cd "$ROOT/rust" && cargo build --release --locked -p musaic-cli --target "$TARGET")
mkdir -p "$ROOT/rust/crates/musaic-desktop/binaries"
dest="$ROOT/rust/crates/musaic-desktop/binaries/musaic-cli-$TARGET$ext"
cp "$TARGET_DIR/$TARGET/release/musaic-cli$ext" "$dest"
echo "staged $dest"
