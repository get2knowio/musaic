# Research: One-click connection to Claude Desktop

Sources read on 2026-11-24: the MCP bundle manifest specification (version 0.3) and its JSON schema, the bundle tool's README and command reference, the Claude help article on local MCP servers in Claude Desktop, the announcement of desktop extensions. Code anchors verified at `main` `bf45aa60`.

## What the code has today

| Fact | Where |
| --- | --- |
| Claude Desktop's configuration file per platform; the `musaic` entry (`{"type":"stdio","command":<abs musaic-cli>,"args":["mcp"],"env":{}}`, `--config-dir` appended only when given); "ours" means the command's file name is `musaic-cli`/`musaic-cli.exe` and `args[0] == "mcp"`; a mounted application image is refused | `rust/crates/musaic-cli/src/setup.rs` (`HostPaths::under` :34-52, `entry_json`, `is_ours`, `detected`, `in_appimage`) |
| The CLI picks its pairing at run time from the keyring; no pairing id or secret is in the entry | `rust/crates/musaic-cli/src/client.rs:134-160` |
| The app finds the CLI as the sibling of its own executable, falling back to the bare name | `rust/crates/musaic-app/src/ops/agent.rs:454-478` (`connect_hint`) |
| Shell requests the page can make without IPC: dialogs, folder picker, reveal; each behind the launch cookie, exact Host, same Origin, POST; none takes a path | `rust/crates/musaic-desktop/src/gate.rs` (`Hooks`, `guard`); `TauriHooks` in `src/lib.rs` |
| No save-file dialog and no open-with-default-application call exist; the opener plugin is registered and used for reveal and external links | `rust/crates/musaic-desktop/src/lib.rs` |
| Downloads from the backend origin are allowed; the database backup is saved that way | `musaic-desktop/src/lib.rs` (`on_download`), `nav.rs` (`download_allowed`), `frontend/src/pages/SettingsBackup.tsx:92` |
| The skill is embedded: nine files, digests and sizes in `skill.lock.json`, two of them generated at build time | `rust/crates/musaic-commands/src/skill.rs` (`bundle()`), `skills/musaic-work-delegation/` |
| A connected host's name, version and extensions are stored per pairing and announced as `AgentStateChanged(hosts)` | `rust/crates/musaic-app/src/agent/hosts.rs`, `musaic-db` `agent_hosts` |
| One workspace version; the descriptor carries `app_version`; nothing compares it with the CLI's | `rust/Cargo.toml:7`, `musaic-commands/src/descriptor.rs:20` |
| No zip writer in the dependency tree; `flate2`, `crc32fast` and `tempfile` are present | `rust/Cargo.lock` |
| The Settings section is titled "Assistants" | `frontend/src/pages/Settings.tsx:96`, `SettingsAgents.tsx` |

## Decisions

**D1. Two shapes, one constant.** The documentation neither permits nor forbids a command outside the bundle directory. *Chosen*: implement both shapes; `shape_for(platform)` picks one; the default is the installed-program shape, changed only by a recorded observation. *Rejected*: self-contained only (a second copy that drifts, on every platform, when it may not be needed); installed only (the feature would hang on an undocumented behaviour).

Manifest, installed-program shape: `server.type = "binary"`, `entry_point` and `mcp_config.command` both the absolute program location, `args = ["mcp"]` plus `--config-dir <dir>` when the app's configuration directory is not the default. Self-contained: the program at `server/musaic-cli` (`.exe` on Windows) with the executable bit in the zip entry, `entry_point = "server/musaic-cli"`, `command = "${__dirname}/server/musaic-cli"`. Both: `manifest_version "0.3"`, `name "musaic"`, `display_name "Musaic"`, `version` the app version, a constant description and author, `icon "icon.png"` (the 128-pixel application icon, embedded with `include_bytes!`), `tools_generated: true`, `compatibility.platforms` holding the current platform, no `user_config`, no `env`.

**D2. Where the generator lives.** *Chosen*: `musaic-app`, because the operation must be on the operations surface and needs the app's executable location, version and configuration directory. It reads no database. *Rejected*: `musaic-cli` (a subcommand that opens Claude would be a second, scriptable trigger, against FR-004); `musaic-desktop` (would put logic in a face).

**D3. One definition of the host's file and of ownership.** *Chosen*: pure functions in `musaic-commands` (the only crate both sides already depend on; it stays free of database, app, axum and tokio). *Rejected*: copying the rule into the app (two rules that can disagree about whose entry it is).

**D4. Verifying the program location.** A regular file beside the running executable, canonicalised. Refused when the location will not last. Under `/Volumes/` (macOS) the mount decides: read-only or unreadable is a disk image and refused; writable is an external drive and accepted (`statvfs`, behind the injected `HostEnv`). The fallback to the bare name `musaic-cli` stays for `connect_hint` (a hint for a terminal) but is never used for a bundle.

**D5. The hand-off.** *Chosen*: a shell request in the gate, like reveal: the page sends an empty POST, the shell calls one operation and opens the returned path. The path never reaches the page. *Rejected*: an `/api` route that returns a path (tools and routes return no filesystem path); a Tauri command (the web view has no IPC, and must not gain it).

**D6. Lifetime of the file.** It is not documented whether the host copies the file when it is opened or when Install is clicked, so the file must outlive the dialog. *Chosen*: 60 minutes, then removed by the periodic tick; all leftovers removed at startup; removed at once when the open fails.

**D7. Repeated clicks.** An in-flight flag on the operation; the dialog also disables its button while a request is pending.

**D8. Saving the skill.** *Chosen*: a download, as the backup is saved. *Rejected*: a new save-file dialog in the shell (a new shell request and hook for no gain).

Zip details: entries in the bundle's order, the fixed DOS timestamp 1980-01-01, deflate, no extra fields, so the same app version gives the same bytes.

**D9. Versions.** The observation the CLI already sends gains an optional `cliVersion`. It is kept in memory: it describes a live connection and must not need a schema change. "Older" is decided by comparing release versions including the pre-release part; when the two cannot be compared, they are shown as different without the word "older".

**D10. Refreshing the readiness report.** No event reports a change in Claude's files. The query is fetched on opening the dialog and after a hand-off, not kept fresh by the bus.

**D11. Detecting Claude Desktop.** macOS: `Claude.app` in `/Applications` or `~/Applications`, or the configuration directory exists. Windows: the configuration directory under `%APPDATA%` exists, or the per-user install directory under `%LOCALAPPDATA%` (`AnthropicClaude`) exists. A miss costs a message and the other route; the dialog offers "I have Claude Desktop installed" nowhere, to keep one path.

**D12. Detecting the installed extension.** Observed on macOS (Claude Desktop 6.28899.6): `<Claude support directory>/Claude Extensions/local.mcpb.musaic.musaic`, for a manifest with author name "Musaic" and name "musaic". *Chosen*: a pure function in `musaic-commands` gives that folder per platform (Windows: the same relative folder under `%APPDATA%\Claude`, unverified); callers check that it is a directory. The manifest's author name and name are constants shared with that function, so they cannot drift apart. An unreadable parent is `Unknown`.

## Open observations (need a real host; recorded in `evidence/compatibility.md`)

Whether the installed-program shape runs; the unsigned-bundle wording; what a second, newer bundle does; where the extension is removed and unpacked; coexistence with a configuration entry; Windows; Cowork.
