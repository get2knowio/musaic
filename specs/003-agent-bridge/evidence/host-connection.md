# Evidence: a real MCP host connected to `musaic-cli`

Recorded for #348 (task T069). A real host, Claude Code, was connected to a built
`musaic-cli mcp` over stdio, against a standalone Musaic server on a scratch config folder
with a copy of the public synthetic fixture library. One headless session was run per MCP
protocol profile. This page holds the versions, the profile each run negotiated, the tools
the host received, the protocol transcript (trimmed, with secrets and local paths removed),
the `doctor` output, and what was not recorded.

## Setup

| | |
|---|---|
| Date | 2026-11-21 |
| OS | macOS 27.2 (build 26B123), arm64 |
| Host | Claude Code 2.1.330 (`claude -p`, headless), MCP runtime arm `v2` (its default) |
| Server | `musaic-api` 0.2.0-rc.1, debug build at commit 8d6b2b6e, `--verification-memory-secrets --network offline --port 0` |
| CLI | `musaic-cli` 0.2.0-rc.1, same build, `--config-dir <tmp>/config --dev-credentials mcp` |
| MCP SDK | `rmcp` 3.11.0 (from `rust/Cargo.lock`) |
| Library | a copy of `rust/testdata/synthetic-library/library` under `<tmp>/library`, scanned once (13 albums) |
| Pairing | created with `POST /api/agent/pairings`, scopes `library.read` and `proposal.submit` |

The host was given a temporary MCP config passed with `--mcp-config <file>
--strict-mcp-config`; no user or project host configuration was read or changed. Its one
server entry ran a small shell wrapper that started `musaic-cli … mcp` and copied the
process's stdin and stdout to files with `tee`; the transcript below is that copy. The
session was limited with `--allowedTools` to `musaic_capabilities` and
`musaic_library_list`. The prompt, the same for both runs:

> Using the musaic MCP tools only: call musaic_capabilities, then list the first 3 albums
> with musaic_library_list, then stop.

## Results

| Run | Host setting | Handshake | Negotiated revision | Host's view of the connection | Outcome |
|---|---|---|---|---|---|
| 1 | default | `server/discover` probe, then stateless requests with `_meta` | **2026-07-28** | `"protocolEra":"modern","negotiatedProtocolVersion":"2026-07-28"` | both calls succeeded, 4 turns |
| 2 | `MCP_PROTOCOL_NEGOTIATION=legacy` | `initialize` / `notifications/initialized` | **2025-11-25** | `"protocolEra":"legacy","negotiatedProtocolVersion":"2025-11-25"` | both calls succeeded, 4 turns |

The "host's view" column is quoted from Claude Code's debug log
(`--debug-file`), whose full line for run 1 reads:

```text
MCP server "musaic": Successfully connected (transport: stdio) in 120ms
MCP server "musaic": Connection established with capabilities: {"hasTools":true,"hasPrompts":false,"hasResources":false,"hasResourceSubscribe":false,"serverVersion":{"name":"musaic","version":"0.2.0-rc.1"},"protocolEra":"modern","negotiatedProtocolVersion":"2026-07-28"}
```

(run 2: connected in 35 ms, the same capabilities with `"protocolEra":"legacy"`).

### Capabilities and extensions

- Server, both runs: `capabilities: {"tools": {}}`, no extensions, server info
  `musaic 0.2.0-rc.1`, and the same `instructions` string. `server/discover` lists
  `supportedVersions: ["2026-07-28", "2025-11-25"]`.
- Client, run 1: `roots.listChanged`, `elicitation` (`form`, `url`), and the extension
  `io.modelcontextprotocol/tasks` (sent on `server/discover` and `tools/list`, not on
  `tools/call`). Musaic uses none of them. Client, run 2: `roots.listChanged` and
  `elicitation` (`form`, `url`).
- Effective: tools only. No prompts, resources, subscriptions or tasks.

### Tools the host received

Twelve tools, the same in both runs, with their annotations:

| Tool | readOnly | destructive | idempotent | openWorld |
|---|---|---|---|---|
| `musaic_capabilities` | yes | no | yes | no |
| `musaic_library_list` | yes | no | yes | no |
| `musaic_album_get` | yes | no | yes | no |
| `musaic_resolution_get` | yes | no | yes | no |
| `musaic_findings_list` | yes | no | yes | no |
| `musaic_fix_preview` | no | no | no | no |
| `musaic_fix_apply` | no | yes | yes | yes |
| `musaic_scan_start` | no | no | yes | no |
| `musaic_checks_start` | no | no | yes | yes |
| `musaic_analysis_start` | no | yes | yes | no |
| `musaic_operation_get` | yes | no | yes | no |
| `musaic_operation_cancel` | no | no | yes | no |

## Transcripts

`-->` is host to `musaic-cli`, `<--` is `musaic-cli` to host, one JSON-RPC message per
line in the order sent. Trimmed for reading: each tool's `inputSchema` and `outputSchema`
are elided, the `text` content of a tool result (the same JSON as its
`structuredContent`) is elided, and Claude Code's per-call tool-use ids are replaced. No
other change. The MCP stream carries no pairing secret, proof, nonce or token: those stay
between `musaic-cli` and the app's loopback agent endpoint, which is not part of MCP. The
stream also carried no filesystem path.

### Run 1: default (2026-07-28)

```text
--> {"jsonrpc": "2.0", "id": "server-discover-probe-1", "method": "server/discover", "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientInfo": {"name": "claude-code", "title": "Claude Code", "version": "2.1.330", "description": "Anthropic's agentic coding tool", "websiteUrl": "https://claude.com/claude-code"}, "io.modelcontextprotocol/clientCapabilities": {"roots": {"listChanged": true}, "elicitation": {"form": {}, "url": {}}, "extensions": {"io.modelcontextprotocol/tasks": {}}}}}}
<-- {"jsonrpc": "2.0", "id": "server-discover-probe-1", "result": {"resultType": "complete", "supportedVersions": ["2026-07-28", "2025-11-25"], "capabilities": {"tools": {}}, "instructions": "Musaic agent tools. Call musaic_capabilities first. Changes to files wait for the user's approval in the Musaic app.", "ttlMs": 0, "cacheScope": "private", "_meta": {"io.modelcontextprotocol/serverInfo": {"name": "musaic", "version": "0.2.0-rc.1"}}}}
--> {"method": "tools/list", "jsonrpc": "2.0", "id": 0, "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientInfo": {"name": "claude-code", "title": "Claude Code", "version": "2.1.330", "description": "Anthropic's agentic coding tool", "websiteUrl": "https://claude.com/claude-code"}, "io.modelcontextprotocol/clientCapabilities": {"roots": {"listChanged": true}, "elicitation": {"form": {}, "url": {}}, "extensions": {"io.modelcontextprotocol/tasks": {}}}}}}
<-- {"jsonrpc": "2.0", "id": 0, "result": {"resultType": "complete", "ttlMs": 0, "cacheScope": "private", "tools": [{"name": "musaic_capabilities", "title": "Capabilities", "annotations": {"readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false}, "inputSchema": "<elided>", "outputSchema": "<elided>"}, {"name": "musaic_library_list", "title": "List albums", "annotations": {"readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false}, "inputSchema": "<elided>", "outputSchema": "<elided>"}, {"name": "musaic_album_get", "title": "Get an album", "annotations": {"readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false}, "inputSchema": "<elided>", "outputSchema": "<elided>"}, {"name": "musaic_resolution_get", "title": "Get the stored resolution", "annotations": {"readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false}, "inputSchema": "<elided>", "outputSchema": "<elided>"}, {"name": "musaic_findings_list", "title": "List findings", "annotations": {"readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false}, "inputSchema": "<elided>", "outputSchema": "<elided>"}, {"name": "musaic_fix_preview", "title": "Preview a fix", "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false}, "inputSchema": "<elided>", "outputSchema": "<elided>"}, {"name": "musaic_fix_apply", "title": "Apply a fix", "annotations": {"readOnlyHint": false, "destructiveHint": true, "idempotentHint": true, "openWorldHint": true}, "inputSchema": "<elided>", "outputSchema": "<elided>"}, {"name": "musaic_scan_start", "title": "Start a library scan", "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false}, "inputSchema": "<elided>", "outputSchema": "<elided>"}, {"name": "musaic_checks_start", "title": "Start health checks", "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": true}, "inputSchema": "<elided>", "outputSchema": "<elided>"}, {"name": "musaic_analysis_start", "title": "Start analysis", "annotations": {"readOnlyHint": false, "destructiveHint": true, "idempotentHint": true, "openWorldHint": false}, "inputSchema": "<elided>", "outputSchema": "<elided>"}, {"name": "musaic_operation_get", "title": "Get an operation", "annotations": {"readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false}, "inputSchema": "<elided>", "outputSchema": "<elided>"}, {"name": "musaic_operation_cancel", "title": "Cancel an operation", "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false}, "inputSchema": "<elided>", "outputSchema": "<elided>"}]}}
--> {"method": "tools/call", "params": {"name": "musaic_capabilities", "arguments": {}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientInfo": {"name": "claude-code", "title": "Claude Code", "version": "2.1.330", "description": "Anthropic's agentic coding tool", "websiteUrl": "https://claude.com/claude-code"}, "io.modelcontextprotocol/clientCapabilities": {"roots": {"listChanged": true}, "elicitation": {"form": {}, "url": {}}}, "claudecode/toolUseId": "<tool use id>", "progressToken": 1}}, "jsonrpc": "2.0", "id": 1}
<-- {"jsonrpc": "2.0", "id": 1, "result": {"resultType": "complete", "content": [{"type": "text", "text": "<the same JSON as structuredContent, serialised>"}], "structuredContent": {"appVersion": "0.2.0-rc.1", "configuredRoots": [{"name": "library", "rootId": "4082e8b40df5f191"}], "contractVersion": "1", "instanceId": "i_76288416873cd61d", "policy": {"compute.analyze": "refuse", "compute.scan": "refuse", "covers.write": "refuse", "files.rename": "refuse", "library.read": "allow", "proposal.submit": "allow", "tags.write": "refuse"}, "profiles": ["mcp-2026-07-28", "mcp-2025-11-25"], "registryDigest": "ac53804474bd9e035d9b2bd5f12350076d06d7ab71562dc85ff4922fbae656a0", "scopes": ["library.read", "proposal.submit"]}, "isError": false}}
--> {"method": "tools/call", "params": {"name": "musaic_library_list", "arguments": {"limit": 3}, "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientInfo": {"name": "claude-code", "title": "Claude Code", "version": "2.1.330", "description": "Anthropic's agentic coding tool", "websiteUrl": "https://claude.com/claude-code"}, "io.modelcontextprotocol/clientCapabilities": {"roots": {"listChanged": true}, "elicitation": {"form": {}, "url": {}}}, "claudecode/toolUseId": "<tool use id>", "progressToken": 2}}, "jsonrpc": "2.0", "id": 2}
<-- {"jsonrpc": "2.0", "id": 2, "result": {"resultType": "complete", "content": [{"type": "text", "text": "<the same JSON as structuredContent, serialised>"}], "structuredContent": {"items": [{"albumId": "e7c7bb03208efeba", "artist": "Aurora Fields", "hasCover": true, "missing": false, "title": "First Light", "trackCount": 5, "year": "2019"}, {"albumId": "c4a35efa5c818ed0", "artist": "Aurora Fields", "hasCover": false, "missing": false, "title": "Second Light", "trackCount": 4, "year": "2021"}, {"albumId": "dd2ddf2b1f00dc84", "artist": "Aurora Fields", "hasCover": true, "missing": false, "title": "Folder Art", "trackCount": 2, "year": "2022"}], "nextCursor": "333a65663231306365643133303632356165", "total": 13}, "isError": false}}
```

### Run 2: `MCP_PROTOCOL_NEGOTIATION=legacy` (2025-11-25)

```text
--> {"method": "initialize", "params": {"protocolVersion": "2025-11-25", "capabilities": {"roots": {"listChanged": true}, "elicitation": {"form": {}, "url": {}}}, "clientInfo": {"name": "claude-code", "title": "Claude Code", "version": "2.1.330", "description": "Anthropic's agentic coding tool", "websiteUrl": "https://claude.com/claude-code"}}, "jsonrpc": "2.0", "id": 0}
<-- {"jsonrpc": "2.0", "id": 0, "result": {"protocolVersion": "2025-11-25", "capabilities": {"tools": {}}, "serverInfo": {"name": "musaic", "version": "0.2.0-rc.1"}, "instructions": "Musaic agent tools. Call musaic_capabilities first. Changes to files wait for the user's approval in the Musaic app."}}
--> {"jsonrpc": "2.0", "method": "notifications/initialized"}
--> {"method": "tools/list", "jsonrpc": "2.0", "id": 1}
<-- {"jsonrpc": "2.0", "id": 1, "result": {"tools": [{"name": "musaic_capabilities", "title": "Capabilities", "annotations": {"readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false}, "inputSchema": "<elided>", "outputSchema": "<elided>"}, {"name": "musaic_library_list", "title": "List albums", "annotations": {"readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false}, "inputSchema": "<elided>", "outputSchema": "<elided>"}, {"name": "musaic_album_get", "title": "Get an album", "annotations": {"readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false}, "inputSchema": "<elided>", "outputSchema": "<elided>"}, {"name": "musaic_resolution_get", "title": "Get the stored resolution", "annotations": {"readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false}, "inputSchema": "<elided>", "outputSchema": "<elided>"}, {"name": "musaic_findings_list", "title": "List findings", "annotations": {"readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false}, "inputSchema": "<elided>", "outputSchema": "<elided>"}, {"name": "musaic_fix_preview", "title": "Preview a fix", "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false}, "inputSchema": "<elided>", "outputSchema": "<elided>"}, {"name": "musaic_fix_apply", "title": "Apply a fix", "annotations": {"readOnlyHint": false, "destructiveHint": true, "idempotentHint": true, "openWorldHint": true}, "inputSchema": "<elided>", "outputSchema": "<elided>"}, {"name": "musaic_scan_start", "title": "Start a library scan", "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false}, "inputSchema": "<elided>", "outputSchema": "<elided>"}, {"name": "musaic_checks_start", "title": "Start health checks", "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": true}, "inputSchema": "<elided>", "outputSchema": "<elided>"}, {"name": "musaic_analysis_start", "title": "Start analysis", "annotations": {"readOnlyHint": false, "destructiveHint": true, "idempotentHint": true, "openWorldHint": false}, "inputSchema": "<elided>", "outputSchema": "<elided>"}, {"name": "musaic_operation_get", "title": "Get an operation", "annotations": {"readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false}, "inputSchema": "<elided>", "outputSchema": "<elided>"}, {"name": "musaic_operation_cancel", "title": "Cancel an operation", "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false}, "inputSchema": "<elided>", "outputSchema": "<elided>"}]}}
--> {"method": "tools/call", "params": {"name": "musaic_capabilities", "arguments": {}, "_meta": {"claudecode/toolUseId": "<tool use id>", "progressToken": 2}}, "jsonrpc": "2.0", "id": 2}
<-- {"jsonrpc": "2.0", "id": 2, "result": {"content": [{"type": "text", "text": "<the same JSON as structuredContent, serialised>"}], "structuredContent": {"appVersion": "0.2.0-rc.1", "configuredRoots": [{"name": "library", "rootId": "4082e8b40df5f191"}], "contractVersion": "1", "instanceId": "i_76288416873cd61d", "policy": {"compute.analyze": "refuse", "compute.scan": "refuse", "covers.write": "refuse", "files.rename": "refuse", "library.read": "allow", "proposal.submit": "allow", "tags.write": "refuse"}, "profiles": ["mcp-2026-07-28", "mcp-2025-11-25"], "registryDigest": "ac53804474bd9e035d9b2bd5f12350076d06d7ab71562dc85ff4922fbae656a0", "scopes": ["library.read", "proposal.submit"]}, "isError": false}}
--> {"method": "tools/call", "params": {"name": "musaic_library_list", "arguments": {"limit": 3}, "_meta": {"claudecode/toolUseId": "<tool use id>", "progressToken": 3}}, "jsonrpc": "2.0", "id": 3}
<-- {"jsonrpc": "2.0", "id": 3, "result": {"content": [{"type": "text", "text": "<the same JSON as structuredContent, serialised>"}], "structuredContent": {"items": [{"albumId": "e7c7bb03208efeba", "artist": "Aurora Fields", "hasCover": true, "missing": false, "title": "First Light", "trackCount": 5, "year": "2019"}, {"albumId": "c4a35efa5c818ed0", "artist": "Aurora Fields", "hasCover": false, "missing": false, "title": "Second Light", "trackCount": 4, "year": "2021"}, {"albumId": "dd2ddf2b1f00dc84", "artist": "Aurora Fields", "hasCover": true, "missing": false, "title": "Folder Art", "trackCount": 2, "year": "2022"}], "nextCursor": "333a65663231306365643133303632356165", "total": 13}, "isError": false}}
```

## `musaic-cli doctor`

`musaic-cli --config-dir <tmp>/config --dev-credentials doctor`, with the server running
(the output was identical before and after the two sessions), exit code 0:

```json
{
  "configDir": "<tmp>/config",
  "descriptor": {"instanceId": "i_76288416873cd61d", "pidAlive": true, "port": 61556, "status": "ok"},
  "executable": "<scratch>/bin/musaic-cli",
  "extensions": [],
  "pairing": {"id": "p_2dc5077cda0f38e0", "status": "active"},
  "profiles": ["mcp-2026-07-28", "mcp-2025-11-25"],
  "scopes": ["library.read", "proposal.submit"],
  "transport": "stdio",
  "version": "0.2.0-rc.1"
}
```

After the server was stopped, exit code 1:

```json
{
  "configDir": "<tmp>/config",
  "descriptor": {"status": "absent"},
  "executable": "<scratch>/bin/musaic-cli",
  "extensions": [],
  "pairing": {"id": "p_2dc5077cda0f38e0", "status": "not_checked"},
  "profiles": ["mcp-2026-07-28", "mcp-2025-11-25"],
  "scopes": [],
  "transport": "stdio",
  "version": "0.2.0-rc.1"
}
```

`doctor` lists the profiles the CLI speaks, not the one a host negotiated; the negotiated
revision comes from the host (its debug log) or the transcript.

## The model's answers

Run 1 (abridged): "Both calls succeeded. Capabilities: Musaic 0.2.0-rc.1, contract version
1, one configured root (`library`). This pairing has the `library.read` and
`proposal.submit` scopes; scanning, analysis, tag writes, cover writes and file renames are
all refused by policy. First 3 albums (of 13 total): Aurora Fields, First Light (2019, 5
tracks, cover); Aurora Fields, Second Light (2021, 4 tracks, no cover); Aurora Fields,
Folder Art (2022, 2 tracks, cover). Stopping here as requested."

Run 2 gave the same facts in a slightly different layout.

## Observed limitations

- With the modern profile, Claude Code printed on stderr, twice each, `unknown format
  "uint32" ignored in schema` for `AlbumSummary.trackCount` and `total` in
  `musaic_library_list`'s output schema (13 `"format": "uint32"` annotations in the full
  `tools/list` answer, generated by `schemars`). The host ignores the format and the calls
  succeed; run 2 printed no such warning.
- The client's `io.modelcontextprotocol/tasks` extension is offered and not used; Musaic
  answers every call synchronously and hands long work back as an operation handle.
- `doctor` with the app stopped reports `descriptor.status: "absent"` and exit 1, not an
  `APP_NOT_RUNNING` error code (that code is what a tool call returns).

## Not recorded

- **Claude Desktop**: not available headless on this machine; not connected.
- **`musaic-cli setup`**: not run; the host was given a temporary config instead, so no
  user host configuration was touched. `setup` is covered by the CLI's tests.
- **Approval round trip**: the session was limited to the two read tools; no
  `musaic_fix_preview` / `musaic_fix_apply` call and no Review inbox approval were made
  through a real host.
- **macOS Keychain prompt on first read**: the runs used `--verification-memory-secrets`
  and `--dev-credentials`, so the pairing secret came from the scratch folder, not the
  Keychain.
- **An installed build**: the binaries were debug builds from the workspace, not the CLI
  inside an installed `Musaic.app`.
