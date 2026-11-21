# Contract: `agent-instance.json`

The descriptor lives at `<config dir>/agent-instance.json`. It is separate from the
desktop's `desktop-instance.json`, which holds a credential (`focus`) and must never be
read by the CLI.

```json
{
  "formatVersion": 1,
  "instanceId": "i_3f9a0c1d2e4b5a69",
  "appVersion": "0.2.0-rc.1",
  "protocolProfiles": ["mcp-2026-07-28", "mcp-2025-11-25"],
  "contractVersion": "1",
  "host": "127.0.0.1",
  "port": 53817,
  "pid": 4242
}
```

## Writer (the app)

The desktop app writes the descriptor after its loopback server is bound, at
`musaic-desktop/src/lib.rs` `boot` next to `lock.publish`. The standalone `musaic-api`
writes it after `local_addr`. The write goes like this:

1. Create `agent-instance.json.tmp-<pid>`: mode 0600 on Unix. On Windows, before anything
   is written to it, replace its DACL with one ACE granting the current user full control
   and turn inheritance off (`icacls <dir> /restore <aclfile>` with the SDDL
   `D:P(A;;FA;;;<SID>)`, research R13), then
   read the DACL back and check it as the reader does. If that fails, the temp file is
   removed and no descriptor is written.
2. Write the body and fsync the file.
3. Rename it into place.
4. fsync the directory, on Unix.

The app removes the file in `teardown` and on the standalone server's exit. It removes the
file only if the file still holds its own `instanceId`.

## Reader (the CLI)

The CLI refuses the descriptor, and maps each refusal to an error code, in this order:

| Condition | Error |
|---|---|
| File absent | `APP_NOT_RUNNING` |
| Path is a symlink (lstat) or not a regular file | `DESCRIPTOR_INVALID` |
| Unix: owner uid ≠ current euid (or the euid cannot be learned), or mode has group/other bits | `DESCRIPTOR_INVALID` |
| Windows: a reparse point | `DESCRIPTOR_INVALID` |
| Windows: the current user's SID or the file's DACL cannot be learned; the DACL is null or inherits from the folder (not `D:P…`); or an allow ACE names anyone but the current user or SYSTEM (deny ACEs are allowed) | `DESCRIPTOR_INVALID` |
| Larger than 4 KiB, not JSON, unknown fields, `formatVersion` ≠ 1 | `DESCRIPTOR_INVALID` |
| `host` ≠ `127.0.0.1`, or `port` outside 1..65535 | `DESCRIPTOR_INVALID` |
| Connection refused, or `hello` times out (2 s) | `APP_NOT_RUNNING` (stale) |
| `hello.instanceId` ≠ descriptor `instanceId` | `APP_NOT_RUNNING` (stale) |
| `serverProof` fails during exchange | `INSTANCE_MISMATCH` |

The PID is never used to decide anything. `doctor` only reports whether a process with
that PID exists.
