# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
project aims to follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!--
Add entries under Unreleased as you go. On release, rename the section to the version +
date and start a fresh Unreleased. Categories: Added, Changed, Deprecated, Removed, Fixed,
Security.
-->

## [Unreleased]

## [0.2.0-rc.1] - 2026-11-14

A rewrite. The Python/FastAPI backend and the Electron shell are replaced by a Rust
application packaged with Tauri 2. The React interface and the SQLite database format are
kept; an existing database is migrated in place. The Python implementation is preserved
at the tag `python-final`.

**About these builds.** Installers are published for macOS (Apple Silicon and Intel)
and Linux (x64, AppImage and deb), with their SHA-256 sums in `SHA256SUMS.txt`. They are
**not code-signed or notarised**.

**No Windows build.** The automated tests do not yet pass on Windows, so no Windows
installer is published until they do. A Windows installer was attached to this release
at first and has been withdrawn for that reason. Progress is tracked in
[#178](https://github.com/get2knowio/musaic/issues/178).

- **Run by a person so far: only the macOS Apple Silicon build.** The macOS Intel and
  Linux builds are produced by CI from the same source and have not been tested by hand
  yet. Reports of what works and what does not are very welcome. The automated tests
  pass on macOS and Linux.
- **Opening the unsigned app on macOS:** install with the one-line `install.sh` command
  from the README (a `curl` download should not be quarantined; this is not yet confirmed on a second machine), or, for a `.dmg` downloaded in a
  browser, use **System Settings → Privacy & Security → Open Anyway** after the first
  refusal, or run `xattr -dr com.apple.quarantine /Applications/Musaic.app`. The
  browser-download route has not been tested.
- **Cover downloads now refuse plain `http` URLs and local-network addresses** (for
  example a NAS, `localhost` or a `192.168.x.x` address); only `https` URLs on public hosts
  are fetched. Use **Upload** on the album's cover for an image on your own machine or
  network.

### Added

- Native desktop app: the API server runs inside the app on a random loopback port; one
  window, single instance per config folder, remembered window size and position.
- Native folder picker for the library folder; "Show in Finder / Explorer / file
  manager" for an album folder and for the app's trash.
- Fingerprinting (Chromaprint), ReplayGain and BPM computed in-process; `fpcalc` is no
  longer needed.
- Asking before quitting while work (a scan, an apply, a check run…) is in progress.
- Third-party notices shipped with the app (`NOTICES.txt`).

### Changed

- The database is upgraded to schema v18 on first start, after a backup copy is written
  to `~/.musaic/library.v16.before-rust.db`.
- API keys are stored in the OS keyring (macOS Keychain, Windows Credential Manager,
  Secret Service on Linux) instead of the database; existing keys are moved there on
  first start and removed from the database, its backup and older snapshots.
- External links open in the system browser; only public `https` links are opened.
- The IBM Plex fonts are bundled instead of loaded from Google Fonts.

### Removed

- The built-in AI backend. Controls that need a language model are hidden; MusicBrainz,
  AcoustID and Cover Art Archive lookups and the fixes that use them work as before. An
  MCP server, so that an external agent can drive Musaic, is the next planned piece.
- The Electron shell, the bundled Python runtime and the Python package.

### Security

- The local API checks `Host` and `Origin` on every request, and the desktop app requires
  a per-launch cookie and sends a strict Content-Security-Policy.
- Cover downloads accept only `https` URLs on public hosts, re-checked on every redirect
  and at connection time. Plain-`http` and local-network cover URLs are now refused; use
  cover upload for those images.

[Unreleased]: https://github.com/get2knowio/musaic/compare/v0.2.0-rc.1...HEAD
[0.2.0-rc.1]: https://github.com/get2knowio/musaic/compare/python-final...v0.2.0-rc.1
