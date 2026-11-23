# Tasks
Each box requires implementation through a normal entry point and matching tests. The slices share the worktree. Coordinate registry, migration and restore hooks explicitly.

- [X] T001 [P] [US1/4] Add next SQLite migration in musaic-db, constrained queue, attempts, replay, results, grouped historical mapping, audit/outbox and epoch. Test repeated migration, each legacy state, unique active work and crash rollback.
- [X] T002 [P] [US1] Implement nine registered work tools, typed errors, scopes, normalized idempotency, leases, expiry, release, monotonic progress, ownership and cancellation through musaic-commands and app ops. Test separate principals/connections, fencing, stale context, restart and bounded retries.
- [X] T003 [US1/4] Route normal desktop job creation and deterministic preflight through delegated work. Preserve historical views and stop production provider execution. Verify ordinary no-agent scanning/resolution/checks.
- [X] T004 [US2] Implement typed results, evidence bounds, offline pending, current ID/ownership and cover checks. Connect review, preview, approval/discard, backup and stale handling. Test ambiguous album to approved file edit, grouped review, and stale or unsupported proposals.
- [X] T005 [P] [US3] Persist app-owned operations and Task mappings, owner checks, checkpoints, idempotency, expiry and safe restart outcome. Preserve confirmed partial writes. Cover crash before/after task creation, disconnect and terminal retention.
- [X] T006 [US3] Wire modern per-request Tasks through rmcp, authenticated bridge and durable app ops; keep fallback handles, input replay, cancellation acknowledgement and tool/transport error distinction. Add real profile fixtures and redacted transcripts.
- [X] T007 [US1/2/3] Integrate restore/root-switch invalidation, quit behavior, durable outbox and UI event refresh. Test restore and stopped bridge, scoped access, and safe cancellation.
- [X] T008 [US1/2/3/4] Update catalog, architecture and operations inventories, user docs and conformance evidence. Run targeted tests and required frontend/Rust checks. Record actual checks and limitations.
