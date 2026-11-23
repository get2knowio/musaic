# Tasks: MCP Skills, Events and external-agent hosts

**Input**: `specs/005-skills-events-agent-hosts/` (spec.md, plan.md, research.md, data-model.md, contracts/, quickstart.md)

Format: `- [ ] T### [P?] [US#?] description with path`. `[P]` = different files, no dependency on an unfinished task. Every phase ends green on its targeted tests. Paths under `rust/crates/` unless they start with `frontend/`, `docs/`, `skills/` or `specs/`.

## Phase 1: Setup

- [X] T001 Build the UI once (`cd frontend && npm ci && npm run build`) and confirm `cargo test -p musaic-commands -p musaic-mcp` passes on the untouched branch; record the baseline test counts in `specs/005-skills-events-agent-hosts/evidence/checks.md`
- [ ] ~~T002 Create crate `musaic-launch` (`musaic-launch/Cargo.toml`, `src/lib.rs`, workspace lints, no `musaic-db`/`axum`/`tokio` dependency) and confirm `musaic-cli/tests/no_db_owner.rs` still passes~~ (removed: launcher cut from scope)

## Phase 2: Foundational (blocks every story)

- [X] T003 Add the event catalog and wire types to `musaic-commands/src/events.rs`: extension ids, the two event names, `EventDescriptor`, `EventOccurrence`, `WorkEventData`, `PollParams`, `PollResult`, `HostObservation`, the error-code constants, and `events_catalog_json` / `events_catalog_markdown` (contracts/events.md); unit tests for schema shape (`data` has exactly five required keys, `additionalProperties: false`)
- [X] T004 Add `AgentWhat::Hosts` in `musaic-events/src/event.rs`, keep it report-only in `musaic-events/src/catalog.rs` (no sweep wake, no findings notify), give it the relay name `hosts` in `musaic-events/src/relay.rs` and extend `musaic-events/tests/` and `musaic-api/tests/stream_equivalence.rs` coverage for the new variant
- [X] T005 Migration `v22_to_v23` in `musaic-db/src/migrate.rs` with `musaic-db/src/delegation_events.sql`: tables `delegation_events`, `delegation_event_state` (prune watermark, data-model), `agent_host_observations`, column `work_attempts.last_heartbeat_at`; bump `SCHEMA_VERSION` to 23; migration tests (fresh create, v22 fixture copy upgraded in a temp dir, repeat is a no-op); update every test or golden that pins the schema version, saying why

**Checkpoint**: `cargo test -p musaic-commands -p musaic-events -p musaic-db`

## Phase 3: User Story 1 - An agent learns the workflow from Musaic (P1)

**Independent test**: conformance harness, modern and legacy client (contracts/skills.md tests 1 to 5).

### Slice 1: skill assets and drift check

- [X] T006 [US1] Write `skills/musaic-work-delegation/SKILL.md` (frontmatter `name`, `description`, `metadata.version: 1.0.0`) covering the nine behaviours of FR-002, the no-permission statement of FR-003, and pointers to the references
- [X] T007 [P] [US1] Write the hand-authored references in `skills/musaic-work-delegation/references/`: `work-lifecycle.md`, `metadata-evidence.md` (MusicBrainz identifier types, multi-disc alignment, uncertainty), `cover-research.md`, `result-schemas.md`, `async-operations.md`, `recovery.md`; synthetic albums only, with one successful, one ambiguous and one no-match worked example (FR-005)
- [X] T008 [US1] `musaic-commands/src/skill.rs`: embed the bundle with `include_bytes!`, the allowlist, URI construction, digest and size, frontmatter parsing, the exact-match lookup with the refusal rules of research D2, and generators for `references/tools.md` (from `REGISTRY`) and `references/events.md` (from the event catalog); unit tests: name equals directory, manifest complete and unique, every refusal case, digests stable
- [X] T009 [US1] Extend `musaic-cli/src/catalog_cmd.rs`: `catalog --check/--write` also covers `docs/agent-events.{json,md}`, the generated skill references and `skills/musaic-work-delegation/skill.lock.json`; `--write` refuses to update the lock when bytes changed and the version did not; tests for each drift and for the version rule; generate and commit the files
- [X] T010 [US1] Test that no skill file contains a credential, account or subscription wording, a filesystem path outside examples, or a tool name absent from `REGISTRY` (`musaic-commands/tests/skill_assets.rs`)

### Slice 2: Skills, Resources and prompt over MCP

- [X] T011 [US1] `musaic-mcp/src/skills.rs` and `lib.rs`: declare `resources`, `prompts` and the skills extension (no `directoryRead`); implement `list_skills`, `get_skill`, `list_resources`, `read_resource`; gate `skills/*` to modern requests (`-32601` on legacy); result cache metadata per contracts/skills.md
- [X] T012 [US1] `musaic-mcp/src/prompts.rs`: `list_prompts` / `get_prompt` for `musaic_research_album` with `album_id` validation
- [X] T013 [US1] Conformance harness `musaic-mcp/tests/conformance.rs` (in-memory duplex transport, a fake `Backend`): a modern client and a legacy client; tests 1 to 5 of contracts/skills.md, including digest and size verification of every manifest entry and the full refusal list
- [X] T014 [US1] Update `musaic-app/src/agent/read.rs` so `musaic_capabilities.extensions` lists the offered extensions from one constant in `musaic-commands`; regenerate `docs/agent-tools.{json,md}` if the output changed

### Slice 7: installed skill (fallback)

- [X] T015 [US1] `musaic-cli/src/skill_install.rs` and `setup.rs`/`main.rs`: `setup --skill` and `uninstall --skill` for host `claude-code` with the install record, backup, owned-file update and uninstall rules of research D4, `--dry-run`, and the "does not show that the host has loaded the skill" line; tests in a temp home: fresh install, unmodified update, modified-file backup, foreign directory backup, uninstall leaves foreign files, dry run writes nothing
- [X] T016 [US1] `musaic-cli/src/doctor.rs`: report installed skill version and whether it matches the bundled one; test

**Checkpoint**: `cargo test -p musaic-commands -p musaic-mcp -p musaic-cli` and `cargo run -p musaic-cli -- catalog --check`

## Phase 4: User Story 2 - An agent hears about work and never double-claims (P1)

**Independent test**: contracts/events.md tests 1 to 11.

### Slice 3: event history and bridge

- [X] T017 [US2] `musaic-db/src/delegated_work.rs`: `record_change` writes a `delegation_events` row when a work item is created or its state changes (`requested` when the result is `queued`, else `updated`); set `last_heartbeat_at` on claim, renew and progress; delete other-epoch event rows in `rotate_in`; first enumerate every transition of spec 004's state machine and record in research.md D5 which path writes it (every change of stored work state writes one event, FR-015); tests for every transition in a full lifecycle (which write, which do not) using separate connections
- [X] T018 [US2] `musaic-db/src/delegation_events.rs`: `head`, `page(epoch, after, filters, limit)`, `oldest`, `prune(before)`; `prune` also advances `delegation_event_state.pruned_through`; cursor encode/decode lives in `musaic-app`; tests (prune never touches `delegated_work` or the audit, FR-021)
- [X] T019 [US2] `musaic-app/src/agent/events.rs`: cursor codec, `events_list`, `events_poll` (null cursor, resume, truncation by retention / epoch / `maxAgeMs`, filters, current-visibility check, `hasMore`, `nextPollMs`), the waiter condition variable in `AgentHub` notified by `publish_pending`, revocation and shutdown, the 4-waiter cap, and the 7-day prune in `tick`, truncation by `pruned_through`; update `last_seen` / `waiting_polls`
- [X] T020 [US2] `musaic-app/src/ops/agent.rs` + `record.rs`: operations `events_list` and `events_poll` (permission via `decide` on the work-read kind; revoked → forbidden); no `serde_json` under `src/ops/`
- [X] T021 [US2] Ops tests `musaic-app/tests/ops_agent_events.rs` with the injected clock: contracts/events.md tests 2 to 8 and 11, including reopen-the-database resume "a parked poll does not delay a concurrent tool call", and a stalled subscriber (parked polls never read) leaves a scan and an apply unslowed and holds no per-subscriber buffer (SC-007; measured by observed progress against a run with none attached, no fixed deadline)
- [X] T022 [US2] `musaic-api`: routes `events/list` and `events/poll` in `agent_guard.rs` (`ROUTES`) and `routes/agent.rs`, rendering in `render/`; rows in `docs/operations.md` (update its counts); `operations_inventory`, `thin_handlers` and guard tests pass

### Slice 4: Events over MCP

- [X] T023 [US2] `musaic-mcp/src/lib.rs` `Backend`: add `events_list`, `events_poll`, `observe_host`; implement them in `musaic-cli/src/client.rs` over the bridge (re-handshake on 401 as today; the client's read timeout for `events/poll` exceeds the 25 000 ms `waitMs` by a margin)
- [X] T024 [US2] Spike, then implement, `musaic-mcp/src/events.rs`: `on_custom_request` for `events/list` and `events/poll`; `events/subscribe` / `events/unsubscribe` → `Unsupported`; gate on modern profile and the declared client extension (`-32601` otherwise); map bridge errors to the JSON-RPC codes
- [X] T025 [US2] `events/stream` in `musaic-mcp/src/events.rs`: the loop of research D7 (`active`, `event`, `heartbeat`, re-`active` on truncation, `terminated`), subscription id in `_meta`, cancellation, the 8-stream cap. If rmcp 3.11.0 cannot hold a custom request open while sending notifications, do not declare `push`, set `delivery: ["poll"]` in the catalog, return `Unsupported` for `events/stream`, and record it in research.md and the compatibility note
- [X] T026 [US2] Harness tests in `musaic-mcp/tests/conformance.rs`: contracts/events.md tests 1, 2, 6 (two clients race a claim through a fake backend with an atomic claim), 7, 9 (cancel also leaves the outbox untouched, FR-025), 10
- [X] T027 [US2] End-to-end test over the real bridge in `musaic-api/tests/agent_events_bridge.rs`: pair, poll bootstrap, create work, poll, restart the server on the same database, resume with identical `eventId`s; revoke → refused

**Checkpoint**: `cargo test -p musaic-db -p musaic-app -p musaic-api -p musaic-mcp -p musaic-cli`

## Phase 5: User Story 3 - The listener sees what is connected and waiting (P2)

**Independent test**: spec US3 acceptance scenarios 1 to 6.

### Slice 5: host observations and diagnostics

- [X] T028 [US3] `musaic-db/src/agent_hosts.rs`: upsert/get/delete observation; delete on pairing revoke; tests
- [X] T029 [US3] `musaic-app/src/agent/hosts.rs` + `ops/agent.rs`: `observe_host` (bound and clean strings, throttle, publish `AgentStateChanged(Hosts)`), `session_status`, `diagnostics` (four facts with times, host block; no mode); ops tests `musaic-app/tests/ops_agent_hosts.rs`: never-connected pairing shows only paired; each step flips exactly one fact; revoked pairing
- [X] T030 [US3] `musaic-api`: bridge routes `session/observe`, `session` (`agent_guard.rs` `ROUTES`, `routes/agent.rs`); desktop route `GET /api/agent/diagnostics` (`routes/agent.rs`); inventory rows; tests
- [X] T031 [US3] `musaic-mcp/src/lib.rs` + `musaic-cli/src/client.rs` + `musaic-cli/src/doctor.rs`: send the observation when client info, profile or declared extensions are first seen or change; `doctor` prints the stored observation and subscription state for its pairing; tests
- [X] T032 [P] [US3] Frontend: `frontend/src/lib/api.ts` + `frontend/src/hooks/useAgents.ts` (`["agent","diagnostics"]`, `staleTime: Infinity`; no mutation handler calls `invalidateQueries`, rule 5), diagnostics block and the "Process pending Musaic work" disclosure in `frontend/src/pages/SettingsAgents.tsx`, copy in `frontend/src/lib/agentCopy.ts`; no "Ready" label; vitest in `frontend/src/pages/settingsAgents.test.tsx`
- [X] T033 [US3] `frontend/src/hooks/useLibraryEventsStream.ts`: map `hosts` → `["agent","diagnostics"]`, and refresh diagnostics on `work`; tests

### Slice 6: delegated work in Review

- [X] T034 [US3] `musaic-db/src/delegated_work.rs` desktop listing with holder, `last_heartbeat_at`, lease expiry, attempts, last error for the six states (reuse or replace the unused `desktop_list`); `musaic-app` `jobs.delegated_work` with `waitingForAgent` (queued and no pairing has an active subscription); ops test `musaic-app/tests/ops_jobs_delegated_work.rs` (each state; Missing albums still listed, nothing touches files; retry requeues exactly once)
- [X] T035 [US3] `musaic-api`: `GET /api/research/work` in `routes/jobs.rs`; inventory row; tests
- [X] T036 [P] [US3] Frontend: `frontend/src/components/DelegatedWork.tsx` (grouped by state, "Investigating" label, album link, holder / heartbeat / lease, "Waiting for an agent", Retry through the existing decision call), mounted in `frontend/src/pages/Review.tsx`; `frontend/src/lib/api.ts` call and hook `frontend/src/hooks/useDelegatedWork.ts` with key `["research","work"]` and `staleTime: Infinity`; Retry's mutation handler does not call `invalidateQueries` (rule 5); vitest incl. keyboard reachability of Retry

**Checkpoint**: targeted Rust tests; `cd frontend && npx vitest run src/pages/settingsAgents.test.tsx src/components/DelegatedWork.test.tsx src/hooks`

## Phase 6: User Story 4 - REMOVED: Musaic never launches an agent program (scope cut)

**Independent test**: contracts/launcher.md tests 1 to 8 with the stand-in program.

- [ ] ~~T037 [US4] `musaic-launch/src/adapter.rs`: `Adapter`, `Capabilities`, `LaunchSpec`, `Spawner`, `Child`, `ProbeResult`, the registry (only `claude-code`), a real `Spawner` over `std::process::Command` (argument array, `env_clear`, `current_dir`, piped output, no shell; `CREATE_NO_WINDOW` on Windows)~~ (removed: launcher cut from scope)
- [ ] ~~T038 [US4] `musaic-launch/src/supervisor.rs`: the pure state machine of contracts/launcher.md with injected clock, jitter and spawner; unit tests for every table row, backoff bounds and the three-failure stop~~ (removed: launcher cut from scope)
- [ ] ~~T039 [US4] `musaic-launch/src/claude_code.rs`: run `claude --help` (installed 2.1.x) and the documented flag list; implement `probe` (version) and `command` using only verified flags; write the verified flags and where each was checked into `specs/005-skills-events-agent-hosts/contracts/launcher.md`; `proven()` returns `false`; unit test of the exact argument array and the MCP config file content~~ (removed: launcher cut from scope)
- [ ] ~~T040 [US4] Stand-in agent test binary and integration tests `musaic-launch/tests/standin.rs`: contracts/launcher.md tests 2, 6 and 8 with real processes (cleared environment, fresh `cwd` removed afterwards, cancel ends the child, bounded cleaned output)~~ (removed: launcher cut from scope)
- [ ] ~~T041 [US4] `musaic-db/src/agent_launcher.rs`: launcher row and runs (keep 50, startup marks stale `running` as failed); tests~~ (removed: launcher cut from scope)
- [ ] ~~T042 [US4] `musaic-app/src/agent/launcher.rs` + `ops/agent.rs`: path + SHA-256 verification, `launcher_get/set/probe/retry/cancel`, the supervisor thread woken by the work family through a bus subscriber registered in `musaic-events/src/catalog.rs` and wired in `musaic-app/src/app.rs` (never called directly, rule 1) and by the tick, the "unheld queued work exists" query, revocation of the launcher pairing disables it, shutdown cancels the run before `stop_workers` returns; publish `AgentStateChanged(Launcher)`; ops tests `musaic-app/tests/ops_agent_launcher.rs` with a fake spawner: off by default spawns nothing; burst → one run; held work → no run; changed digest / missing file / unproven adapter refused; three failures (including spawn errors) stop; retry resumes~~ (removed: launcher cut from scope)
- [ ] ~~T043 [US4] `musaic-api/src/routes/agent.rs` (+ `render/`): launcher routes of contracts/desktop-routes.md; inventory rows; tests~~ (removed: launcher cut from scope)
- [ ] ~~T044 [P] [US4] Frontend: `frontend/src/components/AgentLauncher.tsx` in Settings → Agents (adapter, executable + Probe, pairing, switch disabled with reason for an unproven adapter, run status, diagnostics, Retry, Cancel, the "what is restricted" statement); hook `["agent","launcher"]` (`staleTime: Infinity`, no `invalidateQueries` in handlers, rule 5); vitest~~ (removed: launcher cut from scope)

**Checkpoint**: none (phase removed)

## Phase 7: Docs, evidence and polish

- [X] T045 [P] Write `docs/mcp.md`: profiles and negotiation, Skills (and the activation / integrity / consent distinction), Resources and prompt fallbacks, Events (types, cursors, truncation, at-least-once, error codes), one execution mode (the user connects their own agent host; Musaic starts no process) with host setup guides, `setup --skill` and `doctor`, the cloud phase (specified, not enabled, with its prerequisites), migration notes, the compatibility note from research.md; says "local agents supported"
- [X] T046 [P] Update `docs/agents.md`, `docs/operations.md` (rows and counts), and `CLAUDE.md` (crate map: no new crate; schema v23, event history, the new `AgentWhat::Hosts` family, the skill assets and their drift check)
- [X] T047 Security tests: malicious instruction text in album metadata and in a result does not appear in any event `data` and is not acted on by any server path (`musaic-app/tests/security_agent_events.rs`); no model credential or agent login value appears in settings, database, keyring entries or log after an events session (SC-011, FR-030, FR-035); no new listener is opened (FR-045); invalid manifest/lock; setup with a conflicting or malformed host config changes nothing
- [X] T048 Compatibility report `specs/005-skills-events-agent-hosts/evidence/compatibility.md`: harness table (modern, legacy) and real-host table (host, version, OS, SDK version, profile, skill activation, Tasks polling/input/cancel, Events mode, unattended wake); every cell a result, "unsupported" or "untested"; harness results never listed as a real host
- [X] T049 File a GitHub issue in `get2knowio/musaic` for each item left open (at least: real-host end-to-end evidence for the user-managed mode, if not run; reading the `ext-events` / `ext-skills` repositories against the implemented shapes; Windows/Linux skills-directory locations for other hosts); record the numbers here. Filed: #367 (real-host evidence for Skills and Events), #368 (read ext-events / ext-skills against the shapes), #369 (`setup --skill` for other hosts and OSes), #370 (stalled-subscriber test: add an apply)
- [X] T050 Run the full chain: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo run -p musaic-cli -- catalog --check`, frontend lint, both typechecks, vitest, build, and `cargo test -p musaic-desktop --test boot -- --ignored --nocapture`; record counts in `specs/005-skills-events-agent-hosts/evidence/checks.md`
- [X] T051 Walk `quickstart.md` steps 1 to 5 and 7 on a scratch config dir, offline; delete temp dirs and screenshots afterwards; note anything not verified
- [ ] ~~T052 [US4] Development-only `--dev-allow-unproven-adapters` (desktop, and `musaic-api` if the launcher is reachable there): refused at startup unless the config dir passes the `--dev-memory-secrets` check; threaded through the app builder as a boolean; with it `launcher_set` may enable an unproven adapter and `launcher_get` reports `unprovenAllowed: true` (UI shows the switch enabled with an "unproven adapter, development run" note; `proven()` stays false); tests: refused on a non-scratch dir, ops test refuses enabling without it and allows with it; document in `docs/mcp.md` and `contracts/launcher.md`~~ (removed: launcher cut from scope)

## Dependencies

- Phase 2 blocks everything. T003 before T008, T017 to T026. T005 before T017, T028, T041.
- US1: T006, T007 → T008 → T009 → T010; T008 → T011, T012 → T013; T008 → T015 → T016.
- US2: T017 → T018 → T019 → T020 → T021 → T022 → T023 → T024 → T025 → T026, T027.
- US3 needs T019 (in-memory facts) and T023 (observation call). T032, T036 are parallel with the Rust work of their slice once the route shapes are fixed.
- US4 (launcher) is removed from scope.
- T045 to T051 last; T050 after everything. T036 and T032 both edit `frontend/src/lib/api.ts`: do not run them in parallel.

## Requirement coverage

FR-001 to 005: T006 to T010. FR-006 to 011, 013: T011 to T014, T045. FR-012: T015, T016. FR-014 to 025: T003, T017 to T027. FR-026, 027: T032, T045. FR-028 to 035: T037 to T044, T047. FR-036, 037: T028 to T033. FR-038 to 042: T034 to T036, T033. FR-043 to 045: T045. FR-046 to 051: T009, T016, T031, T045 to T049. SC-013: T050.

Success criteria: SC-001, 002, 008: T013. SC-003: T021, T027. SC-004: T021. SC-005: T026. SC-006: T021, T027. SC-007: T021. SC-009: T029, T032, T036. SC-010: T042. SC-011: T047, T048. SC-012: T048. SC-013: T050.

Tasks with no requirement of their own: T001, T002 (setup), T014 (capabilities list, research D8), T049, T050, T051 (polish).
