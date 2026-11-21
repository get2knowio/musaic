#!/usr/bin/env bash
# Check that the installers `cargo tauri build` produced carry the agent CLI
# at the place docs/agents.md ("Where the CLI is") names, and that it runs.
#
#   scripts/check_bundled_cli.sh <bundle-dir> <bundles> [<target-triple>]
#
#   <bundle-dir>  rust/target[/<triple>]/release/bundle
#   <bundles>     the comma list given to `cargo tauri build --bundles`
#                 (app, dmg, deb, appimage, nsis)
#   <triple>      the build's target; when it is not this machine's own
#                 (the Intel Mac build cross-compiled on Apple Silicon) the
#                 CLI's architecture is checked instead of running it
#
# Per bundle:
#   app       Musaic.app/Contents/MacOS/musaic-cli
#   dmg       the same, inside the mounted disk image
#   deb       usr/bin/musaic-cli in the package's data
#   appimage  usr/bin/musaic-cli inside the image; run from the mounted image,
#             `musaic-cli setup --dry-run` must refuse with the copy-it-out
#             remedy (needs FUSE: libfuse2 on Ubuntu)
#   nsis      a silent install into a scratch folder: musaic-cli.exe next to
#             the app's executable (musaic-desktop.exe), then a silent
#             uninstall
#
# "Runs" means `musaic-cli --version` prints the workspace version and
# `musaic-cli doctor` against an empty scratch config dir reports the
# descriptor absent with exit code 1 (no app running). Nothing reads or
# writes ~/.musaic: HOME, the config dir and the host config files all point
# into a scratch folder that is removed at the end.
set -euo pipefail

if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
  echo "usage: $0 <bundle-dir> <bundles> [<target-triple>]" >&2
  exit 64
fi
BUNDLE_DIR="$1"
# Absolute, because some checks run an installer after cd-ing into a scratch dir.
[ -d "$BUNDLE_DIR" ] && BUNDLE_DIR="$(cd "$BUNDLE_DIR" && pwd)"
BUNDLES="$2"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
HOST="$(cd "$ROOT/rust" && rustc -vV | sed -n 's/^host: //p')"
TARGET="${3:-$HOST}"
VERSION="$(sed -n '/^\[workspace\.package\]/,/^\[/ s/^version *= *"\([^"]*\)".*/\1/p' "$ROOT/rust/Cargo.toml")"
test -n "$VERSION" || { echo "no workspace version in rust/Cargo.toml" >&2; exit 1; }
test -d "$BUNDLE_DIR" || { echo "no bundle directory at $BUNDLE_DIR" >&2; exit 1; }

TMP_BASE="${RUNNER_TEMP:-${TMPDIR:-/tmp}}"
# Git Bash on Windows: RUNNER_TEMP is a Windows path.
if command -v cygpath >/dev/null; then TMP_BASE="$(cygpath -u "$TMP_BASE")"; fi
SCRATCH="$(mktemp -d "${TMP_BASE%/}/musaic-bundle-check.XXXXXX")"
CLEANUP=()
cleanup() {
  local i
  for ((i = ${#CLEANUP[@]} - 1; i >= 0; i--)); do eval "${CLEANUP[$i]}" || true; done
  rm -rf "$SCRATCH"
}
trap cleanup EXIT

failures=0
fail() { echo "FAIL: $*" >&2; failures=$((failures + 1)); }
pass() { echo "ok: $*"; }

# A home and config dir for the CLI that are not the user's.
SCRATCH_HOME="$SCRATCH/home"
SCRATCH_CONFIG="$SCRATCH/config"
mkdir -p "$SCRATCH_HOME" "$SCRATCH_CONFIG"
in_scratch_env() {
  env HOME="$SCRATCH_HOME" USERPROFILE="$SCRATCH_HOME" APPDATA="$SCRATCH_HOME/AppData/Roaming" \
    MUSAIC_CONFIG_DIR="$SCRATCH_CONFIG" "$@"
}

# Run the CLI at $1 (label $2): --version, then doctor.
check_runs() {
  local cli="$1" label="$2" out code
  if [ "$TARGET" != "$HOST" ]; then
    # Cross-built: the right architecture is all that can be checked here.
    if command -v lipo >/dev/null; then
      local arch="${TARGET%%-*}"
      [ "$arch" = "aarch64" ] && arch="arm64"
      if lipo -archs "$cli" | tr ' ' '\n' | grep -qx "$arch"; then
        pass "$label is built for $TARGET (not run: host is $HOST)"
      else
        fail "$label is not built for $TARGET: $(lipo -archs "$cli")"
      fi
    else
      echo "note: $label not run (target $TARGET, host $HOST)"
    fi
    return
  fi
  if out="$(in_scratch_env "$cli" --version 2>&1)" && [ "$out" = "musaic-cli $VERSION" ]; then
    pass "$label --version: $out"
  else
    fail "$label --version printed '$out', expected 'musaic-cli $VERSION'"
  fi
  code=0
  out="$(in_scratch_env "$cli" --config-dir "$SCRATCH_CONFIG" doctor 2>&1)" || code=$?
  if [ "$code" -eq 1 ] && printf '%s' "$out" | grep -q '"descriptor":{"status":"absent"}'; then
    pass "$label doctor (no app running): exit 1, descriptor absent"
  else
    fail "$label doctor: exit $code, output: $out"
  fi
}

check_app() {
  local app="$1" label="$2" cli="$1/Contents/MacOS/musaic-cli"
  if [ -f "$cli" ] && [ -x "$cli" ]; then
    pass "$label: Contents/MacOS/musaic-cli is present"
    check_runs "$cli" "$label musaic-cli"
  else
    fail "$label has no executable Contents/MacOS/musaic-cli"
    ls -l "$app/Contents/MacOS" >&2 || true
  fi
}

one_file() {
  # The single file matching a glob, or nothing.
  local matches=("$@")
  if [ "${#matches[@]}" -eq 1 ] && [ -e "${matches[0]}" ]; then
    printf '%s' "${matches[0]}"
  fi
}

check_bundle_app() {
  local app
  app="$(one_file "$BUNDLE_DIR"/macos/*.app)"
  if [ -z "$app" ]; then fail "expected one .app under $BUNDLE_DIR/macos"; return; fi
  check_app "$app" "$(basename "$app")"
}

check_bundle_dmg() {
  local dmg mnt app
  dmg="$(one_file "$BUNDLE_DIR"/dmg/*.dmg)"
  if [ -z "$dmg" ]; then fail "expected one .dmg under $BUNDLE_DIR/dmg"; return; fi
  mnt="$SCRATCH/dmg"
  mkdir -p "$mnt"
  if ! hdiutil attach -nobrowse -readonly -noautoopen -mountpoint "$mnt" "$dmg" >/dev/null; then
    fail "$(basename "$dmg") could not be mounted"
    return
  fi
  CLEANUP+=("hdiutil detach -quiet '$mnt'")
  app="$(one_file "$mnt"/*.app)"
  if [ -z "$app" ]; then fail "$(basename "$dmg") holds no single .app"; return; fi
  check_app "$app" "$(basename "$dmg"):$(basename "$app")"
}

check_bundle_deb() {
  local deb dir
  deb="$(one_file "$BUNDLE_DIR"/deb/*.deb)"
  if [ -z "$deb" ]; then fail "expected one .deb under $BUNDLE_DIR/deb"; return; fi
  dir="$SCRATCH/deb"
  dpkg-deb -x "$deb" "$dir"
  if [ -x "$dir/usr/bin/musaic-cli" ]; then
    pass "$(basename "$deb"): /usr/bin/musaic-cli is present"
    check_runs "$dir/usr/bin/musaic-cli" "$(basename "$deb") /usr/bin/musaic-cli"
  else
    fail "$(basename "$deb") has no executable usr/bin/musaic-cli"
    ls -l "$dir/usr/bin" >&2 || true
  fi
}

check_bundle_appimage() {
  local image name dir mnt pid code out i
  image="$(one_file "$BUNDLE_DIR"/appimage/*.AppImage)"
  if [ -z "$image" ]; then fail "expected one .AppImage under $BUNDLE_DIR/appimage"; return; fi
  name="$(basename "$image")"
  chmod +x "$image"

  # 1. Present and runnable, from an extracted copy.
  dir="$SCRATCH/appimage"
  mkdir -p "$dir"
  (cd "$dir" && "$image" --appimage-extract >/dev/null)
  if [ -x "$dir/squashfs-root/usr/bin/musaic-cli" ]; then
    pass "$name: usr/bin/musaic-cli is inside the image"
    check_runs "$dir/squashfs-root/usr/bin/musaic-cli" "$name usr/bin/musaic-cli"
  else
    fail "$name has no executable usr/bin/musaic-cli inside"
    ls -l "$dir/squashfs-root/usr/bin" >&2 || true
    return
  fi
  rm -rf "$dir"

  # 2. Run from the mounted image, as a user would reach it, setup refuses
  # the image-internal path and says to copy the CLI out.
  "$image" --appimage-mount >"$SCRATCH/appimage-mount.txt" 2>"$SCRATCH/appimage-mount.err" &
  pid=$!
  CLEANUP+=("kill $pid 2>/dev/null; wait $pid 2>/dev/null")
  mnt=""
  for i in $(seq 1 30); do
    mnt="$(head -n 1 "$SCRATCH/appimage-mount.txt" 2>/dev/null || true)"
    if [ -n "$mnt" ] && [ -x "$mnt/usr/bin/musaic-cli" ]; then break; fi
    if ! kill -0 "$pid" 2>/dev/null; then break; fi
    sleep 1
  done
  if [ -z "$mnt" ] || [ ! -x "$mnt/usr/bin/musaic-cli" ]; then
    fail "$name could not be mounted: $(cat "$SCRATCH/appimage-mount.err")"
    return
  fi
  # A Claude Code config in the scratch home, so there is a host to plan for.
  printf '{}\n' >"$SCRATCH_HOME/.claude.json"
  code=0
  out="$(in_scratch_env "$mnt/usr/bin/musaic-cli" setup --dry-run --host claude-code 2>&1)" || code=$?
  if [ "$code" -eq 1 ] && printf '%s' "$out" | grep -q '"action":"refuse"' \
     && printf '%s' "$out" | grep -q 'inside the AppImage' \
     && printf '%s' "$out" | grep -q 'copy it out'; then
    pass "$name: setup from the mounted image refuses, with the copy-it-out remedy"
  else
    fail "$name: setup from $mnt did not refuse as expected (exit $code): $out"
  fi
  if [ "$(cat "$SCRATCH_HOME/.claude.json")" != "{}" ] || [ "$(ls -A "$SCRATCH_HOME")" != ".claude.json" ]; then
    fail "$name: setup --dry-run changed the scratch home: $(ls -A "$SCRATCH_HOME")"
  fi
}

check_bundle_nsis() {
  local setup dir win_dir code
  setup="$(one_file "$BUNDLE_DIR"/nsis/*-setup.exe)"
  if [ -z "$setup" ]; then fail "expected one -setup.exe under $BUNDLE_DIR/nsis"; return; fi
  dir="$SCRATCH/nsis-install"
  win_dir="$(cygpath -w "$dir")"
  # NSIS: /S silent, /D=<dir> last and unquoted. Keep Git Bash from turning
  # the switches into paths.
  code=0
  MSYS2_ARG_CONV_EXCL='*' "$setup" /S "/D=$win_dir" || code=$?
  if [ "$code" -ne 0 ]; then fail "$(basename "$setup") /S exited $code"; return; fi
  CLEANUP+=("MSYS2_ARG_CONV_EXCL='*' '$dir/uninstall.exe' /S")
  if [ -f "$dir/musaic-desktop.exe" ] && [ -f "$dir/musaic-cli.exe" ]; then
    pass "$(basename "$setup"): musaic-cli.exe is next to musaic-desktop.exe"
    check_runs "$dir/musaic-cli.exe" "$(basename "$setup") musaic-cli.exe"
  else
    fail "$(basename "$setup") did not install musaic-cli.exe next to musaic-desktop.exe"
    ls -l "$dir" >&2 || true
  fi
}

IFS=',' read -r -a kinds <<<"$BUNDLES"
for kind in "${kinds[@]}"; do
  case "$kind" in
    app) check_bundle_app ;;
    dmg) check_bundle_dmg ;;
    deb) check_bundle_deb ;;
    appimage) check_bundle_appimage ;;
    nsis) check_bundle_nsis ;;
    *) fail "unknown bundle kind '$kind'" ;;
  esac
done

if [ "$failures" -ne 0 ]; then
  echo "$failures check(s) failed" >&2
  exit 1
fi
echo "the bundled musaic-cli is in place in: $BUNDLES"
