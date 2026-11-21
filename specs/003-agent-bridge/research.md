# Research: Agent Bridge

Checked against main `00113c9b` and the published MCP documentation, 2026-11-20.

## R1. MCP profiles

- **Decision**: The server supports two profiles:
  - **Modern**: MCP 2026-07-28 (`server/discover`, per-request `_meta` with
    `io.modelcontextprotocol/protocolVersion` and `…/clientCapabilities`, `resultType:
    "complete"` on every result, `ttlMs` and `cacheScope` on `tools/list`).
  - **Legacy**: MCP 2025-11-25 (`initialize` / `notifications/initialized`).

  An `initialize` request switches the stdio process to legacy for its lifetime. A request
  with modern `_meta` is served statelessly. The server never exits on `server/discover`.
  Claude Code caches a "legacy only" verdict for 7 days when a server exits on the probe.
- **Rationale**: The published 2026-07-28 spec makes `server/discover` mandatory for servers
  and describes how dual-era servers behave (basic/versioning, transports/stdio). Claude
  Code's v2 runtime probes for 2026-07-28 and falls back. Claude Desktop's revision is not
  documented, so assume legacy.
- **Errors**: unknown tool or malformed params → JSON-RPC error (`-32602`); unsupported
  version → `-32022` with `data.supported`; domain failures → a `tools/call` result with
  `isError: true` and `structuredContent: {code, message}`.
- **Structured content**: the legacy profile requires an object, so every tool output is
  an object (lists are wrapped as `{items, nextCursor}`).
- **Alternatives**: modern only (rejected, Claude Desktop may not speak it); legacy only
  (rejected, the prompt pins the modern profile).

## R2. Rust MCP SDK

- **Decision**: `rmcp = "=3.11.0"`, `default-features = false`, features
  `["server", "transport-io"]`, confined to `musaic-mcp`. A test drives the stdio server
  with both handshakes before anything else builds on it. If 3.11.0 fails either profile,
  the fallback is a hand-written JSON-RPC adapter (about 600 lines, tools only) behind the
  same `musaic-mcp` API. Each of rmcp's gaps is put behind an adapter module with tests.
- **Rationale**: rmcp is the reference SDK: Apache-2.0, MSRV 1.88, with `server/discover`
  and `negotiate_initialize` in its changelog. tokio is already in the workspace.
- **Alternatives**: `rust-mcp-sdk` 2.0 and `pmcp` (smaller communities); hand-written from
  the start (we would have to track spec errata ourselves).

## R3. Pairing secret and mutual proof

- **Decision**:
  - **Secret and storage**: The secret is 32 random bytes (getrandom), stored in the OS
    keyring under service `musaic-agent`, account `<configDirId>:<pairingId>`.
    `configDirId` is the first 16 hex characters of SHA-1 of the resolved config dir,
    the same derivation as album ids. The app writes the entry when the user creates a
    pairing and deletes it on revoke. The CLI reads it.
  - **What the DB keeps**: `secret_fingerprint` = SHA-256(secret), used only to detect a
    keyring entry that no longer matches.
  - **Handshake**: `hello` returns `{instanceId, serverNonce}`. The CLI checks
    `instanceId` against the descriptor, then sends
    `exchange {pairingId, instanceId, serverNonce, clientNonce, clientProof}` where
    `clientProof = HMAC-SHA256(secret, "musaic-agent/v1/client" ‖ pairingId ‖ instanceId ‖
    serverNonce ‖ clientNonce)`. The app answers `{token, expiresAt, scopes,
    serverProof}` with the matching `…/server` label. The CLI uses the token only after
    `serverProof` verifies.
  - **Properties**: Nonces are single-use and expire after 60 s. The secret never crosses
    the wire, so a process squatting on the port learns nothing.
- **Rationale**: The constitution says secrets live only in the OS keyring, so no
  credential file is written. A challenge-response is the only way to authenticate the
  instance without revealing the secret.
- **Honest limitation** (goes in docs): every process running as the same OS user can
  read the keyring entry, so a pairing authenticates the OS user's processes, not one
  agent. On macOS the Keychain may ask once to allow `musaic-cli` to read the entry.
- **Dev and test runs**: with `--dev-memory-secrets` (desktop) or
  `--verification-memory-secrets` (server), which are already limited to temp and `work`
  dirs, the app also writes `<config>/agent-dev-credentials/<pairingId>` (mode 0600). The
  CLI reads it only with `--dev-credentials` and the same scratch-dir rule. In-process
  tests inject a memory `CredentialSource`.
- **Alternatives**:
  - An owner-only credential file in normal runs: rejected, it would break the
    keyring-only rule.
  - Sending the secret over loopback: rejected, a port squatter could capture it.

## R4. Access tokens and lockout

- **Decision**:
  - **Tokens**: 256-bit random, hex, in an in-memory map in `AgentHub`
    (`token → {pairingId, instanceId, scopes, expiresAt}`). They live 15 minutes, are sent
    as `Authorization: Bearer`, and are compared in constant time (the existing `ct_eq`
    pattern). The CLI renews by exchanging again 60 s before expiry. Revoking a pairing
    drops its tokens.
  - **Lockout**: one global counter of failed exchanges. Five failures in 60 s lock
    exchanges for 60 s, doubling up to 15 min; a success resets it.
  - **Testability**: all time comes from an injected clock.
- **Logging**: the `authorization` header is already scrubbed (`musaic-app/src/logging.rs`
  `SECRET_HEADERS`); a test proves it for agent routes.

## R5. Route family and guard

- **Decision**: `musaic-api` builds a separate `agent_router()` for `/__musaic/agent/v1/*`.
  In `router_with`, a path-prefix branch sends `/__musaic/agent/` to that router before
  `launch_guard` runs. The desktop gate (`musaic-desktop/src/gate.rs`) passes the prefix
  straight through.
- **`agent_guard`**, in order:
  1. The peer is loopback. The servers switch to
     `into_make_service_with_connect_info::<SocketAddr>()`.
  2. Exactly one `Host` header, equal to `127.0.0.1:<bound port>`.
  3. No `Origin` header, and no `Sec-Fetch-Site` other than `none`.
  4. `Content-Type: application/json` on requests with a body.
  5. Body at most 256 KiB.
  6. A cookie header alone grants nothing. A request with a launch cookie and no bearer
     token is refused like any other unauthenticated request.

  Then routes other than `hello` and `exchange` must carry a valid bearer token, which
  resolves to a `Principal`.
- **Desktop-facing management routes**: `/api/agent/*` (pairings, policy, approvals,
  activity) stay on the normal `/api` stack with its launch gate and Host/Origin check.
  Only the webview manages agents.
- **Rule 7**: every agent route is a thin handler that calls one `ops().agent().…`
  operation, with an inventory row in `docs/operations.md`. The `thin_handlers` test
  allow-list gains nothing new.

## R6. Read tools without side effects

- **Album list**: `library.list_albums(false)` returns the whole snapshot. The agent
  operation pages over it. The cursor is opaque base64 of `{offset, snapshotDigest}`; when
  the snapshot changes the cursor is refused with `CURSOR_STALE`. The optional `query` is
  a case-insensitive substring match on `display_artist(…, true)` and album title.
- **Album output**: only the id, folder name, display fields, tags, cover presence,
  missing flag, tracks (file names plus tags) and `revision`. Never `library_path`,
  `path`, `cover_file` or `sample_track`.
- **Resolution**: `library.resolution` can fingerprint and call MusicBrainz while holding
  the work guard. The agent read returns only what is already memoized or stored, through
  a new read-only accessor on the resolver memo (`musaic-services` production parts). It
  never resolves; with nothing stored it returns `{status: "not_resolved"}`.
- **Findings**: `checks.list_findings`, paged by the agent operation. The output has
  `{findingId, checkId, albumId, severity, title, status, firstSeen, lastSeen}`. The
  check-specific `detail` is left out because it may contain paths.

## R7. Revision digest

- **Decision**: `revision` = the first 32 hex characters of SHA-256 over the album's
  stored tag fields (canonical JSON of the album row minus volatile columns such as
  `scanned_at`), its cover presence and hash, and each FLAC's `(listed name, size, mtime
  in nanoseconds)` sorted by name. It is computed on read, never stored. The existing
  `album_digests` covers only the DB row, so file metadata is added on top.

## R8. Proposals, apply and rename

- **Proposal patch**:
  `{tags?: {album: {FIELD: value|null}, tracks: {fileName: {FIELD: value|null}}}, cover?:
  {source: "resolution"} | {source: "url", url}, rename?: true}`.
  - Field names: the existing editable tag vocabulary (`ApplyChanges` suggestions and
    clears). `null` means clear.
  - Track keys: file names from `album_get`.
  - URL covers: `https`, and the host must pass `check_public_url` at preview time (cheap
    check, no download).
- **Preview**:
  1. Validate the patch and check the base revision.
  2. Compute the diff with a dry-run apply (`ApplyChanges{dry_run: true}`); the existing
     prepare path never writes.
  3. Compute predicted rename targets by evaluating the templates on the patched tag
     values. New pure function `musaic-fsops::plan::render_targets(templates, values)`,
     extracted from `derive_plan` rather than duplicated.
  4. Store the proposal (`agent_proposals`) with `digest = SHA-256(canonical patch ‖
     albumId ‖ baseRevision)`.
- **Apply**, after the authorization decision:
  1. Revalidate: proposal unexpired, digest matches, pairing not revoked and still
     allowed, album present, revision unchanged, restore not running.
  2. Run `apply.apply_changes` with the patch (tags and cover) and collect per-file
     outcomes, with paths stripped to file names.
  3. If `rename` is set, call a new service `rename_album_to_templates(album_id)`. It
     factors the template-rename half of `check_fixes.rs:1396` (`derive_plan` +
     `execute_renames` + `AlbumRenamed`) out of its finding binding.
  4. The two steps run under one work guard taken by the operation. The rename step is
     skipped if the tag step reported errors.
- **Events**: the same as the desktop path, because the same services publish them.
- **Before-image (FR-054)**: the operation runs three steps between revalidation and
  the first write.
  1. Read the current tag values of the touched fields from each target file, using the
     same musaic-tags reader the dry run uses.
  2. Copy the current cover (the embedded picture of the first file, or the
     `cover.jpg` sidecar) to `<config>/agent-changes/<id>/`.
  3. Insert the `agent_changes` row with state `recorded`.

  If any of these fails, the operation refuses and writes nothing. After the apply, the
  operation updates the row's state and outcome. A test helper replays a before-image
  through the apply pipeline and proves the round trip (SC-009). A user-facing Undo
  belongs to a follow-up issue.

## R9. Operations registry and idempotency

- **Registry**: `AgentHub.operations` is an in-memory map
  `operationId → {pairingId, kind, state, counts, result, cancel flag}`, holding at most
  1,000 entries; finished ones are evicted oldest first.
- **Idempotency**: a map `(pairingId, key) → (inputDigest, operationId)`, kept for the
  app's run.
- **Workers**: background threads named `agent-op-*`. They are joined at shutdown through
  the existing `begin_shutdown` path, and refused once shutdown starts.
- **Scan**: `scan.start(false)` and `scan.cancel()`. If a scan is already running, the
  operation follows it.
- **Checks**: loop over check ids, calling `checks.run(id, scope, albums)` per check;
  cancellation is checked between checks.
- **Analysis**: loop over albums × kinds with `analyze.run`; cancellation is checked
  between albums.

## R10. Bus event and UI

- **Event**: `AgentStateChanged { what: Pairings | Approvals | Activity, ids: Vec<String> }`
  carries references only. Its producers are the `ops::agent` mutations and the expiry
  ticker.
- **Catalog**: add it to `CATALOG`, skip it for the sweep wake and the findings notify,
  relay it to SSE, and let `useLibraryEventsStream` invalidate `['agent', what]`.
- **Expiry**: approvals expire through an `AgentHub` ticker that runs every 30 s and
  publishes the event. At startup every pending card is marked expired ("app restarted").

## R11. Setup

- **Hosts**:
  - Claude Desktop: `claude_desktop_config.json`
    - macOS: `~/Library/Application Support/Claude/`
    - Windows: `%APPDATA%\Claude\`
    - Linux: `~/.config/Claude/`
  - Claude Code user scope: `~/.claude.json`.
- **Entry**: `mcpServers.musaic = {"type": "stdio", "command": <absolute CLI path>,
  "args": ["mcp"], "env": {}}`, plus `"--config-dir", <dir>` when the config dir is not
  the default.
- **Ownership**: an entry is ours when its `command` path's file name is `musaic-cli` (or
  `musaic-cli.exe`) and its args start with `mcp`. Anything else named `musaic` is
  foreign, and setup refuses.
- **Apply**:
  1. Parse the file; refuse it if it is malformed.
  2. Back up to `<file>.musaic-backup-<timestamp>`.
  3. Write a temp file in the same dir, then rename it.
  4. On any failure, restore every file already written from its backup.
- **Dry run**: prints the same plan as JSON.
- **Other hosts**: print the entry for manual addition.
- **`uninstall`**: removes only owned entries, with the same backup and rollback.
- **Testing**: every path takes a `HostPaths` value, so tests run against a temp home.

## R12. Packaging

- **Decision**: Tauri `bundle.externalBin: ["binaries/musaic-cli"]`. The build scripts
  compile `musaic-cli` for the target triple and copy it to
  `rust/crates/musaic-desktop/binaries/musaic-cli-<triple>[.exe]` (gitignored).
- **Install locations**:
  - macOS: `Musaic.app/Contents/MacOS/musaic-cli`
  - Windows: next to `Musaic.exe`
  - Linux deb: `/usr/bin/musaic-cli`
  - Linux AppImage: inside the image. `setup` refuses an AppImage-internal path and tells
    the user to copy the CLI out.
- **CI**: `release.yml` and `package_macos.sh` build the CLI before `cargo tauri build`.
  `rust-tests.yml` runs `cargo run -p musaic-cli -- catalog --check` after Clippy on all
  three OSes.
- **Not verifiable in this run**: an actual installer build on each platform. Reported as
  not verified.

## R13. Windows descriptor ACL

- **Decision**: no explicit ACL; see the plan's Complexity Tracking. The CLI still checks
  that the descriptor is a regular file, not a reparse point or symlink, and that its
  endpoint is loopback. A follow-up issue covers an explicit ACL.
