# The desktop shell (Tauri 2)

Crate: `rust/crates/musaic-desktop`. It turns the Musaic backend and the React UI
(`frontend/`) into one desktop application. The earlier Python version used an
Electron shell with a separate Python server process; the Tauri shell keeps the
same model of "UI talks HTTP to a local backend", minus the child process.

## Design: an in-process server and one webview

The `musaic-api` router runs inside the desktop process on `127.0.0.1:<port>`
(port 0, i.e. a free port chosen at each launch, unless `--port` /
`MUSAIC_PORT` says otherwise). It serves `/api` *and* the built UI, and the
single window navigates to that origin. Because the page is same-origin with the
API, the UI's ordinary `fetch('/api/…')` calls, its App-level `EventSource`
streams, the streaming POST responses (apply, analyze, quick-fix-all) and
`<img src="/api/albums/…/cover">` work unchanged. There is no Tauri IPC.

The loopback HTTP server is one face over `musaic_app::ops`, the application's operations
surface: each route parses its request, calls one operation and renders the result, so the
guard, the validation order, the events and the follow-up notifications live in the
operation, not in the route. Another face (an MCP server, a native UI) calls the same
operations in-process, with no HTTP in between. See [operations.md](operations.md) for the
inventory of operations and routes and the rules a face follows.

Plain HTTP to a loopback origin is ordinary web traffic for WKWebView (macOS),
WebView2 (Windows) and WebKitGTK (Linux), so server-sent events and streamed
response bodies behave as in a browser. This has been confirmed on macOS; on
Windows and Linux it has not been tried by hand yet (see "What has been tested").

Tauri's custom protocol was not used: its handler answers with one complete
response, so `EventSource` and streamed bodies would not work without rewriting
the UI's transport.

### Start-up

1. Parse options (`src/options.rs`).
2. Take the single-instance lock on the **config dir** (`src/instance.rs`):
   an exclusive lock on `<config>/desktop.lock`, plus `<config>/desktop-instance.json`
   (mode 0600) holding `{pid, port, focus}`. A second launch with the same
   config dir posts `/__musaic/focus` with the secret from that file and exits;
   the first instance un-minimises, shows and focuses its window. The lock is per
   config dir, not per machine, so a test run never collides with a real
   instance.
3. Open one window (1280×860, minimum 720×480) on a local boot page
   (`shell/`), which shows start-up errors (`#error=…`). Shown runs restore the
   saved window size and position (see "Window state").
4. On a background thread: build the application (`musaic-app`), bind the
   loopback server, write the instance file and the agent descriptor
   (`<config>/agent-instance.json`, see "Agent bridge"), and navigate the window
   to the one-time launch URL (see "Launch cookie").
5. Write the same rotating, secret-masking `<config>/server.log` as the server
   binary; the level comes from `MUSAIC_LOG_LEVEL`.

### Teardown

Closing the window, Cmd-Q, `AppHandle::exit`, SIGTERM / SIGINT / SIGHUP on Unix
and Ctrl-C on Windows all end in Tauri's exit event: every SSE stream is ended,
the server drains for up to 3 seconds (then remaining connections are aborted),
the application shuts down (a running library scan cancelled, so it saves nothing
and starts nothing, and waited for; fingerprint pool joined,
the threads of agent operations asked to stop at their next item and joined) and
the instance file and the agent descriptor are removed. No child processes are ever spawned,
except, on Windows, the system's `whoami.exe` and `icacls.exe`, run briefly and without a
window to give the agent descriptor its owner-only access control list.

### Quitting while work runs

Quitting while something is in progress (a scan, an apply, an analysis, a
restore, a check run, a bulk quick fix, the fingerprint pool, any other
write, or research an assistant has claimed) first asks "Quit anyway?" with **Quit** / **Keep running**; an idle app
quits at once. The rule lives in `musaic_api::WorkSnapshot::quit_prompt` and
`src/quit.rs` (one prompt at a time; once the user said Quit, the window close
and exit request that follow are not asked again); the native dialog is shown
with `rfd` from `src/lib.rs`.

| Route | Asks while busy? |
|---|---|
| Window close button, Cmd-W, Window / File → Close Window | yes |
| App menu → Quit Musaic, Cmd-Q (macOS) | yes |
| Last window gone | yes (normally already answered by the close that caused it) |
| Dock → Quit, a quit Apple Event, logout / restart / shut down (macOS) | no |
| SIGTERM / SIGINT / SIGHUP, Ctrl-C | no: nobody is there to answer |

On macOS the app menu's Quit item is Musaic's own "Quit Musaic" item (Cmd-Q),
not the predefined one, because the predefined item quits without giving the
app a chance to ask. Dock → Quit and logout go through a path the shell cannot hold
without unsafe Objective-C calls (the workspace forbids `unsafe`), so they quit
without asking; their teardown is the same clean path. Windows and Linux have no
app menu, so there only the close button, the last window and signals apply.

## Security

- **Loopback only.** The server binds `127.0.0.1`. musaic-api's Host / Origin
  check (`musaic-api/src/host_check.rs`) refuses requests whose `Host` or
  `Origin` is not the app's own loopback origin.
- **Launch cookie** (`musaic-api/src/launch.rs`). Each launch draws a random
  256-bit token. The window's first navigation is
  `GET /__musaic/launch?t=<token>` (Host must be exactly `127.0.0.1:<port>`; it
  works once, then answers 410), which sets
  `musaic_launch=<token>; HttpOnly; SameSite=Strict; Path=/` and refreshes to
  `/`. Every other request without the cookie gets 403. Order of checks: window
  layer → Host / Origin → cookie → routes. This keeps out other local users and
  processes that can reach the port but not the config dir. The standalone
  `musaic-api` server binary does not use the cookie.
- **Content-Security-Policy** on every response (`src/gate.rs`, `APP_CSP`):
  scripts, styles, fonts, images and connections from the app's own origin only
  (`img-src` also allows `data:` and `blob:`), no frames or objects. The IBM
  Plex fonts are bundled with the UI. A proposed remote cover is shown through
  a same-origin route: `GET /api/ai/jobs/{job_id}/cover-preview` (the URL a
  research result stored) or `GET /api/albums/{album_id}/cover-preview` (the Cover Art
  Archive front of the album's MusicBrainz anchor). The request names a research
  request or an album, never a URL; the server fetches under the cover-download rules below.
- **No IPC.** No capabilities, no `#[tauri::command]`s, `withGlobalTauri: false`.
  The dialog, opener and window-state plugins are registered but called from
  Rust only; the opener's own link-click script is switched off. Native menu
  commands reach the page as a fixed, allow-listed DOM event the shell
  evaluates, and their enabled state comes back through
  `POST /__musaic/menu-state` (see "The menu bar and commands").
- **Navigation lock** (`src/nav.rs`). The window may show the backend origin,
  its `blob:` URLs, `about:blank` and the boot page. A link to an `https` URL on
  a public host (default port, no user name or password) opens in the system
  browser; everything else is refused: plain `http`, `localhost`, `*.local`,
  `*.internal`, `*.home.arpa`, `*.lan`, single-label names, private / loopback /
  link-local addresses, other ports, and other schemes. `window.open` and
  `target=_blank` never create a window. Only the URL text is judged (the shell
  fetches nothing).
- **Cover downloads** (`musaic-net/src/preview.rs`) accept only `https` URLs on
  public hosts, re-checked on every redirect and at connection time. Plain-`http`
  and local-network cover URLs are refused; use cover upload for those images.
- **Secrets** live in the OS keyring (macOS Keychain, Windows Credential
  Manager, Secret Service on Linux). `--dev-memory-secrets` keeps them in memory
  for development and tests and is refused unless the config dir is under the
  system temp dir or a directory named `work`.

## Native integration

All of these are small endpoints under `/__musaic/` that need the launch cookie,
the exact Host and the app's own Origin, and are reached only from the window.
An initialization script defines a frozen `window.__musaicDesktop` marker
(backend origin only) that tells the UI which of them exist
(`frontend/src/lib/desktop.ts`); a plain browser has no marker and keeps its
browser behaviour.

- **Confirm / alert.** On macOS the webview implements no `window.confirm()` /
  `alert()` panel (confirm would always return false). The initialization
  script replaces both with a synchronous same-origin request to
  `/__musaic/dialog/{confirm,alert}`, which shows a native dialog.
- **Folder picker.** `POST /__musaic/dialog/folder` opens the system "choose a
  folder" panel and answers `{"path": "/abs/dir"}`, or `{"path": null}` when
  cancelled. It reads nothing and returns only the path the user chose.
- **Reveal in Finder / Explorer / the file manager.**
  `POST /__musaic/reveal/album/{album_id}` shows an album's folder,
  `POST /__musaic/reveal/trash` the app's trash folder (`<config>/trash`). The
  page sends an album id, never a path; the shell looks the folder up in the
  configured library, so nothing else can be revealed. 404 for an unknown id or
  a folder not on disk (a Missing album), 409 when no library is configured.
- **Window state** (`src/window_state.rs`). Size, position and maximized state
  are kept in `<config>/window-state.json`. A window whose title bar would not
  be reachable on any attached display is centred. `--hidden` runs neither read
  nor write the file.
- **Downloads** (the database backup) are allowed only from the backend origin
  and saved to the Downloads folder under a unique name.
- **Drag and drop**: Tauri's file-drop handler is off, so HTML drops reach the
  cover-upload dialog.
- **External links** open in the system browser (see Navigation lock).

- **Menu bar** (macOS; `src/menu.rs`, see below).

There is no updater, no deep-link handler and no tray icon.

## The menu bar and commands

On macOS the shell builds its own menu bar from the table in `src/menu.rs`
(#333, #334):

| Menu | Items |
|---|---|
| Musaic | About Musaic · Settings… (⌘,) · Services · Hide Musaic, Hide Others, Show All · Quit Musaic (⌘Q) |
| File | Choose Music Folder… · Rescan Library · Cancel Scan · Back Up Catalog Database… · Restore Catalog Database… · Close Window (⌘W) |
| Edit | Undo, Redo, Cut, Copy, Paste, Select All · Find in Library (⌘F) |
| View | Show Library (⌘1) · Show Review (⌘2) · Show Activity · Tiles · List · Sort By ▸ · Appearance ▸ Light / Dark · Enter Full Screen |
| Album | Show in Finder · Edit My Tags… · Audio Analysis… |
| Window | Minimize, Zoom, Bring All to Front (macOS adds the window list) |
| Help | Keyboard Shortcuts (macOS adds the Help search field) |

A label ending in "…" asks for more input before anything happens. A
command opens a screen or a section (Settings, Backup & restore, the
inspector's My Tags or Audio analysis); none of them writes a file by being
chosen: the writes stay behind that screen's own buttons. Undo through
Select All, Close Window, Minimize, Zoom, Full Screen, Services and Hide are
the platform's own items, so native text editing and window handling stay as
they were.

What each command does and when it is available is decided once, in the
frontend's command registry (`frontend/src/lib/commands.ts`). The menu item,
the window's own control (the titlebar's Rescan and View menu, the Library
filter bar's search, sort and Tiles/List, the sidebar, the inspector's Show
in Finder) and the keyboard shortcut all run the same registry command under
the same availability rule. Album commands act on the album open in the
inspector, from Library or from a Review item; they are dimmed with no album
open, for an album whose folder is missing (Show in Finder, Edit My Tags…,
Audio Analysis…), and every command is dimmed while a modal dialog is open.
Rescan, Choose Music Folder…, Back Up… and Restore… are dimmed while a scan
runs; Cancel Scan only then.

### How a menu command reaches the page (no IPC)

The webview still has no IPC, no capabilities and the same CSP and
navigation lock. The two directions are:

- **Shell → page.** Choosing an item evaluates, in the main window, a fixed
  script `window.dispatchEvent(new CustomEvent("musaic:command", { detail: "<id>" }))`
  (`menu::dispatch_script`). The id is the `&'static str` of an entry in the
  shell's allow-list (`menu::COMMANDS`), looked up from the menu item's own
  id; nothing from the page, a URL or the event text is ever interpolated.
  The script is evaluated only while the window shows the backend origin.
  A script the shell evaluates is not subject to the page's CSP, so the CSP
  is unchanged, and the event can do nothing the page's own code could not:
  the page's command host (`frontend/src/hooks/useCommands.ts`) ignores ids
  that are not in its registry and runs a known one only when it is
  available.
- **Page → shell.** The page posts each command's `enabled` (and, for Tiles /
  List, the sort orders and Light / Dark, `checked`) to
  `POST /__musaic/menu-state`, which needs the launch cookie, the exact Host
  and the app's own Origin, like the reveal endpoints. The shell keeps only
  ids in its allow-list and only sets enabled / checked on its own items.
  Until the page has posted, every command item is dimmed.

The marker (`window.__musaicDesktop.nativeMenu`) says whether a menu bar
exists. Windows and Linux have no menu bar: there every command stays in the
window (the sidebar for Library, Review and Settings; the titlebar's View
menu for Show Activity, Appearance and Keyboard Shortcuts; the filter bar for
search, sort and Tiles/List; the inspector for Show in Finder), and the menu
keys work with Ctrl (Ctrl-, Settings, Ctrl-F Find in Library, Ctrl-1 / Ctrl-2
Library / Review), handled by the page (`useGlobalShortcuts`). In a plain
browser those keys stay the browser's. Everywhere: `/` focuses the Library
search, `,` opens Settings, `?` shows Keyboard Shortcuts, F6 moves focus
between the album list and the inspector, Esc closes the inspector.

Not yet in the menu bar (#334 lists them): Musaic Help, Album → Review
Corrections… and Add Cover… / Change Cover….

## Data locations

| What | Where |
|---|---|
| Database, settings (including the theme and tile size), logs, thumbnails, trash, window state, the record of each multi-disc conversion (`conversions/`, one JSON file each; kept outside the database so a restore does not lose it) | the config dir: `--config-dir`, else `MUSAIC_CONFIG_DIR`, else `~/.musaic` (Windows: `%USERPROFILE%\.musaic`) |
| API keys | the OS keyring, service name `musaic` |
| Agent pairing secrets (one per pairing, plus an index of the active pairing ids; no key is ever written to a file) | the OS keyring, service name `musaic-agent`, account `<config dir id>:<pairing id>` (a development run with in-memory secrets writes `<config>/agent-dev-credentials/<pairing id>` at mode 0600 instead) |
| Where an agent's CLI finds the running app (`{instanceId, port, pid, ...}`; holds no credential; written at start, removed at teardown only if it still names this instance) | `<config>/agent-instance.json`, mode 0600 (Windows: an access control list for the current user alone, not inherited) |
| The before-image of every change an agent made (prior tag values, names, and byte-for-byte copies of the cover sidecars and embedded pictures a cover change replaced); kept 90 days, then pruned at start-up | `<config>/agent-changes/<change id>/` |
| Web view storage (HTTP cache, and a copy of the theme and tile size used only for the first paint; the web view's address changes on every launch, so nothing there is kept from one launch to the next) | managed by the platform's webview, outside the config dir: macOS `~/Library/WebKit/io.get2know.musaic` and `~/Library/Caches/io.get2know.musaic`; Windows `%LOCALAPPDATA%\io.get2know.musaic\`; Linux `~/.local/share/io.get2know.musaic/` |

A launch from Finder, the Start menu or a desktop launcher has no
`MUSAIC_CONFIG_DIR`, so it opens `~/.musaic`. A database written by the Python
version is backed up to `library.v16.before-rust.db` and then migrated in place.

## Agent bridge

The shell also serves a second route family on the same loopback port for an assistant's
CLI: `/__musaic/agent/v1/*` (docs/agents.md, SECURITY.md "Agent access"). Its guard
(`musaic-api/src/agent_guard.rs`) runs before the launch cookie and the Host / Origin
check, which stay exactly as they were for every other path; `src/gate.rs` passes the
prefix straight through. The server is started with connection info (peer and local
address) so the guard can check both. `musaic-api`'s loopback server writes
`agent-instance.json` once the port is bound and removes it when the server stops.
The webview manages pairings and decides approvals through the ordinary `/api/agent/*`
routes (launch cookie required), refreshed by the `AgentStateChanged` bus event.

Research requests and the operations behind MCP Tasks are stored in the same library database as the albums. Musaic has no built-in model and starts no agent: the desktop resolves an album locally when it can, and an assistant you added can claim unresolved metadata or cover research while the bridge is running. A bulk Fix all can send what the resolver cannot settle as one batch of requests. Review groups the results by album request. Approving a proposal checks the current files and saves a before-image for Undo.

The quit dialog lists external claims separately from local work. Closing the app leaves those claims to expire. Reopening with the same library preserves an unexpired claim. Restoring a database or switching libraries invalidates existing claims and Task handles.

## Options

`--config-dir DIR` / `MUSAIC_CONFIG_DIR`, `--port N` / `MUSAIC_PORT` (default
0), `--network live|offline|replay` with `--replay-dir`, `--frontend-dir DIR` /
`MUSAIC_FRONTEND_DIR` (else the bundle's `frontend/` resource, else in debug
builds the repository's `frontend/dist`), `--dev-memory-secrets`, `--hidden`,
and `--selftest-report FILE` (used by the boot test; needs dev secrets). The
bind host is always `127.0.0.1`.

## Running and testing

```bash
(cd frontend && npm ci && npm run build)      # the crate's build copies frontend/dist
cd rust
cargo test -p musaic-desktop                  # unit tests, gate, second instance, server log
cargo test -p musaic-desktop --test boot -- --ignored --nocapture
#   hidden-window boot test: start-up, serving, focus request, quit routes,
#   the menu bar (macOS: its layout, the page's menu state, a command
#   delivered to the page), teardown
cargo run -p musaic-desktop -- --config-dir "$(mktemp -d)/config" --dev-memory-secrets --network offline
```

Never point a development run at `~/.musaic` or at a real music library.

## Packaging

One-time tools (from crates.io):

```bash
cargo install tauri-cli --version 2.12.4 --locked
cargo install cargo-about --version 0.9.3 --locked --features cli
```

On a Mac, from the repository root:

```bash
scripts/package_macos.sh
```

It builds the UI (`npm ci` only when `frontend/node_modules` is missing),
writes `rust/target/desktop-notices/NOTICES.txt` with
`scripts/desktop_notices.py` (third-party licences: Rust crates via
cargo-about, the npm packages bundled into the UI, the bundled fonts, SQLite and
the Chromaprint notice), then runs, in `rust/crates/musaic-desktop/`,
`CI=true cargo tauri build --ci --bundles app,dmg --config tauri.bundle.conf.json`
with every Apple / Tauri signing variable unset. Output:

```
rust/target/release/bundle/macos/Musaic.app
rust/target/release/bundle/dmg/Musaic_<version>_<arch>.dmg
```

Notes:

- `tauri.bundle.conf.json` is merged over `tauri.conf.json` for bundling only:
  it adds `NOTICES.txt` to the resources, so plain `cargo build` / `cargo test`
  do not need the generated file.
- `tauri.bundle.conf.json` also names the agent CLI as an `externalBin`
  (`binaries/musaic-cli`). `scripts/stage_cli_sidecar.sh <triple>` builds `musaic-cli`
  for the target and copies it to
  `rust/crates/musaic-desktop/binaries/musaic-cli-<triple>[.exe]` (gitignored) before
  `cargo tauri build`; `scripts/package_macos.sh` and the release workflow both call it.
  Only a bundling build needs the file, so plain `cargo build` / `cargo test` stay green
  without it.
- `scripts/check_bundled_cli.sh <bundle-dir> <bundles> [<triple>]` then checks each
  installer for the CLI at the place docs/agents.md names, and runs it (`--version`, and
  `doctor` against a scratch config dir, which must report the descriptor absent):
  - app, dmg: `Musaic.app/Contents/MacOS/musaic-cli` (the dmg is mounted read-only);
  - deb: `usr/bin/musaic-cli` in the package data (`dpkg-deb -x`);
  - AppImage: `usr/bin/musaic-cli` in the extracted image, and, run from the mounted
    image (`--appimage-mount`, needs libfuse2), `musaic-cli setup --dry-run` must
    refuse with the copy-it-out remedy;
  - NSIS: a silent install (`/S /D=<scratch>`) must put `musaic-cli.exe` next to
    `musaic-desktop.exe` (the main binary keeps the crate's bin name; the default
    folder is `%LOCALAPPDATA%\Musaic`), then a silent uninstall.

  A cross-built target (the Intel Mac build on Apple Silicon) gets an architecture
  check (`lipo`) instead of a run. `scripts/package_macos.sh` ends with it.
- `CI=true` makes the dmg step skip its Finder AppleScript (no window layout,
  no Automation prompt).
- `cargo tauri build` rewrites the crate's `tauri-build` line in `Cargo.toml`;
  the script restores the file afterwards.
- The bundle is not signed or notarised (`signingIdentity: null`); the binary
  carries only the linker's ad-hoc signature, so macOS warns about a downloaded
  copy (see the README for how to open it).

Linux (AppImage, deb) and Windows (NSIS `-setup.exe`) bundles are built by
`.github/workflows/release.yml` on the platform's own runner. Its build steps (toolchain,
packaging tools, UI, notices, the CLI sidecar, `cargo tauri build --no-sign`, then
`check_bundled_cli.sh`) live in the composite action `.github/actions/desktop-bundle`,
which `.github/workflows/bundle-check.yml` also runs (macOS arm64, Linux x64, Windows
x64) on pull requests that touch the desktop crate, the CLI, the packaging scripts or the
workflows, and by hand; it uploads nothing. A release therefore cannot ship an installer
whose CLI is missing or does not run. The Windows installer is unsigned and is built
from the release after v0.2.0-rc.1 on; MSI is not used because its bundler rejects
pre-release versions. Linux needs the WebKitGTK 4.1,
GTK 3 and libsoup 3 development packages (listed in the README); Windows needs
the WebView2 runtime, which Windows 11 includes.

## What has been tested

- **macOS, Apple Silicon:** built, packaged and run: the hidden-window boot test
  against the bundled binary (start-up, serving, the second-launch focus
  request, the quit routes, clean teardown). The clicks and keystrokes of the
  quit routes and the native dialogs in a shown window are not covered by the
  automated tests.
- **macOS Intel, Linux:** built by CI only, not run by a person yet. Windows:
  the automated tests run and are required in CI, but nobody has run the app or
  the installer by hand. Server-sent events and streamed responses in WebView2 and WebKitGTK, the
  Explorer / file-manager reveal paths, the Secret Service keyring and the
  installers themselves are untested there.
- Not tested anywhere: the real OS keyring from the automated tests (they use
  in-memory secrets by design), and a download of the unsigned app through a
  browser (Gatekeeper / SmartScreen prompts).
