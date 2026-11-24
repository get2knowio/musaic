# Data model: One-click connection to Claude Desktop

Nothing is stored. No table, column or setting is added; the schema stays v24.

## Types (typed results on `musaic_app::ops::agent`; no `serde_json` in `ops/`)

**`ClaudeDesktopStatus`**

| Field | Type | Meaning |
| --- | --- | --- |
| `platform` | `Darwin \| Win32 \| Linux` | the running platform |
| `bundle_supported` | bool | false on Linux |
| `claude_found` | bool | D11 |
| `config_entry` | `Absent \| Ours \| Foreign \| Unreadable` | the `musaic` entry in Claude's configuration file |
| `extension` | `Detected \| NotDetected \| Unknown` | the extension's folder in Claude Desktop's support directory (FR-034) |
| `cli` | `Found { command: Command } \| Missing \| Unstable { reason }` | the app's own program |
| `shape` | `Installed \| SelfContained` | for this platform |
| `route` | `Bundle \| ConfigFileOnly \| Blocked` | derived: `Bundle` only when supported, found, entry absent and the program found |
| `setup_command` | `Option<Command>` | the configuration-file route, with the program location filled in |
| `uninstall_command` | `Option<Command>` | present when `config_entry` is `Ours` |
| `app_version` | String | |

**`Command`**: `program: String`, `args: Vec<String>`. The one value both the report and the manifest are built from.

**`PreparedBundle`** (never rendered to a route): `path: PathBuf`, `command: Command`, `shape`.

**`SkillArchive`**: `file_name: String`, `bytes: Vec<u8>`, `version: String`.

**`OpError` uses**: `Conflict` (a hand-off is being prepared; or `route` is not `Bundle`, with the reason), `Unavailable`-style failure for a temporary directory that cannot be written.

## Live session facts (memory only)

`cli_version: Option<String>` per pairing, set by the session observation, cleared on revoke and at shutdown. `diagnostics` adds `cliVersion`, `appVersion`, `cliOlder: Option<bool>` per assistant.

## Files

| File | Where | Lifetime |
| --- | --- | --- |
| `Musaic.mcpb` | `<system temp>/musaic-connect-<random>/` (directory private to the user) | removed on failed open, after 60 minutes, and at startup |
| `musaic-work-delegation-<version>.zip` | wherever the download is saved | the listener's |

Bundle entries, in order: `manifest.json`, `icon.png`, and in the self-contained shape `server/musaic-cli[.exe]`.
Archive entries: `musaic-work-delegation/SKILL.md`, then `musaic-work-delegation/references/*.md` in the embedded bundle's order.
