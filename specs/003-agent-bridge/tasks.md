# Tasks: Agent Bridge — Shared Tools, stdio CLI and Secure Desktop Bridge

**Input**: `specs/003-agent-bridge/` (plan.md, spec.md, research.md, data-model.md, contracts/, quickstart.md)

**Tests**: These are required. The constitution (V) and spec acceptance call for tests on
every FR. Every new operation needs an ops test, and every new route needs a row in
`docs/operations.md`.

**Slices** (from plan.md): S1 is the contract (Phase 2a), S2 is the pairing backbone
(Phase 2b), and S3 to S7 map to the user-story phases.

## Path conventions

All Rust paths below are relative to `rust/crates/` unless they start with `rust/`,
`frontend/`, `docs/` or `.github/`. Run Rust commands from `rust/`.

Each slice must end with
`cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`
and the slice's targeted tests passing.

---

## Phase 1: Setup

- [X] T001 Create crate `musaic-commands` (lib, edition 2024, `[lints] workspace = true`; deps serde, serde_json, schemars 1, sha2 0.11, hmac (matching sha2 0.11), getrandom 0.4, thiserror) in `musaic-commands/Cargo.toml` and `musaic-commands/src/lib.rs`
- [X] T002 [P] Create crate `musaic-mcp` (lib; deps musaic-commands, `rmcp = { version = "=3.11.0", default-features = false, features = ["server", "transport-io"] }`, tokio rt + io-std, serde_json) in `musaic-mcp/Cargo.toml` and `musaic-mcp/src/lib.rs`
- [X] T003 [P] Create crate `musaic-cli` (bin `musaic-cli` + lib; deps musaic-commands, musaic-mcp, clap 4 derive, ureq 3.5 with default features off, keyring 4.2, serde_json with `preserve_order`, getrandom, sha2) in `musaic-cli/Cargo.toml`, `musaic-cli/src/main.rs` and `musaic-cli/src/lib.rs`
- [X] T004 Add a dependency-boundary test `musaic-cli/tests/no_db_owner.rs`. It parses `cargo metadata` (as `musaic-app/tests/ops_boundary.rs` does) and asserts that musaic-cli's dependency closure contains none of `musaic-db`, `rusqlite`, `musaic-app` or `musaic-api`, and that musaic-commands' closure contains none of `musaic-db`, `musaic-app`, `axum` or `tokio`

## Phase 2a: Foundational: contract and catalog (S1)

- [X] T005 [P] Define `PermissionKind` (`library.read`, `proposal.submit`, `compute.scan`, `compute.analyze`, `tags.write`, `covers.write`, `files.rename`), `Decision` (`allow`, `ask` or `refuse`; Refuse is strongest), the defaults per FR-032 and `decide(scopes, policy, kinds)` ("strictest over kinds; kind ∉ scope ⇒ Refuse") in `musaic-commands/src/kinds.rs`, with unit tests
- [X] T006 [P] Define `ToolErrorCode` (the stable strings listed in contracts/tools.md, tool errors and transport errors) and `ToolError {code, message, details?}` in `musaic-commands/src/errors.rs`
- [X] T007 Define typed Input and Output structs for all 12 tools in `musaic-commands/src/tools/{capabilities,library,album,resolution,findings,fix,scan,checks,analysis,operation}.rs`. Requirements:
  - every input uses `#[serde(deny_unknown_fields)]`;
  - bounds come from contracts/tools.md: limit 1..100 (default 50), query ≤ 200, id lists ≤ 500, checkIds ≤ 20, kinds 1..2, idempotencyKey 8–128 chars from `[A-Za-z0-9._:-]`, tag values ≤ 1000 chars, cursor ≤ 512;
  - every output is an object;
  - each tool has a `validate()` that enforces the bounds and returns `INVALID_INPUT` with the field name.
- [X] T008 Implement the registry `ToolDef {id, cli_name, title, description, kinds, annotations{read_only, destructive, idempotent, open_world}, input_schema(), output_schema()}` and `REGISTRY` (12 entries, the order of contracts/tools.md), plus the `NOT_OFFERED` list (FR-005), in `musaic-commands/src/lib.rs`
- [X] T009 Implement catalog rendering in `musaic-commands/src/catalog.rs`:
  - `catalog_json()`: deterministic, with key order fixed;
  - `catalog_markdown()`;
  - `registry_digest()`: SHA-256 of `catalog_json`.
- [X] T010 [P] Implement the auth wire types and proofs in `musaic-commands/src/auth.rs`:
  - `HelloResponse` and `ExchangeRequest`/`ExchangeResponse`;
  - `client_proof` and `server_proof`: HMAC-SHA256 with the labels `musaic-agent/v1/client` and `musaic-agent/v1/server` over `pairingId‖instanceId‖serverNonce‖clientNonce`, length-prefixed;
  - a constant-time `verify`.

  Unit tests cover known vectors, swapped labels failing, and any changed field failing.
- [X] T011 [P] Implement the descriptor in `musaic-commands/src/descriptor.rs`:
  - `Descriptor` (fields per contracts/descriptor.md, `deny_unknown_fields`);
  - `write_atomic_owner_only(dir, &Descriptor)`: tmp file, mode 0600 on Unix, fsync, rename, fsync of the dir on Unix;
  - `remove_if_own(dir, instance_id)`;
  - `read_validated(dir) -> Result<Descriptor, DescriptorError>`, applying the checks of the reader table in order (symlink via lstat, regular file, Unix owner == euid and no group/other bits, ≤ 4 KiB, formatVersion 1, host `127.0.0.1`, port 1..65535).

  On Windows, the reader rejects reparse points. Use std only, no unsafe: on Unix use `std::os::unix::fs::MetadataExt` and compare uid with the owner of a freshly created temp file to learn the euid without libc.
- [X] T012 Add tests in `musaic-commands/tests/`:
  - `schemas.rs`:
    - every input schema has `additionalProperties: false`, and every output schema has type object;
    - unknown fields are rejected;
    - bounds are enforced;
    - annotations match kinds (read-only exactly when kinds == {library.read} and the tool is not cancel).
  - `descriptor.rs`: symlink, other-owner (skipped unless root is available, documented), mode 0644, oversize, bad version, non-loopback host, bad port, and round trip.
- [X] T013 Implement `musaic-cli catalog --check | --write [--dir <docs dir>]` in `musaic-cli/src/catalog_cmd.rs`. It writes `docs/agent-tools.json` and `docs/agent-tools.md`; `--check` exits 1 on drift and prints which file differs. Generate and check in `docs/agent-tools.json` and `docs/agent-tools.md`.
- [X] T014 Add test `musaic-cli/tests/catalog_check.rs`: the checked-in `docs/agent-tools.*` equal the generated output (SC-005), and a modified copy in a temp dir fails the check.

**Checkpoint S1**: `cargo test -p musaic-commands -p musaic-cli` passes, then fmt and clippy are clean.

## Phase 2b: Foundational: pairing backbone (S2)

- [X] T015 Add schema v21 in `musaic-db/src/migrate.rs`:
  - `V21_STATEMENTS` creating `agent_pairings`, `agent_policies`, `agent_proposals`, `agent_approvals`, `agent_changes` and `agent_activity` with the columns and constraints of data-model.md;
  - `v20_to_v21`, with `v19_to_v20` changed to set 20;
  - `upgrade()` gains `if found <= 20`;
  - module docs updated;
  - `SCHEMA_VERSION = 21` in `musaic-db/src/lib.rs`.
- [X] T016 Update the schema tests and goldens for v21 as a deliberate change ("adds agent tables for the agent bridge"): `musaic-db/tests/fresh_db.rs` (deltas, "[16] -> [21]"), `rust/testdata/golden/persistence/fresh-db.json`, `musaic-db/tests/old_schemas.rs`, `python_written_db.rs`, `snapshot_scenario.rs`, `musaic-api/tests/schema_upgrade_copy.rs`, `musaic-services/tests/disc_backfill.rs`. Run `cargo test -p musaic-db -p musaic-api -p musaic-services`.
- [X] T017 Implement the SQL module `musaic-db/src/agent.rs`:
  - pairings: insert; list; get; revoke (sets `revoked_at` once); count active; touch `last_used_at`;
  - policies: get map; set partial;
  - proposals: insert; get; prune older than 7 days;
  - approvals: insert; list by state; decide (`UPDATE … WHERE state='pending'` returns whether it changed); `expire_all_pending(reason)`; expire due;
  - changes: insert recorded; finish(state, outcome); prune older than 90 days returning ids;
  - activity: insert, keeping the last 100 per pairing; list.

  Add unit tests in the same file.
- [X] T018 Add `AgentStateChanged { what: Pairings | Approvals | Activity, ids: Vec<String> }` to `musaic-events/src/event.rs` (`Event`, `EventKind`, names). Register it in `musaic-events/src/catalog.rs` `CATALOG`: relayed to SSE, skipped by the check-sweep wake and the findings notify, and no rerun subscriber. Extend `musaic-events/tests/` with `agent_state_changed.rs`, asserting it wakes no sweep and triggers no rerun.
- [X] T019 Add keyring support for agent secrets in `musaic-secrets/src/agent.rs`:
  - `agent_account(config_dir_id, pairing_id)`;
  - the `AGENT_SERVICE = "musaic-agent"` constant;
  - `config_dir_id(resolved_path)`: first 16 hex chars of SHA-1, the same as the album-id derivation in `musaic-core/src/album_id.rs`; reuse that function rather than duplicating it.

  Thread a second `SharedSecrets` for the agent service through `AppBuilder` in `musaic-app/src/app.rs`. It is a memory store when the desktop or server runs with memory secrets (`musaic-desktop/src/backend.rs:41`, `musaic-api/src/main.rs:472`).
- [X] T020 Implement `AgentHub` in `musaic-app/src/agent/mod.rs`, with an injected `Clock` trait (real and fake):
  - `instance_id` (`i_` + 16 hex, created at build);
  - nonces: 60 s, single-use;
  - tokens: 15 min, constant-time lookup, revoke-by-pairing;
  - lockout: 5 failures in 60 s ⇒ 60 s lock, doubling up to 15 min, reset on success;
  - operations registry: at most 1000 entries, evicting finished ones oldest first;
  - idempotency map.

  Add it to `AppInner`, and at startup call `expire_all_pending("app restarted")` and the prunes.
- [X] T021 Write the descriptor at server start and remove it at shutdown:
  - desktop: in `musaic-desktop/src/lib.rs` `boot` next to `lock.publish(port, focus)` (≈:430), and remove in `teardown` (≈:489) via `remove_if_own`;
  - standalone: in `musaic-api/src/main.rs` after `local_addr` (≈:568), and remove after serve ends (≈:585).

  Use `musaic_commands::descriptor`. `protocolProfiles` = `["mcp-2026-07-28","mcp-2025-11-25"]`.
- [X] T022 Add the `ops::agent` group (`Agent<'a>`) in `musaic-app/src/ops/agent.rs` and register it in `musaic-app/src/ops/mod.rs`. Pairing operations, each publishing `AgentStateChanged` where it mutates:
  - `create_pairing(name, scopes)`: name 1–64 chars trimmed; scopes non-empty and a subset of the 7; refuse when 20 pairings are active; 32-byte secret to the agent keyring only (plus `<config>/agent-dev-credentials/<id>` at 0600 when memory secrets are on); fingerprint SHA-256 into the DB; returns the connect hint `{command, args}` and never the secret;
  - `list_pairings`;
  - `revoke_pairing`: drops tokens, deletes the keyring entry and dev file;
  - `set_policy`;
  - `activity`;
  - `hello`;
  - `exchange`: verify the proof against the keyring secret and the fingerprint; on any failure return a uniform `NOT_PAIRED`, counting toward the lockout; `LOCKED_OUT` carries `retryAfterSeconds`; on success issue a token, touch `last_used_at` and log activity;
  - `authenticate(token) -> Principal`.

  Use the record types in `ops/record.rs` (add them to `RECORD_TYPES` in `musaic-app/tests/ops_boundary.rs`). No serde_json or wire words in `ops/agent.rs`.
- [X] T023 Add `musaic-app/tests/ops_agent.rs` (required by `ops_all_groups`). It uses a fake clock, a temp config dir and memory secrets, and covers:
  - create, list and revoke;
  - the 21st pairing refused;
  - the secret only in the store and never in any record or DB column;
  - the exchange happy path, a wrong proof, a replayed nonce, an expired nonce, another instance id, a revoked pairing;
  - lockout timings (5, then 60 s, then 120 s, …, capped at 15 min, reset);
  - token expiry at 15 min;
  - restart (a new AgentHub) invalidates tokens while the pairing survives;
  - two config dirs are isolated (FR-029);
  - the `AgentStateChanged` publishes.
- [X] T024 Implement `agent_guard` in `musaic-api/src/agent_guard.rs` per contracts/agent-http.md, in order:
  1. loopback peer, via `ConnectInfo<SocketAddr>`;
  2. exactly one Host equal to `127.0.0.1:<bound port>`;
  3. no Origin, and `Sec-Fetch-Site` absent or `none`;
  4. body at most 256 KiB;
  5. JSON content type on bodies;
  6. bearer token, except for hello and exchange.

  Switch `musaic-api/src/loopback.rs:113` and `musaic-api/src/main.rs:579` to `into_make_service_with_connect_info::<SocketAddr>()`. Branch `/__musaic/agent/` to `agent_router()` before `launch_guard` in `router_with` (`musaic-api/src/lib.rs:478-504`), and let `musaic-desktop/src/gate.rs` pass the prefix through.
- [X] T025 Add thin routes in `musaic-api/src/routes/agent.rs`: `POST /__musaic/agent/v1/hello`, `POST /__musaic/agent/v1/exchange`, `GET/POST /api/agent/pairings`, `DELETE /api/agent/pairings/{id}`, `PUT /api/agent/pairings/{id}/policy` and `GET /api/agent/pairings/{id}/activity`. Map `OpError` and agent errors to statuses per contracts/agent-http.md. Add rows to `docs/operations.md` (10 columns; update the row counts at :56-65) and to `EXTRA_ROUTES` in `musaic-api/tests/inventory.rs`.
- [X] T026 Add `musaic-api/tests/agent_guard.rs` covering:
  - hostile Host (`evil.com`, `localhost:<port>`, a wrong port, two Host headers);
  - any Origin, including `null` and a loopback origin;
  - `Sec-Fetch-Site: cross-site`;
  - a launch cookie alone on agent routes (refused);
  - 300 KiB body → 413;
  - `text/plain` → 415;
  - no or invalid bearer → 401;
  - existing `/api` routes still enforce the launch gate and Host/Origin unchanged (FR-027);
  - an `authorization` header never appears in the process log (FR-028).

**Checkpoint S2**: `cargo test -p musaic-db -p musaic-events -p musaic-app -p musaic-api` passes, and `ops_boundary`, `ops_all_groups`, `thin_handlers` and `operations_inventory` are green.

## Phase 3: User Story 1: an agent reads the library (P1, S3) 🎯 MVP

**Goal**: A paired CLI and MCP server lists and reads albums, resolutions and findings through the running app.

**Independent test**: quickstart §2 up to `library-list`. The CLI album ids equal the desktop's `/api/albums` ids.

- [X] T027 [US1] Add a read-only accessor for a stored or memoized resolution, which never resolves, takes no work guard and does no network or DB write, in `musaic-services/src/production.rs` (resolver memo) and `musaic-services/src/library_ops.rs` (`stored_album_resolution`). Unit test: calling it on an unresolved album returns None and leaves the DB unchanged.
- [X] T028 [US1] Add `musaic-services/src/agent_read.rs` with:
  - `album_summary` and `album_detail` (no `library_path`, `path`, `cover_file` or `sample_track`; artist via `display_artist(…, true)`, rule 4);
  - `revision_digest(conn, album_id, library_dir)`: the first 32 hex chars of SHA-256 over canonical row fields (excluding `scanned_at`), cover presence and hash, and sorted `(listed name, size, mtime ns)` of each FLAC (FR-051);
  - paging helpers (cursor = base64 `{offset, snapshotDigest}`, `CURSOR_STALE` on mismatch).

  Unit tests: any path field leaking fails the test; the revision changes on a tag change, a file rename, a size or mtime change, and a cover change, and is stable otherwise.
- [X] T029 [US1] Add the read tool operations on `ops::agent`: `capabilities`, `library_list`, `album_get`, `resolution_get` and `findings_list`. The finding output has `{findingId, checkId, albumId?, severity, title, status, firstSeen, lastSeen}`, with `detail` excluded.
  - In `musaic-commands`, add the typed enums `ToolCall` (one variant per tool, holding its Input) and `ToolOutput` (one variant per tool, holding its Output), plus `parse_call(tool_id, body) -> Result<ToolCall, ToolError>` (unknown tool → `NOT_FOUND`; serde or `validate()` failure → `INVALID_INPUT`).
  - Add the dispatcher `call_tool(principal, ToolCall) -> Result<ToolOutput, ToolError>`, which:
    1. decides per kinds;
    2. records activity: every start, apply and authorization refusal goes to `agent_activity` and to one process-log line `agent pairing=<name> tool=<id> outcome=<…>`, with no argument values (FR-052);
    3. dispatches.

  Everything is in `musaic-app/src/ops/agent.rs`, with no `serde_json` there, so the `ops_boundary` allow-list stays unchanged. Also add `list_tools`.
- [X] T030 [US1] Add routes `POST /__musaic/agent/v1/tools/{toolId}` and `GET /__musaic/agent/v1/tools` in `musaic-api/src/routes/agent.rs`. The route parses the body with `musaic_commands::parse_call`, calls `ops().agent().call_tool`, and renders `ToolOutput` or `ToolError`: 404 for an unknown tool, 400 `INVALID_INPUT` for schema failures, 200 `{ok, result|error}` otherwise. Add the inventory rows in `docs/operations.md` and `musaic-api/tests/inventory.rs`.
- [X] T031 [US1] Extend `musaic-app/tests/ops_agent.rs` with the read tools on the synthetic library (`musaic_testdata::synthetic_library()`, copied to a temp dir):
  - `library_list` ids equal `library.list_albums` ids (SC-001);
  - limit bounds, paging and query;
  - the cursor goes stale after a rescan;
  - `album_get` has no path anywhere, checked with a recursive scan of the JSON for `/` or `\` in values other than bare names (FR-007);
  - `resolution_get` → `not_resolved` with no network and no work guard taken (assert `gate.running()` is empty during the call) (FR-053);
  - `findings_list` excludes `detail`;
  - each output validates against its schema (jsonschema dev-dep) (FR-006);
  - an unpaired, revoked or expired principal and a scope without `library.read` are refused (SC-002).
- [X] T032 [US1] Implement the CLI client in `musaic-cli/src/client.rs`:
  - discovery (`read_validated`);
  - `hello` with a 2 s timeout ⇒ `APP_NOT_RUNNING` on refused or timeout or instance mismatch;
  - `exchange` with a client nonce; verify `serverProof` before using the token, else `INSTANCE_MISMATCH`;
  - renew 60 s before expiry; on a 401, retry once.

  Use ureq against `http://127.0.0.1:<port>` with exactly that Host and no Origin.
- [X] T033 [US1] Implement `CredentialSource` in `musaic-cli/src/credentials.rs`:
  - `OsKeyring`: service `musaic-agent`, account `<configDirId>:<pairingId>`;
  - `DevFile`: only with `--dev-credentials` and a config dir under the system temp dir or a dir named `work`, mirroring `musaic-api/src/verification.rs` `scratch_dir_ok`; refused otherwise;
  - `Memory`: tests.

  Pairing selection: `--pairing`, else the only one with a credential, else `NOT_PAIRED`.
- [X] T034 [US1] Implement the CLI commands `tools`, `call <cli-name>` and the `<cli-name>` aliases with `--input`, `--input-file` or stdin, in `musaic-cli/src/main.rs`:
  - one JSON document on stdout;
  - exit codes 0, 2 (tool error), 3 (transport), 64 (usage);
  - the `APP_NOT_RUNNING` remedy text from contracts/cli.md;
  - no secret in argv: there is no flag that takes a secret.
- [X] T035 [US1] Implement the MCP adapter in `musaic-mcp/src/lib.rs`:
  - an rmcp `ServerHandler`; `list_tools` from `REGISTRY`, in deterministic order; `call_tool` → a `Backend` trait (implemented by the CLI client);
  - domain errors → `isError: true` with `structuredContent {code, message}`; unknown tool → `-32602`;
  - both profiles: modern `server/discover` with `resultType`, `ttlMs` and `cacheScope`, and legacy `initialize`;
  - never exit on `server/discover`.

  Put any rmcp gap behind `musaic-mcp/src/profile.rs` with tests. If rmcp 3.11.0 cannot serve both profiles, replace it with a hand-written JSON-RPC adapter behind the same API and record why in research.md.
- [X] T036 [US1] Implement `musaic-cli mcp`: build a tokio runtime, serve stdio, send all logs and diagnostics to stderr, exit 0 on EOF.
- [X] T037 [US1] Add `musaic-mcp/tests/profiles.rs`, driving the server over in-memory duplex pipes with a fake Backend:
  - legacy `initialize` → `initialized` → `tools/list` → `tools/call`;
  - modern `server/discover` → `tools/call` with `_meta`, plus a missing `_meta` → `-32602`;
  - an unsupported version → `-32022`;
  - unknown tool → protocol error;
  - domain error → `isError`;
  - envelopes never mixed after `initialize`;
  - `structuredContent` validates against `outputSchema`.
- [X] T038 [US1] Add `musaic-cli/tests/end_to_end.rs`, starting `musaic-api`'s router in-process on an ephemeral port with a temp config dir, memory secrets and the synthetic library copy, writing the descriptor, and running the CLI lib with a Memory credential source. Cases:
  - `library-list` equals the desktop list (SC-001);
  - a missing descriptor → `APP_NOT_RUNNING`;
  - a stale descriptor (port closed, wrong instanceId) → `APP_NOT_RUNNING`;
  - a symlinked descriptor → `DESCRIPTOR_INVALID`;
  - a squatter server that answers hello with the right instanceId but cannot produce `serverProof` → `INSTANCE_MISMATCH`, with the squatter's received bodies asserted to contain no secret (edge case);
  - a restart (new instanceId) → the CLI rediscovers;
  - another config dir's credential → `NOT_PAIRED`.
- [X] T039 [US1] Add a stdout-purity test in `musaic-cli/tests/stdout_purity.rs`: spawn the built `musaic-cli mcp` binary (`env!("CARGO_BIN_EXE_musaic-cli")`) against a running in-process server. Every stdout line must parse as JSON-RPC (SC-006), including when the app is missing, where the error goes to stderr and stdout stays empty. Also assert that no secret or token appears in stdout, stderr or the server log.
- [X] T040 [US1] Implement `doctor` in `musaic-cli/src/doctor.rs` per contracts/cli.md: descriptor status, PID alive (a hint only), pairing status, scopes from `capabilities`, transport `stdio`, profiles, and extensions (empty). It prints no secret. Test it in `musaic-cli/tests/doctor.rs`: healthy, no app, revoked pairing, and output free of secrets.

**Checkpoint S3 / MVP**: `cargo test -p musaic-commands -p musaic-mcp -p musaic-cli -p musaic-app -p musaic-api` passes.

## Phase 4: User Story 2: propose a fix and the user approves it (P1, S4)

**Goal**: preview, then apply, with Allow, Ask or Refuse; revalidation; a before-image; the same events as a desktop edit.

**Independent test**: quickstart §2 from `fix_preview` on.

- [X] T041 [US2] Extract a pure `render_targets(templates, values) -> RenameTargets` from `derive_plan` in `musaic-fsops/src/plan.rs` (no behaviour change; existing plan tests stay green). Add unit tests.
- [X] T042 [US2] Factor `rename_album_to_templates(album_id)` (derive_plan + execute_renames + publish `AlbumRenamed`, skip Missing) out of the finding-bound path at `musaic-services/src/check_fixes.rs:1396` into `musaic-services/src/library_ops.rs`, keeping the fix path calling it. The existing check-fix tests stay green.
- [X] T043 [US2] Implement proposal logic in `musaic-services/src/agent_proposal.rs`:
  - validate the patch: non-empty; FIELD in the editable vocabulary; value ≤ 1000 chars or null; fileName among the album's files; cover `resolution` (the album has an anchored cover candidate) or an `https` URL whose host passes `musaic-net` `check_public_url`; rename only `true`;
  - derive kinds;
  - compute the diff by dry-run apply (`ApplyChanges{dry_run: true}`) plus `render_targets`;
  - effects;
  - `digest = SHA-256(canonical patch ‖ albumId ‖ baseRevision)`.
- [X] T044 [US2] Implement the before-image (FR-054) in `musaic-services/src/agent_changes.rs`:
  - read the prior values of the touched fields per target file with the musaic-tags reader;
  - read the prior names when renaming;
  - copy the prior cover bytes (the embedded picture of the first file, or the `cover.jpg` sidecar) to `<config>/agent-changes/<id>/cover.<ext>` byte for byte;
  - insert an `agent_changes` row with state `recorded`.

  Any failure returns an error before a write. Also add `finish(state, outcome)` and the 90-day prune, which removes the directories.
- [X] T045 [US2] Add ops `fix_preview` and `fix_apply` on `ops::agent`.
  - **fix_preview**: requires `proposal.submit`. Refuses a Missing album and `STALE_REVISION`. Stores an immutable proposal that expires after 30 min. Never writes to the collection.
  - **fix_apply**: requires the proposal's kinds.
    - Checks idempotency; the same key with different input → `IDEMPOTENCY_CONFLICT`.
    - Then decides:
      - Refuse → `FORBIDDEN`;
      - Ask → an approval card (expiry 30 min) plus an operation in `awaiting_approval`, publishing `AgentStateChanged(Approvals)`;
      - Allow → run.
    - **Run**: take the work guard for Apply, then:
      1. revalidate per FR-036: proposal unexpired, digest, pairing not revoked and still allowed, album present, same-file identity of each target, revision unchanged, restore not running;
      2. record the before-image;
      3. `apply.apply_changes`;
      4. if `rename` is set and there were no tag errors, `rename_album_to_templates`;
      5. strip paths from the outcomes to file names;
      6. finish the change record;
      7. log activity.

  Code goes in `musaic-app/src/ops/agent.rs` and `musaic-app/src/agent/run.rs`.
- [X] T046 [US2] Add ops `list_approvals` and `decide_approval(id, approve)`, which only the desktop route reaches. On approve, run the waiting operation, again revalidating everything. On deny, set state `denied` and write nothing. Add the expiry ticker (30 s), which marks due cards `expired` and publishes. Add `operation_get` and `operation_cancel`: own operations only; an apply is cancellable only while `awaiting_approval`, which also marks its card `denied` with reason "cancelled by assistant".
- [X] T047 [US2] Add desktop routes `GET /api/agent/approvals`, `POST /api/agent/approvals/{id}/approve` and `POST /api/agent/approvals/{id}/deny` (409 when not pending) in `musaic-api/src/routes/agent.rs`, with inventory rows in `docs/operations.md` and `musaic-api/tests/inventory.rs`.
- [X] T048 [US2] Extend `musaic-app/tests/ops_agent.rs` for proposals and apply, on a temp copy of the synthetic library and fake clock. Each of these cases runs with FLAC SHA-256 digests taken before and after, proving 0 files changed (SC-003, FR-037):
  - preview writes nothing;
  - Ask creates a card and returns `awaiting_approval`;
  - read tools answer while a card is pending (FR-039, SC-008);
  - deny;
  - expiry after 30 min;
  - Refuse policy;
  - scope missing a kind while the policy says Allow (FR-033);
  - digest mismatch;
  - stale revision after an outside tag change, after an outside file rename, and after the album goes Missing;
  - restore running;
  - pairing revoked between Ask and approve;
  - the agent cancelling while awaiting;
  - restart expiring a pending card ("app restarted").

  Also:
  - Approve applies, and the published bus events equal those of the same `apply.apply_changes` call made directly as a desktop edit (SC-004, FR-044).
  - A URL cover goes through the public-HTTPS check (offline transport ⇒ a reported failure, no write).
  - A rename produces `AlbumRenamed`.
  - Idempotency: same key and input returns the same handle; same key with other input → `IDEMPOTENCY_CONFLICT`.
  - There is no tool that can approve.
- [X] T049 [US2] Add `musaic-app/tests/agent_changes_roundtrip.rs` (SC-009):
  - for tag-only, cover and rename applies, a test helper replays the `agent_changes` before-image through `apply.apply_changes` and `rename_album_to_templates`, then asserts every touched field, name and the cover bytes equal their prior state;
  - a failed before-image write (read-only `agent-changes` dir) refuses the apply with 0 files changed.

**Checkpoint S4**: `cargo test -p musaic-fsops -p musaic-services -p musaic-app -p musaic-api` passes.

## Phase 5: User Story 3: an agent starts scans, checks and analysis (P2, S5)

**Goal**: handles, idempotency, policy and cooperative cancel for compute.

**Independent test**: start a checks run with compute set to Allow, poll it to completion, and compare the findings with a desktop-started run.

- [X] T050 [US3] Add ops for the three start tools in `musaic-app/src/ops/agent.rs` and `musaic-app/src/agent/run.rs`. All three apply idempotency and the policy (Ask ⇒ an approval card with action `scan_start`, `checks_start` or `analysis_start` and a request digest). They run on `agent-op-*` threads, are refused after `begin_shutdown`, and are joined at shutdown.
  - **`scan_start`**: `configuredRootId` must equal `config_dir_id(resolved library dir)`; otherwise `NOT_FOUND`, and with no library `NO_LIBRARY`. Calls `scan.start(false)`. If a scan is already running, follows it. Cancel → `scan.cancel()`.
  - **`checks_start`**: for each check id (default: all, in registry order), calls `checks.run(id, scope, albums)`. Cancel between checks; counts are per check.
  - **`analysis_start`**: requires both `compute.analyze` and `tags.write`. Loops over albums × kinds with `analyze.run`, skips Missing albums, and cancels between albums.

  Each start and its outcome is recorded in `agent_activity` and as a process-log line (FR-052).
- [X] T051 [US3] Extend `musaic-app/tests/ops_agent.rs` for the compute starts:
  - `checks_start` findings equal a direct `checks.run` result;
  - the same key returns the same handle, and other input → conflict;
  - cancel mid-run ends `cancelled` with partial counts, observed through progress rather than timing;
  - a scan already running → the handle follows it;
  - `analysis_start` with `tags.write` set to Refuse → `FORBIDDEN` and no files changed;
  - a wrong `configuredRootId` → `NOT_FOUND`;
  - `operation_get` of another pairing's operation → `NOT_FOUND`;
  - `operation_cancel` on a finished operation → `already_finished`.
- [X] T052 [US3] Add `capabilities.configuredRoots: [{rootId, name}]` (the name is the folder's base name, not its path) to the capabilities output in `musaic-commands/src/tools/capabilities.rs`. Regenerate `docs/agent-tools.*`.

**Checkpoint S5**: `cargo test -p musaic-app -p musaic-commands -p musaic-cli` passes, and `catalog --check` passes.

## Phase 6: User Story 4: the user manages pairings and policies (P2, S6)

**Goal**: the Settings → Agents section, and approval cards in the Review inbox.

**Independent test**: Vitest specs, plus a manual check in the real app (scratch config, offline).

- [X] T053 [P] [US4] Add `frontend/src/hooks/useAgents.ts` with:
  - the queries `useAgentPairings`, `useAgentApprovals(state)` and `useAgentActivity(id)`, all `staleTime: Infinity`;
  - the mutations create, revoke, setPolicy, approve and deny, which never call `invalidateQueries` (rule 5);
  - API types in `frontend/src/lib/api` following the existing pattern.
- [X] T054 [US4] In `frontend/src/hooks/useLibraryEventsStream.ts`, handle the `AgentStateChanged` listener by invalidating `['agent', what]`, and include it in `resyncAllBusCaches`.
- [X] T055 [US4] Add the Settings section `agents` to `frontend/src/lib/settingsSections.ts` and `frontend/src/pages/Settings.tsx`. The new `frontend/src/pages/settings/AgentsSection.tsx` follows contracts/desktop-ui.md:
  - the empty state;
  - the pair form, with read and propose checked by default;
  - connect instructions, never showing the secret;
  - the pairing list, with a policy selector per kind (disabled outside the scope), Revoke with a confirmation, and activity;
  - the credential-holder note.

  Keyboard operable, with labels.
- [X] T056 [US4] Add the `approval` item kind in `frontend/src/hooks/useReview.ts` (`ReviewItem` union, `buildReviewItems`, `reviewBadge` counts, `SourceStatus` for the approvals source) and render it in `frontend/src/pages/Review.tsx`: plain-language text, the diff label, time left, and Approve and Deny buttons. Opening an item has no side effect.
- [X] T057 [P] [US4] Add Vitest specs:
  - `frontend/src/pages/settings/AgentsSection.test.tsx`: create, policy disabled outside the scope, revoke confirmation, no secret rendered;
  - `frontend/src/hooks/useReview.test.ts`: approvals counted in the badge, unavailable vs empty;
  - `frontend/src/hooks/useLibraryEventsStream.test.ts`: the `AgentStateChanged` invalidation;
  - `frontend/src/pages/settingsNavigation.test.tsx`: the new section.
- [ ] T058 [US4] Verify in the real app (the `run` skill; `cargo run -p musaic-desktop -- --config-dir "$(mktemp -d)/config" --dev-memory-secrets --network offline`): pair, set a policy, drive an approval through `musaic-cli --dev-credentials`, approve it in the Review inbox, and revoke. Delete the screenshots and temp dirs afterwards.

**Checkpoint S6**: `npm run lint && npm run typecheck && npm run typecheck:test && npm test && npm run build` pass, and `cargo test -p musaic-api` passes.

## Phase 7: User Story 5: setup and doctor (P3, S7)

**Goal**: safe host configuration and a bundled CLI.

- [X] T059 [US5] Implement `setup` and `uninstall` in `musaic-cli/src/setup.rs` per research R11 and contracts/cli.md:
  - `HostPaths` injectable (Claude Desktop per OS, Claude Code `~/.claude.json`);
  - detection;
  - an entry is owned when the file name of `command` is `musaic-cli[.exe]` and the args start with `mcp`;
  - `--dry-run` writes nothing;
  - back up to `<file>.musaic-backup-<timestamp>`;
  - write a temp file then rename it;
  - preserve key order (`preserve_order`);
  - refuse malformed JSON or a foreign `musaic` entry;
  - roll back every file on failure;
  - refuse an AppImage-internal executable path, with a remedy;
  - `skills: []`;
  - for other hosts, print the manual entry.
- [X] T060 [US5] Add `musaic-cli/tests/setup.rs` using a temp HOME:
  - dry run changes nothing (byte compare);
  - add preserves unrelated entries byte for byte (SC-007);
  - update of an owned entry;
  - a foreign entry → refuse, nothing changed;
  - malformed → refuse;
  - an interrupted setup (an injected write failure on the second file) restores both files exactly;
  - uninstall removes only the owned entry;
  - the backup exists.
- [X] T061 [US5] Bundle the CLI:
  - add `bundle.externalBin: ["binaries/musaic-cli"]` to `musaic-desktop/tauri.conf.json`;
  - gitignore `musaic-desktop/binaries/`;
  - build `musaic-cli --release --target <triple>` and copy it to `binaries/musaic-cli-<triple>[.exe]` in `scripts/package_macos.sh` (before the Cargo.toml save, ≈:41) and in `.github/workflows/release.yml` (before `cargo tauri build`, ≈:225);
  - make the desktop dev build (`cargo build -p musaic-desktop`) not require the sidecar, or have `musaic-desktop/build.rs` create a placeholder only for non-bundle builds, so `cargo test --workspace` stays green.
  - Follow-up (#350): the staging step is now one script, `scripts/stage_cli_sidecar.sh`, and every installer build ends with `scripts/check_bundled_cli.sh` (the CLI at its documented place, `--version` and `doctor` run; the AppImage's `setup` refusal). release.yml and the new `.github/workflows/bundle-check.yml` share the composite action `.github/actions/desktop-bundle`. The Windows CLI sits next to `musaic-desktop.exe`, not `Musaic.exe`; docs corrected.
- [X] T062 [US5] Add a CI catalog check: in `.github/workflows/rust-tests.yml`, after Clippy (≈:195), add the step `cargo run -p musaic-cli --locked -- catalog --check`.

**Checkpoint S7**: `cargo test -p musaic-cli` passes, `cargo build -p musaic-desktop` works, and `scripts/package_macos.sh` produces an app with `Contents/MacOS/musaic-cli` (run it if time allows; otherwise report it as not verified).

## Phase 8: Polish and cross-cutting

- [X] T063 [P] Write `docs/agents.md`:
  - what an agent can and cannot do (link the generated catalog);
  - pairing;
  - `musaic-cli setup`, `doctor` and the manual entry;
  - token lifetimes (15 min), renewal, revocation; restart invalidates tokens, pairings survive;
  - approval cards and their 30 min expiry; a restart expires pending cards;
  - before-images under `<config>/agent-changes/`, kept 90 days;
  - the credential-holder limitation (FR-030);
  - macOS Keychain prompt;
  - PATH differences for GUI-launched hosts: always use the absolute path that setup writes;
  - per-platform CLI locations;
  - the AppImage note.
- [X] T064 [P] Add an "Agent access" subsection to `SECURITY.md` after "The local API" (≈:60): the route family and guard, mutual proof, the keyring service `musaic-agent`, lockout, no paths, the Windows descriptor note, and the limitation. Update "Secrets" (≈:87) for the second service name.
- [X] T065 [P] In `docs/desktop.md`, document `agent-instance.json` and `agent-changes/` in Data locations (≈:239), the descriptor write and remove in Start-up and Teardown, and the bundled CLI in Packaging (≈:275).
- [X] T066 [P] Update `CLAUDE.md`:
  - crate map rows for `musaic-commands`, `musaic-mcp` and `musaic-cli`;
  - rule 1: `AgentStateChanged` in the event list;
  - rule 7: agent routes are a face over `ops::agent`, and the CLI never links the db;
  - the security posture: the agent route family;
  - commands: `musaic-cli catalog --check`.
- [X] T067 Check that every new route has its `docs/operations.md` row and that the row counts are updated. Run `cargo test -p musaic-api --test operations_inventory --test thin_handlers --test inventory`.
- [X] T068 File follow-up issues with `gh issue create` in get2knowio/musaic, applying the public-text rule, and record each number here:
  - (a) "Explicit owner-only ACL for agent-instance.json on Windows";
  - (b) "Undo for applied changes, using the agent before-image records, also for the user's own edits";
  - (c) "Record a real MCP host connection (Claude Desktop / Claude Code) with versions and a secret-stripped transcript", only if T069 is not done;
  - (d) "Cancellation inside checks runs and analysis (services), beyond item boundaries";
  - (e) "Installer verification of the bundled musaic-cli on Windows and Linux".
- [X] T069 (done for #348: Claude Code 2.1.330 connected headless to `musaic-cli mcp` on a scratch config, once per profile, negotiating 2026-07-28 by default and 2025-11-25 with `MCP_PROTOCOL_NEGOTIATION=legacy`; Claude Desktop, an approval round trip and the Keychain prompt were not recorded, as the evidence file says) Optional live evidence: if a real host is available, connect it against a scratch config dir and save a secret-stripped transcript to `specs/003-agent-bridge/evidence/host-connection.md`. Otherwise mark this task skipped and rely on T068(c).
- [X] T070 Full gate:
  - `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`;
  - the frontend lint, typecheck, typecheck:test, test and build;
  - `cargo test -p musaic-desktop --test boot -- --ignored --nocapture`, since the desktop changed;
  - `cargo run -p musaic-cli -- catalog --check`.

  Record the counts.

---

## Dependencies

- Setup (T001–T004) comes first, then S1 (T005–T014), then S2 (T015–T026). Both are blocking.
- US1 (T027–T040) needs S1 and S2. It is the MVP.
- US2 (T041–T049) needs US1's dispatcher (T029) and routes (T030).
- US3 (T050–T052) needs T029 and T046 (operations and approvals).
- US4 UI (T053–T058) needs the S2 routes and T047. It can start after T047, in parallel with US3.
- US5 (T059–T062) needs only S1 and the CLI skeleton (T032–T034). It can run in parallel with US2 to US4.
- Polish comes last. T068 can be filed at any time.

## Parallel opportunities

- T002 and T003 after T001.
- T005, T006, T010 and T011 are in separate files.
- S2: T018 and T019 alongside T015–T017.
- US1: T035–T037 (musaic-mcp) in parallel with T027–T031 (app side).
- US4 frontend (T053, T057) in parallel with the US3 backend.
- US5 (T059–T060) in parallel with US2.
- Docs T063–T066 in parallel.

## Implementation strategy

Deliver the MVP (through Phase 3) first: an agent can read the library safely. Then add
US2, the reason to connect an agent, then compute, UI, setup and packaging. After each
checkpoint the workspace is green.

## Follow-up issues filed (T068)

- (a) #346 Explicit owner-only ACL for agent-instance.json on Windows
- (b) #347 Undo for applied changes, using the agent before-image records (also for the user's own edits)
- (c) #348 Record a real MCP host connection (Claude Desktop / Claude Code) with versions and a secret-stripped transcript
- (d) #349 Cancellation inside check runs and analysis (services), beyond item boundaries
- (e) #350 Installer verification of the bundled musaic-cli on Windows and Linux. Addressed by `bundle-check.yml` and the check at the end of every release build (see T061); macOS verified locally with `scripts/package_macos.sh`. The issue closes once `bundle-check` has passed on all three platforms.
