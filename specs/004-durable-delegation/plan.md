# Implementation plan
Feature 004 targets the Rust 1.99.1 desktop workspace, SQLite, rmcp 3.11.0 and the existing React review interface. Implement these independently testable slices in order. Each slice includes its public-fixture tests.
1. Move work ownership into Musaic’s database. Migrate legacy jobs, retaining raw history, linked children, a single review group and a unique unresolved input. Transactions include the state transition, audit, outbox and replay record. Add fenced, principal-scoped tools through the existing operation registry and bridge. Exercise contention with separate connections and an injectable clock.
2. Route desktop enqueue and review through the same services. Extract deterministic preflight from the provider worker. Validate evidence against current album ownership, network policy and version, and use the existing apply pipeline with backup for an explicit desktop approval. On restored data or a root switch invalidate affected references before more work is handed out.
3. Persist operations where the agent hub currently uses memory, including final decisions, partial results and cancellation. Connect native Tasks to their operation through the authenticated bridge. Commit the handle before publishing it. Preserve the legacy polling path. Keep terminal Task retention separate from work and operation retention. Reconnect a fresh process to verify durability.
4. Integrate restore, quit, UI updates and the two protocol profiles. Exercise the production entry points, regenerate the tool catalog, update inventories and verify both desktop review and no-agent operation.

## Constitution check
I. Every file change requires an explicit desktop decision with a current preview, backup and normal apply semantics. Missing albums are skipped.
II. Maintain loopback pairing, scoped access, keyring and redirect-checked public HTTPS. Offline verification stays pending.
III. One operation per face. Every state transition commits first, then publishes a reference event; UI refreshes on those events.
IV. Snapshot identities, epoch, generation and input digest are checked against current state. Reject or require a fresh preview after a mismatch.
V. Use synthetic audio and fake clocks. Run fmt, Clippy, Rust tests, frontend lint, both TypeScript checks, Vitest and the production build.
VI. Review, progress, retry and shutdown controls report meaningful status and are keyboard accessible.
