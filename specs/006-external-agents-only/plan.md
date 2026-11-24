# Implementation Plan: External agents only (remove the built-in model provider)

**Branch**: `006-external-agents-only` | **Date**: 2026-11-23 | **Spec**: [spec.md](spec.md)
**Input**: Feature specification from `specs/006-external-agents-only/spec.md`

## Summary

Make the delegated path of specs 003–005 the only way research is done. Fold the old `ai_jobs` table into the delegated-work store at schema v24 (a `research_history` table beside `delegated_work`, linked by the existing `legacy_job_id`), make requests follow their album through rename, adoption and merge, and make delegated work block merges and conversions. Let bulk Fix all turn unsettled albums into one batch of requests (a `batch_id` on each request, a small `work_batches` origin row), exposed to agents through a batch filter, a 25-item claim and a multi-lease renew, and to the listener as one entry with counts and a cancel. Then delete the provider, its worker, queue switch, routes, settings and keys, remove the desktop's model gating, apply the wording table, collapse `AiWording`, and update the docs.

## Technical Context

**Language/Version**: Rust 1.99.1 (edition 2024, `unsafe` forbidden); TypeScript / React
**Primary Dependencies**: axum, rusqlite, `rmcp =3.11.0`, schemars, TanStack Query. No dependency is added; `musaic-ai`'s `replay` feature is removed.
**Storage**: SQLite, schema v23 → v24 (one migration, `v23_to_v24`); keyring entries deleted once
**Testing**: `cargo test --workspace`; vitest; fixture databases at v16 (committed), v20, v22, v23 (new, committed, synthetic); `catalog --check`
**Target Platform**: macOS, Linux, Windows desktop; loopback only
**Project Type**: desktop app (Rust workspace + web UI)
**Performance Goals**: a 1 000-item batch claimable in 40 calls (SC-003); a batch is O(pages) relay events, not O(items) (research D12)
**Constraints**: the migration runs on real user databases after the startup snapshot; no new listener; the CLI links no database crate; resolver-only quick-fix frames stay byte-identical
**Scale/Scope**: batches up to thousands of items; a removal touching ~13 crates and ~40 frontend files

## Constitution Check

*GATE: passed before research; re-checked after design (below the same list).*

- **I. The library is the user's.** Pass. The v24 step reads no album and deletes no album row; rows whose album is gone are kept. The startup snapshot (v17–v23) or the `.before-rust` backup (≤ v16) is taken before it, and restore migrates a staged copy first (research D3). Bulk delegation skips Missing albums before the resolver runs; nothing new writes a file. Requests move with an album id in the same transaction as the album row; the two existing paths that drop an album row are unchanged.
- **II. Local-first, secure by default.** Pass. Nothing calls a model or starts a program; no listener is added (FR-033). Retired keys are deleted from the keyring once and their names stay on every scrub list (D14); nothing logs or returns them. Agent routes and the guard are unchanged; the two new tools use the existing bridge route and the research scope. Agents still get references only (batch ids are opaque UUIDs; events carry no text).
- **III. One operations surface, events for fan-out.** Pass. Three new routes, each one operation with an inventory row and an ops test; four routes and their operations removed with their rows. Every request of a batch commits its audit, outbox and event-history rows in its own transaction, as a single request does; a bulk run still publishes `BulkBatchCompleted` once; batch cancel publishes after commit. The re-derivation after a rename is a bus subscriber on `AlbumRenamed`, not a call from the rename path. The UI refreshes from `AgentStateChanged(work)`; mutation handlers invalidate nothing. Boundary tests keep passing (see Slices).
- **IV. Derive from current state.** Pass. Batch counts and "unfinished" are derived, never stored. A batch item re-derives its input when its album is renamed; a held item's result is checked against current state as in spec 004. History rows are shown as stored, labelled as history.
- **V. Test-backed, deterministic.** Pass. Each slice names its tests; fixture databases are synthetic and copied before opening; goldens that change are listed with the reason (Goldens). Claim races use separate connections; clocks are injected.
- **VI. Plain, accessible interface.** Pass. The wording table replaces plumbing words; a batch is one keyboard-reachable entry with a cancel and a way into its items; the Fix all choice is a labelled checkbox with the count in scope.

No violation to justify.

## Design decisions (detail and rejected alternatives in [research.md](research.md))

1. **History table, not new columns** (D1): `research_history` holds every old job row verbatim; requests link by `legacy_job_id`; unknown types are history-only records.
2. **v24 step** (D2, [data-model.md](data-model.md)): one version-gated transaction; pin v22→v23 to the literal 23 first; never edit `delegated_work.sql`. The per-row v22 mapping is extracted in place as `musaic_db::delegated_work::link_job` (`pub(crate)`; no new `legacy` module: `migrate_legacy` lives in `delegated_work.rs` and keeps calling it, v24 calls it with the existing epoch).
3. **Migration safety** (D3, D4): existing snapshot, restore and newer-file refusal; new committed fixtures at v20, v22, v23 via a `#[doc(hidden)] upgrade_to` seam.
4. **Follow the album** (D5, D6): `delegated_work` and `research_history` join `ALBUM_ID_HOLDERS` and `move_album_rows`; merge and conversion refuse on unresolved requests and repoint finished ones; queued items re-derived on `AlbumRenamed`.
5. **Batch = column + origin row** (D7); **bulk delegation through an app-supplied callback** (D8), opt-in per run (`research=1`); signature settled below ("Settled interfaces").
6. **Two new tools** (D9) `musaic_work_claim_many`, `musaic_work_renew_many`; `batchId` on summaries, a list filter; `work_replays` widened.
7. **Additive `batchId` on `.v1` events** (D10); skill 1.1.0 (D11).
8. **One desktop entry per batch** (D12); coalesced `AgentStateChanged`; **Clear counts requests** (D13).
9. **Settings and keys** (D14); **`musaic-ai` shrinks, keeps its name** (D15); **`/api/ai/jobs` keeps its shape** (D16); **four routes removed** (D17); **wording in the UI slice, `AiWording` collapse last** (D18).

## Slices

Each slice leaves the whole workspace green (ciLocal: fmt, clippy `-D warnings`, `cargo test --workspace`, frontend lint, both typechecks, vitest, build; `catalog --check`) and is reviewable alone. The order differs from the brief in one place: the frontend stops calling provider routes (slice 3) **before** Rust deletes them (slice 4), so no slice ships a UI that calls a missing route.

### Slice 1 — v24 and the single store

What: pin v22→v23 to `23`; `v23_to_v24` and `research_v24.sql` (data-model); `musaic_db::research_history` module (get, list, delete, repoint, the `JobRow` row type, `JOB_TYPES`, `LIST_LIMIT`, and a `#[doc(hidden)] import_legacy_job` seam for test harnesses); `delegated_work::link_job` extracted from `migrate_legacy` and shared; move `record_mb_lookup_failure` and `mark_metadata_ai_acted_on` to `albums.rs`; every `ai_jobs` reader moved (data-model, "Reads that move"): `desktop_research` (`group`, `children`, `jobs`, `hydrate`), `research/legacy.rs`, `ops/jobs.rs` (`get`, `active_for_album`, `delete`, `delete_many`, `queue_state`), `ops/apply.rs::job_cover_preview`, `research_jobs::queue_counts`, `backup.rs` counts, `no_cover_art.rs`, `inputs.rs` (`INPUTS_VERSION` → `check-inputs/3`), `merge.rs`, `multi_disc.rs` (`active_jobs`, `set_blockers`), `renames.rs` (`move_album_rows`, `ALBUM_ID_HOLDERS`, the doc table), `multi_disc_gather.rs` report; `delegated_work::unresolved_for_albums`; the `AlbumRenamed` re-derivation subscriber for queued items (registered by `musaic-app`, see "Settled interfaces"); `QueueState` gains `clearable` (research D13), rendered in the jobs stream's `queue_state`; `ops::jobs::{retry, mark_acted_on}` stop calling `musaic_services::ai_jobs` (a group id or a history record answers `Conflict`, anything else `NotFound`) so no kept route reaches the dropped table. The worker cannot run against a v24 database (it reads and writes `ai_jobs`), so this slice also cuts the legacy path off at the top: `legacy_jobs` is forced false (the builder setters stay, inert, until slice 4), the worker and job-log thread are no longer started, and the legacy enqueue branches (`ops/jobs.rs:206-211`, `ops/fixes.rs:108-118`) are removed so every enqueue goes through `desktop_research::enqueue`. The tests that drive the worker or read the `ai_jobs` table through the old queue are deleted here rather than in slice 4: `musaic-ai/tests/{worker_loop, golden_worker, no_provider_wording}.rs`, `musaic-api/tests/{jobs_worker_http, ai_decisions, jobs_http}.rs` (`jobs_http.rs` is one replay that compares `ai_jobs` rows after every step; a new `research_jobs_http.rs` covers the kept routes over the single store), `musaic-db/tests/ai_jobs.rs`, `musaic-services/tests/ai_jobs_acted_on.rs`; `golden/ai/worker.json` goes with them. `golden/ai/http.json` is **kept**: it records the Python app, is no longer replayed against routes, and still supplies 9 of the 11 422 byte samples of `musaic-api/tests/validation_bytes.rs`. `check_fixes_http.rs` is rewritten over research requests with its golden (see Goldens). Slice 4 deletes the now-dead code.

Moved, not deleted, in this slice: `JobRow` (it is exactly a `research_history` row), `JOB_TYPES` and `LIST_LIMIT` move to `musaic_db::research_history`; `job_to_py` keeps taking `&JobRow`. The `musaic_db::ai_jobs` module itself stays compiled until slice 4, re-exporting `JobRow` and holding its old functions as dead code, because `musaic-ai/src/{worker,jobs}.rs`, `musaic-services/src/{ai_jobs,fix_flow}.rs` and `musaic-app/src/ai.rs` still call it and are deleted only in slice 4 (its SQL fails only at run time, and nothing calls it after the cut-off). Slice 4 deletes the module.

Tests: `musaic-db/tests/v24_migration.rs` (new: every fixture v16/v20/v22/v23 copied to a temp dir → v24, history view projection before/after equal on title, hint, times, outcome for every status and type including `lyrics`; unreadable `result_json` kept; album gone kept; second open changes nothing (`sqlite3` dump equal); interrupt trigger at 24 then retry; v24 file refused by a build constant of 23 via `NewerSchema` simulation; restore of a v20 backup migrates); `musaic-db/tests/old_schemas.rs`, `fresh_db.rs`, `python_written_db.rs`, `support/delegation_schema.rs` (`V24_OBJECTS`), `delegation_events.rs` (asserts version 23), `delegated_work.rs` (its v22 tests read `ai_jobs` after migrating); `merge_album_rows.rs` (refusal on each unresolved state, repoint of finished requests and history); `musaic-fsops/tests/golden_scenarios.rs` (blockers from the single store); `musaic-api/tests/scan_adopt.rs` (requests follow adoption); `musaic-api/tests/multi_disc_{convert,gather,preview}_http.rs` and `musaic-checks/tests/{common/mod.rs, runner_rough_edges.rs}` seed old jobs through `import_legacy_job` instead of `INSERT INTO ai_jobs`; `musaic-app/tests/ops_fixes.rs` stops counting `ai_jobs`; a rename test that a queued request is re-derived and claimable after rename; `musaic-checks` no-cover-art rule from both tables; `musaic-app/tests/ops_jobs.rs`, `delegation.rs`, `research/legacy.rs` tests on `research_history`; `musaic-api/tests/cover_preview_http.rs` reading history; `musaic-db/tests/backup_restore.rs` counts; `musaic-api/tests/research_jobs_http.rs` (new: the kept `/api/ai/jobs` routes answer from the single store in `job_to_py`'s shape, and their 422 bodies equal `golden/ai/http.json`'s byte for byte).

[P] after the musaic-db core lands: musaic-checks (`no_cover_art.rs`, `inputs.rs`) ∥ musaic-fsops (`multi_disc.rs`) ∥ musaic-app (`desktop_research.rs`, `research/legacy.rs`, `ops/jobs.rs`, `ops/apply.rs`) ∥ musaic-api tests. `merge.rs` and `renames.rs` stay with the db core (one worker).

### Slice 2 — Batches end to end

What, in dependency order:
- db: `work_batches` writes; `enqueue` with `batch_id`; `claim_many`, `renew_many`; list filter; `batch_id` on event rows; batch list/detail/cancel queries.
- commands [P with db]: `WorkListInput.batch_id`, `WorkSummary.batch_id`, `WorkClaimManyInput/Output`, `WorkRenewManyInput/Output`, two `REGISTRY` entries, `call.rs` variants, `WorkEventData.batch_id` and `payload_schema()`.
- app: `agent/delegation.rs` (two tools, scope, replay, error mapping); `publish_pending` drains every outbox page and publishes one `AgentStateChanged` per page; `desktop_research::enqueue` taking a batch and a `publish` switch, so the bulk delegate defers `publish_pending` to every 128th request and the end of the run; `ops::jobs::{research_batches, research_batch, cancel_research_batch}`; `ops::fixes::quick_fix_all(check_id, research, sink)` passing the delegate callback.
- services: a `delegate` parameter on `quick_fix_all` (not a `FixEnv` field: only the bulk path uses it; see "Settled interfaces"); `try_quick_fix_finding` → `NeedsResearch` for the four checks when a delegate is given; `QuickFixSummary` gains optional `sent_for_research`, `already_requested`, `batch_id` (it stops being `Copy`); the existing Missing skip in `quick_fix_loop` (`missing_album_of`) stays ahead of the resolver and the delegate.
- api: three routes (`contracts/desktop-routes.md`), `research=1` query on quick-fix-all, render, inventory rows; `musaic-api/tests/inventory.rs` pins the three new `EXTRA_ROUTES` and lists `research` as a deliberate query parameter added to a Python route.
- skill and catalogs: SKILL.md + `work-lifecycle.md`, version 1.1.0, `catalog --write`.
- frontend: `api.ts` types (`batchId`, batch types); `useResearchBatches` (key `["research","batches"]`, already invalidated by the `work` family's `["research"]` prefix, so `useLibraryEventsStream.ts` is unchanged); `DelegatedWork.tsx` folds batched items into one row per batch with counts, opens to the items, Cancel; Activity and Review entries; `BulkQuickFixDialog.tsx` choice (default on when `useAgentPairings` lists at least one pairing with `revokedAt === null`) and summary text.

Tests: `musaic-db/tests/delegated_work.rs` (claim-many: 3 eligible of 25 asked; none; stale skipped; batch and kind filters; replay returns same ids and re-derived tokens; different input same key → conflict; race with two connections, no item held twice; renew-many partial refusals); `musaic-commands/tests/{schemas,descriptor}.rs` (23 tools, kinds, annotations, events payload schema includes optional `batchId`); the other tool-count pins: `musaic-mcp/tests/profiles.rs`, `musaic-cli/tests/stdout_purity.rs`, `musaic-api/tests/agent_guard.rs`; `musaic-mcp/tests/conformance.rs` (tools listed, events carry `batchId`); `musaic-app/tests/ops_jobs.rs` (batch list/detail/cancel: awaiting-review untouched), `ops_fixes.rs` (bulk with research on: N unsettled → N requests in one batch, Missing skipped, existing request counted and not moved, research off → no batch; stop part-way keeps made requests, `finished_at` NULL → unfinished; one `BulkBatchCompleted`), `ops_agent.rs` (scope refusals for the new tools), `ops_agent_events.rs` (events carry `batchId`, survive Clear); `musaic-api/tests/stream_equivalence.rs` (quick-fix-all with `research=1`), `operations_inventory.rs`, `thin_handlers.rs`, `inventory.rs`; `musaic-cli/tests/catalog_check.rs`; SC-003 test (1 000 items, 40 claim-many calls, every item claimed once); frontend: `DelegatedWork.test.tsx`, `BulkQuickFixDialog.test.tsx`, Activity/Review batch entry.

[P]: db ∥ commands; then app ∥ services (different crates; the callback signature is the one under "Settled interfaces"); then api ∥ frontend ∥ skill text. `catalog --write` runs last, once.

### Slice 3 — Frontend: no model gating, the wording table

What: delete `hooks/useAiStatus.ts`, `useAiProviders`/`useResumeQueue`/`useRetryJob`/`useMarkJobActedOn` (`hooks/useJobs.ts`); `useQueueState` stays, hydrated with zeros instead of from the settings, and carries `clearable` for the Clear button; the provider parts of `lib/aiGating.ts` (keep `modelFreeCoverage`, used by corrections), `getAiProviders`/`resumeQueue`/`retryJob`/`markJobActedOn` and the provider types in `lib/api.ts`, the settings fields; `pages/Settings.tsx`: `AIUnavailableSection`, `AIBackendSection`, `AISection`, `FireCrawlSection` and the `noAiBackend` gate; `PausedBanner`; every `ai.ready`/`aiReady` branch (keep neutral text): `JobStatusBadge`, `DetailSheet/ResolutionBadge`, `DetailSheet/PropTable`, `Review.tsx`, `SettingsBackup.tsx`, `JobRow.tsx`, `DetailSheet/index.tsx`; Fix: `FindingFixButton.tsx` and `FindingRow.tsx` always offer the hint for the four checks, `lib/findingActions.ts` keeps one copy per check; `BulkQuickFixDialog.tsx` loses its `useAiStatus` gate and "No AI quota" line; `DetailSheet/index.tsx:498` shows Request research for any present, idle album without an unresolved request; the old-job Retry and acted-on branches in `DetailSheet` and `JobRow`; the wording table (`contracts/wording.md`) including Settings → Agents and the Activity card.

Tests: the ~22 files that seed `["ai-providers"]` lose that seeding; `test/aiVocabulary.test.tsx` and `pages/noModelCopy.test.tsx` are replaced by one `noProviderWording.test.tsx` (SC-005 strings absent from Settings, Settings → Agents, Activity, album panel, Review, finding buttons); `FindingFixButton.test.tsx` (hint offered, copy), `findingActionLabels.test.tsx`, `SettingsAgents.test.tsx` (FR-030 labels), `ActivityPanel` card title, `DetailSheet` Request research offered (new `components/DetailSheet/requestResearch.test.tsx`; there is no general DetailSheet test file); `findingActionLabels.test.tsx` lives in `components/`, `useLibraryEventsStream.test.tsx` in `hooks/`; `lib/errors.test.ts` and `coverPreview.test.ts` updated.

[P] by file set (no two workers share a file): (a) `Settings.tsx`, `SettingsAgents.tsx`, `agentCopy.ts`, `SettingsBackup.tsx`; (b) `ActivityPanel.tsx`, `JobRow.tsx`, `Review.tsx`, `useActivity.ts`, `activityModel.ts`; (c) `FindingFixButton.tsx`, `FindingRow.tsx`, `findingActions.ts`, `BulkQuickFixDialog.tsx`; (d) `DetailSheet/*` (which holds `HintDialog.tsx`, `ResolutionBadge.tsx`, `PropTable.tsx`, `AlbumHero.tsx`), `JobStatusBadge.tsx`. `api.ts`, `useJobs.ts`, `aiGating.ts`, `useAiStatus.ts` go first, in one worker.

### Slice 4 — Provider removal in Rust

What: `musaic-ai`: delete `provider.rs`, `payload.rs`, `transcript.rs`, `worker.rs` (its `WorkerEnv` becomes `ResolverEnv` in `synth.rs` or a small `env.rs`), `prompts.rs` (two constants move to `parse.rs`), the parse functions, `jobs::{queue_paused, set_queue_paused, max_parallel, queue_state, stream_payload}`, the `replay` feature (and the dev-dependency self-feature in `musaic-app` and `musaic-api`); `musaic-app`: builder `ai_provider`, `ai_worker`, `ai_call_timeout`, `legacy_jobs`, `legacy_worker`; `AiQueue` provider/opts/worker/start/stop; `ai_run_next`, `ai_worker_running`, `ai_provider_infos/ids`, `job_transitions` and the job-log thread (`ai.rs:151`); `AppEnv::firecrawl_key`; `ops::jobs::{retry, resume_queue, providers, mark_acted_on}`; the legacy branch in `ops/fixes.rs:108-118`; `musaic-services`: `ai_jobs.rs` deleted, its three kept helpers (`check_job_type`, `album_for_job`, `bulk_delete_statuses`) moved to a new `musaic-services/src/research_request.rs`; `fix_flow::fix_finding`, `WorkKind::AiJob` (`gate.rs:66,78,96`, and in `musaic-app/src/work.rs:165,183`), `FixEnv.model_unavailable` (`check_fixes.rs:278,481,700`; also `musaic-app/src/envs.rs:143`), `TAGS_AI` (`:667`); the retired-key startup step in `keyscrub.rs` (see "Settled interfaces"); `musaic-api`: the four routes and handlers, `ROUTING_ALIASES` unchanged, `state.rs:76-92` pass-throughs, `src/ai.rs` re-export if empty; settings and secrets per `contracts/settings-and-secrets.md`; `musaic-desktop/src/nav.rs` FireCrawl link; `ops::jobs` `queue_state` fixed at `paused: false`, `max_parallel: 1` (D16); then the `musaic_db::ai_jobs` module, last, once nothing calls it.

Deleted tests (the worker-driving ones already went in slice 1): provider cases in `musaic-ai/tests/golden_units.rs` and `positional_skip.rs` if provider-only; `musaic-api/tests/ai_wording.rs` and `common/ai_wording.rs`; the remaining provider parts of `log_secrets` (keeps redaction of both names, drops the provider builder), `misc` (issue #199 providers test), `capture_replay` (providers special case `:242-243,517-526`), `deliberate.rs` (the acted-on and retry probes), `golden_check_fixes.rs` (`FixEnv.model_unavailable`); `musaic-app/tests` provider setup in `common/mod.rs` and the `.ai_worker(false)` / `ai_provider(...)` builder calls in `delegation.rs`, `ops_agent.rs`, `ops_agent_events.rs`, `ops_fixes.rs`.

New/changed tests: startup keyring deletion (once, marker, retry); `settings_patch` ignores retired names; `GET /api/settings` has none of the six fields; `operations_inventory`, `ops_all_groups`, `thin_handlers`, `ops_boundary`, `no_db_owner` unchanged and passing.

[P] by crate: `musaic-ai` ∥ `musaic-secrets` + `musaic-db/settings.rs` ∥ `musaic-api` routes/tests/goldens ∥ `musaic-desktop/nav.rs`; `musaic-app` and `musaic-services` last (they depend on the others' removals); the `musaic_db::ai_jobs` module after both.

### Slice 5 — `AiWording` collapse

What: delete `musaic-core/src/ai_wording.rs` and its export; `App::ai_wording`; the `NamesAi` text in `musaic-checks/src/wording.rs`, `musaic-resolve/src/resolution.rs::reword_notes` (the neutral note becomes the only note), `musaic-ai/src/synth.rs`, `musaic-services/src/gate.rs`, `musaic-app/src/work.rs`, `ops/checks.rs`, `ops/library.rs`, `musaic-checks/src/runner.rs`. Every call site already passes `Neutral` after slice 4, so no listener-visible change.

Tests: `musaic-checks/tests/{golden_pure, golden_scenarios}.rs` and `musaic-ai/tests/golden_units.rs` drop their `NamesAi` cases (see Goldens).

Not parallel: one worker (the enum's removal breaks every site at once).

### Slice 6 — Docs

What: `README.md` (`:136-138` and any provider text), `docs/operations.md` (counts at `:57-63`; rows `:102`, `:144`, `:145`, `:151-169`, `:179`; three new rows; four removed; notes `:22-25`, `:421-454`), `docs/agents.md` (wording table, batches, "research requests", "21 tools" → 23), `docs/mcp.md` (two tools, the `batchId` compatibility note), `docs/desktop.md` if it mentions the provider, `CLAUDE.md` (crate map: `musaic-ai`, `musaic-app` "job worker host", `musaic-db` "schema v24", `musaic-commands` "23 typed tools"; rule 6 "active AI job" → "unresolved research request"; rule 1 batch note), `AGENTS.md` (its own copy), `SECURITY.md` if it names model keys.

Tests: `operations_inventory.rs` (rows), `catalog --check`. A grep in review: no "built-in provider", "AI job", "FireCrawl" outside history notes; hits in comments are reworded in place (known today: `frontend/src/{App.tsx, components/Sidebar.tsx, components/AlbumList.tsx, components/Tile.tsx, components/TileGrid.tsx, lib/activityPanel.ts, pages/Library.tsx}`).

[P] per file.

## Goldens and recorded data that change (deliberate; each change says why in its commit)

| File | Slice | Change and reason |
| --- | --- | --- |
| `rust/testdata/golden/ai/http.json` | — | kept unchanged: a recording of the Python app's queue routes; no longer replayed against routes (`jobs_http.rs` goes in slice 1), but `validation_bytes.rs` takes 9 of its 11 422 byte samples from it and `research_jobs_http.rs` compares the kept routes' 422 bodies with it |
| `rust/testdata/golden/ai/worker.json` | 1 | deleted: provider worker scenarios (same reason) |
| `rust/testdata/golden/ai/units.json` | 4, 5 | provider sections (payload, transcript, prompts, parse of model replies) deleted in 4; `synthesize` kept; `NamesAi` entries deleted in 5 |
| `rust/testdata/golden/api/routes.json` | 4 | the four removed Python routes (providers, queue/resume, retry, acted-on); 71 → 67, and the count in `musaic-api/tests/inventory.rs:82,312` |
| `rust/testdata/golden/api/route_order.json` | 4 | routes and probes for the four paths |
| `rust/testdata/golden/api/cases.json`, `wired.json` | 4 | settings bodies lose `ai_provider_id`, `ai_model_id`, `ai_max_parallel`, `ai_queue_paused`, `ai_api_key_set`, `firecrawl_api_key_set` (FR-003); `ai_acted_on_at` on albums stays |
| `rust/testdata/captures/api-offline/api/081-get-api-ai-providers.json` + `index.json` entry | 4 | deleted with the route |
| `captures/api-offline/api/080-get-api-settings.json`, `007-patch-settings-acoustid-key.json` | 4 | settings fields removed |
| `captures/api-offline/openapi.json`, `api/001-get-openapi-json.json` | 2, 4 | three paths added (2); four removed and settings schema shrunk (4) |
| `captures/api-offline/api/082-get-api-ai-jobs.json` | — | unchanged: the recorded body is `{"jobs": []}` (the list route renders no `queue_state`), which the single store reproduces |
| `captures/api-offline/api/083-get-api-db-info.json`, `084-get-api-db-snapshots.json` | — | unchanged: both record `"ai_jobs": 0` for an empty library, and the key and the zero stay |
| `captures/api-offline/checkpoints/*.json` | — | not edited: Python-app table dumps that no test reads |
| `rust/testdata/golden/services/check_fixes.json` | 1 | cases recorded with the legacy enqueue (an `ai_jobs` row) now record a research request and the message "Research requested. You can follow it in Review." — the single Fix delegates in every build (FR-008). Its two replays change with it: `musaic-api/tests/check_fixes_http.rs` projects its per-step state from `delegated_work` and drops the provider builder; `musaic-api/tests/golden_check_fixes.rs` simulates the host queue and changes only where the golden does |
| `rust/testdata/golden/fsops/scenarios.json` | 1 | inputs that seed `ai_jobs` are seeded through `research_history::import_legacy_job` (the v24 fold's own code) by the harness; expectations change only where a conversion member has an old `completed` job (now an awaiting-review request), which now blocks (D6) |
| `rust/testdata/golden/checks/cases.json` | 1 | input rows seeded through `import_legacy_job`; expectations unchanged (FR-024 keeps the rule; a request linked to an old job is judged by its history record) |
| `rust/testdata/golden/persistence/backup.json` | 1 | expected unchanged (two old rows → two requests); the fixture-count key `ai_jobs` keeps its name |
| `rust/testdata/golden/persistence/fresh-db.json`, `old-schemas/v01…v16.json` | 1 | unchanged: they record what the Python app wrote; the Rust deltas in `old_schemas.rs:295-316` and `fresh_db.rs:177-199` gain the v24 delta |
| `musaic-checks` golden cases with `NamesAi` (`golden_pure.rs`, `golden_scenarios.rs` data) | 5 | the `NamesAi` variants are deleted (no build can show them) |
| `captures/verify-records/many/records.json` | — | unchanged: it holds `"ai_jobs": 0` counts only, and `validation_bytes.rs` reads only its 422 bodies |
| `rust/testdata/db/v20-jobs.db`, `v22-jobs.db`, `v23-jobs.db` | 1 | new synthetic fixtures (not goldens), with a README line |

## Project Structure

### Documentation (this feature)

```text
specs/006-external-agents-only/
├── plan.md
├── research.md
├── data-model.md
├── quickstart.md
├── contracts/
│   ├── agent-tools.md
│   ├── events.md
│   ├── desktop-routes.md
│   ├── settings-and-secrets.md
│   └── wording.md
├── checklists/requirements.md
└── tasks.md            (speckit-tasks)
```

### Source Code (repository root)

```text
rust/crates/musaic-db/src/          migrate.rs (pin, v23_to_v24, upgrade_to), research_v24.sql, research_history.rs (new),
                                    delegated_work.rs (batch, claim_many, renew_many, unresolved_for_albums, link_job),
                                    delegation_events.rs, research_jobs.rs, renames.rs, merge.rs, backup.rs, albums.rs,
                                    settings.rs; ai_jobs.rs (deleted in slice 4)
rust/crates/musaic-checks/src/      checks/no_cover_art.rs, inputs.rs, wording.rs, runner.rs
rust/crates/musaic-fsops/src/       multi_disc.rs
rust/crates/musaic-services/src/    check_fixes.rs, fix_flow.rs, ai_jobs.rs (removed), research_request.rs (new), gate.rs,
                                    settings_patch.rs, multi_disc_gather.rs, keyscrub.rs
rust/crates/musaic-commands/src/    lib.rs (REGISTRY 23), tools/work.rs, call.rs, events.rs
rust/crates/musaic-app/src/         desktop_research.rs, research/legacy.rs, agent/delegation.rs, ai.rs, app.rs, work.rs, envs.rs,
                                    ops/{jobs,fixes,apply,checks,library,settings}.rs
rust/crates/musaic-ai/src/          lib.rs, synth.rs, pyjson.rs, parse.rs, jobs.rs (provider.rs, payload.rs,
                                    transcript.rs, worker.rs, prompts.rs deleted)
rust/crates/musaic-api/src/         lib.rs (routes, EXTRA_ROUTES), routes/{jobs,fixes,settings,preview}.rs, render/, state.rs
rust/crates/musaic-secrets/src/     lib.rs (MIGRATED/RETIRED), view.rs, migrate.rs
rust/crates/musaic-core/src/        ai_wording.rs (deleted, slice 5)
rust/crates/musaic-desktop/src/     nav.rs
skills/musaic-work-delegation/      SKILL.md, references/work-lifecycle.md, generated references, skill.lock.json
docs/                               operations.md, agents.md, mcp.md, agent-tools.*, agent-events.*
frontend/src/                       lib/{api,aiGating,findingActions,agentCopy,activityModel,coverPreview}.ts,
                                    hooks/{useJobs,useAiStatus,useActivity,useDelegatedWork}.ts,
                                    pages/{Settings,SettingsAgents,SettingsBackup,Review}.tsx,
                                    components/{ActivityPanel,JobRow,JobStatusBadge,FindingFixButton,FindingRow,
                                    BulkQuickFixDialog,DelegatedWork,ResearchGroup}.tsx,
                                    components/DetailSheet/* (HintDialog, ResolutionBadge, PropTable, AlbumHero, index)
```

**Structure Decision**: no new crate; every change sits in the crate that owns it today. `musaic-ai` keeps its name and shrinks (D15).

## Risks the implementer most needs to know

- **`SCHEMA_VERSION` in the v22→v23 step.** `migrate.rs:429` writes the constant; bump it without pinning that line to `23` and v24 is silently skipped on every v22 file.
- **Do not edit `delegated_work.sql`.** v21→v22 includes it; a column added there would make v24's `ALTER` fail on files migrating from ≤ v21.
- **Stale context after a rename.** `base_revision` hashes the path; without the `AlbumRenamed` re-derivation, every queued item of a renamed album is unclaimable (claim answers `STALE_CONTEXT` forever). Test it. `delegated_work::hydrate_legacy` cannot be reused for this: it only fills an empty `base_revision` of a linked old job.
- **Slice 1 must cut the worker off, but must not delete `musaic_db::ai_jobs`.** The worker reads and writes `ai_jobs`; slice 1 stops starting it, removes the legacy enqueue branches and deletes the worker-driving tests and goldens, or the workspace is red until slice 4. Do not `#[ignore]` them instead. The module itself still has compile-time callers until slice 4.
- **Event and refetch storm.** `desktop_research::enqueue` calls `publish_pending` after every request, so coalescing per outbox page alone does nothing for a bulk run (each call finds one or two rows). The bulk delegate must defer publishing (every 128 requests and once at the end) and `publish_pending` must drain every page; otherwise a 1 000-item batch is 1 000 relay events and refetches, and the relay queue drops oldest.
- **Second resolver pass.** `desktop_research::enqueue` resolves again, but through the app's one `Arc<musaic_resolve::Resolver>`, whose memo is keyed by album id and content hash: an album the quick path left unchanged is a memo hit, so no second MusicBrainz pass. It re-resolves only when the quick path wrote something (content changed: correct) or a MusicBrainz request failed (not memoized: a retry is correct). No resolution is handed through; T056 asserts the memo hit.
- **Every check reruns once after upgrade** (`INPUTS_VERSION` bump). Expected; the sweep is background and yields.
- **No-cover-art now also sees native delegated cover research**, not only old jobs; a native failed cover request suppresses the finding within the window, as an old job did (FR-024). Checks goldens must not move; if one does, the input seeding is wrong.
- **Frontend tests**: 23 files seed the `["ai-providers"]` cache; slice 3 touches them all. Group the edits by file set to avoid merge churn.
- **Keyring on Linux and Windows**: a delete may fail when the secret service is locked; the marker is written only after success, so the step retries quietly. Never log a value. `SecretStore::delete` already exists and treats a missing entry as success, in the keyring and the in-memory store.
- **`catalog --check` and the skill lock**: any tool description change alters served bytes; bump `metadata.version` before `--write` or the lock refuses.
- **Adjacent gap not fixed here**: `agent_proposals.album_id` and `agent_changes.album_id` are not repointed on an id change either; file an issue in slice 1.
- **`validation_bytes.rs` depends on `golden/ai/http.json`.** Deleting that golden would silently drop 9 of 11 422 byte samples and fail the test; it is kept (Goldens).
- **Tool-count pins outside `musaic-commands`**: `musaic-mcp/tests/profiles.rs`, `musaic-cli/tests/stdout_purity.rs` and `musaic-api/tests/agent_guard.rs` assert 21; all move to 23 in slice 2.

## Settled interfaces (written during analysis; the tasks rely on them)

- **Bulk delegation callback** (`musaic-services/src/check_fixes.rs`):

  ```rust
  /// What the host did with one album the resolver could not settle.
  pub enum Delegated {
      /// A new request was made, in the run's batch.
      Requested { batch_id: String },
      /// The album already had an unresolved request of that kind; it keeps its own batch membership.
      AlreadyRequested,
      /// The host refused (album went Missing, guard, database error): counted as an error with this text.
      Refused(String),
  }
  pub type Delegate<'d> = dyn FnMut(&mut Connection, &AiJobRequest) -> Delegated + 'd;

  pub fn quick_fix_all(
      env: &mut FixEnv<'_>,
      check_id: &str,
      between: &mut dyn FnMut(),
      delegate: Option<&mut Delegate<'_>>,   // Some ⇔ research=1
      sink: &mut dyn FnMut(QuickFixEvent) -> ControlFlow<()>,
  ) -> Result<(), OpError>;
  ```

  `try_quick_fix_finding(env, finding_id, research: bool)` returns `Result<Option<QuickFix>, String>` with `enum QuickFix { Done(FixResult), NeedsResearch(AiJobRequest) }`; `NeedsResearch` reuses the single Fix's `AiJobRequest` (album id, job type, empty hint, `applied_summary`). `ops::fixes::quick_fix_all(check_id, research, sink)` generates the batch id and start time before the loop and builds the closure over `desktop_research::enqueue(app, conn, &job, Some(&batch), Publish::Deferred)`; the first request's transaction creates the `work_batches` row (`INSERT OR IGNORE`), so a run that makes none makes no batch.
- **JobRow**: `musaic_db::research_history::JobRow` (same fields as today; built from a `research_history` row or by `desktop_research::group`). `musaic_db::ai_jobs` re-exports it until slice 4.
- **`link_job`**: `musaic_db::delegated_work::link_job(tx, epoch, &JobRow) -> Result<()>` (`pub(crate)`), the v22 per-row mapping; `migrate_legacy` creates the epoch and calls it per row, `v23_to_v24` reads the current epoch and calls it for unlinked known-type rows. `research_history::import_legacy_job(tx, &JobRow)` (`#[doc(hidden)]`) inserts the history row and calls `link_job`: the harness seam for goldens that seed old jobs.
- **Rename re-derivation**: `desktop_research::subscribe_rename_rederive(app)`, registered from `musaic-app/src/app.rs` beside `subscribe_legacy_hydration` (priority 250, name `rederive_after_rename`). Not in `musaic-events/src/catalog.rs`: the catalog mirrors the Python app's subscriber list over `SideEffects` and cannot reach app code. For each **queued** item of the event's `new_album_id` in the current epoch it recomputes `desktop_research::input(app, conn, id, hint)` with the hint from the item's own `input_json` and writes it through a new `delegated_work::rederive_queued(conn, id, base_revision, input_json, input_digest, now)` (`UPDATE OR IGNORE … WHERE id=? AND state='queued'`, revision bumped, `record_change`), then `publish_pending`.
- **Retired keyring entries**: `musaic_services::keyscrub::remove_retired_keys(db, secrets, log)`, beside the secret scrub that already owns startup secret handling; `musaic-app/src/app.rs` calls it right after `key_scrub.run(&db, secrets.as_ref(), "startup")`. Marker: settings row `retired_keys_removed = 1` (not in the settings view). A restored older database lacks the marker, so the next start deletes again, which is harmless.
- **Captures that do not move**: `082-get-api-ai-jobs.json` (`{"jobs": []}`), `083`/`084` (`"ai_jobs": 0`), `verify-records/many/records.json` (counts only) and `checkpoints/*.json` (unread Python-app dumps) are not edited; T036 and T099 confirm byte identity rather than edit.

## Settled items (owner ruling)

- **Batched items and the library views: SETTLED 2026-11-23, option (c).** The Library page shows the status of batched requests. An album that has a request in a batch gets the same tile/row status pill as any other album with a request, in every active state, and the Library "to review" filter finds a batched proposal that is awaiting review. The Activity panel and Review still show one entry per batch. D12 stands for `GET /api/ai/jobs` (batched items stay out of it, no hundreds of cards); instead the Library derives per-album status from the job cards plus the per-item delegated work list (`GET /api/research/work`, operation `jobs.delegated_work`), extending that read if a field is missing (the album id, the state and the kind per item must be present, awaiting-review items included). It stays SSE-driven (rule 5): no `invalidateQueries` in mutation handlers, `staleTime: Infinity`. `active_for_album` includes batched items in every case, so the album panel never offers a duplicate request.

Every other architectural question met a defensible default recorded in research.md (rule 1: the re-derivation is a subscriber; rule 6: blockers and id moves in the album transaction; rule 7: three thin routes, four removed; security: retired keys scrubbed, never moved).

## Complexity Tracking

| Addition | Why needed | Simpler alternative rejected because |
| --- | --- | --- |
| `research_history` beside `delegated_work` | Unknown old types and verbatim timestamps; no rebuild of a table with dependents | Columns on `delegated_work` need a rebuild under foreign keys and put history-only rows in the agent-visible table |
| `work_batches` | The origin (check, start, finished) is not derivable from items | A batch state machine was refused by the spec |
| Two new tools | A 25-item claim and a multi-lease renew (FR-017) | Widening the single tools changes their output shape (FR-035) |
| `work_replays` rebuild | Its CHECK lists the replayed operations | A prefixed key under `claim` would overload the single claim's replay space |
