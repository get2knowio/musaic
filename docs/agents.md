# Assistants and the agent bridge

Musaic can let an assistant you choose read your library and suggest fixes, and apply them
only when you approve. This page says what an assistant can and cannot do, how to connect
one, and what to know about it. The tools themselves are listed in
[agent-tools.md](agent-tools.md) (generated from the code; `agent-tools.json` is the same
catalog for programs).

## What an assistant can and cannot do

Twelve tools, in three groups:

- **Read** (no approval ever needed, no network, nothing written): capabilities, list the
  albums, read one album (its tags, tracks and a revision), the resolution Musaic already
  stored for it, the open findings, and the state of an operation it started.
- **Propose and apply**: preview a change to one album's tags, cover or file names against the
  album's current revision, then apply the preview. Applying runs now or waits for you,
  depending on the pairing's policy (below).
- **Start work**: a library scan, health checks, or an analysis (tempo, ReplayGain) of some
  albums. Each returns a handle to poll, and can be stopped between items.

An assistant cannot: run SQL or a shell, read a file path, read or write keys, delete or
move anything outside the rename a proposal asks for, restore or download the database,
change settings, or change its own permissions or approve its own requests. Tool results
carry ids and bare file and folder names, never a filesystem path.

## Pairing

1. In Musaic, open **Settings, Agents** and pair an assistant: give it a name and tick what it
   may ever do (reading and proposing are ticked by default). Musaic makes a secret, keeps
   it in your operating system's keyring and never shows it.
2. Connect the host:

   ```bash
   musaic-cli setup --dry-run     # what would change, nothing written
   musaic-cli setup               # Claude Desktop and Claude Code, with a backup of each file
   musaic-cli doctor              # is the app running, is the pairing active, which profile
   ```

   `setup` writes one entry named `musaic` into the host's `mcpServers`, using the
   absolute path of `musaic-cli`. It backs each file up first
   (`<file>.musaic-backup-<time>`), keeps every other entry exactly as it was, refuses a file
   that is not valid JSON or a `musaic` entry it did not write, and puts every file back if
   any step fails. `musaic-cli uninstall` removes only an entry it wrote.

3. For any other MCP host, add this by hand to its server list (the absolute path is what
   `setup` prints as `manualEntry`):

   ```json
   {"mcpServers": {"musaic": {"type": "stdio", "command": "/absolute/path/to/musaic-cli", "args": ["mcp"], "env": {}}}}
   ```

   Always use the absolute path: a host started from a launcher or the Dock does not have
   your shell's `PATH`.

The CLI never starts Musaic. If the app is not running, a tool call answers
`APP_NOT_RUNNING` ("Start Musaic, then try again"). With a non-default config folder pass
`--config-dir` (the entry `setup` writes includes it when you gave one).

## Where the CLI is

| Platform | Location |
|---|---|
| macOS | `Musaic.app/Contents/MacOS/musaic-cli` |
| Windows | next to `Musaic.exe` |
| Linux, deb | `/usr/bin/musaic-cli` |
| Linux, AppImage | inside the image, which is not a stable path: `setup` refuses it. Copy `musaic-cli` out (for example to `~/.local/bin`) and run setup from there |

On macOS the Keychain may ask once whether `musaic-cli` may read the pairing's entry.

## Permissions and approvals

Each pairing has a scope (the permissions it may ever use: `library.read`,
`proposal.submit`, `compute.scan`, `compute.analyze`, `tags.write`, `covers.write`,
`files.rename`) and, inside it, a policy per permission: **Allow**, **Ask** or **Refuse**.
A permission outside the scope is refused whatever the policy says. Defaults: reading and
proposing Allow; scanning, analysing and every write Ask. When an action needs several
permissions the strictest wins. Reading and proposing can only be Allow or Refuse.

An action you set to Ask appears as a card in the **Review inbox**: what the assistant wants
to do, the change in words, and how long is left. Approve or Deny. Opening a card does
nothing. Only you can decide; no tool can.

- A card expires after **30 minutes** (a stale card cannot be approved). Restarting Musaic
  expires every pending card (the assistant must ask again).
- Approving does not apply a stored plan: Musaic re-derives the change from the files as
  they are and refuses if anything moved (the album's revision, its files, the pairing,
  the proposal's expiry). A refusal after your approval changes no file.
- Denied, expired, refused, stale and cancelled requests change no file.
- Reads keep working while a request waits.

## Before-images

Before an assistant's change writes anything, Musaic records the prior value (or absence) of
every tag field it touches, the prior file and folder names, and, for a cover, a copy of
all the prior cover art (the `cover.*` sidecar files and every picture embedded in each
track), under `<config>/agent-changes/<id>/` (and in the database). The record is
kept for 90 days. If it cannot be written the change is refused and nothing is written.
There is no Undo button yet; the record is what an undo will use.

## Tokens, restarts, revoking

The CLI proves it holds the pairing's secret and gets a **15-minute** token, renewed a
minute before it ends. Restarting Musaic invalidates every token (the pairing survives; the
CLI finds the new instance by itself). **Revoking** a pairing stops its tokens at once, deletes
its keyring entry and denies its waiting cards. After five failed attempts within a minute
the app refuses further attempts for a minute, then longer.

To rotate a credential, revoke the pairing and create a new one.

## What a pairing really authenticates

A pairing proves that the caller holds the secret in your keyring. Any process running as
you can read that entry, so a pairing identifies **whoever holds the local credential, not
a particular assistant program or model**. Keep the scope small, leave writes on Ask, and
revoke pairings you no longer use.

## Troubleshooting

`musaic-cli doctor` prints one JSON document: the executable and version, the descriptor
(`ok`, `absent`, `stale` or `invalid`), the pairing's status, its scopes, the transport and
the MCP profiles the server speaks (`mcp-2026-07-28`, `mcp-2025-11-25`) and the extensions
(none). It prints no secret and makes no tool call but `capabilities`. Exit code 0 means
healthy, 1 means a problem was found. For the other commands: 0 success, 2 a tool error,
3 a transport error (app not running and similar), 64 usage.
