# Contract: Agent HTTP route family (`/__musaic/agent/v1`)

The app serves these routes from its loopback server. Only `agent_guard` protects them
(research R5). They never consult the launch cookie, and the launch cookie never grants
access to them.

## Guard (in order; first failure wins)

| Check | Failure |
|---|---|
| Peer address is loopback | 403 `{code:"FORBIDDEN"}` |
| Exactly one `Host`, equal to `127.0.0.1:<bound port>` | 403 `FORBIDDEN` |
| No `Origin` header; `Sec-Fetch-Site` absent or `none` | 403 `FORBIDDEN` |
| Body ≤ 256 KiB | 413 `INVALID_INPUT` |
| Body present ⇒ `Content-Type: application/json` | 415 `INVALID_INPUT` |
| Except hello/exchange: `Authorization: Bearer <token>` valid, unexpired, current instance, pairing not revoked | 401 `NOT_PAIRED` / `PAIRING_REVOKED` / `INSTANCE_MISMATCH` |

## Routes

Each route below is thin: it parses the request, calls one operation and renders the
result. Each one has a row in `docs/operations.md`.

| Method | Path | Operation | Body → Response |
|---|---|---|---|
| POST | `/__musaic/agent/v1/hello` | `agent.hello` | `{}` → `{instanceId, serverNonce, profiles[], contractVersion}` |
| POST | `/__musaic/agent/v1/exchange` | `agent.exchange` | `{pairingId, instanceId, serverNonce, clientNonce, clientProof}` → `{token, expiresAt, scopes[], serverProof}`. 401 `NOT_PAIRED` on any failure, without saying which part failed. 429 `LOCKED_OUT` `{retryAfterSeconds}` |
| POST | `/__musaic/agent/v1/tools/{toolId}` | `agent.call_tool` | the tool's input → `{ok:true, result}` \| `{ok:false, error:{code,message,details?}}` (HTTP 200 for domain errors; 404 for an unknown tool id; 400 for input that fails schema validation, with `INVALID_INPUT`) |
| GET | `/__musaic/agent/v1/tools` | `agent.list_tools` | → the catalog JSON (the same bytes as `docs/agent-tools.json` with `registryDigest`) |

All tools share a single route that dispatches on the path parameter. The route parses
the body with `musaic_commands::parse_call(toolId, body)` into a typed `ToolCall`, calls
the one operation `agent.call_tool(principal, call)`, and renders the typed `ToolOutput`.
That operation authorizes the call and dispatches to the per-tool operation. The design
keeps one inventory row and one place where authorization happens, and keeps JSON out of
`ops/`.

## Timing

The nonce lives 60 s and is single-use. The token lives 15 min. The CLI renews it 60 s
before expiry, and on a 401 it retries once with a fresh `hello` and `exchange`.

# Desktop-facing management routes (`/api/agent/*`)

These routes are on the normal `/api` stack (launch gate and Host/Origin unchanged), and
are used only by the webview.

| Method | Path | Operation | Notes |
|---|---|---|---|
| GET | `/api/agent/pairings` | `agent.list_pairings` | `[{id, name, scopes, policy, createdAt, lastUsedAt, revokedAt}]` (never a secret) |
| POST | `/api/agent/pairings` | `agent.create_pairing` | `{name, scopes[]}` → `{id, …, connect: {command, args}}`. The secret goes to the keyring only. 409 when 20 pairings are already active |
| DELETE | `/api/agent/pairings/{id}` | `agent.revoke_pairing` | drops tokens and the keyring entry |
| PUT | `/api/agent/pairings/{id}/policy` | `agent.set_policy` | `{kind: decision}` partial map |
| GET | `/api/agent/pairings/{id}/activity` | `agent.activity` | last 100 |
| GET | `/api/agent/approvals` | `agent.list_approvals` | `?state=pending` (default) \| `all` |
| POST | `/api/agent/approvals/{id}/approve` | `agent.decide_approval` | 409 if it is no longer pending |
| POST | `/api/agent/approvals/{id}/deny` | `agent.decide_approval` | 409 if it is no longer pending |
| GET | `/api/agent/changes` | `agent.list_changes` | `?pairing=<id>` (optional; 404 for an unknown pairing) → newest first, at most 20: `[{id, pairingId, pairingName, albumId, albumLabel, createdAt, state, summary, undoes, undoable, reason}]`. `albumId` is the album's id now; `undoable`/`reason` are derived from the files at the time of the request |
| POST | `/api/agent/changes/{id}/undo` | `agent.undo_change` | FR-055. → `{change, undoId, complete, message}`. 404 unknown change; 409 with a plain `detail` when the change is not applied or partial, already undone, an undo itself, its album is Missing, or a restore or other work is running; 409 with `detail: {message, changed: ["<file>: <field>", …]}` when the files changed since (nothing written); 503 when nothing could be put back |

Every mutation here publishes `AgentStateChanged` (an undo: `what: "changes"`, plus an
`activity` row with tool `undo`). There is no undo on the agent route family: no tool
offers it, and the catalog lists it under what is deliberately not offered.
