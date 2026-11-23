# Implementation Plan: MCP Skills, Events and external-agent hosts

**Branch**: `005-skills-events-agent-hosts` | **Date**: 2026-11-23 | **Spec**: [spec.md](spec.md)
**Input**: Feature specification from `specs/005-skills-events-agent-hosts/spec.md`

## Summary

> **Scope change, 2026-11-23:** the agent launcher (design decision 8, slice 8, the `musaic-launch` crate, the `agent_launcher` / `agent_runs` tables, the launcher routes and UI card, `AgentWhat::Launcher`, and `contracts/launcher.md`) is removed. Musaic starts no agent process. Text below that mentions the launcher is superseded by this note.

Publish a versioned work-delegation skill over MCP (Skills extension on the `2026-07-28` profile, plain Resources and a prompt on both), add a retained event history beside the delegation outbox and serve it through `events/list`, `events/poll` and `events/stream`, record what each paired host negotiated, show delegated work and agent diagnostics in the desktop, and add an optional, off-by-default launcher for one external agent program. Everything reaches the database through `musaic_app::ops`; the stdio process stays a bridge client.

## Technical Context

**Language/Version**: Rust 1.99.1 (edition 2024, `unsafe` forbidden); TypeScript / React for the UI
**Primary Dependencies**: `rmcp =3.11.0` (features `server`, `transport-io`; native Skills model and handlers, `on_custom_request` for Events), axum, rusqlite, `ureq` in the CLI, `sha2`; TanStack Query in the UI. No new third-party crate is expected; any addition is justified in the PR.
**Storage**: SQLite, schema v22 → v23 (one migration, `v22_to_v23`)
**Testing**: `cargo test --workspace`; vitest; an in-repo conformance harness driving the stdio server as a modern and as a legacy client
**Target Platform**: macOS, Linux, Windows desktop; loopback only
**Project Type**: desktop app (Rust workspace + web UI)
**Performance Goals**: a waiting subscriber costs one parked blocking call and no per-subscriber buffer; event pages ≤ 100
**Constraints**: the CLI never links the database; agent routes stay request/response JSON under the existing guard (256 KiB, no streaming body); no new listener; no credentials for any agent program
**Scale/Scope**: a handful of pairings, tens to hundreds of work items, 7 days of events

## Constitution Check

*GATE: passed before research; re-checked after design.*

- **I. The library is the user's.** Nothing here writes to an album. A launched agent uses the same claim and result tools, and only a desktop decision applies a proposal. Missing albums are untouched because no file operation is added.
- **II. Local-first, secure by default.** No new listener. New agent routes sit behind the existing agent guard; new desktop routes behind the launch cookie and Host/Origin check. Skill files are an embedded allowlist, never a filesystem path. The launcher takes a listener-chosen executable verified by path and SHA-256, an argument array, a minimal environment and a temporary directory; Musaic stores no agent credential. Event data holds references only. The tools still return no filesystem path.
- **III. One operations surface, events for fan-out.** Every new route parses, calls one operation and renders. New operations get inventory rows and ops tests. The event history row is written in the transaction that changes the work; delivery is woken by the bus and reads the table. The UI refreshes from `AgentStateChanged`.
- **IV. Derive from current state.** Events carry ids and a revision; agents re-read work through the tools. Cursors are bound to the library epoch; a mismatch yields `truncated`. Diagnostics are observations with timestamps, never a stored "ready".
- **V. Test-backed, deterministic.** Injectable clock for retention, leases, backoff and heartbeats; a stand-in agent program for the launcher; the conformance harness for both profiles; drift checks for the tool catalog, event catalog and skill assets. Only synthetic fixtures.
- **VI. Plain, accessible interface.** Four separate facts per agent with times; work listed by state with keyboard-reachable Retry; launcher failures shown with the reason and an explicit retry.

No violation to justify.

## Design decisions (detail in [research.md](research.md))

1. **Skill assets** live at `skills/musaic-work-delegation/` and are embedded with `include_bytes!` from `musaic-commands` (module `skill`), which also generates `references/tools.md` and `references/events.md` from the registry and the event catalog. A committed `skill.lock.json` holds the version and every file's digest and size; `catalog --check` fails on any difference, and `--write` refuses to rewrite the lock when bytes changed and the version did not.
2. **Skills over MCP** use rmcp's `list_skills` / `get_skill` / `read_resource`, gated to modern requests. `directoryRead` is not declared. URIs are `skill://musaic-work-delegation/<file>`; lookups match the allowlist exactly after rejecting anything that is not a byte-for-byte listed URI.
3. **Fallbacks**: `resources/list` and `resources/read` serve the same files on both profiles; prompt `musaic_research_album` returns the guide; `musaic-cli setup --skill` installs into the host's skills directory with an install record.
4. **Event history**: table `delegation_events`, written by `record_change` whenever a work item is created or its state changes. `eventId` is `evt_<sequence>` qualified by epoch; the cursor is an opaque encoding of `(epoch, sequence)`. Pruned at 7 days by the app's tick; other-epoch rows removed on rotation.
5. **Events over the bridge**: two request/response routes, `events/list` and `events/poll`, the latter with an optional bounded wait woken by the delegation publish. `events/stream` is implemented in the stdio process as a loop of waiting polls that writes `notifications/events/*`.
6. **Host observations**: the stdio process posts what it saw (client name and version, profile, declared Tasks / Skills / Events) to a new bridge route; the desktop keeps the latest per pairing plus in-memory "last reachable" and "last waiting poll".
7. **Desktop**: `GET /api/agent/diagnostics`, `GET /api/research/work`, and the launcher routes; Settings → Agents gains diagnostics and the launcher card; Review gains the delegated-work section. A new `AgentWhat::Hosts` and `AgentWhat::Launcher` drive refreshes.
8. **Launcher**: new crate `musaic-launch` (adapter trait, the Claude Code adapter, a pure supervisor state machine with injected clock and spawner). `musaic-app` hosts the supervisor thread and its ops. State in two new tables, not in the pinned settings defaults.

## Slices

Each slice leaves the workspace green and is reviewable alone.

1. **Skill assets and drift check.** `skills/` directory, generated references, lock file, `musaic_commands::skill`, event catalog (`docs/agent-events.{json,md}`), `catalog --check/--write` extended. Tests: frontmatter/name agreement, lock, traversal-safe lookup, generated-reference drift.
2. **Skills, Resources and prompt over MCP.** `musaic-mcp` handlers and gating; conformance harness (modern and legacy client) under `musaic-mcp/tests/`. Tests: list/get/read digests, unlisted lookup by URI, refusals, no skill methods on legacy, prompt and plain resource.
3. **Event history.** Migration v23, `delegation_events` writes in `record_change`, heartbeat column, prune, epoch handling; `ops::agent::{events_list, events_poll}`, bridge routes, inventory rows. Tests with separate connections and the injected clock: null cursor, resume across reopen, truncation, epoch, filters, scope, revocation, wait and wake, waiter cap.
4. **Events over MCP.** Custom-request handling in `musaic-mcp`, the stream loop, cancellation, heartbeats, the client declaration gate; `Backend` gains events and observation calls; CLI client implements them. Harness tests: list, poll bootstrap, stream active/event/heartbeat/terminated, cancel, duplicate claim race.
5. **Host observations and diagnostics.** Bridge `session/observe` and `session` routes, table, ops, `GET /api/agent/diagnostics`, `doctor` output, `musaic_capabilities` extension list. UI: diagnostics in Settings → Agents.
6. **Delegated work in Review.** `jobs.delegated_work` op and route, UI section with holder, heartbeat, lease, Retry; "waiting for an agent" state; "Process pending Musaic work" instructions.
7. **Skill install via setup.** `setup --skill`, install record, update/uninstall, `doctor` skill status.
8. **Launcher.** `musaic-launch`, tables, ops and routes, supervisor wiring and shutdown, UI card. Tests with the stand-in program. The real adapter stays `proven = false` until the compatibility report records a real run.
9. **Docs and evidence.** `docs/mcp.md`, `docs/agents.md`, `docs/operations.md`, CLAUDE.md, compatibility report under `specs/005-skills-events-agent-hosts/evidence/`, quickstart run.

## Project Structure

### Documentation (this feature)

```text
specs/005-skills-events-agent-hosts/
├── plan.md
├── research.md
├── data-model.md
├── quickstart.md
├── contracts/
│   ├── skills.md
│   ├── events.md
│   ├── bridge-routes.md
│   ├── desktop-routes.md
│   └── launcher.md
├── evidence/            # compatibility report, checks (written during implementation)
└── tasks.md
```

### Source Code (repository root)

```text
skills/musaic-work-delegation/         SKILL.md, references/*.md, skill.lock.json
docs/                                  mcp.md (new), agent-events.{json,md} (generated), agents.md, operations.md
rust/crates/musaic-commands/src/       skill.rs, events.rs (event catalog, cursor-free wire types), catalog.rs
rust/crates/musaic-db/src/             delegated_work.rs, delegation_events.sql, agent_hosts.rs, agent_launcher.rs, migrate.rs
rust/crates/musaic-app/src/agent/      events.rs, hosts.rs, launcher.rs; ops/agent.rs, ops/jobs.rs, record.rs
rust/crates/musaic-api/src/            agent_guard.rs (ROUTES), routes/agent.rs, routes/jobs.rs, render/
rust/crates/musaic-mcp/src/            lib.rs, skills.rs, events.rs, prompts.rs; tests/conformance.rs
rust/crates/musaic-cli/src/            client.rs, setup.rs, skill_install.rs, doctor.rs, catalog_cmd.rs
rust/crates/musaic-launch/             adapter.rs, claude_code.rs, supervisor.rs (new crate)
rust/crates/musaic-events/src/         event.rs (AgentWhat::Hosts, AgentWhat::Launcher)
frontend/src/                          pages/SettingsAgents.tsx, pages/Review.tsx, components/DelegatedWork.tsx,
                                       components/AgentLauncher.tsx, hooks/useAgents.ts, hooks/useLibraryEventsStream.ts, lib/api.ts
```

**Structure Decision**: extend the existing crates along their current roles; one new leaf-ish crate, `musaic-launch`, so process launching is testable without the app and stays out of `musaic-cli`'s dependency closure.

## Risks

- **rmcp custom requests and long-lived requests.** `events/stream` needs notifications tied to an open request and cancellation by `notifications/cancelled`. If rmcp 3.11.0 cannot hold a custom request open while sending custom notifications, the stream is not declared (`delivery: ["poll"]` only) and the compatibility note says so. Slice 4 proves this first.
- **Server capabilities per profile.** If `get_info` cannot differ between `initialize` and `server/discover`, the extensions are declared once and the handlers refuse legacy sessions with `-32601`.
- **Real-host evidence.** Needs an installed, signed-in agent and the listener's usage; it is run once, deliberately, at the end, and otherwise recorded as untested.
- **Size.** Nine slices. The launcher (slice 8) is the one to cut if the run has to stop short; slices 1 to 7 stand without it.

## Complexity Tracking

| Addition | Why needed | Simpler alternative rejected because |
| --- | --- | --- |
| New crate `musaic-launch` | Process launch with injected spawner and clock, usable from `musaic-app` only | Putting it in `musaic-services` mixes process control into album services; in `musaic-cli` it would be on the wrong side of the bridge |
| `delegation_events` beside the outbox | The outbox is deleted on publish and has no state | Reusing the audit table would expose internal transition names and has no retention |
