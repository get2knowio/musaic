# Contract: routes and shell requests

## `GET /api/agent/claude-desktop` → operation `agent.claude_desktop_status`

Read-only. Response (camelCase JSON):

```json
{
  "platform": "darwin",
  "bundleSupported": true,
  "claudeFound": true,
  "configEntry": "absent",
  "cli": { "state": "found", "command": { "program": "/Applications/Musaic.app/Contents/MacOS/musaic-cli", "args": ["mcp"] } },
  "shape": "installed",
  "route": "bundle",
  "setupCommand": { "program": "/Applications/Musaic.app/Contents/MacOS/musaic-cli", "args": ["setup", "--host", "claude-desktop"] },
  "uninstallCommand": null,
  "appVersion": "0.2.0"
}
```

An `extension` field (`detected | not-detected | unknown`) sits beside `configEntry` (FR-034). `configEntry`: `absent | ours | foreign | unreadable`. `cli.state`: `found | missing | unstable` (`unstable` carries `reason`: `disk-image | app-image | translocated | temporary`). `route`: `bundle | config-file-only | blocked`. It is the one `/api` response in the agent family that carries a filesystem location, and only the app's own program; it is a web-view route behind the launch gate, never an agent tool.

## `GET /api/agent/skill-archive` → operation `agent.skill_archive`

`200`, `Content-Type: application/zip`, `Content-Disposition: attachment; filename="musaic-work-delegation-<version>.zip"`. Deterministic bytes.

## `POST /__musaic/connect/claude-desktop` (desktop shell only) → operation `agent.claude_desktop_bundle`, then the opener

Guard: launch cookie, exact Host, same Origin, POST, empty body (a body is refused). Response:

```json
{ "opened": true, "command": { "program": "…/musaic-cli", "args": ["mcp"] }, "shape": "installed" }
```

or `{ "opened": false, "reason": "not-ready | busy | write-failed | open-failed", "message": "…" }` with the generated file already removed. No path to the generated file is returned.

Page marker: `window.__musaicDesktop.connectClaude === true` when the request exists.

## Diagnostics addition (`GET /api/agent/diagnostics`)

Each assistant gains `cliVersion: string | null`, `cliOlder: boolean | null`; the response gains `appVersion`.

## Agent bridge addition (`POST /__musaic/agent/v1/session/observe`)

Optional request field `cliVersion` (string, cleaned like the other observed strings: control characters stripped, at most 80 characters). Older programs omit it.

## Not added

No agent tool, no MCP method, no CLI subcommand that builds or opens a bundle. `docs/agent-tools.*`, `docs/agent-events.*` and the skill bytes are unchanged.
