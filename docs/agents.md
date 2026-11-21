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
  albums. Each returns a handle to poll, and can be stopped: a scan through its own
  cancel, health checks before the next album a check looks at, an analysis before the
  next file it measures. A file being written is always finished first. A health check
  stopped part-way keeps the findings it wrote, resolves none, and is run again later by
  the background sweep. An analysis stopped part-way keeps the tags of the files it wrote
  (ReplayGain then writes no album gain, which needs every track), and the album is
  rescanned as after a finished one.

An assistant cannot: run SQL or a shell, read a file path, read or write keys, delete or
move anything outside the rename a proposal asks for, restore or download the database,
change settings, change its own permissions or approve its own requests, or undo a change
(only you can, below). Tool results
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

Tested hosts: Claude Code 2.1.330 on macOS, with both profiles (it negotiates
2026-07-28 by default, and 2025-11-25 with `MCP_PROTOCOL_NEGOTIATION=legacy`); see
[the recorded connection](../specs/003-agent-bridge/evidence/host-connection.md). Claude
Desktop has not been connected to a built Musaic yet.

The CLI never starts Musaic. If the app is not running, a tool call answers
`APP_NOT_RUNNING` ("Start Musaic, then try again"). With a non-default config folder pass
`--config-dir` (the entry `setup` writes includes it when you gave one).

## Where the CLI is

| Platform | Location |
|---|---|
| macOS | `Musaic.app/Contents/MacOS/musaic-cli` |
| Windows | `musaic-cli.exe` in the install folder, next to the app's `musaic-desktop.exe` (`%LOCALAPPDATA%\Musaic` unless you chose another folder) |
| Linux, deb | `/usr/bin/musaic-cli` |
| Linux, AppImage | `usr/bin/musaic-cli` inside the image, which is not a stable path: `setup` refuses it. Copy it out and run setup from there, for example: `./Musaic_<version>_amd64.AppImage --appimage-extract`, then copy `squashfs-root/usr/bin/musaic-cli` to `~/.local/bin` |

Every installer the release builds is checked for these locations before it is published
(`scripts/check_bundled_cli.sh`): the CLI must be there and answer `--version` and
`doctor`.

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
Once the change finishes, Musaic also notes what it left: each file's name now, the
values of the same fields, and fingerprints of the cover art it wrote.

## Undoing an assistant's change

In Settings → Assistants, open **Recent changes** under a pairing (revoked ones too) and
press **Undo** beside a change. After you confirm ("Put back the tags, names and cover
this assistant changed on <Album>?"), Musaic renames the files and folder back, puts
every tag field the change touched back to its old value (several values and the other
spellings of a removed field included), and restores the cover art byte for byte: the
`cover.*` files and every picture embedded in each track. The library, the checks and
any open album update as after any edit.

Undo refuses, and changes nothing, when:

- anything the change wrote has changed since: a later edit of one of the same fields, a
  new cover, a renamed or moved file or folder. The message lists the files and fields;
  your later edits are never overwritten;
- the album is missing from disk (rescan once its folder is back);
- a restore or other work is running (try again when it ends);
- the change was already undone, wrote nothing, or is itself an undo;
- its record is older than 90 days (pruned) or was made before Undo existed.

An undo is recorded like a change (what it overwrote is kept), and shows in the pairing's
activity as `undo`. Only you can undo: no assistant tool does it. Undo covers changes an
assistant made; your own edits in Musaic cannot be undone this way yet.

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

The descriptor (`<config>/agent-instance.json`) is `invalid` when someone else could read
it. On Unix the app writes it with mode 0600. On Windows the app gives it an access
control list that grants only you and does not inherit from the folder, using the
system's `icacls.exe`; the CLI refuses one that inherits, grants anyone else (SYSTEM
aside), or whose list it cannot read. Quitting and reopening Musaic writes a fresh one.
