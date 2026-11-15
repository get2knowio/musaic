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
   loopback server, write the instance file, and navigate the window to the
   one-time launch URL (see "Launch cookie").
5. Write the same rotating, secret-masking `<config>/server.log` as the server
   binary; the level comes from `MUSAIC_LOG_LEVEL`.

### Teardown

Closing the window, Cmd-Q, `AppHandle::exit`, SIGTERM / SIGINT / SIGHUP on Unix
and Ctrl-C on Windows all end in Tauri's exit event: every SSE stream is ended,
the server drains for up to 3 seconds (then remaining connections are aborted),
the application shuts down (a running library scan cancelled, so it saves nothing
and starts nothing, and waited for; fingerprint pool joined, job worker stopped) and
the instance file is removed. No child processes are ever spawned.

### Quitting while work runs

Quitting while something is in progress (a scan, an apply, an analysis, a
restore, a check run, a bulk quick fix, a job, the fingerprint pool, any other
write) first asks "Quit anyway?" with **Quit** / **Keep running**; an idle app
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

On macOS the default app menu's Quit item is replaced by Musaic's own "Quit
Musaic" item (Cmd-Q), because the predefined item quits without giving the app
a chance to ask. Dock → Quit and logout go through a path the shell cannot hold
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
  Plex fonts are bundled with the UI. A job's suggested remote cover is shown
  through the same-origin route `GET /api/ai/jobs/{job_id}/cover-preview`; the
  server fetches the stored URL under the cover-download rules below.
- **No IPC.** No capabilities, no `#[tauri::command]`s, `withGlobalTauri: false`.
  The dialog, opener and window-state plugins are registered but called from
  Rust only; the opener's own link-click script is switched off.
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

There is no updater, no deep-link handler and no tray icon.

## Data locations

| What | Where |
|---|---|
| Database, settings, logs, thumbnails, trash, window state | the config dir: `--config-dir`, else `MUSAIC_CONFIG_DIR`, else `~/.musaic` (Windows: `%USERPROFILE%\.musaic`) |
| API keys | the OS keyring, service name `musaic` |
| Web view storage (UI preferences, HTTP cache) | managed by the platform's webview, outside the config dir: macOS `~/Library/WebKit/io.get2know.musaic` and `~/Library/Caches/io.get2know.musaic`; Windows `%LOCALAPPDATA%\io.get2know.musaic\`; Linux `~/.local/share/io.get2know.musaic/` |

A launch from Finder, the Start menu or a desktop launcher has no
`MUSAIC_CONFIG_DIR`, so it opens `~/.musaic`. A database written by the Python
version is backed up to `library.v16.before-rust.db` and then migrated in place.

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
#   hidden-window boot test: start-up, serving, focus request, quit routes, teardown
cargo run -p musaic-desktop -- --config-dir "$(mktemp -d)/config" --dev-memory-secrets --network offline
```

Never point a development run at `~/.musaic` or at a real music library.

## Packaging

One-time tools (from crates.io):

```bash
cargo install tauri-cli --version "^2" --locked          # CI pins 2.12.4
cargo install cargo-about --locked --features cli        # CI pins 0.9.3
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
- `CI=true` makes the dmg step skip its Finder AppleScript (no window layout,
  no Automation prompt).
- `cargo tauri build` rewrites the crate's `tauri-build` line in `Cargo.toml`;
  the script restores the file afterwards.
- The bundle is not signed or notarised (`signingIdentity: null`); the binary
  carries only the linker's ad-hoc signature, so macOS warns about a downloaded
  copy (see the README for how to open it).

Windows (NSIS installer) and Linux (AppImage, deb) bundles are built by
`.github/workflows/release.yml` on each platform's own runner, with the same
notices step and `cargo tauri build --no-sign`. Linux needs the WebKitGTK 4.1,
GTK 3 and libsoup 3 development packages (listed in the README); Windows needs
the WebView2 runtime, which Windows 11 includes.

## What has been tested

- **macOS, Apple Silicon:** built, packaged and run: the hidden-window boot test
  against the bundled binary (start-up, serving, the second-launch focus
  request, the quit routes, clean teardown). The clicks and keystrokes of the
  quit routes and the native dialogs in a shown window are not covered by the
  automated tests.
- **macOS Intel, Windows, Linux:** built by CI only, not run by a person yet.
  Server-sent events and streamed responses in WebView2 and WebKitGTK, the
  Explorer / file-manager reveal paths, the Secret Service keyring and the
  installers themselves are untested there.
- Not tested anywhere: the real OS keyring from the automated tests (they use
  in-memory secrets by design), and a download of the unsigned app through a
  browser (Gatekeeper / SmartScreen prompts).
