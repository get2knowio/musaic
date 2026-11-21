# Contract: `musaic-cli`

Global options:

- `--config-dir <dir>`: the default resolves the same way as the app (`--config-dir`, then
  `MUSAIC_CONFIG_DIR`, then `~/.musaic`).
- `--pairing <id>`: the default is the only active pairing that has a credential;
  otherwise the command fails with `NOT_PAIRED` and lists the candidates.
- `--dev-credentials`: accepted only when the config dir is under the system temp
  directory or under a directory named `work`.

Secrets never appear in arguments.

| Command | Output (stdout) | Exit |
|---|---|---|
| `mcp` | MCP JSON-RPC only (both profiles, R1). Diagnostics go to stderr. | 0 on clean EOF |
| `tools` | catalog JSON | 0 |
| `call <cli-name> [--input <json> \| --input-file <path> \| stdin]` | `{ok, result \| error}` | 0 ok; 2 tool error; 3 transport error (`APP_NOT_RUNNING` …); 64 usage |
| `<cli-name> [--input …]` | alias for `call` | as above |
| `doctor` | `{executable, version, configDir, descriptor{status, instanceId?, port?, pidAlive?}, pairing{id?, status}, scopes[], transport:"stdio", profiles[], extensions[]}`. It prints no secret and makes no tool call other than `capabilities`. | 0 healthy, 1 problems found |
| `setup [--dry-run] [--host claude-desktop\|claude-code\|all]` | plan or result JSON: `{hosts[{id, path, detected, action: add\|update\|none\|refuse, reason?}], files[{path, backup?}], skills: []}` | 0, or 1 if any host refused |
| `uninstall [--dry-run] [--host …]` | the same shape | 0 / 1 |
| `catalog --check \| --write [--dir docs]` | diff summary | 0 matches, 1 drift |

Every non-`mcp` command prints exactly one JSON document on stdout. Failure output has the
shape `{ok:false, error:{code, message, remedy?}}`. The `APP_NOT_RUNNING` remedy reads:
"Start Musaic, then try again. The CLI never starts Musaic itself."

## setup file rules (R11)

These rules are mandatory:

- Back up a file before changing it.
- Write atomically: a temp file in the same directory, then a rename.
- Preserve unrelated keys and key order (serde_json `preserve_order`).
- Refuse a file that is not valid JSON, and refuse a `musaic` entry that is not ours.
- Roll back every file if any write fails.
- `uninstall` removes only owned entries.
