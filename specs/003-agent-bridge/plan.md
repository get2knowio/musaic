# Implementation Plan: Agent Bridge — Shared Tools, stdio CLI and Secure Desktop Bridge

**Branch**: `003-agent-bridge` | **Date**: 2026-11-20 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `specs/003-agent-bridge/spec.md`

## Summary

Add a registry of twelve agent tools (`musaic-commands`), an MCP adapter (`musaic-mcp`,
built on `rmcp` pinned at `=3.11.0`, serving the 2026-07-28 and 2025-11-25 profiles over
stdio) and a bundled CLI (`musaic-cli`). The CLI finds the running app through a
per-config-dir `agent-instance.json`, proves both sides with an HMAC challenge over a
pairing secret kept in the OS keyring, and calls a new `/__musaic/agent/v1` route family
on the app's loopback server. Each agent route parses its input, calls one operation on a
new `musaic_app::ops::agent` group, and renders the result. That group enforces scope and
policy, records proposals and approval cards (schema v21), and calls the existing
operations and services for reads, apply, scan, checks and analysis. The desktop gains
Settings → Agents (pairings, policy, activity) and approval cards in the Review inbox,
both kept fresh by a new bus event. `setup`, `uninstall` and `doctor` manage Claude Desktop
and Claude Code configuration. The CLI ships inside each desktop bundle as a Tauri
`externalBin`.

## Technical Context

**Language/Version**: Rust 1.99.1 (edition 2024, `unsafe` forbidden); TypeScript/React for the UI.

**Primary Dependencies**:
- Existing: axum 0.8, tokio, serde, serde_json, sha2, getrandom 0.4, keyring 4.2, ureq 3.5.
- New: `rmcp =3.11.0` (`default-features = false`, features `server`, `transport-io`), in
  `musaic-mcp` only.
- New: `schemars` 1.x, to derive the tool schemas from the typed inputs and outputs.
- New: `clap` 4 (derive), for the CLI.
- New: `hmac` 0.13, matched to the sha2 0.11 line already in the workspace.
- New dev-dependency: `jsonschema`, to validate outputs against their schemas in tests.

**Storage**: SQLite schema v20 → v21, adding six `agent_*` tables. Prior covers for the
before-image are stored under `<config dir>/agent-changes/`. The pairing secret is
kept in the OS keyring under service `musaic-agent`, account `<configDirId>:<pairingId>`.
Tokens, operation handles and idempotency keys live in memory only.

**Testing**: `cargo test` covers:
- unit tests;
- ops tests (`tests/ops_agent.rs`);
- API route tests;
- CLI tests run in-process with an injected credential source;
- MCP tests that drive stdio with both handshakes;
- generated-catalog check.

Vitest covers the Settings → Agents page and the Review inbox approval items.

**Target Platform**: macOS, Windows, Linux desktop (Tauri 2).

**Project Type**: desktop app (Rust workspace + React UI) plus a bundled CLI.

**Performance Goals**: read tools answer within normal API latency while an approval is
pending (SC-008); list pages ≤ 100 items.

**Constraints**:
- Loopback only.
- No second database owner: the CLI must not link `musaic-db` or `rusqlite`, and a test
  enforces this.
- stdout of `musaic-cli mcp` carries protocol messages only.
- No secret in arguments, URLs, logs or responses.

**Scale/Scope**: about 12 tools, about 20 new HTTP routes, 3 new crates, 1 migration,
2 UI surfaces.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Verdict | How |
|---|---|---|
| I. The library is the user's | Pass | Writes only via the apply pipeline and the rename service. Missing albums refused. Revalidated against revision digest and same-file before apply. Deny, expiry and cancel touch nothing. The v21 migration is covered by the existing pre-migration snapshot (`startup.rs` `snapshot_before_schema_step`). Dev runs use scratch config dirs. |
| II. Local-first, secure by default | Pass, with one justified deviation (see Complexity Tracking) | The agent routes keep loopback binding and add their own guard (loopback peer, exact Host, no Origin, JSON, 256 KiB). The launch gate and Host/Origin check are unchanged for existing routes. Secrets stay only in the OS keyring, and the DB holds only a SHA-256 fingerprint. Mutual HMAC means a port squatter learns nothing. Agents get ids, not paths. `SECURITY.md` gains an "Agent access" section. Windows descriptor ACL: see Complexity Tracking. |
| III. One operations surface, events for fan-out | Pass | New `ops::agent` group. Agent routes are thin (`thin_handlers` covers the new route file). Every route gets a `docs/operations.md` row. Agent-caused mutations reuse the existing ops, so the domain events are the same. The new `AgentStateChanged` event drives UI invalidation and is skipped by the sweep wake and the rerun subscribers. |
| IV. Derive from current state | Pass | Proposals store the patch and the base revision. Apply re-derives the diff and the rename plan against current disk and DB, and refuses on drift. Stored proposal text is used for labels only. The display artist goes through `display_artist` (rule 4). |
| V. Test-backed, deterministic | Pass | Every FR in the contract list has a test task. Fake clock for token, proposal and lockout expiry. The fresh-db golden is edited deliberately for v21, with the reason stated. Catalog `--check` runs in CI. |
| VI. Plain, accessible interface | Pass | Settings → Agents and the approval cards are keyboard operable, plain language ("Let this assistant change tags?"). Approve and deny are on-screen controls. Opening a card has no side effect. |

Post-design re-check (after Phase 1): unchanged, all pass. The one deviation is listed below.

## Project Structure

### Documentation (this feature)

```text
specs/003-agent-bridge/
├── plan.md
├── research.md
├── data-model.md
├── quickstart.md
├── contracts/
│   ├── tools.md            # the 12 tools: inputs, outputs, kinds, annotations, errors
│   ├── agent-http.md       # /__musaic/agent/v1 routes, auth handshake, guard rules
│   ├── desktop-http.md     # /api/agent/* routes for Settings → Agents and approvals
│   ├── descriptor.md       # agent-instance.json format and validation
│   └── cli.md              # musaic-cli commands, exit codes, setup file edits
└── tasks.md
```

### Source Code (repository root)

```text
rust/crates/musaic-commands/        NEW: the contract crate, which a test bars from depending on db, app, axum or tokio
  src/lib.rs                        registry: ToolDef {id, cli_name, title, description, kinds, annotations, input/output schema fns}
  src/tools/*.rs                    typed Input/Output structs (serde deny_unknown_fields + schemars)
  src/kinds.rs                      PermissionKind, Decision, Scope
  src/errors.rs                     ToolErrorCode (stable strings)
  src/auth.rs                       hello/exchange wire types, HMAC proof fns, token header name
  src/descriptor.rs                 Descriptor type, write_atomic_owner_only, read_validated
  src/catalog.rs                    render catalog JSON + Markdown; digest
rust/crates/musaic-mcp/             NEW: depends on rmcp; adapter only
  src/lib.rs                        ServerHandler impl: list_tools from registry, call_tool → Backend trait
  src/profile.rs                    profile detection/reporting (modern vs legacy) for doctor
rust/crates/musaic-cli/             NEW: bin `musaic-cli`; depends on commands, mcp, ureq, keyring, clap
  src/main.rs, src/client.rs        discovery + handshake + token renewal + HTTP calls
  src/credentials.rs                CredentialSource trait: OsKeyring, DevFile (scratch dirs only), Memory (tests)
  src/setup.rs                      host detection, dry-run plan, atomic merge with backup + rollback, uninstall
  src/doctor.rs
  src/catalog_cmd.rs                `catalog --check` / `catalog --write`
rust/crates/musaic-db/src/agent.rs  NEW: SQL for agent_* tables; migrate.rs v20_to_v21
rust/crates/musaic-app/src/agent/   NEW: AgentHub (tokens, lockout, operations registry, idempotency), descriptor lifecycle
rust/crates/musaic-app/src/ops/agent.rs  NEW ops group: pairings, policy, exchange, authenticate, tools, approvals, activity
rust/crates/musaic-services/src/agent_*.rs  NEW where domain logic is needed: revision digest, proposal diff, template rename by album id
rust/crates/musaic-api/src/agent_guard.rs   NEW middleware for /__musaic/agent
rust/crates/musaic-api/src/routes/agent.rs  NEW thin routes (agent family + /api/agent desktop family)
rust/crates/musaic-events/src/event.rs      + AgentStateChanged
rust/crates/musaic-desktop/                 descriptor write/remove in boot/teardown; gate passes /__musaic/agent through; externalBin
frontend/src/pages/settings/Agents*.tsx     NEW Settings section
frontend/src/hooks/useAgents.ts             NEW (pairings, approvals; staleTime Infinity, SSE-invalidated)
frontend/src/hooks/useReview.ts             + approval items
docs/agent-tools.md, docs/agent-tools.json  GENERATED catalog (checked in)
docs/agents.md                              NEW: setup, pairing, lifetimes, limitations, PATH notes
```

**Structure Decision**: Three new crates with a one-way dependency order.
- `musaic-commands` (the contract) is a leaf crate. `musaic-api`, `musaic-app`,
  `musaic-mcp` and `musaic-cli` all use it.
- `musaic-mcp` depends on `musaic-commands` and `rmcp`.
- `musaic-cli` depends on `musaic-commands` and `musaic-mcp`, and never on `musaic-db`,
  `musaic-app` or `musaic-api`.
- The app side follows the existing layering: db → services → app (ops) → api (face).

## Slices (each leaves the workspace green)

1. **S1 Contract and catalog.** Adds `musaic-commands` with the twelve typed tools,
   permission kinds, error codes, wire types and descriptor code, then generates
   `docs/agent-tools.{json,md}`. Tests: the catalog check, unknown-field rejection, and
   schema bounds.
2. **S2 Pairing backbone.**
   - Schema v21 and the `musaic-db::agent` SQL. Updates the fresh-db golden.
   - The `AgentHub` and `ops::agent`: pairing create/list/revoke, policy, hello/exchange,
     authenticate, lockout, activity.
   - Keyring entries under the `musaic-agent` service.
   - The descriptor, written at server start and removed at shutdown, in both the desktop
     and the standalone server.
   - `agent_guard`.
   - Routes `/__musaic/agent/v1/{hello,exchange}` and `/api/agent/pairings*`.
   - `AgentStateChanged`.
3. **S3 Read tools end to end (MVP).**
   - Read operations: capabilities, library_list, album_get, resolution_get (stored only)
     and findings_list, with their routes.
   - `musaic-cli` with discovery, handshake, `call`, `tools`, the `mcp` command and
     `doctor`.
   - The `musaic-mcp` adapter.
   - Tests: CLI lists the same albums as the desktop; both MCP profiles over stdio; no
     stdout contamination.
4. **S4 Proposals, approvals and apply.**
   - Revision digest, `fix_preview` and `fix_apply`, with the before-image
     (`agent_changes`, FR-054) recorded before the first write.
   - Approval cards and their desktop routes, plus the template rename by album id.
   - Idempotency, and operation_get / operation_cancel for applies.
   - Tests: deny, expiry, stale input and restore leave files unchanged; the same events
     as a desktop edit.
5. **S5 Compute starts.** Adds scan_start, checks_start and analysis_start as handles in
   the operations registry, with cancellation at item boundaries.
6. **S6 Desktop UI.** Adds the Settings → Agents section and approval items in the Review
   inbox, with the SSE invalidation for `AgentStateChanged`.
7. **S7 Setup and packaging.** Adds `setup`, `uninstall` and dry-run for Claude Desktop
   and Claude Code; the `externalBin` in Tauri config, `package_macos.sh` and
   `release.yml`; the CI catalog `--check` step; and the docs (`docs/agents.md`,
   `SECURITY.md`, `docs/desktop.md`, `docs/operations.md`, the CLAUDE.md crate map and
   rule notes).

## Complexity Tracking

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|-------------------------------------|
| On Windows, the descriptor's owner-only protection relies on the per-user profile directory's inherited ACL, not an explicit ACL (FR-018). | Setting an explicit ACL needs Win32 security APIs. The workspace forbids `unsafe` and has no ACL code. | Adding an ACL crate needs a review of its own. The risk is low: the descriptor holds no credential, the endpoint must pass the mutual HMAC proof before anything secret flows, and the CLI rejects symlinks and non-loopback endpoints on every OS. A follow-up issue is filed. |
| Dev and test runs write the pairing secret to `<config>/agent-dev-credentials/<id>` (mode 0600). | This happens only with `--dev-memory-secrets` or `--verification-memory-secrets`, which are already refused outside temp and `work` dirs. A separate CLI process cannot read the app's in-memory store, and these runs must not touch the real OS keyring. | Using the real keyring in dev runs would break the "never touch real secrets in development" rule. Normal runs never write the file, and the CLI reads it only with `--dev-credentials` under the same scratch-dir rule. |
| Three new crates. | The prompt requires them. The CLI must not link the database (constitution II: no second owner), so the contract types have to live in a leaf crate that the app and the CLI share. | Putting the registry in `musaic-api` would make the CLI link axum, the app and the db. |
