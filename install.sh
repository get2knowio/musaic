#!/bin/sh
# Musaic installer (macOS and Linux).
#
#   curl -fsSL https://raw.githubusercontent.com/get2knowio/musaic/main/install.sh | sh
#
# While only pre-release builds are published (or to grab the latest -rc build),
# install the newest release INCLUDING pre-releases:
#   curl -fsSL https://raw.githubusercontent.com/get2knowio/musaic/main/install.sh | sh -s -- --prerelease
#
# Downloads the Musaic desktop app for your platform from the chosen GitHub
# release, checks it against the release's SHA256SUMS.txt, and installs it:
#   macOS  Musaic.app into /Applications (or ~/Applications when that is not writable)
#   Linux  the AppImage as ~/.local/bin/musaic, plus a launcher entry
# Re-running upgrades an existing install. Your library data (~/.musaic) is not
# touched. Windows: download the -setup.exe from the Releases page instead.
#
# Options:
#   --prerelease        Install the newest release INCLUDING pre-releases
#                       (default: only the latest stable release is considered)
#
# Environment overrides:
#   MUSAIC_VERSION      release tag to install (e.g. v0.2.0-rc.1); overrides --prerelease
#   MUSAIC_PRERELEASE   set to "true" for the same effect as --prerelease
#   MUSAIC_INSTALL_DIR  where to install (macOS: the folder that gets Musaic.app;
#                       Linux: the folder that gets the `musaic` executable)
#   MUSAIC_REPO_SLUG    owner/repo (default: get2knowio/musaic)
#   MUSAIC_ASSET_URL    install this file instead of looking one up (https:// or
#                       file://); no checksum is checked for it
set -eu

REPO_SLUG="${MUSAIC_REPO_SLUG:-get2knowio/musaic}"
VERSION="${MUSAIC_VERSION:-latest}"
PRERELEASE="${MUSAIC_PRERELEASE:-false}"
INSTALL_DIR="${MUSAIC_INSTALL_DIR:-}"
ASSET_URL="${MUSAIC_ASSET_URL:-}"

info() { printf '\033[1;36m[musaic]\033[0m %s\n' "$1"; }
warn() { printf '\033[1;33m[musaic]\033[0m %s\n' "$1" >&2; }
die()  { printf '\033[1;31m[musaic] error:\033[0m %s\n' "$1" >&2; exit 1; }

# Args (passed via `sh -s -- <args>` when piped from curl). Kept POSIX-simple.
while [ $# -gt 0 ]; do
  case "$1" in
    --prerelease) PRERELEASE="true"; shift ;;
    -h|--help)
      printf 'Usage: install.sh [--prerelease]\n'
      printf '  --prerelease   Install the newest release including pre-releases\n'
      exit 0 ;;
    *) die "Unknown option: $1 (see --help)." ;;
  esac
done

command -v curl >/dev/null 2>&1 || die "curl is required."

# --- detect platform -------------------------------------------------------
# Asset names come from the release workflow (Tauri's bundle names):
#   Musaic_<version>_aarch64.dmg   Musaic_<version>_x64.dmg
#   Musaic_<version>_amd64.AppImage
os="$(uname -s)"; arch="$(uname -m)"
case "$os" in
  Darwin)
    case "$arch" in
      arm64 | aarch64) SUFFIX="_aarch64.dmg" ;;
      x86_64) SUFFIX="_x64.dmg" ;;
      *) die "unsupported Mac architecture: $arch" ;;
    esac ;;
  Linux)
    case "$arch" in
      x86_64 | amd64) SUFFIX="_amd64.AppImage" ;;
      *) die "no Linux build for $arch yet (x86_64 only). Build from source: https://github.com/${REPO_SLUG}#build-from-source" ;;
    esac ;;
  *) die "unsupported system: $os. On Windows, download the -setup.exe from https://github.com/${REPO_SLUG}/releases" ;;
esac

WORK="$(mktemp -d)"
MOUNT=""
cleanup() {
  [ -n "$MOUNT" ] && hdiutil detach "$MOUNT" -quiet >/dev/null 2>&1 || true
  rm -rf "$WORK"
}
trap cleanup EXIT INT TERM

# --- resolve which release to pull -----------------------------------------
# An explicit MUSAIC_VERSION always wins (pin a tag). Otherwise --prerelease
# picks the newest release including pre-releases; the default is the latest
# STABLE release (GitHub's /releases/latest excludes pre-releases). Draft
# releases are never visible here.
latest_prerelease_tag() {
  curl -fsSL "https://api.github.com/repos/${REPO_SLUG}/releases?per_page=20" 2>/dev/null \
    | grep '"tag_name"' \
    | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/' \
    | grep -E '^v[0-9]' \
    | head -1
}

SUMS_URL=""
if [ -n "$ASSET_URL" ]; then
  DL_URL="$ASSET_URL"
  warn "Installing ${DL_URL} as given (MUSAIC_ASSET_URL); no checksum is checked."
else
  if [ "$VERSION" != "latest" ]; then
    API_URL="https://api.github.com/repos/${REPO_SLUG}/releases/tags/${VERSION}"
  elif [ "$PRERELEASE" = "true" ]; then
    info "Resolving the latest pre-release…"
    tag="$(latest_prerelease_tag || true)"
    if [ -n "${tag:-}" ]; then
      VERSION="$tag"
      API_URL="https://api.github.com/repos/${REPO_SLUG}/releases/tags/${tag}"
    else
      warn "Could not resolve a pre-release; falling back to the latest stable release."
      API_URL="https://api.github.com/repos/${REPO_SLUG}/releases/latest"
    fi
  else
    API_URL="https://api.github.com/repos/${REPO_SLUG}/releases/latest"
  fi

  curl -fsSL "$API_URL" -o "$WORK/release.json" 2>/dev/null \
    || die "no release found at ${API_URL}. If only pre-releases exist yet, re-run with --prerelease."
  urls="$(grep -oE '"browser_download_url": *"[^"]+"' "$WORK/release.json" \
    | sed -E 's/.*"(https:[^"]+)"/\1/')"
  # `case` instead of a pattern built from $SUFFIX: the dots stay literal.
  DL_URL=""
  for u in $urls; do
    case "$u" in
      *"$SUFFIX") [ -z "$DL_URL" ] && DL_URL="$u" ;;
      */SHA256SUMS.txt) SUMS_URL="$u" ;;
    esac
  done
  [ -n "$DL_URL" ] || die "this release has no file ending in ${SUFFIX}. See https://github.com/${REPO_SLUG}/releases"
fi

# --- download and verify ---------------------------------------------------
FILE="$WORK/$(basename "$DL_URL")"
info "Downloading $(basename "$DL_URL")"
curl -fSL --progress-bar "$DL_URL" -o "$FILE" || die "failed to download $DL_URL"

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

if [ -n "$SUMS_URL" ]; then
  curl -fsSL "$SUMS_URL" -o "$WORK/SHA256SUMS.txt" || die "failed to download $SUMS_URL"
  want="$(grep -F "  $(basename "$FILE")" "$WORK/SHA256SUMS.txt" | head -1 | cut -d' ' -f1)"
  [ -n "$want" ] || die "SHA256SUMS.txt has no entry for $(basename "$FILE"); not installing."
  got="$(sha256_of "$FILE")"
  [ "$want" = "$got" ] || die "checksum mismatch for $(basename "$FILE") (expected $want, got $got); not installing."
  info "Checksum verified."
elif [ -z "$ASSET_URL" ]; then
  warn "This release has no SHA256SUMS.txt; installing without a checksum check."
fi

# --- install: macOS --------------------------------------------------------
if [ "$os" = "Darwin" ]; then
  if [ -z "$INSTALL_DIR" ]; then
    if [ -w /Applications ]; then INSTALL_DIR="/Applications"; else INSTALL_DIR="$HOME/Applications"; fi
  fi
  mkdir -p "$INSTALL_DIR"
  TARGET="$INSTALL_DIR/Musaic.app"

  # Replacing the bundle under a running app leaves it half old, half new.
  # Only the copy being replaced matters; another copy elsewhere may stay open.
  if pgrep -f "$TARGET/Contents/MacOS/" >/dev/null 2>&1; then
    die "Musaic ($TARGET) is running. Quit it and run the installer again."
  fi

  MOUNT="$WORK/mnt"
  mkdir "$MOUNT"
  hdiutil attach "$FILE" -nobrowse -readonly -noautoopen -mountpoint "$MOUNT" -quiet \
    || { MOUNT=""; die "could not open the disk image."; }
  [ -d "$MOUNT/Musaic.app" ] || die "the disk image holds no Musaic.app."

  info "Installing to $TARGET"
  # Copy beside the target first, then swap: a failed copy never leaves a
  # half-written app in place.
  rm -rf "$TARGET.installing"
  ditto "$MOUNT/Musaic.app" "$TARGET.installing"
  rm -rf "$TARGET"
  mv "$TARGET.installing" "$TARGET"
  hdiutil detach "$MOUNT" -quiet >/dev/null 2>&1 || true
  MOUNT=""

  # The builds are not signed or notarised. A file fetched by curl carries no
  # download quarantine flag, but clear it anyway in case the image came from
  # a browser download (MUSAIC_ASSET_URL=file://...).
  xattr -dr com.apple.quarantine "$TARGET" >/dev/null 2>&1 || true

  info "Installed Musaic."
  info "  Open it from Launchpad or Spotlight, or run:  open \"$TARGET\""
  info "  Your library data lives in ~/.musaic and was not touched."
  exit 0
fi

# --- install: Linux --------------------------------------------------------
[ -n "$INSTALL_DIR" ] || INSTALL_DIR="$HOME/.local/bin"
mkdir -p "$INSTALL_DIR"
TARGET="$INSTALL_DIR/musaic"

chmod +x "$FILE"
mv "$FILE" "$TARGET.installing"
mv "$TARGET.installing" "$TARGET"
info "Installed Musaic to $TARGET"

# A launcher entry so Musaic shows up in the desktop's application menu.
APPS_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
if mkdir -p "$APPS_DIR" 2>/dev/null; then
  cat >"$APPS_DIR/musaic.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Musaic
Comment=Browse and maintain a FLAC music collection
Exec="$TARGET"
Terminal=false
Categories=AudioVideo;Audio;
EOF
  info "Added a launcher entry: $APPS_DIR/musaic.desktop"
fi

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) warn "$INSTALL_DIR is not on your PATH. Add it:"
     warn "  export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
esac

info "Run it with:  musaic"
info "  Saving API keys needs a Secret Service provider (GNOME Keyring or KWallet)."
info "  Your library data lives in ~/.musaic and was not touched."
