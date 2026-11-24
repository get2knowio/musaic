# Evidence: spec 006 checks

## Baseline (untouched branch, main 655bb04a, 2026-11-23)

| Command | Result |
| --- | --- |
| `cargo test --workspace --no-fail-fast` (from `rust/`) | 1861 passed, 0 failed, 10 ignored |
| `npm test` (from `frontend/`) | 512 passed, 0 failed |
| `cargo test -p musaic-app --test ops_boundary --test ops_all_groups` | 2 + 5 passed |
| `cargo test -p musaic-api --test thin_handlers --test operations_inventory` | 4 + 2 passed |
| `cargo test -p musaic-cli --test no_db_owner` | 2 passed |

Pre-existing failures: none.

## Goldens and recorded data changed (reasons)

(filled in per slice)

### Slice 1

- `rust/testdata/golden/ai/worker.json`: deleted (T016). The built-in worker's scenarios; the worker read and wrote the old job table, which schema v24 drops, and it is no longer started, so the scenarios have no behaviour left to record. Its three replays (`musaic-ai/tests/golden_worker.rs`, `no_provider_wording.rs`, `musaic-api/tests/jobs_worker_http.rs`) went with it; `musaic-ai/tests/golden_units.rs` no longer compares the not-ready provider text with that golden.
- `rust/testdata/golden/ai/http.json`: kept unchanged (`validation_bytes.rs` samples its 422 bodies).
- `rust/testdata/db/v20-jobs.db`, `v22-jobs.db`, `v23-jobs.db`: new synthetic fixtures (not goldens), made by the ignored generator `musaic-db/tests/make_job_fixtures.rs`; README line in `rust/testdata/db/README.md`.
- `rust/testdata/golden/fsops/scenarios.json`, `golden/checks/cases.json`: unchanged; their harnesses now seed old jobs through `research_history::import_legacy_job` and every expectation still holds. `scenarios.json` (T033) needed no edit: its harness seeds every job `queued`, so no conversion member has an old `completed` job; the D6 tightening (awaiting review blocks a conversion) is covered by `musaic-api/tests/multi_disc_preview_http.rs` and `multi_disc_gather_http.rs` instead.
- `rust/testdata/golden/services/check_fixes.json` (T032): changed. Reason: every build now delegates (FR-008), so a fix the resolver cannot settle makes a research request instead of an old job row. The eight steps that queued a job now answer "Research requested. You can follow it in Review." (the tags-out-of-sync resolver step "Wrote 1 album field(s). Research requested. You can follow it in Review."), and each step that makes a new request records `AgentStateChanged {what: work}`. "fix no-cover-art again (job conflict)" now answers `ok: true` with the existing request (the same research joins the request already there; the Python app answered "Already has an active job"), and "fix track-gaps" on an album that already has a cover request makes a second request of the other kind instead of a conflict, so later states carry one more row. The state key `ai_jobs` keeps its name and holds one row per request group (album, job type mapped back from the kinds, hint, job-style status). The reason is also recorded in the golden's `_meta.rust_changes`. `check_fixes_http.rs` and `golden_check_fixes.rs` change with it.
- `rust/testdata/golden/persistence/backup.json`, `fresh-db.json`, `old-schemas/*.json` (T035): unchanged; their tests pass.
- `rust/testdata/captures/api-offline/api/083-get-api-db-info.json`, `084-get-api-db-snapshots.json`, `captures/verify-records/many/records.json`, `captures/api-offline/checkpoints/*.json` (T036): not edited; `capture_replay` and `validation_bytes` pass against them unchanged.
- Adjacent gap filed as #377 (agent proposals and change records keep the old album id after a rename, adoption or merge).


### Slice 1 checkpoint (T038)

| Command | Result |
| --- | --- |
| `cargo fmt --all --check` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo test --workspace --no-fail-fast` (covers every targeted T038 test file) | 1855 passed, 0 failed, 11 ignored |

### Slice 2 (Rust half)

- Goldens and recorded data: none changed. `rust/testdata/golden/services/check_fixes.json` and every recorded quick-fix-all frame are untouched: a run without `research=1` renders the same `done` frame (the research keys are added only when research was asked for).
- `rust/testdata/captures/api-offline/api/openapi.json` and `001-get-openapi-json.json` (T064): not edited. They are the Python app's recorded FastAPI document, which carries none of the earlier `EXTRA_ROUTES` either; `capture_replay.rs` skips `001-` and the route inventory is pinned by `golden/api/routes.json` and `musaic-api/tests/inventory.rs` (which now lists the four new extra routes and `research` as a deliberate query parameter of quick-fix-all).
- New synthetic test data only: `musaic-app/tests/common/mod.rs::clone_finding_albums` makes empty album folders and copied rows inside each test's scratch copy, for the 1 000-album runs.

### Slice 2 checkpoint, Rust side (T066)

| Command | Result |
| --- | --- |
| targeted: `musaic-db --test delegated_work`; `musaic-app --test ops_jobs_batches --test ops_fixes --test ops_agent --test ops_agent_events --test claim_many_scale --test publish_coalesce`; `musaic-api --test stream_equivalence --test operations_inventory --test thin_handlers --test inventory --test agent_guard --test research_jobs_http`; `musaic-commands`, `musaic-mcp` | all pass (18; 6, 20, 48, 11, 1, 1; 8, 2, 4, 10, 9, 4; commands 70, mcp 27) |
| `cargo test --workspace --no-fail-fast` (covers `musaic-cli` and the boundary tests `ops_boundary`, `ops_all_groups`, `thin_handlers`, `operations_inventory`, `no_db_owner`) | 1891 passed, 0 failed, 11 ignored |
| `cargo fmt --all --check` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo run -p musaic-cli -- catalog --check` | exit 0 |

### Slice 4

Goldens and recorded data (T091 to T097, T099). Data edits only; the tests that read them run at the slice 4 checkpoint.

- `rust/testdata/golden/api/routes.json` (T092): the four removed routes (`GET /api/ai/providers`, `POST /api/ai/queue/resume`, `POST /api/ai/jobs/{job_id}/retry`, `POST /api/ai/jobs/{job_id}/acted-on`) deleted, 71 to 67 entries; the counts in `musaic-api/tests/inventory.rs` follow. Reason: FR-004, the routes are removed.
- `rust/testdata/golden/api/route_order.json` (T093): the four `APIRoute` entries deleted (76 to 72 routes) and the 19 probes that exercised them deleted (279 to 260): every probe on `/api/ai/providers`, `/api/ai/queue/resume`, `/api/ai/jobs/x/retry`, `/api/ai/jobs/x/acted-on`, plus `POST /api/ai/jobs/active/retry` (404 "Job not found") and `POST /api/ai/jobs/active/acted-on` (422), which reached the removed retry and acted-on routes through Starlette's first match. They were deleted rather than rewritten because the file holds the Python app's recorded answers and there is no recording of these paths without the routes. Reason: FR-004.
- `rust/testdata/golden/api/cases.json` (T094): the six fields (`ai_provider_id`, `ai_model_id`, `ai_max_parallel`, `ai_queue_paused`, `ai_api_key_set`, `firecrawl_api_key_set`) deleted from the 15 recorded settings bodies. The six cases whose recorded answer was a validation of a retired field are deleted (`settings max_parallel 0`, `… 9`, `… str`, `… float`, `settings paused bad`, `settings provider unknown`; 130 to 124 cases, three fewer 422s). The six 200 cases that PATCH only retired names (`settings max_parallel numeric string`, `settings paused yes`, `settings paused false`, `settings provider claude`, `settings model`, `settings model reset`) are kept with their requests as recorded: their answer is now the unchanged settings body, which pins that `PATCH` ignores those names. `settings two errors` keeps its request and loses the `ai_max_parallel` error from its 422 body (relabelled `settings retired name ignored beside an error`). The album field `ai_acted_on_at` is untouched. Reason: FR-003.
- `rust/testdata/golden/api/deliberate.json` (T094): the six fields deleted from its five settings bodies; the request of `settings max_parallel and key` is kept as recorded (its `ai_max_parallel` is now an ignored name). Reason: FR-003.
- `rust/testdata/golden/api/wired.json` (T094): unchanged; it records no settings body and none of the four routes.
- `rust/testdata/captures/api-offline/api/081-get-api-ai-providers.json` and its `index.json` entry (T095): deleted; no other capture renumbered (113 entries). Reason: FR-004.
- `rust/testdata/captures/api-offline/api/080-get-api-settings.json`, `007-patch-settings-acoustid-key.json` (T096): the six fields deleted from the recorded bodies. Reason: FR-003.
- `rust/testdata/captures/api-offline/api/openapi.json`, `001-get-openapi-json.json` (T097): the four paths deleted, the `ActedOnPayload` schema (used only by acted-on) deleted, and `SettingsPatchPayload` loses the six PATCH-side properties `ai_provider_id`, `ai_model_id`, `ai_max_parallel`, `ai_queue_paused`, `ai_api_key`, `firecrawl_api_key` (the document records no settings response schema, so the `_set` names do not appear). The research-batch paths were never added (slice 2) and are not added now. The two documents stay identical. Reasons: FR-004 (routes), FR-003 (settings).
- `rust/testdata/captures/api-offline/python.log.tail.txt`, `checkpoints/*.json`: not edited (raw Python app output that no test reads; the log tail keeps one `GET /api/ai/providers` line).
- `musaic-api/tests/schema_upgrade_copy.rs` (T091): the v18 fixture also seeds `ai_api_key` and `firecrawl_api_key` rows; the pre-upgrade copy is checked to hold none of the three secret values and no secret row.
- T099: `rust/testdata/captures/api-offline/api/082-get-api-ai-jobs.json` not edited; its recorded response is status 200, `application/json`, body `{"jobs": []}`. Byte-identical replay is checked by `capture_replay` at the slice 4 checkpoint.
- `rust/testdata/golden/ai/units.json` (T098): every provider section deleted (`extract_json`, `parse_metadata`, `parse_cover`, `validate_mbid`, `payload`, `prompts`, `recorder`, `format_transcript`, `summarize_tool_*`, `truncate`, `truncate_limit`, `friendly_error`, `no_op_confirmation`, `provider_runtime_status`); `_meta` and `synthesize` kept byte for byte. Reason: FR-001, the provider, its prompts, payload, transcript and reply parsing are gone; the resolver synthesis stays.
- `rust/testdata/golden/settings-defaults.json`: not edited (the Python app's record). `musaic-db/tests/python_written_db.rs` drops `ai_max_parallel`, `ai_queue_paused`, `firecrawl_api_key` from the comparison and checks they have no default now. Reason: FR-003.
- `rust/testdata/golden/persistence/settings.json` and the secrets settings golden: not edited; `musaic-secrets/tests/settings_golden.rs` removes the six fields from the Python app's body (each must be present in the record). Reason: FR-003.
- Test counts that moved with the data: `musaic-api/tests/validation_bytes.rs` (byte samples 11 to 10: the route_order 422 sample was the `active/acted-on` probe; public 422 bodies 44 to 41; private total 62 to 59, not run here: no private test data on this machine), `golden_cases.rs` (422s 16 to 13, provider delta removed), `deliberate.rs` (cases 130 to 124; gated writing routes 40 to 37).

### Slice 4 checkpoint (T100, T101)

| Command | Result |
| --- | --- |
| T100: `cargo test -p musaic-app --test ops_boundary --test ops_all_groups`; `cargo test -p musaic-api --test thin_handlers --test operations_inventory`; `cargo test -p musaic-cli --test no_db_owner` | 2 + 5; 4 + 4; 2 passed (unchanged tests) |
| T101 targeted: `musaic-ai` (golden_units 2, no_model_calls 2, positional_skip 1); `musaic-secrets` (39, 2 ignored); `musaic-services --test retired_keys` (4); `musaic-app --test ops_settings --test delegation --test ops_agent --test ops_agent_events --test ops_fixes --test security_agent_events` (12, 9, 48, 11, 20, 3); `musaic-api --test settings_retired --test schema_upgrade_copy --test inventory --test log_secrets --test deliberate --test golden_check_fixes --test capture_replay` (1, 11, 10, 1, 19, 5, 1) | all pass |
| `cargo test -p musaic-desktop` | 34 passed, 2 ignored |
| `cargo test --workspace --no-fail-fast` | 1886 passed, 0 failed, 11 ignored |
| `cargo fmt --all --check` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo run -p musaic-cli -- catalog --check` | exit 0 |

### Slice 5

- `rust/testdata/golden/ai/units.json` (T103): not edited. Its `synthesize` cases are the Python app's record and carry the Python app's notes text; `musaic-ai/tests/golden_units.rs` now has one test that compares the synthesizers with the record through one enumerated delta (the Python notes text reads `synth::DERIVED_NOTES`, "Derived from MusicBrainz."), still byte for byte on the order-sensitive `metadata_text`. The test that ran the synthesizers in the Python wording is deleted: no build can show it.
- `rust/testdata/golden/checks/pure.json`, `scenarios.json`: not edited. `golden_pure.rs` keeps the delta test (the three descriptions that name AI are shown as `wording::NEUTRAL_DESCRIPTIONS`) as its only meta test and drops the Python-wording assertions of `summaries_as_shown`; `golden_scenarios.rs`'s `default_post_scan_and_summary` replays the record with the same description delta (it asserts each replaced description named AI in the record). Neutral expectations did not move.
- Resolver goldens (`golden/resolver/*`) and the API captures: not edited. The resolver still records the Python app's unanchored note (it is memoized and recorded); `AlbumResolution::reword_notes()` now always shows the neutral note.

### Slice 5 checkpoint (T104)

| Command | Result |
| --- | --- |
| `cargo test -p musaic-core -p musaic-checks -p musaic-resolve -p musaic-ai -p musaic-services -p musaic-app --no-fail-fast` | all pass |
| `cargo fmt --all --check` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo test --workspace --no-fail-fast` | 1882 passed, 0 failed, 11 ignored (4 fewer than slice 4: the `AiWording` unit test, the work snapshot's no-provider quit test, and the Python-wording halves of `golden_pure` and `golden_units` merged into their delta tests) |

### Slice 6

- Docs only, plus comment rewording found by the T114 grep (Rust comments in `musaic-app`, `musaic-api`, `musaic-checks`, `musaic-db`, `musaic-desktop`, `musaic-resolve`, `musaic-services`, two test comments, one frontend comment in `DetailSheet/corrections.ts`) and one user-visible refusal: the merge refusal now says "has a research request (…) that is <status>; finish or cancel it first" instead of naming an AI job (no test or golden pins it). Retired-key code keeps naming the retired keys (`musaic-secrets`, `musaic-db/src/settings.rs`, `migrate.rs`).
- No served skill byte changed; `docs/agent-tools.*` and `docs/agent-events.*` are as generated in T063.
- `docs/operations.md` keeps 112 rows (67 Python routes, 43 `EXTRA_ROUTES`, 2 aliases), as `operations_inventory.rs` requires: T106's "111" predates the group-cancel route added in slice 2.
- `AGENTS.md` had drifted from `CLAUDE.md` (schema v21, twelve tools, the old `musaic-ai` row, catalog text); its body is now `CLAUDE.md`'s below its own header and its "Before finishing a change" paragraph.

### Slice 6 checkpoint (T113, T115)

| Command | Result |
| --- | --- |
| `cargo test -p musaic-api --test operations_inventory` | 4 passed |
| `cargo run -p musaic-cli -- catalog --check` | exit 0 |
| `cargo test --workspace --no-fail-fast` (same run as T104, after every slice 6 source edit) | 1882 passed, 0 failed, 11 ignored |
| `npm test` (from `frontend/`) | 56 files, 545 passed |
| `npm run lint` | 0 errors (warnings only in files this spec did not touch) |


## Final chain (T117, T118)

Run on 2026-11-24 after every slice, from `rust/` and `frontend/`. The first column is the parent's run; the second is the rerun after the small wording fixes found by the quickstart walk (T119, below).

| Command | Result | After the T119 fixes |
| --- | --- | --- |
| `cargo fmt --all --check` | clean | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean | clean |
| `cargo test --workspace --no-fail-fast` | 1882 passed, 0 failed, 11 ignored | 1882 passed, 0 failed, 11 ignored (assertions added inside an existing test) |
| `cargo run -p musaic-cli -- catalog --check` | exit 0 | exit 0 |
| `npm run lint` | 0 errors, 34 warnings (files this spec did not touch) | 0 errors, 34 warnings |
| `npm run typecheck` | ok | ok |
| `npm run typecheck:test` | ok | ok |
| `npm test` | 56 files, 545 passed | 56 files, 546 passed (one new model test) |
| `npm run build` | ok | ok |
| T118: `cargo test -p musaic-desktop --test boot -- --ignored --nocapture` (after the UI build) | passed | 2 passed |

Against the baseline (1861 passed, 10 ignored; frontend 512): no baseline failures, and none now.

## Quickstart walk (T119)

Setup: `SCRATCH="$(mktemp -d)"` (under the system temp dir), the synthetic library copied to `$SCRATCH/library` (the repository copy untouched), `musaic-api --config-dir "$SCRATCH/config" --verification-memory-secrets --network offline --port 18765` with `MUSAIC_FRONTEND_DIR=frontend/dist`, driven with `curl` for the API steps and with Chrome (DevTools protocol, a scratch browser context) on the same server for the UI steps. The desktop shell was not launched: the plain server hosts the same routes, the agent bridge and the dev credential files when its secrets are in memory, and the desktop's own pieces (launch cookie, navigation lock) are covered by the boot test. Agent tool calls: `musaic-cli call --config-dir "$SCRATCH/config" --dev-credentials --pairing <id> <tool> --input …` (the quickstart's command line lacked `call` and `--pairing`; fixed in `quickstart.md`). Scan: 13 albums, 20 open findings after the checks ran.

### US1, Fix sends what the resolver cannot settle

1. Review lists "No cover art" for Second Light; its button is "Find cover art…". Pass.
2. Its description is "Tries MusicBrainz and the Cover Art Archive first. Anything it can't settle is sent to your assistant for research. Add a hint to help."; the other three research checks read "Tries MusicBrainz first. …". Pass. The hint dialog itself was not opened in the browser (covered by the HintDialog and FindingFixButton tests).
3. `POST /api/checks/no-cover-art/findings/16/fix {"hint": …}` answers `ok: true`, "Research requested. You can follow it in Review.", one queued `cover.research` request with the hint; Review's rail and Activity list it under **Research requests**; the finding row shows **Queued**. Pass.
4. Unidentified album (finding 17) and track gaps (finding 14): each makes one `metadata.identify` request. Fix again with the same input: same request id, no second request (no-cover-art, track-gaps, unidentified-album). Pass. Note: Fix again **with a different hint** makes a second request for the same album and kind. That is FR-011 as written ("one unresolved request per album, kind and input", the spec 004 index on the input digest), but acceptance scenario 5 of US1 ("same kind … no second request") reads stricter; recorded, not changed. Tags out of sync: the synthetic library has no such finding, so it was not walked by hand (covered by `golden/services/check_fixes.json`).
5. Both assistants revoked, then Fix on another album (Twin, finding 20): request made, `queued`, `waitingForAgent: true`, never failed; Activity's card reads **Waiting**, Review shows each such item as waiting. Pass, after a wording fix (below).

### US2, Fix all makes one batch

1. With no assistant added, **Identify all** opens with "Send what the resolver can't settle to my assistant" off and "13 findings in scope". Pass. With an assistant added, the checkbox could not be seen turning on in Chrome: the pairings request stayed queued behind the page's open event streams (Chrome allows six HTTP/1.1 connections per host; the page holds four long-lived streams, and a previous page in the same browser held more). Not a defect of this spec (no stream was added); the default is computed from the pairings as they load (`BulkQuickFixDialog.tsx`), and `BulkQuickFixDialog.test.tsx` covers both defaults.
2. `POST …/unidentified-album/quick-fix-all?research=1`: `done` = 13 total, 11 sent for research, 2 already requested, one `batch_id`. `GET /api/research/batches` lists one batch of 11 with counts by state; Activity shows "11 research requests from Fix all · Album not yet identified" with counts and "See the requests in Review"; Review folds it into one row that opens to its 11 albums. Pass.
3. `work-list {"batchId"}`: the batch's 11 items only. `work-claim-many {batchId, max 25, idempotencyKey "manual-claim-0001"}`: 11 claims, each with its own lease; the same call again returns identical claims. `work-renew-many` with the 11 leases: 11 renewed, none refused. Pass.
4. One claim completed (`no_match`) so it awaits review, then `POST /api/research/batches/{id}/cancel`: 10 cancelled, the one awaiting review kept (`awaiting_review: 1`). Pass.
5. Without `research=1`: `done` without research keys (13 skipped), no batch, no request. One album folder moved out and a rescan marks it Missing; a run with `research=1` reports it `not-applicable` ("Album is missing from disk …") and makes no request for it (9 sent, 3 already requested, new batch of 9). Pass.

### US3, No provider to configure

Settings, Settings → Assistants, Review (all, and the no-cover-art filter), the album panel of a present album with no request and of one with a request awaiting review, and Activity: none of "AI job", "AI backend", "Resume queue", "not available in this build", FireCrawl, provider, model, parallelism, queue pause, "Claude" appears. Settings → Assistants shows **Added** just now, **Connected** just now, **Listening for work** no, **Last request picked up** never. The album panel offers **Request research…** (disabled, titled "Research is already requested for this album", while a request is unresolved). Pass, after a wording fix (below).

### US4, Upgrade keeps history

Each fixture copied to `$SCRATCH/us4-<tag>/config/library.db` and the server started twice on it (`--port 18766`):

| Fixture | Snapshot | After start | Old rows | Second start |
| --- | --- | --- | --- | --- |
| `rust/testdata/db/v20-jobs.db` | `backups/library-<tag>-v20.db` | schema 24, no `ai_jobs`, settings only `retired_keys_removed` | all 10 listed by `GET /api/ai/jobs` with type, hint, creation time and outcome kept | `.dump` identical; job list identical |
| `rust/testdata/db/v22-jobs.db` | `backups/library-<tag>-v22.db` | same | same | same |
| `rust/testdata/db/v23-jobs.db` | `backups/library-<tag>-v23.db` | same | same | same |
| `golden/persistence/old-schemas/v16.db` (+ two key rows, US5) | `library.v16.before-rust.db` and `backups/library-<tag>-v24.db` | same | (no jobs) | same |

Outcomes as the data model maps them: the old `running` job is queued again (interrupted), `paused` is failed (`legacy_state_unsupported`), the `lyrics` job is a history-only card (`completed`, no request behind it), the completed `metadata+cover` job is two requests `awaiting_review` (`legacy_result_unconverted`) whose group lists the old result as `historicalResult`. Its cover preview was not verified: the fixture's old result has an empty cover URL, and `GET /api/ai/jobs/{card id}/cover-preview` looks the card up as an old job id, so for an old job linked to a request group (v22 and v23 linked them) it answers 404 "Job not found" rather than "no suggested cover". Behaviour as before this spec (the route always read the old job by its own id); recorded for the caller, not changed. Merge or convert refused while a member has an unresolved request: the Twin conversion preview answers the `job_active` blocker naming the album. Pass, after a wording fix (below); the merge refusal itself is covered by `merge_album_rows.rs`.

### US5, Keys removed once

Copy of `old-schemas/v16.db` with `ai_api_key` and `firecrawl_api_key` rows added (dummy values). After start: neither row in `settings` (only `retired_keys_removed`), `GET /api/settings` has only `acoustid_api_key_set`, and no file in the config dir (database, WAL, `server.log`, the pre-migration backup, the snapshot) contains either value; the log line names the keys removed, never a value. Second start: the marker is present and the database dump is unchanged. Pass. The keyring half (no delete attempted on the second start) cannot be observed with in-memory keys; it is covered by `musaic-services/tests/retired_keys.rs`.

### Fixes made during the walk (small, with tests)

- Review's waiting note said "Waiting for an agent." (contract: "Waiting for an assistant"): `frontend/src/components/DelegatedWork.tsx`, its test (`DelegatedWork.test.tsx` now asserts the new sentence), two comments in `useLibraryEventsStream.ts` / `.test.tsx`, and `docs/mcp.md`. The skill's `work-lifecycle.md` keeps "Waiting for an agent" (agent-facing text; a served byte would need a skill version bump).
- The toolbar named research requests "jobs" ("6 jobs queued", "1 job running"): `frontend/src/lib/activityModel.ts` now says "N research request(s) queued / running"; `chromeControls.test.tsx` follows and `activityPanel.test.tsx` gains a model test.
- The conversion blocker for a member with an unresolved request said "A job is queued or running … Wait for it to finish." and "… on the Jobs page first.": `musaic-fsops/src/multi_disc.rs` now says "A research request on <album> is waiting or in progress. Let it finish or cancel it in Review first." and "A research request on <album> has a suggestion waiting, made for that disc alone. Apply or discard it in Review first."; the blocker code `job_active` is unchanged. `musaic-api/tests/multi_disc_preview_http.rs` asserts both texts. No golden records these messages.

### Not verified

- The desktop shell on macOS by hand (the boot test runs it hidden), and any other platform (Linux, Windows): CI.
- A real assistant (Claude Desktop, Claude Code or another MCP host) taking work end to end; `musaic-cli setup` was not run. Tool calls were made by hand through `musaic-cli call`.
- Live network: every run was `--network offline`; no MusicBrainz, AcoustID or Cover Art Archive request, so the resolver settled nothing and no cover preview was fetched.
- The OS keyring (memory secrets only).
- The tags-out-of-sync Fix by hand (no such finding in the synthetic library), the hint dialog's own text in a browser, and the checkbox turning on with an assistant added (see US2.1).

## Summary (T120)

- Baseline failures: none (baseline 1861 passed, 10 ignored; frontend 512 passed).
- Final counts: see "Final chain" above.
- Issues: #377 filed in T037 (agent proposals and change records keep the old album id after a rename, adoption or merge); #375 pre-existing, referenced in `spec.md` Assumptions (approving several proposals of a batch at once), confirmed in T116, not filed again. No issue filed during T116 to T121.
- Goldens and recorded data changed, with reasons: listed per slice above (slice 1: `golden/ai/worker.json` deleted, `golden/services/check_fixes.json` changed, three new synthetic fixtures `rust/testdata/db/v{20,22,23}-jobs.db`; slice 4: `golden/api/routes.json`, `route_order.json`, `cases.json`, `deliberate.json`, `golden/ai/units.json`, the capture `081-get-api-ai-providers.json` deleted, `080-…`, `007-…`, `openapi.json`, `001-…` edited; slices 2, 5 and 6 and T119 changed none).
- Deviations from plan and tasks (collected from the slice notes):
  - Slice 1: `scenarios.json` (T033) needed no edit; the D6 tightening (awaiting review blocks a conversion) is covered by the multi-disc HTTP tests instead.
  - Slice 2: `openapi.json` / `001-get-openapi-json.json` (T064) not edited: they are the Python app's FastAPI record and carry no extra routes; the route inventory is pinned by `golden/api/routes.json` and `inventory.rs`.
  - Slice 4: `route_order.json` probes on removed routes deleted rather than rewritten (no recording exists without the routes); the six settings cases that PATCH only retired names kept as recorded (they now pin that the names are ignored).
  - Slice 5: the Python-wording halves of `golden_pure` and `golden_units` merged into their delta tests (4 fewer workspace tests than slice 4).
  - Slice 6: `docs/operations.md` keeps 112 rows, not T106's 111 (the group-cancel route added in slice 2); `AGENTS.md` had drifted and now carries `CLAUDE.md`'s body below its own header.
  - T119: the walk used the plain server with Chrome instead of the desktop shell (see above); three wording fixes made; the quickstart's CLI command line corrected.
