# Durable delegation and native asynchronous MCP Tasks
**Feature Branch**: `004-durable-delegation`
**Created**: 2026-11-22
**Input**: Prompt 2 · Durable delegation & native async MCP Tasks

## Clarifications

### Session 2026-11-22
- What happens to valid claims if the desktop quits? The persisted lease and its original expiry survive startup. The closed bridge prevents further progress until the app is running.
- Can host input approve a proposed edit? Only a desktop decision on the exact reviewed proposal authorizes a collection change.
- What does a host without Tasks see? An ordinary tool result with a durable operation ID, state and suggested polling interval.
- How are unverifiable results handled offline? Keep a visible pending verification and allow verification to resume against the same current context.
- How should interrupted writes resume? Surface confirmed partial effects and require a new preview for remaining writes.

## User Scenarios & Testing

1. **P1 · Delegate an ambiguous album.** A listener requests research after local resolution cannot settle an album. Two paired agents see it, one claims it, and the listener receives one reviewable proposal even if the agent retries after a dropped response. Test by racing claims and restarting between completion and retry.
2. **P1 · Review research before applying.** The listener sees evidence and the exact current diff, independently approves or discards it, and can recover through the ordinary backup path. Test an offline lookup, a stale album revision, and a successful approved apply.
3. **P1 · Follow a long operation.** A host starts a scan or analysis and reconnects after a restart. It can retrieve the actual terminal outcome and request cancellation. Run the same scenario with a Tasks-capable host and a legacy host.
4. **P2 · Upgrade a library with old jobs.** A listener retains pending research, applied/discarded history and grouped metadata and cover requests across upgrade and repeated startup. Test each legacy state and a repeated migration.

## Requirements

### Key entities and lifecycle boundaries
1. Domain WorkRequest: Musaic has research to delegate; it exists before any agent connects and outlives protocol sessions.
2. Operation: Musaic is executing a bounded command (scan, analysis, validation, apply); durable progress and result ownership live in the app.
3. MCP Task: a protocol handle for one client-requested operation, exposed only when the request declares Tasks support. It is not the external agent's reasoning session and must not recursively wait for that same caller to perform research.

An Events occurrence announces domain work, not a CreateTaskResult pushed without a request. musaic_work_claim and musaic_work_complete normally return quickly. A long completion-validation step may create an operation and Task; the accepted proposal remains a separate artifact.

## Work schema v1

Add a delegated_work table and supporting attempts, results and idempotency records through the next schema migration; do not hardcode a schema version from these notes. Proposed fields:

| Field | Contract |
| --- | --- |
| workId | Opaque UUID; existing legacy ID retained separately |
| schemaVersion | 1 |
| kind | metadata.identify, metadata.correct, cover.research |
| albumId, libraryEpoch, baseRevision | Server snapshot identity; epoch changes on restore/library switch |
| state | queued, claimed, running, awaiting_review, applied, discarded, failed, cancelled |
| inputJson, inputDigest | Bounded immutable work context, schema-validated; no keys or default audio upload |
| createdAt, updatedAt, availableAt | Server timestamps |
| principalId, leaseTokenHash, leaseExpiresAt, claimGeneration | Owner, secret lease verifier and monotonic fencing generation |
| attempts, maxAttempts, lastError | Bounded retries; proposed default maxAttempts 3 |
| resultId, legacyJobId | Validated result and migration linkage |

Store work_attempts with generation, claimant and terminal reason; work_results with result digest, evidence and validation findings; idempotency records scoped to principal, operation and key with normalized input digest. Same key+same payload returns the original result; same key+different payload returns IDEMPOTENCY_CONFLICT.

Initial lease 5 minutes, renew every 60 seconds while actively working, cap each renewal to 5 minutes. Use the app's injectable clock. Lease renewal is a distinct operation from progress. A restart preserves unexpired leases; a sweep requeues expired attempts with incremented generation. Never run the old blanket reset-stuck-running over delegated jobs.

Transitions: queued → claimed → running → awaiting_review → applied/discarded. Claimed/running may release to queued or fail/cancel. An expired attempt requeues unless maxAttempts reached, then fails. Review is not an agent lease: once a valid result is accepted into awaiting_review the lease is cleared. queued → cancelled is allowed. Only a desktop-authorized action can retry failed/cancelled work. Application state transitions append an audit record and durable outbox row in the same transaction.

Atomic claim is a database transaction/CAS on eligible state and availability. Every lease-bound write verifies authenticated principal, workId, generation, lease token, expiry, libraryEpoch and allowed transition. A stale worker cannot overwrite a newer result. Concurrent agents may observe the same event, but only one claim succeeds.

## Work tool contract

All are registered application tools, not MCP methods.

| Tool | Required fields and semantics |
| --- | --- |
| musaic_work_list | state/kind filters, cursor?, limit 1..100; returns authorized summaries, no lease secrets |
| musaic_work_get | workId; returns versioned context, input digest and result schema reference |
| musaic_work_claim | workId, idempotencyKey; returns workId, claimGeneration, leaseToken, leaseExpiresAt, inputDigest |
| musaic_work_renew | workId, claimGeneration, leaseToken; returns renewed expiry |
| musaic_work_progress | lease fields, monotonic sequence, stage, completed?, total?, bounded message; stale sequence is ignored |
| musaic_work_complete | lease fields, idempotencyKey, inputDigest, result; validates, stores proposal, transitions to awaiting_review |
| musaic_work_fail | lease fields, code, message, retryable; app decides retry/backoff, agent cannot set policy |
| musaic_work_release | lease fields, reason; gives work back without creating a result |
| musaic_work_cancel | workId, expectedRevision; only pairing with cancellation scope and allowed user policy |

Unknown work returns NOT_FOUND without leaking another principal's work. Distinguish ALREADY_CLAIMED, LEASE_EXPIRED, STALE_GENERATION, STALE_CONTEXT, INVALID_RESULT, RESTORING, POLICY_DENIED and RATE_LIMITED as typed tool-domain failures. Do not reuse HTTP codes as JSON-RPC method errors.

## Result schema and acceptance

Result envelope: schemaVersion=1, workId, kind, inputDigest, outcome=proposal|no_match|needs_input, evidence[], confidence, uncertainties[], recommendation. confidence is finite 0..1 and never substitutes for evidence. Each evidence record carries source URL/identifier, retrievedAt, claim and relevant limitations. Bound evidence count to 50 and result size to 256 KiB. recommendation is a tagged union:

- Metadata: MusicBrainz release-group/release/recording identifiers with explicit type, album fields and per-track changes keyed to Musaic track IDs; no arbitrary file path or SQL. Never treat a release-group as a specific pressing.
- Cover: candidate HTTPS URL, source page, dimensions if known, rationale and rights/usage uncertainty; Musaic fetches and validates the asset itself.
- no_match or needs_input: explanation and a concrete unresolved question; do not invent IDs or convert ambiguity to a confident match.

Validate schema, UUID syntax, IDs and referenced album/track ownership; verify MusicBrainz identifiers with existing network services; check stale revision/epoch before storing acceptance. Cover fetching reuses Musaic's public-HTTPS restrictions and validates every redirect, byte limit, image type and decoded pixel dimensions. Result URLs/text remain untrusted data. Offline verification remains pending rather than silently accepted.

A valid proposal uses existing fix-preview/diff machinery. Agent completion never changes FLAC bytes, sets an anchor or applies art automatically. A successful apply or discard is a separate desktop decision bound to the exact proposal. Preserve deterministic resolver operations without an agent.

## Migration from historical AI jobs

Observed [job types/statuses](https://github.com/get2knowio/musaic/blob/adaa4b1883e51be38d1d79bc67ae4fdcf284b22f/rust/crates/musaic-db/src/ai_jobs.rs#L22-L50), [startup/completion transitions](https://github.com/get2knowio/musaic/blob/adaa4b1883e51be38d1d79bc67ae4fdcf284b22f/rust/crates/musaic-db/src/ai_jobs.rs#L284-L340), [acted-on semantics](https://github.com/get2knowio/musaic/blob/adaa4b1883e51be38d1d79bc67ae4fdcf284b22f/rust/crates/musaic-services/src/ai_jobs.rs#L227-L255) and [worker](https://github.com/get2knowio/musaic/blob/adaa4b1883e51be38d1d79bc67ae4fdcf284b22f/rust/crates/musaic-ai/src/worker.rs) are the migration baseline.

Preserve ai_jobs as historical records and write a unique legacyJobId migration mapping. queued and interrupted running become queued delegated work. Existing completed results become reviewable only after conversion/validation; preserve raw legacy data and surface unsupported results as legacy history. acted_on preserves applied/discarded, timestamps and audit. failed stays failed until user retries. Split metadata+cover into linked metadata.identify and cover.research work with a shared requestGroupId; preserve one parent review card. Retry of migration must not duplicate children. Maintain one unresolved request per album/kind/inputDigest, including awaiting_review.

Remove model execution from the production worker path. Extract useful deterministic resolver/synthesis preflight into normal services: resolve locally first and enqueue only unresolved research. AppBuilder must not spawn the old agent provider worker for migrated jobs. Retain the provider trait only if existing compatibility tests require it during transition; production must ship no LLM network dependency or credentials.

## Async operation and MCP Task architecture

Persist operations and mcp_tasks within the same app-owned database, under current restore/write rules. operations carry operationId, kind, principalId, idempotencyKey, state, progress, cancellability, checkpoint/result and libraryEpoch. mcp_tasks maps taskId to operationId and principal, timestamps, retention and profile version. Task IDs are opaque and authorization-checked on every call; a bridge disconnect does not cancel app work.

Modern adapter:

- Check io.modelcontextprotocol/tasks in each request's capabilities before returning a task-shaped result.
- The server decides whether a long tools/call returns immediately or a durable handle; never emit a handle before it is queryable.
- Implement tasks/get, tasks/update and tasks/cancel exactly as documented. Final output is available through tasks/get. Persist protocol state/results across bridge and app restart.
- Preserve protocol states working, input_required, completed, failed, cancelled. A completed tool result with isError=true remains protocol completed; reserve failed for JSON-RPC execution failure.
- Retain unique inputRequests keys; accept inputResponses through tasks/update without double-applying responses. Desktop approval remains authoritative; host input cannot manufacture a desktop approval grant.
- Cancellation acknowledgement means request received, not work stopped. Stop at safe boundaries, keep confirmed partial effects and report actual terminal outcome.
- Advertise and serve notifications/tasks through negotiated subscriptions/listen only when implemented; polling remains available.
- Proposed retention: ttlMs=null for active work, terminal results retained 7 days after completion. Document allowed protocol TTL semantics, cleanup and TASK_EXPIRED recovery through operationId; do not delete domain work/results with Task cleanup.

Operations use durable checkpoints where supported. An interrupted scan/analysis may restart only after proving idempotence and revalidating root/epoch. Never replay file writes blindly; interrupted apply returns a reviewable partial outcome requiring re-preview for unfinished changes.

Fallback clients receive a normal tool result containing an operationId, state and suggested polling interval; musaic_operation_get/cancel provides the same lifecycle. Never block minutes to simulate async and never return CreateTaskResult to an unnegotiated client. Short reads, claim/renew/progress and ordinary completion remain immediate.

## Restore, shutdown and event correctness

Do not hold a work guard across hours of external reasoning. Persist each queue transaction using existing app/DB gates; acquire mutation guards for real validation/apply work only. On restore or library switch, increment libraryEpoch and invalidate/review affected claims, proposals and operation handles. Prevent restored outbox rows from masquerading as new work under a previous epoch. Quit handling reports local operations separately from externally claimed work; app closure leaves claims to expire safely. UI must state that delegation cannot progress through a stopped desktop bridge.

## Success criteria and acceptance tests
- Two agents race for one work item: exactly one claim; former generation completion fails after expiry/reclaim.
- Duplicate completion survives retry and restart without duplicate proposal, anchor update or file write.
- Inject crashes after enqueue, outbox insertion, claim, result commit and task creation; recovery preserves queue/outbox consistency.
- Migration preserves every legacy state and combined-job association; second migration is a no-op; completed continues to block duplicate unresolved work.
- Modern client sees nonblocking scan/analysis handles, polls after restart, supplies input once, and observes cooperative cancellation accurately.
- Legacy client uses identical operations via application handles; unnegotiated Tasks responses never occur.
- Restoring during active work, library change, epoch mismatch, unauthorized task access, hostile covers, invalid evidence and stale proposal are tested.
- The no-agent path completes scanning, deterministic resolution and checks without a model provider.
- Verify an ambiguous album: enqueue → claim → investigate → submit → review → approved apply with backup, with agent credentials absent from Musaic.
- Run repository-required Rust/UI checks in isolated fixtures. Record actual SDK/profile/client versions and redacted transcript.

Deliver state-machine diagrams, schema definitions, error catalog and conformance fixtures alongside implementation. Track domain work and Task IDs separately in diagnostics without logging lease tokens.

## Dependencies and scope
The paired stdio bridge and registered application tools from spec 003 are available on main. The modern protocol contract follows the [published Tasks documentation](https://modelcontextprotocol.io/extensions/tasks/overview). External-host wakeups and distribution of agent Skills are separate work.
