# Musaic

A local-first desktop app for browsing and maintaining a personal FLAC music collection.

Point Musaic at a folder of FLAC files. Every folder that directly contains FLACs is an
album. From there you can:

- browse the library as an album grid or a sortable list;
- open an album to see its tracks, tags, cover art and open issues;
- identify albums against MusicBrainz, by folder name and track lengths or by audio
  fingerprint (AcoustID), and apply the corrections it finds — with a diff preview before
  anything is written;
- write Vorbis tags, rename files and folders to your templates, and embed or save cover
  art (from the Cover Art Archive, a URL, or a file you upload);
- run health checks across the library — unidentified albums, tags out of step with
  MusicBrainz, missing cover art, track-number gaps, inconsistent artist sort names,
  duplicate tracks, filenames that do not match your templates, multi-disc sets not kept
  in one folder — and fix them one at a time or in bulk (the multi-disc check only shows
  what a conversion would do, for now);
- compute BPM and ReplayGain;
- send what the resolver cannot settle to an assistant you choose, as research requests
  it picks up over MCP (one album at a time, or a whole Fix all run as one batch); you
  review and apply each result.

Everything runs on your machine. Fingerprinting, ReplayGain and BPM are computed
in-process; no external tools (such as `fpcalc`) are needed. The only network traffic is
to MusicBrainz, AcoustID and the Cover Art Archive (and to a cover URL you give it).

Musaic is a Rust application with a React user interface, packaged as a desktop app with
[Tauri](https://tauri.app/).

## Install

**macOS and Linux**, one line:

```bash
curl -fsSL https://raw.githubusercontent.com/get2knowio/musaic/main/install.sh | sh -s -- --prerelease
```

`--prerelease` is needed while only release candidates are published; drop it once
there is a stable release. The script downloads the build for your system from the
newest release, checks it against the release's `SHA256SUMS.txt`, and installs it:
`Musaic.app` into `/Applications` on macOS, or the AppImage as `~/.local/bin/musaic`
(with a launcher entry) on Linux. Run it again to upgrade. Your library data in
`~/.musaic` is not touched. To pin a version: `MUSAIC_VERSION=v0.2.0-rc.2` before `sh`.
On macOS this route should also avoid the "unidentified developer" prompt described
below, because a file fetched by `curl` is not marked as an internet download.

**If you prefer to download by hand:** get the installer for your system from the
[Releases](https://github.com/get2knowio/musaic/releases) page:

| System | File |
|---|---|
| macOS, Apple Silicon | `.dmg` with `aarch64` in its name |
| macOS, Intel | `.dmg` with `x64` in its name |
| Linux (x64) | `.AppImage` or `.deb` |
| Windows (x64), from v0.2.0-rc.2 on | `-setup.exe` |

Each release lists the SHA-256 of every file in `SHA256SUMS.txt`.

**Windows:** the automated tests now run on Windows and are required in CI, and the
release workflow builds an unsigned NSIS installer (`-setup.exe`), from v0.2.0-rc.2 on.
v0.2.0-rc.1 has no Windows installer. Windows SmartScreen warns about an unsigned
installer: choose "More info", then "Run anyway".

**Tested so far:** only the macOS Apple Silicon build has been run by a person. The
Linux and Intel Mac builds are produced by CI from the same source and have not yet been
tested by hand. Reports of what works and what does not are very welcome. The automated
tests pass on macOS and Linux.

### The builds are not signed

The installers are not code-signed or notarised yet, so your system will warn you the
first time you open Musaic.

**macOS.** The one-line installer above is the suggested route: a file fetched by `curl`
carries no download quarantine flag, so macOS should open the app without the
"unidentified developer" refusal. If you download the `.dmg` with a browser instead, drag Musaic to
Applications; macOS will then refuse to open it the first time. Either:

- try to open Musaic once, then go to **System Settings → Privacy & Security**, find the
  message about Musaic near the bottom and choose **Open Anyway**; or
- remove the download quarantine flag in Terminal:

  ```bash
  xattr -dr com.apple.quarantine /Applications/Musaic.app
  ```

The browser-download route has not been tested yet: we do not know whether current macOS
offers **Open Anyway** for this build or reports the app as damaged (in which case the
`xattr` command above is the way in). Reports welcome.

**Linux.** For the AppImage: `chmod +x Musaic_*.AppImage` and run it. For the `.deb`:
`sudo apt install ./Musaic_*.deb`. Musaic uses WebKitGTK 4.1. Saving API keys needs a
Secret Service provider (GNOME Keyring or KWallet) to be running; without one, Musaic
works but cannot store keys, and the Settings page says so.

## Where your data lives

| What | Where |
|---|---|
| Library database, settings, logs, thumbnails, trash | `~/.musaic/` (Windows: `%USERPROFILE%\.musaic\`) |
| API keys (for example your AcoustID key) | the OS keyring: macOS Keychain, Windows Credential Manager, or the Secret Service on Linux — service name `musaic` |
| Web view storage (cache) | macOS: `~/Library/WebKit/io.get2know.musaic` and `~/Library/Caches/io.get2know.musaic`. Windows: `%LOCALAPPDATA%\io.get2know.musaic\`. Linux: `~/.local/share/io.get2know.musaic/` |

Inside `~/.musaic/`:

| Path | What |
|---|---|
| `library.db` | SQLite database: scanned albums, fingerprints, findings, jobs, settings |
| `config.json` | the selected library folder |
| `server.log` | the app's log (rotated at 5 MiB, five generations; key values are never written) |
| `thumbs/` | cover thumbnails |
| `trash/` | files Musaic deleted (for example duplicate tracks); move them back to restore |
| `window-state.json` | window size and position |

Your FLAC files and their `cover.jpg` files remain the source of truth; the database is a
cache and a store for work in progress. Set `MUSAIC_CONFIG_DIR` (or pass `--config-dir`)
to use a different folder.

## Upgrading from the Python version

Earlier versions of Musaic (up to `v0.1.0-rc.4`) were a Python/FastAPI server with an
Electron shell. This release replaces them. If you used the Python version:

- **Your database migrates in place.** On first start Musaic copies
  `~/.musaic/library.db` to `~/.musaic/library.v16.before-rust.db`, then upgrades
  `library.db` to the new schema. Albums, fingerprints, MusicBrainz anchors, findings and
  settings carry over. The copy is your way back: to return to the Python version, quit
  Musaic and copy the backup over `library.db`.
- **API keys move to the OS keyring.** Keys the Python version kept in the database are
  moved into the keyring on first start and removed from the database and from the backup
  copy once the keyring holds them. If the keyring is not available (see Linux above),
  the keys stay where they were until a later start can move them.
- Album ids are derived from folder paths exactly as before, so nothing is orphaned.

## What is different from the Python version

- **Research goes to an assistant you add, not to a built-in model.** MusicBrainz,
  AcoustID and Cover Art Archive lookups work as before and settle most fixes. What they
  cannot settle becomes a research request that an assistant you add in **Settings,
  Assistants** picks up over MCP ([docs/agents.md](docs/agents.md)); Musaic itself calls
  no model and starts no agent. Research from before this version is kept; the model and
  web-scraping settings and their keys are removed (the keys are deleted from the keyring
  once).
- **Cover downloads are stricter.** Musaic downloads a cover only from an `https` URL on a
  public host, and checks every redirect the same way. Plain `http` URLs and addresses on
  your local network (such as a NAS or `localhost`) are refused. Upload the image from
  your own machine instead (the **Upload** / **Replace** button on the album's cover).
- **A desktop app first.** The server and the window are one process; there is nothing to
  start separately. The API answers only on `127.0.0.1`.
- **Same interface.** The React UI is the one the Python version served, with small
  changes.

The Python implementation is preserved at the git tag
[`python-final`](https://github.com/get2knowio/musaic/tree/python-final).

## Roadmap

The MCP server is in place: an assistant you add can read the library, take research
requests and propose changes for you to approve ([docs/agents.md](docs/agents.md),
[docs/mcp.md](docs/mcp.md)).

## Build from source

You need:

- Rust — the version pinned in `rust/rust-toolchain.toml`, which `rustup` installs for you;
- Node.js 22.12 or newer, to build the UI;
- the Tauri CLI and `cargo-about` for packaging (the versions the release workflow uses):
  `cargo install tauri-cli --version 2.12.4 --locked` and
  `cargo install cargo-about --version 0.9.3 --locked --features cli`;
- on Linux, the WebKitGTK development packages. On Debian/Ubuntu:

  ```bash
  sudo apt-get install build-essential pkg-config libssl-dev \
    libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev \
    librsvg2-dev patchelf
  ```

- on Windows, the MSVC build tools and the WebView2 runtime.

Build the UI, then run the app against a scratch config folder:

```bash
(cd frontend && npm ci && npm run build)
cd rust
cargo run -p musaic-desktop -- --config-dir "$(mktemp -d)/config" --dev-memory-secrets
```

`--dev-memory-secrets` keeps keys in memory instead of the OS keyring; it is refused
unless the config folder is under the system temp folder. Leave both options out to use
`~/.musaic` and the real keyring.

Package an unsigned macOS app and disk image (output under
`rust/target/release/bundle/`):

```bash
scripts/package_macos.sh
```

The release workflow (`.github/workflows/release.yml`) shows the equivalent steps for
Windows and Linux. See [CONTRIBUTING.md](CONTRIBUTING.md) for tests and development, and
[docs/desktop.md](docs/desktop.md) for how the desktop shell works.

## License

[MIT](LICENSE). Third-party notices ship with the app (`NOTICES.txt`).
