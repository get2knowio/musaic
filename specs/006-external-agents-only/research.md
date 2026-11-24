# Research: External agents only

Baseline: `main` at `655bb04a` (branch `006-external-agents-only`), surveyed 2026-11-23 by three read-only passes (database and `ai_jobs` seams; provider, tools and routes; frontend). Every anchor below was checked against that tree. Corrections to the spec's and the brief's anchors are at the end.

## Decisions

### D1. Where the old job rows live: a history table beside `delegated_work`, not new columns on it

- **Decision**: v24 creates `research_history`, a column-for-column home for every `ai_jobs` row (title, path, type, hint, status, outcome, error, stored result, the four timestamps as the original ISO text, `attach_cover`), copies every row into it, back-fills `delegated_work` links for any known-type row that has none, and drops `ai_jobs`. `delegated_work.legacy_job_id` (already present, unique per kind) stays the link from a request to its history record; a combined metadata-and-cover job keeps its two requests in one `group_id`, as v22 made them. A row of an unknown type gets a history record and no request: that is the history-only record (FR-020).
- **Rationale**: `delegated_work` carries `CHECK (kind IN (…three kinds…))` and `CHECK (schema_version = 1)`; an unknown job type cannot be stored there without rebuilding the table, and `work_attempts` and `work_results` reference it. A rebuild under live foreign keys on a user's database is the riskiest SQL in the feature for no gain. A history-only row in `delegated_work` would also have to be excluded from every agent query by hand; in its own table it cannot leak to an agent. Copying text timestamps verbatim makes "same title, hint, times and outcome" (SC-004) true by construction instead of by conversion.
- **Alternatives rejected**: (a) new nullable columns on `delegated_work` (title, hint, raw result…): needs the rebuild for unknown types, and puts history-only rows in the agent-visible table. (b) Renaming `ai_jobs` in place: keeps the old name, its queue semantics and its indexes; the point is that nothing reads a queue table any more. (c) Dropping unknown-type rows: refused by the spec (Q5).

### D2. The v24 step: one version-gated transaction, written to be re-runnable

- **Decision**: `v23_to_v24` follows the house pattern (IMMEDIATE transaction, re-read the version, act only at 23, commit). Inside: create the new tables and columns, rebuild `work_replays` (D9), `INSERT OR IGNORE` every `ai_jobs` row into `research_history`, run the link back-fill (the v22 per-row logic, extracted in place as `delegated_work::link_job` so both steps share it; v24 passes the existing epoch), delete the retired provider settings rows, `DROP TABLE IF EXISTS ai_jobs` and its indexes, and set the version to the literal `24`. The v22→v23 step is first pinned to the literal `23` (it writes `SCHEMA_VERSION` today, `migrate.rs:429`, so bumping the constant would make it skip v24).
- **Rationale**: atomicity is what makes the existing steps safe to re-run; `OR IGNORE` and `IF EXISTS` make the body itself harmless if an interrupted earlier attempt is ever replayed by hand. The same pin was needed for v20→v21 in spec 004.
- **Alternatives rejected**: running the fold at app start outside the migration (two code paths decide what "migrated" means); a marker row instead of the version (the version is the marker).

### D3. Safety around the migration on real databases

- **Decision**: no new mechanism; the existing ones are confirmed and tested for v24. A v17–v23 file is copied by `startup.rs::snapshot_before_schema_step` (`VACUUM INTO library-<tag>-v<found>.db`, secrets scrubbed) because `16 < found < SCHEMA_VERSION`; a v16-or-older file gets `library.v<N>.before-rust.db`. Restore of an older backup runs `Db::open` on the staged copy, so it migrates through v24 before it replaces the live file. A v24 file opened by an older build is refused by the existing `Error::NewerSchema` (`lib.rs:222-226`) and a v24 upload by `RestoreError::Newer` (`backup.rs:335-340`); the downgrade path is restoring the v23 snapshot.
- **Tests**: fixture databases at v16 (the committed `old-schemas/v04…v16.db`, two job rows each) and new committed fixtures at v20, v22 and v23 (D4), each copied to a temp dir, migrated, compared before/after, migrated again (no change), and interrupted by a trigger at `version=24` then retried (the pattern of `musaic-db/tests/delegated_work.rs:80-200`).

### D4. Fixtures at v20, v22 and v23 are generated once and committed

- **Decision**: add `musaic_db::migrate::upgrade_to(conn, target)` (`#[doc(hidden)]`, test seam) and an `#[ignore]`d generator test that copies `db/v16-empty.db`, migrates to 20, 22 and 23, inserts synthetic job rows in every status (`queued`, `running`, `completed`, `failed`, `acted_on` applied, `acted_on` discarded, and an unsupported status) and every type (`metadata`, `cover`, `metadata+cover`, `lyrics`), with a row whose album is gone and a row whose `result_json` is not JSON. For v22 and v23 the job rows are inserted before the v21→v22 step so the real step links them. The three files go under `rust/testdata/db/` with a README line saying how they were made.
- **Rationale**: no v17–v23 fixture exists today (tests drop tables back to v21, or build v18 by hand). Frozen files keep testing what an older build actually wrote even if a step's code is later touched. Synthetic data only.
- **Alternative rejected**: building them on the fly in each test (they would silently change with the current code).

### D5. Requests follow their album through rename, adoption and merge

- **Decision**: add `delegated_work` and `research_history` to `ALBUM_ID_HOLDERS` and to `move_album_rows` (`renames.rs:72-111`), so rename and scan adoption repoint them in the same transaction as the album row (FR-023), and `album_key_in_use` refuses a target id that still holds research. Merge (`merge.rs::merge_album_rows_tx`) refuses while any member has an unresolved request (queued, claimed, running, awaiting review; any epoch is irrelevant because only the current epoch can be unresolved) and repoints finished requests and history records to the survivor. `MergeReport.ai_jobs` keeps its wire key (`jobs_repointed`, `multi_disc_gather.rs:1325`) and counts both.
- **Follow-up on stale context**: an unresolved request's `base_revision` hashes the album row including its path (`agent_read.rs:207`), so after a rename a queued item would be refused as stale at every claim. The app registers a subscriber on `AlbumRenamed` (rule 1; precedent: `hydrate_legacy_research` on `LibraryScanCompleted`, `desktop_research.rs:98`) that re-derives input, revision and digest for the new id's **queued** items: `desktop_research::input` with the hint read from the item's own `input_json`, written by a new `delegated_work::rederive_queued` (`hydrate_legacy` cannot be reused: it only fills an empty `base_revision` of a linked old job). It is registered by `musaic-app` (`app.rs`, beside `subscribe_legacy_hydration`), not in `musaic-events/src/catalog.rs`, which mirrors the Python app's subscribers over `SideEffects` and cannot reach app code. Held items are left to spec 004's rules (the agent's result is refused as stale and the item is requeued or failed).
- **Rationale**: the move must be transactional with the album row (rule 6, FR-023); the re-derivation is cross-cutting work triggered by a published event, which is exactly rule 1's shape.
- **Not in scope**: `agent_proposals.album_id` and `agent_changes.album_id` are not repointed either. That is an existing gap outside research requests; the plan files an issue rather than widening this change.

### D6. Merge and conversion blockers read the single store

- **Decision**: one predicate, `delegated_work::unresolved_for_albums(conn, ids)`, used by `merge.rs` (refusal at `:535-555`), `multi_disc.rs::active_jobs` (`:1572-1582`) and `set_blockers` (`:1880-1897`). Blocker id `job_active` and its text are kept; what counts widens from old `queued/running(/completed)` to the four unresolved states. The predicate returns, per blocking request, the album id, the request's `group_id` (the id the desktop's job routes answer to) and a job-style status (`queued` for queued, `running` for claimed or running, `completed` for awaiting review), so `MergeRefusal::ActiveJob { job_id, status }` keeps its vocabulary.
- **Rationale**: FR-022; today delegated work blocks nothing (an existing gap). The conversion's `active_jobs` looked only at queued and running; including awaiting-review is a deliberate tightening (a proposal waiting for review is built against the folder being moved).

### D7. The batch is a column plus a small origin row

- **Decision**: `delegated_work.batch_id TEXT NULL` (immutable once set) and table `work_batches(id, library_epoch, check_id, started_at, finished_at NULL)`. The batch row is created in the same transaction as the run's first request, so a run that makes no request makes no batch (FR-013). `finished_at` is written only when the loop reaches its end; a run stopped part-way (the listener stopped it, the stream went away, the app quit or crashed) leaves it NULL, and a NULL `finished_at` with no run in progress means "from an unfinished run" (spec edge case 1). Counts by state are derived (`GROUP BY state`). A batch disappears from listings when it has no rows left in `delegated_work` (after Clear).
- **Rationale**: the spec makes a batch a grouping with no lifecycle (Q1); the origin (check, start, finished or not) has to live somewhere and cannot be derived from items.
- **Alternatives rejected**: deriving the batch from `group_id` (a group already means "one desktop review card"; a combined job's two kinds share it); a batch state machine (refused by Q1).

### D8. Bulk Fix all delegates through a callback the app supplies

- **Decision**: `quick_fix_all` gains a `research: bool` request flag (query `?research=1` on the existing route). With it on, `try_quick_fix_finding` (`check_fixes.rs:558-581`) returns a new outcome `NeedsResearch{album_id, job_type, applied_summary}` for the four research-capable checks where today it returns the plain "nothing found" message (`:737`, the resolver message for unidentified, `:934`, `:1055`). The loop calls a `delegate` callback passed to `quick_fix_all` as a parameter (not a `FixEnv` field, since only the bulk path uses it; signature in plan.md, "Settled interfaces"), supplied by `ops::fixes` over `desktop_research::enqueue`, which enqueues one request with the batch id, each in its own transaction (audit, outbox and event-history rows with it, rule 1), creating the batch row on the first. Publishing is deferred to every 128th request and the end of the run (D12). Missing albums are skipped before the resolver runs (rule 6; `quick_fix_loop` already does this through `missing_album_of`). The run's `Done` summary gains `sent_for_research`, `already_requested` and `batch_id`, present only when `research` was on, so every recorded resolver-only frame stays byte-identical. The run still publishes `BulkBatchCompleted` once (`in_bulk_run`). With `research` off nothing changes.
- **Rationale**: services cannot call the app (`desktop_research` lives in `musaic-app`); the single Fix already returns `NeedsAiJob` and lets `ops` enqueue (`ops/fixes.rs:81-107`), and a callback is the same split for a loop. One transaction per request means a run stopped part-way keeps what it made, in one batch (edge case 1).
- **Second resolver call, no second pass**: `desktop_research::enqueue` calls the resolver again (`desktop_research.rs:133`), but every resolve in the app goes through one `Arc<musaic_resolve::Resolver>` whose memo is keyed by album id and the album's content hash (`musaic-resolve/src/resolver.rs`). An album the quick path left untouched is a memo hit; it is resolved again only when the quick path wrote something (the content changed, so a new answer is right) or a MusicBrainz request failed (not memoized, so retrying is right). No resolution is handed through and `enqueue` keeps its inputs.
- **Alternatives rejected**: collecting requests and enqueuing after the loop (a quit loses them all); a separate "research all" operation (the listener asked for one Fix all with a choice).

### D9. Multi-item claim and multi-lease renew are two new tools

- **Decision**: `musaic_work_claim_many {batchId?, kind?, max 1..25 (default 25), idempotencyKey}` → `{claims: [WorkClaimOutput]}` and `musaic_work_renew_many {leases: [{workId, claimGeneration, leaseToken}] 1..25}` → `{renewed: [WorkRenewOutput], refused: [{workId, code}]}`. Both take the research scope (`[LibraryRead, ProposalSubmit]`, `delegation.rs:326-329`) like the single tools. Claim-many runs one IMMEDIATE transaction: expire, select eligible items (current epoch, queued, hydrated, available, optional batch and kind; ordered by `created_at, id`), check each one's context, claim each with a conditional `UPDATE … WHERE id=? AND state='queued'`, write each item's attempt, audit, outbox and event row, and save one replay. An item whose context is stale is skipped, not an error; fewer than asked, or none, is a normal result. Each lease token is derived exactly as today (`work_lease_token(secret, pairing, epoch, work_id, key)`), so the replay stores ids, generations and expiries, never a token. Renew-many renews each lease independently in one transaction and reports per-item refusals with the existing codes.
- **Rationale**: FR-017 and SC-003 (1 000 items in 40 calls). The single tools keep their behaviour (FR-035). The IMMEDIATE lock plus the conditional update gives the same "never held by two" guarantee as the single claim.
- **`work_replays`**: its `CHECK (operation IN ('claim','complete'))` must allow `claim_many`; v24 rebuilds that table (it has no dependents) instead of overloading `claim` with a prefixed key.
- **Alternatives rejected**: an optional `count` on `musaic_work_claim` (changes a single-item tool's output shape); a batch claim (Q1 says a batch has no claim); a limit of 100 (the spec fixes 25).

### D10. The batch reference on events is an additive field; no `.v2` event

- **Decision**: `WorkEventData` gains optional `batchId` (string, omitted when the item has none). `delegation_events` gains a `batch_id` column written by `delegation_events::record` in the same transaction as today, so the reference survives Clear. The two event names stay `musaic.work.requested.v1` and `musaic.work.updated.v1`; the payload schema in the descriptor lists `batchId` as an optional property. `WorkSummary` (list, get, claim outputs) also gains optional `batchId`, and `musaic_work_list` gains an optional `batchId` filter.
- **Why no `.v2`**: the field is optional, carries no new meaning for an item that has none, and changes no existing field. The descriptor (`events/list`) is served live and is the schema an agent reads; the skill teaches agents to re-read it. A `.v2` type would split every stream in two and make every connected agent resubscribe for no semantic change. The one client this could break is one that validated payloads against a cached copy of the old schema with `additionalProperties: false`; the compatibility note in `docs/mcp.md` and `docs/agent-events.md` says so, and the skill version bump (D11) tells agents to refresh.
- **Not added**: a `batchId` filter on `events/poll` arguments (agents coalesce by the field; the list tool filters).

### D11. Skill and catalogs

- **Decision**: `SKILL.md` and `references/work-lifecycle.md` teach "when a burst arrives, list by batch and claim with `musaic_work_claim_many`; renew held leases together"; `metadata.version` goes from `1.0.1` to `1.1.0` (a new capability, nothing removed). `cargo run -p musaic-cli -- catalog --write` regenerates `docs/agent-tools.{json,md}`, `docs/agent-events.{json,md}`, the generated references and `skill.lock.json`. `CONTRACT_VERSION` stays `"1"` (additive). Tool count 21 → 23; error codes unchanged (35).

### D12. One desktop entry per batch

- **Decision**: the jobs projection (`desktop_research::jobs`, behind `GET /api/ai/jobs`) leaves out requests that belong to a batch; a new `GET /api/research/batches` lists batches with origin, size and counts by state, and `GET /api/research/batches/{batch_id}` returns one batch with its items (the `DelegatedItem` shape). `GET /api/research/work` items gain `batchId`; the Review section folds batched items into one row per batch. `POST /api/research/batches/{batch_id}/cancel` cancels every waiting or held item of the batch in one transaction through `cancel_for` (`delegated_work.rs:1138-1158`), one audit, outbox and event row per item.
- **Refresh**: `publish_pending` (`delegation.rs:456-483`) drains every outbox page and publishes one `AgentStateChanged{Work, ids}` per page (up to 128 ids) instead of one per row. That alone does not help a bulk run, because `desktop_research::enqueue` calls `publish_pending` after every request and so finds one or two rows each time: the bulk delegate therefore enqueues with publishing deferred and calls `publish_pending` after every 128th request and once at the end, so a 1 000-item batch is about eight relay events, not a thousand refetches. Events are still published after their commits (the outbox holds them), so rule 1 is unchanged. The UI already invalidates `["research"]` and `["jobs"]` on `work`.
- **`active_for_album`** (`GET /api/ai/jobs/active/{album_id}`) includes batched items: the album panel must see an unresolved request whatever its batch (FR-009, FR-011).
- **Alternative rejected**: grouping in the UI only (the 200-row jobs page limit would be eaten by one batch).

### D13. Clear applied or discarded counts and removes requests

- **Decision**: `DELETE /api/ai/jobs?status=…` (`jobs.delete_many`) keeps its validation (`bulk_delete_statuses`: blank dropped, unknown `Rejected`, `running` `Conflict`) and is reimplemented over the single store, each named old status removing what the jobs projection shows under it. `acted_on` (what Clear sends) removes every request whose state is `applied` or `discarded` (all epochs), with its attempts, results and replays; then every history record left with no linked request that either lost its requests in this call or is a history-only record with old status `acted_on` (applied when `acted_on_action='applied'`, otherwise discarded: the v22 mapping). `failed` removes `failed` and `cancelled` requests and, likewise, history records left unlinked and history-only records of old status `failed` or an unsupported status. `queued` and `completed` answer `Conflict`, as `running` does: an unresolved request is cancelled, never deleted. The count is requests removed plus history-only records removed (a history record whose requests were removed is not counted twice). Audit rows stay (they hold no content); event-history rows stay until their 7-day prune. `jobs.delete` (one card) does the same for one group or history record, and answers `Conflict` while the group has an unresolved item. Albums, files and apply backups are untouched.
- **The number on the button**: the desktop cannot count requests from what it lists (the jobs list has one row per card, and `/api/research/work` omits applied and discarded items), so the jobs stream's `queue_state` gains an additive `clearable` count, computed by the same query `delete_many(['acted_on'])` runs; the Activity panel shows `Clear {clearable} applied or discarded`. `GET /api/ai/jobs` renders no `queue_state`, so its capture does not move.
- **Rationale**: FR-026 counts requests, not cards. A card with one applied and one failed item keeps the failed one.

### D14. Settings, keys and the scrub list

- **Decision**: remove `ai_provider_id`, `ai_model_id`, `ai_max_parallel`, `ai_queue_paused` from the defaults, the view, the patch and the routes; v24 deletes their rows. Split `SECRET_KEYS` into `MIGRATED_SECRET_KEYS` (`acoustid_api_key`, moved to the keyring as today) and `RETIRED_SECRET_KEYS` (`ai_api_key`, `firecrawl_api_key`), whose rows are scrubbed (`secure_delete`, VACUUM, copies and backups, as now) but never moved to the keyring; `SECRET_KEYS` stays the union, so every scrub list (snapshot, restore, log redaction) still names both. A startup step (`musaic_services::keyscrub::remove_retired_keys`, called by `musaic-app` right after the startup key scrub) deletes the two retired keyring entries (service `musaic`) through the existing `SecretStore::delete`, which already treats a missing entry as success in the keyring and the in-memory store, and writes `retired_keys_removed=1` only after both deletes succeed; a failed delete is retried next start and logged without the value. Writing a retired name through `PATCH /api/settings` is ignored like any unknown key.
- **Rationale**: FR-027/028. Moving a retired key into the keyring only to delete it would create the entry the feature removes. The marker keeps keyring calls to one successful start ("deleted once"); a restore of an old database re-runs the row scrub, which needs no marker.

### D15. `musaic-ai` shrinks; it is not renamed

- **Decision**: delete `provider.rs`, `payload.rs`, `transcript.rs`, the worker (the `WorkerEnv` trait becomes a two-method `ResolverEnv` used by `desktop_research`), the provider parts of `jobs.rs` (`queue_paused`, `set_queue_paused`, `max_parallel`, `queue_state`, `stream_payload`) and the `replay` feature. `musaic-services/src/ai_jobs.rs` goes too; its three helpers still needed by `ops::jobs` (`check_job_type`, `album_for_job`, `bulk_delete_statuses`) move to a new `musaic-services/src/research_request.rs`, and `JOB_TYPES` / `JobRow` / `LIST_LIMIT` already live in `musaic_db::research_history` from slice 1. Keep `pyjson`, `synth`, the suggestion types in `parse.rs` (with `SUGGESTABLE_FIELDS` and `TRACK_SUGGESTABLE_FIELDS` moved there from `prompts.rs`, which goes), and `job_to_py` (renamed in place only if it reads clearer; the wire shape is unchanged). The crate keeps its name; the CLAUDE.md crate-map row says what it is now.
- **Rationale**: a rename touches every manifest and import for no behaviour; the crate is the resolver's synthesis plus historical result compatibility, which its row already says.

### D16. `GET /api/ai/jobs` keeps its shape

- **Decision**: the stream's `queue_state` keeps `paused: false` and `max_parallel: 1` fixed (the old defaults), `running`/`queued` counted from the single store, and gains `clearable` (D13); the UI stops reading the first two. Items keep `job_to_py`'s shape; history-only records render with their original type name and status, and `result: null` for review purposes.
- **Rationale**: FR-029 keeps the shapes. `GET /api/ai/jobs` renders only `{"jobs": [...]}` (`render/jobs.rs::job_list`); `queue_state` is in the stream's frames. The recorded capture `captures/api-offline/api/082-get-api-ai-jobs.json` is `{"jobs": []}` and stays byte-identical.

### D17. Routes removed

- **Decision**: remove `GET /api/ai/providers`, `POST /api/ai/queue/resume`, `POST /api/ai/jobs/{job_id}/retry` and `POST /api/ai/jobs/{job_id}/acted-on`, and the operations `jobs.providers`, `jobs.resume_queue`, `jobs.retry`, `jobs.mark_acted_on`. `acted-on` and `retry` acted only on unlinked old rows (`require_historical_job`, `ops/jobs.rs:86`); after v24 those are history-only records, which FR-020 makes neither reviewable nor retryable. Retry of research stays on `POST /api/research/{id}/decision`.
- **Kept**: `GET/POST/DELETE /api/ai/jobs`, the stream and its alias, active-by-album and its alias, `GET/DELETE /api/ai/jobs/{job_id}`, and `GET /api/ai/jobs/{job_id}/cover-preview`, which now reads the history record's stored result (FR-021).

### D18. Wording, once, and the `AiWording` collapse

- **Decision**: the frontend has no wording type; it branches on `ai.ready` / an `aiReady` prop (about 15 sites). Those branches go in the frontend slice, keeping the neutral text, and the wording table (FR-030) lands there too. `AiWording` is a Rust enum in `musaic-core/src/ai_wording.rs` with 61 uses in 13 files (checks' descriptions, resolver notes, synthesis notes, work-kind labels). After the provider slice every call site already passes `Neutral` (readiness is always false), so its own slice deletes the enum, the `NamesAi` text and the golden cases recorded with `NamesAi`, with no behaviour change visible to a listener.

## Corrections to anchors (spec and brief)

| Claimed | Actual |
| --- | --- |
| `merge.rs:535-550` | refusal loop `:535-555`; `:638-661` → UPDATE is `:638-641`, `:661` only the report field |
| `multi_disc.rs:1571-1582`, `:1880-1895` | `fn active_jobs` `:1572-1582`; `set_blockers` `:1880-1897` |
| `no_cover_art.rs:37-51` | `SQL` const `:37-52` |
| `inputs.rs:168-174` | `fold_child_rows` call `:167-174` (bump `INPUTS_VERSION`, `:40`) |
| `ops/apply.rs:392-405` | `job_cover_preview` `:394-408` |
| `renames.rs:84-98`, `:104-112` | `move_album_rows` `:72-98`; `ALBUM_ID_HOLDERS` `:103-111` |
| `delegated_work.rs:1129-1157` (cancel) | `cancel_from_desktop` `:1129-1136`, `cancel_for` `:1138-1158` |
| `lib.rs:46` (schema v23) | constant at `:47` |
| `ops/fixes.rs:81-106` | `:81-107` |
| `check_fixes.rs:568-577` (`try_quick_fix_finding`) | function `:558-581`, dispatch `:572-579` |
| "No deterministic fixes available…" at `:726, :860, :924, :1042` | the string is only at `:737`; those lines start `tags_quick`, `unidentified_quick`, `cover_quick`, `gaps_quick`; their messages are `:737`, the resolver's, `:934`, `:1055` |
| `gate.rs:47` (`WorkKind::AiJob`) | variant `:66`, `ALL` `:78`, label `:96`; `:47` is `RESTORE_REFUSED` |
| `musaic-api/src/lib.rs:323-329` "Python-compat aliases" | entries of `ROUTING_ALIASES` `:320-331` (Starlette routing reproductions) |
| `settings_patch.rs:97-101` | doc `:97-100`, constant `:101` |
| `Settings.tsx:221-256` | `AIUnavailableSection` `:222-235`; the string itself is `aiGating.ts:50` |
| `ActivityPanel.tsx:461-476` | Clear button `:461-474` (text `:472`); "Resume queue" `:441`; `PausedBanner` is `JobRow.tsx:29-62`, rendered at `Review.tsx:546` |
| `SettingsAgents.tsx:185` | `:185` is the "Pair assistant" submit; the four facts render at `:316-322` from `agentCopy.ts:119-124` |
| "`AiWording` (frontend)" | Rust only, `musaic-core/src/ai_wording.rs`; the UI uses `ai.ready` / `aiReady` props |
| "Asking Claude…" strings | Rust only: `TAGS_AI`, `check_fixes.rs:667`; no match in the frontend |
| captures for queue/resume, retry, cover-preview | none; only `081-get-api-ai-providers.json` and `082-get-api-ai-jobs.json` (plus `openapi.json`) |
| fixtures at v20/v22/v23 | none exist; see D4 |
| `legacy::link_job` (plan's first draft) | no `legacy` module exists in `musaic-db`; `migrate_legacy` is in `delegated_work.rs:130-184`, so the extracted function is `delegated_work::link_job` |
| `musaic-services/src/envs.rs:143` | the file is `musaic-app/src/envs.rs` |
| `musaic-api/tests/backup_restore.rs` | the test is `musaic-db/tests/backup_restore.rs` |
| `frontend/src/components/{HintDialog,ResolutionBadge,PropTable}.tsx` | all three are under `components/DetailSheet/` |
| a keyring delete must be added | `SecretStore::delete` exists (`musaic-secrets/src/store.rs:26`), absent entry is `Ok` |
| `golden/ai/http.json` is provider-only | 66 cases of the queue routes, 9 of them 422 bodies that `musaic-api/tests/validation_bytes.rs` samples |
| `golden/api/cases.json`, `wired.json` `/api/ai/*` paths | none; they carry the `ai_*` settings fields and the album field `ai_acted_on_at` |

Further facts the survey found that the spec did not list:

- `musaic-db/src/ai_jobs.rs` also holds two helpers that write `albums`, not `ai_jobs`: `record_mb_lookup_failure` (`:433`) and `mark_metadata_ai_acted_on` (`:442`). They move to `albums.rs` before the module goes.
- `musaic-app/src/ai.rs::job_transitions` (`:191`, `:230`) reads `ai_jobs` for the job log; it goes with the worker.
- `research_jobs::queue_counts` (`:49`) unions unlinked `ai_jobs` rows; it reads only `delegated_work` after v24.
- `DirMigration.ai_jobs` is rendered in the fix response (`musaic-api/src/render/fixes.rs:75`); the key stays, the value counts moved research rows.
- `modelFreeCoverage` (`aiGating.ts:82-94`) also gates MusicBrainz corrections (`DetailSheet/corrections.ts:44,81`) and stays.
