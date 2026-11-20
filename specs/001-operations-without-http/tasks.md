---

description: "Task list for making Musaic's operations usable without HTTP"
---

# Tasks: Operations Usable Without HTTP

**Input**: Design documents from `/specs/001-operations-without-http/`

**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/, quickstart.md

**Tests**: Included. The spec asks for them (FR-020, FR-021, FR-022).

**Organization**: By delivery slice, one route group at a time, because the spec makes that
a hard constraint (FR-023) and each slice must leave the workspace green. The user stories
cut across the groups: every group slice delivers part of US1 (callable in-process) and
US2 (preconditions owned by the operation), and the slices with streams deliver US3. Each
task carries the story it serves, and the "Story coverage" section at the end maps stories
to tasks. US4 (nothing observable changes) is the gate task that closes every slice.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: can run in parallel with the other [P] tasks of its phase (different files, no
  dependency on an unfinished task)
- **[Story]**: US1 in-process operations, US2 preconditions owned, US3 progress events,
  US4 existing app unchanged, US5 boundary stays in place
- Paths are relative to the repository root. `crates/` means `rust/crates/`.

## Rules that apply to every task

- Read `specs/001-operations-without-http/contracts/` before writing an operation, an
  error or an event. Names in `data-model.md` are proposals; its rules are not.
- Never edit a file under `rust/testdata/golden/`, an existing file under
  `rust/crates/musaic-api/tests/`, or anything under `frontend/`. If one would have to
  change, stop that item and flag it to the maintainer (FR-028).
- Move rendering code; do not rewrite it. Key order, float repr and keys present only when
  set must survive byte for byte.
- Keep each route's precondition order as the inventory records it.
- An operation is a blocking function. Nothing under `rust/crates/musaic-app/src/ops/` is
  `async` except waiting on a `Feed`.
- Never point a run at `~/.musaic` or a real library.

**The gate** (referred to as "run the gate" below), from `rust/`:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
git diff --stat main -- testdata/golden crates/musaic-api/tests ../frontend
```

All green, and the diff shows no modified golden, no modified pre-existing
`musaic-api` test file (new files are fine) and nothing under `frontend/`.

---

## Phase 1: Setup

**Purpose**: a working tree in which the gate can run.

- [X] T001 Create the first slice's branch `ops/00-foundation` from `main` (never work on `main`; every later phase gets its own branch from the then-current `main`, named `ops/NN-<group>`, and its own pull request), build the UI once with `(cd frontend && npm ci && npm run build)`, and generate the synthetic audio library with `python3 testdata/synthetic-library/gen_fixture_library.py` from `rust/` (pinned tool versions in `CLAUDE.md`), then `git checkout rust/testdata/synthetic-library/library.manifest.json`
- [X] T002 Run the gate on the untouched tree and record the result (test count, any test skipped for missing private data) in the foundation pull request's description, so later slices compare against it. Do not create `docs/operations.md` yet (T010 does)

---

## Phase 2: Foundational (slice 0: foundation and inventory)

**Purpose**: the types, the empty surface, the three boundary tests and the complete
inventory. No handler changes behaviour in this phase.

**⚠️ CRITICAL**: no group slice starts until this phase is merged.

- [X] T003 Create `rust/crates/musaic-services/src/error.rs` defining `OpError` and `Subject` exactly as `specs/001-operations-without-http/contracts/errors.md` lists them (variants `InvalidInput { field, message }`, `Rejected`, `NotFound { what, id, message }`, `Busy`, `NoLibrary`, `Conflict`, `Gone`, `TooLarge`, `Upstream`, `Unavailable`, `Internal`; every variant carries `message: String`; no variant carries a status code), with `From<gate::Busy>`, `From<musaic_db::Error>` and `From<rusqlite::Error>`; export it from `rust/crates/musaic-services/src/lib.rs`. Do not write any conversion from `OpError` to `HttpError`
- [X] T004 Read every raise site of status 400, 410, 413, 502 and 503 (`grep -rn 'HttpError::new(\|ApiError::new(' rust/crates/musaic-services/src rust/crates/musaic-api/src`) and confirm or rename the `Rejected`, `Gone`, `TooLarge`, `Upstream` and `Unavailable` variants in `rust/crates/musaic-services/src/error.rs` and `specs/001-operations-without-http/contracts/errors.md`; if one variant would need two statuses, split it
- [X] T005 Create the surface skeleton in `rust/crates/musaic-app/src/ops/`: `mod.rs` (the `Ops` handle holding an `App`, `App::ops()`, one accessor per group: `system()`, `library()`, `scan()`, `settings()`, `database()`, `tags()`, `analyze()`, `apply()`, `checks()`, `fixes()`, `jobs()`), `error.rs` (re-export of `OpError`, `Subject`), `record.rs` (empty, with a module comment stating it is the allow-list of record newtypes), `events.rs` (`Listening::{Yes, Gone}` and `Feed<T>` with a blocking `next(timeout)` and an async `changed()`, built on `tokio::sync::watch`, ending when `App::shutdown_signal()` fires), and one empty file per group; add `pub mod ops;` to `rust/crates/musaic-app/src/lib.rs`
- [X] T006 Create `rust/crates/musaic-api/src/render/mod.rs` with `impl From<OpError> for ApiError`: one `match` from variant to status per `contracts/errors.md`, body `{"detail": message}`, and `Internal` going through `ApiError::internal` so the cause is logged and the plain-text 500 is sent; register the module in `rust/crates/musaic-api/src/lib.rs`
- [X] T007 [P] Create `rust/crates/musaic-app/tests/common/mod.rs`: a helper that builds an `App` on a scratch config dir under the system temp dir with in-memory secrets and no network, a helper that points it at a copy of `musaic_testdata::synthetic_library()`, and a helper that holds a restore guard on the app's gate; add `musaic-testdata`, `tempfile` and `serde_json` as dev-dependencies in `rust/crates/musaic-app/Cargo.toml`
- [X] T008 [P] Create `rust/crates/musaic-app/tests/ops_boundary.rs` (FR-020): (a) run `cargo metadata --format-version 1` and fail if `axum`, `tower`, `hyper` or `musaic-api` is in the normal or dev dependency closure of `musaic-app`, or if `http` or `http-body` is a direct dependency of `musaic-app` or `musaic-services` (the `http` crate is allowed transitively: `ureq` under `musaic-net` brings it); (b) scan every file under `rust/crates/musaic-app/src/ops/` and fail on `axum`, `http::`, `StatusCode`, `status_code`, `HttpError`, `serde_json`, or `Json` anywhere but `ops/record.rs` (do not ban the bare word `status`: a job's status and the scan's status are domain terms)
- [X] T009 [P] Create `rust/crates/musaic-api/tests/thin_handlers.rs` (FR-021): a const `CONVERTED: &[&str]` (empty for now) naming files under `rust/crates/musaic-api/src/routes/`; for each, fail if the source contains `begin_work`, `.gate`, `.publish(`, `ai_jobs_changed`, `notify_`, or a `musaic_services::` path not in a const allow-list of pure helpers (start that allow-list empty and add entries only with a comment saying why the helper is not an operation); also fail on `st.0.` and on any `App` method call other than `ops()` and `shutdown_signal()` (FR-007). So that converted handlers need neither, add accessor methods on `AppState` in `rust/crates/musaic-api/src/state.rs` for the HTTP-only settings (`frontend_dir`, `ping_interval`, `allowed_hosts`, `cover_body_limit`, `restore_upload_limit`)
- [x] T010 Fill `docs/operations.md` with the layer description and the inventory table: one row per row of `musaic_api::route_table()`, with the columns of the "Route inventory row" section of `specs/001-operations-without-http/data-model.md` (method, path, group, classification, what moves, operation, result, errors, streamed, precondition order). Fill classification, what moves and precondition order now by reading every handler in `rust/crates/musaic-api/src/routes/*.rs`; leave the operation column as the planned name. Record the route count, and mark rows that are HTTP-face only (static files, the SPA fallback)
- [x] T011 In `docs/operations.md`, record the placement decision and its reason (copy from `specs/001-operations-without-http/research.md` D1, FR-019), and settle the two open points from research: whether the second SSE channel in `rust/crates/musaic-api/src/routes/fixes.rs` (near line 371) is the multi-disc conversion and therefore a ninth streamed operation, and which group `rust/crates/musaic-services/src/recording_canonical.rs` belongs to; update `specs/001-operations-without-http/contracts/events.md` to match
- [X] T012 Create `rust/crates/musaic-api/tests/operations_inventory.rs` (FR-002): parse the inventory table in `docs/operations.md` and fail unless every `(method, path)` of `musaic_api::route_table()` appears in it exactly once and the table has no row the router lacks
- [X] T013 Run the gate; confirm the three new tests pass and that no handler under `rust/crates/musaic-api/src/routes/` changed

**Checkpoint**: the inventory is complete and checked against the router; the surface
exists and is empty; the boundary tests run.

---

## Phase 3: system (slice 1) 🎯 MVP

**Goal**: health, the fingerprint pool's status and feed, and the library change events
feed are operations. Proves the `Feed<T>` shape.

**Independent Test**: `cargo test -p musaic-app --test ops_system` reads health and pool
state and receives a typed library change event after a bus publish, with no HTTP type in
scope.

- [X] T014 [P] [US3] Write `rust/crates/musaic-app/tests/ops_system.rs` first and see it fail: health and pool status return typed values; the pool feed yields the current state first and then a value after a pool change; the library-events feed yields a typed event after `App::bus()` publishes one; a slow feed reader does not block a second reader; every feed ends after `App::begin_shutdown()`
- [ ] T015 [US3] In `rust/crates/musaic-services/src/fingerprint_pool.rs`, remove `sse_frame()` from the pool state, keeping the typed state; move the frame's body construction to `rust/crates/musaic-api/src/render/system.rs`. **Deferred to #315**: the existing `pool_stream_wake.rs` and `fingerprint_pool_golden` tests call `sse_frame` / `to_json`, and existing HTTP tests stay unmodified in this feature
- [X] T016 [US1] Implement `rust/crates/musaic-app/src/ops/system.rs`: `health()`, `pool_status()`, `pool_feed() -> Feed<PoolState>` (current state, then one per change, coalesced), `library_events() -> ` a per-subscriber bounded drop-oldest subscription over `musaic-events/src/relay.rs` yielding typed bus events
- [X] T017 [US4] Rewrite the handlers in `rust/crates/musaic-api/src/routes/system.rs` as parse → one operation → render; pings, channel capacity (8 and 2) and shutdown handling stay in the handler and `rust/crates/musaic-api/src/sse.rs`
- [X] T018 [US5] Add `"system.rs"` to `CONVERTED` in `rust/crates/musaic-api/tests/thin_handlers.rs` and fill the system rows' operation, result and errors columns in `docs/operations.md`
- [X] T019 [US4] Run the gate (`pool_http.rs`, `pool_stream_wake.rs` and `sse_socket.rs` pin this group)

**Checkpoint**: a caller with an `App` and no HTTP can read system state and follow two feeds.

---

## Phase 4: library and scan (slice 2)

**Goal**: library folder, albums, album, tracks, cover, remove, anchor clear, resolution,
and the whole scan lifecycle are operations that take their own guard.

**Independent Test**: `cargo test -p musaic-app --test ops_library --test ops_scan` scans
the synthetic library through `ops().scan()` and sees `start`, album / progress events and
exactly one terminal event; a mutating operation is refused with `OpError::Busy` while a
restore guard is held.

- [X] T020 [P] [US3] Write `rust/crates/musaic-app/tests/ops_scan.rs` first: `subscribe(false)` on a configured synthetic library yields `start`, then `album` / `progress` / `marked_missing` events, then exactly one of `Done` / `Aborted` / `Cancelled` / `Error` and nothing after; a second subscriber replays from the first event; `cancel()` during a scan yields `cancelling` then `Cancelled` and writes no snapshot; `cancel()` works while a restore guard is held; `subscribe` with no library returns `OpError::NoLibrary`; `subscribe` during a restore returns `OpError::Busy`
- [X] T021 [P] [US2] Write `rust/crates/musaic-app/tests/ops_library.rs` first: list albums, get an album, tracks and cover as typed values; an unknown album id returns `OpError::NotFound`; `set_library_dir` with a path that is not a directory returns `OpError::InvalidInput` naming `library_dir`; the same call with an invalid path **and** a restore guard held still returns `InvalidInput`; `remove_album` during a restore returns `OpError::Busy` and the row is still there; `remove_album` when free publishes `AlbumDeleted` without the test publishing anything
- [X] T022 [US1] Add `AlbumRecord` and `TrackRecord` to `rust/crates/musaic-app/src/ops/record.rs` per `data-model.md` (typed accessors `id()`, `path()`, `missing_since()`, `display_artist()` calling `musaic_core::album_fields`, plus ordered iteration for a renderer) and add them to the allow-list in `rust/crates/musaic-app/tests/ops_boundary.rs`
- [X] T023 [US2] Convert `rust/crates/musaic-services/src/library_ops.rs`, `albums.rs` and `covers.rs`: replace the `E: From<HttpError>` generics and `refuse(status, …)` with `OpError`, and return typed results or the record newtypes instead of response bodies
- [X] T024 [US3] In `rust/crates/musaic-services/src/scan.rs`, remove `sse_frame()` and `to_json()` from the scan event and scan state, keeping the typed values; adjust `rust/crates/musaic-services/src/scan_run.rs` `forward_events` to hand typed events to its callback
- [X] T025 [US2] Implement `rust/crates/musaic-app/src/ops/library.rs`: `config()`, `set_library_dir(dir)`, `list_albums(refresh)` (takes the guard only when `refresh` is true), `album(id)`, `tracks(id)`, `cover(id, size)` returning bytes and a content tag, `remove_album(id)`, `clear_mb_anchor(id)`, `resolution(id, force, hint)` (holds the guard for its whole run); each takes its own guard in the order the inventory records
- [X] T026 [US3] Implement `rust/crates/musaic-app/src/ops/scan.rs`: `status()`, `start(force)`, `subscribe(force)` (guard before the library check; a guard not needed because a scan runs is released at once), `rescan()` (guard, then library check, then snapshot reset and start), `cancel()` (no guard); make `App::start_scan`, `App::scan_and_subscribe` in `rust/crates/musaic-app/src/scan.rs` `pub(crate)` unless an existing test or `musaic-desktop` calls them, in which case leave them and note it in `docs/operations.md`
  - Part 1 done; part 2 (T027-T030) must still: switch every handler in `routes/library.rs` to `ops().library()` / `ops().scan()` and render (they still call `musaic_services::library_ops` / `st.start_scan` / `st.scan_and_subscribe` directly, with their own `begin_work`); move the wire rendering now kept in `musaic-services/src/scan.rs` (`ScanEvent::{name,data,sse_frame}`, `ScanEventKind::payload`, `ScanState::to_json`, still read by `golden_scan.rs`, `scan_cancel.rs`, `scan_adopt.rs`, `non_utf8_names.rs` and `scan_run::log_scan_end`) into `render/library.rs`, then delete them; make `App::start_scan` / `scan_and_subscribe` `pub(crate)` once no handler calls them (nothing else does).
- [X] T027 [US4] Create `rust/crates/musaic-api/src/render/library.rs` by moving the body-building code out of `rust/crates/musaic-api/src/routes/library.rs` and the scan event/state rendering out of services (keep `kept_stored_spelling` present only when true); rewrite every handler in `routes/library.rs` as parse → one operation → render, with ETag and cache headers for the cover staying in the handler
- [X] T028 [US5] Add `"library.rs"` to `CONVERTED` in `rust/crates/musaic-api/tests/thin_handlers.rs`; fill the library and scan rows in `docs/operations.md`
- [X] T029 [US4] Move any test in `rust/crates/musaic-services/tests/` that asserted on a body this slice moved (candidates: `golden_scan.rs`, `set_library_dir.rs`, `library_ops_errors.rs`) into a new file under `rust/crates/musaic-api/tests/`, comparing the rendered output with the same golden; leave tests whose assertion is still valid where they are
- [X] T030 [US4] Run the gate (`scan_cancel_http.rs`, `request_order.rs`, `validation_bytes.rs`, `first_run_http.rs`, `scan_thumbs.rs` pin this group)

**Checkpoint**: the spec's streamed case and refused-while-busy case both pass through the
surface alone.

---

## Phase 5: settings and database (slice 3)

**Goal**: settings, prompts, UI preferences and the database's backup, restore and
snapshots are operations; `SettingsChanged` and the job-queue wake are sent by the
operation.

**Independent Test**: `cargo test -p musaic-app --test ops_settings` patches a setting and
observes exactly one `SettingsChanged` on the bus without publishing it.

- [X] T031 [P] [US2] Write `rust/crates/musaic-app/tests/ops_settings.rs` first: `patch` publishes `SettingsChanged` once with the changed names only; a patch that changes the AI setting wakes the jobs feed; `patch` with an unknown or invalid value returns `OpError::InvalidInput` naming the field and changes nothing; no result contains a secret value; create, list, delete and restore a snapshot; a restore while work holds the gate returns `OpError::Busy`
- [X] T032 [US2] Convert `rust/crates/musaic-services/src/settings_patch.rs` and `db_admin.rs` to `OpError` and typed results; add `SettingsRecord` to `rust/crates/musaic-app/src/ops/record.rs` and the boundary test's allow-list
- [X] T033 [US2] Implement `rust/crates/musaic-app/src/ops/settings.rs` (`get()`, `patch(changes)`, `acoustid_prompt()`, `dismiss_acoustid_prompt()`, `ui_prefs()`, `put_ui_prefs(prefs)`), moving the two `SettingsChanged` publishes and the `ai_jobs_changed()` call from `rust/crates/musaic-api/src/routes/settings.rs` (lines near 81, 170 and 236) into the operations
- [X] T034 [US2] Implement `rust/crates/musaic-app/src/ops/database.rs` (`info()`, `backup_to(path)`, `restore_from(path)`, `snapshots()`, `create_snapshot()`, `delete_snapshot(name)`, `restore_snapshot(name)`), each taking the work or restore guard itself; inputs are files on disk, never an upload body
- [X] T035 [US4] Create `rust/crates/musaic-api/src/render/settings.rs` and rewrite `rust/crates/musaic-api/src/routes/settings.rs` as parse → one operation → render; receiving the restore upload to a temp file (with `restore_upload_limit`) and framing the backup download stay in the handler
- [X] T036 [US5] Add `"settings.rs"` to `CONVERTED` in `rust/crates/musaic-api/tests/thin_handlers.rs`; fill the settings and database rows in `docs/operations.md`
- [X] T037 [US4] Run the gate (`ui_prefs_http.rs`, `key_scrub.rs`, `log_secrets.rs`, `schema_upgrade_copy.rs`, `verification_secrets.rs` pin this group)

---

## Phase 6: tags (slice 4)

**Goal**: tag reads, tag writes and the vocabulary are operations that skip Missing albums
themselves.

**Independent Test**: `cargo test -p musaic-app --test ops_tags` writes a tag on a
synthetic album and is refused for an album marked Missing.

- [X] T038 [P] [US2] Write `rust/crates/musaic-app/tests/ops_tags.rs` first: read a track's tags as a `TagSet`; write a tag and observe `AlbumTagsChanged`; a write on an album marked Missing is refused or skipped exactly as the HTTP route does today (read the handler first and assert that outcome); a write during a restore returns `OpError::Busy`; an invalid tag name returns `OpError::InvalidInput`
- [X] T039 [US2] Convert `rust/crates/musaic-services/src/track_tags.rs` and `vocabulary.rs` (and `recording_canonical.rs` if T011 assigned it here) to `OpError` and typed results; add `TagSet` to `rust/crates/musaic-app/src/ops/record.rs` and the boundary test's allow-list
- [X] T040 [US2] Implement `rust/crates/musaic-app/src/ops/tags.rs`, one operation per tags route in the inventory, each taking its guard (mutations only) and using `App::with_tag_env`
- [X] T041 [US4] Create `rust/crates/musaic-api/src/render/tags.rs`; rewrite `rust/crates/musaic-api/src/routes/tags.rs` as parse → one operation → render, removing `with_tags`, `with_conn` and `svc`; if `routes/fixes.rs` still imports `tags::svc`, keep a private copy there until slice 8
- [X] T042 [US5] Add `"tags.rs"` to `CONVERTED` in `rust/crates/musaic-api/tests/thin_handlers.rs`; fill the tags rows in `docs/operations.md`
- [X] T043 [US4] Move any `rust/crates/musaic-services/tests/` test whose pinned body moved (candidates: `golden_tags_vocab.rs`, `remove_tags.rs`) to a new file under `rust/crates/musaic-api/tests/`; run the gate (`tags_vocab_http.rs` pins this group)

---

## Phase 7: analyze (slice 5)

**Goal**: the BPM / ReplayGain analysis is a run with a typed event sink. Proves the
run-with-sink shape on the smallest streamed run.

**Independent Test**: `cargo test -p musaic-app --test ops_analyze` runs an analysis on a
synthetic album and collects its events to the terminal one.

- [X] T044 [P] [US3] Write `rust/crates/musaic-app/tests/ops_analyze.rs` first: a refusal (unknown album, busy) is returned before the sink is called once; a successful run calls the sink in order and ends with exactly one terminal event; a sink that returns `Listening::Gone` after the first event is not called again and the run still completes and publishes its bus event
- [X] T045 [US3] In `rust/crates/musaic-services/src/analyze.rs`, replace `HttpError` with `OpError` and the emitted item's `sse_frame()` with a typed `AnalyzeEvent` enum (defined in `rust/crates/musaic-app/src/ops/events.rs` or in the service and re-exported there)
- [X] T046 [US3] Implement `rust/crates/musaic-app/src/ops/analyze.rs`: `run(album_id, options, sink: &mut dyn FnMut(AnalyzeEvent) -> Listening) -> Result<AnalyzeOutcome, OpError>`, taking the guard before the first event and holding it until after the terminal one, using `App::with_analyze_env`
- [X] T047 [US4] Create `rust/crates/musaic-api/src/render/analyze.rs` (event → SSE payload) and rewrite `rust/crates/musaic-api/src/routes/analyze.rs`: parse, open the SSE channel (capacity 1), run the operation on a blocking thread with a sink that renders, sends and returns `Listening::Gone` once the send fails
- [X] T048 [US5] Add `"analyze.rs"` to `CONVERTED` in `rust/crates/musaic-api/tests/thin_handlers.rs`; fill the analyze rows in `docs/operations.md`
- [X] T049 [US4] Move `rust/crates/musaic-services/tests/golden_analyze.rs` to a new `rust/crates/musaic-api/tests/` file if its pinned body moved; run the gate (`analyze_http.rs` pins this group)

---

## Phase 8: apply (slice 6)

**Goal**: applying corrections, clearing, and the cover upload / download / preview are
operations.

**Independent Test**: `cargo test -p musaic-app --test ops_apply` applies a tag correction
to a synthetic album and ends with the updated album.

- [X] T050 [P] [US3] Write `rust/crates/musaic-app/tests/ops_apply.rs` first: an apply run emits its progress events in order and ends with the updated-album event; it publishes `AlbumTagsChanged` (and `AlbumCoverChanged` when art changed) without the caller publishing; apply on a Missing album is refused as today; apply during a restore returns `OpError::Busy` before any event; a cover larger than `MAX_COVER_UPLOAD_BYTES` returns `OpError::TooLarge`
- [X] T051 [US3] In `rust/crates/musaic-services/src/apply.rs`, replace the `emit(name, json)` items and `sse_frame()` with a typed `ApplyEvent` enum and return `OpError`; leave the `HttpError` struct definition in place (other groups still use it) but stop using it in this file
- [X] T052 [US2] Implement `rust/crates/musaic-app/src/ops/apply.rs`: `run(request, sink)`, `clear(album_id, …)`, and the cover operations (upload from bytes, download from a URL through `musaic-net`'s `check_public_url`, preview), each taking the guard labelled `WorkKind::Apply` where the handler does today, using `App::with_apply_env`
- [X] T053 [US4] Create `rust/crates/musaic-api/src/render/apply.rs`; rewrite `rust/crates/musaic-api/src/routes/apply.rs` and `rust/crates/musaic-api/src/routes/preview.rs` as parse → one operation → render; multipart reading and `cover_body_limit` stay in the handler
- [X] T054 [US5] Add `"apply.rs"` and `"preview.rs"` to `CONVERTED` in `rust/crates/musaic-api/tests/thin_handlers.rs`; fill the apply rows in `docs/operations.md`
- [X] T055 [US4] Move `rust/crates/musaic-services/tests/golden_apply.rs` (and `rewrite_warnings.rs`, `cancelled_requests.rs` if their pinned bodies moved) to new files under `rust/crates/musaic-api/tests/`; run the gate (`apply_clear.rs`, `cover_download_http.rs`, `cover_preview_http.rs` pin this group)

---

## Phase 9: checks (slice 7)

**Goal**: listing checks and findings, running a check, ignore / unignore, the sweep
status, the three previews and the check-runs feed are operations.

**Independent Test**: `cargo test -p musaic-app --test ops_checks` runs a check on the
synthetic library and reads its findings.

- [X] T056 [P] [US1] Write `rust/crates/musaic-app/tests/ops_checks.rs` first: list checks; run one and read typed findings; `ChecksRerunFinished` reaches the library-events feed after the run; an unknown check id returns `OpError::NotFound`; a run during a restore returns `OpError::Busy`; ignore then unignore a finding; the plan, cluster and conversion previews re-derive from current disk state (change a file between two calls and see the preview change); the check-runs feed yields the running state on a run start and end, a value after an ignore and after an unignore, and the sweep's progress
- [X] T057 [US2] Convert `rust/crates/musaic-services/src/check_ops.rs` to `OpError` and typed results (findings stay references: album id, file names)
- [X] T058 [US1] Implement `rust/crates/musaic-app/src/ops/checks.rs`: one operation per checks route in the inventory, `run` taking the guard labelled `WorkKind::CheckRun`, the previews wrapping `musaic_fsops::plan::derive_plan`, `duplicate_tracks::cluster_preview` and `musaic_fsops::multi_disc::derive_conversion`, and `runs_feed() -> Feed<CheckRunsState>` (run start and end, ignore / unignore, sweep progress) combining the checks tick and the sweep's changes as `rust/crates/musaic-api/src/routes/checks.rs` (near line 343) does today
- [X] T059 [US4] Create `rust/crates/musaic-api/src/render/checks.rs`; rewrite `rust/crates/musaic-api/src/routes/checks.rs` as parse → one operation → render (channel capacity 4 and pings stay)
- [X] T060 [US5] Add `"checks.rs"` to `CONVERTED` in `rust/crates/musaic-api/tests/thin_handlers.rs`; fill the checks rows in `docs/operations.md`
- [X] T061 [US4] Run the gate (`check_sweep_http.rs`, `checks_rerun_finished.rs`, `run_check_scope.rs`, `multi_disc_preview_http.rs` pin this group)

---

## Phase 10: fixes (slice 8)

**Goal**: fixing one finding, quick-fix-all, the per-check actions and the multi-disc
conversion are operations; the job-queue notification after an enqueue is sent by the
operation.

**Independent Test**: `cargo test -p musaic-app --test ops_fixes` fixes a finding and runs
quick-fix-all to its `Done` event.

- [X] T062 [P] [US2] Write `rust/crates/musaic-app/tests/ops_fixes.rs` first: fix one finding and see the right bus event; a fix that falls through to an AI job wakes the jobs feed without the caller calling anything (and with no model backend returns the not-available outcome and enqueues nothing); quick-fix-all emits `Start`, one `Progress` per open finding, `Done`, publishes `BulkBatchCompleted` once, and does so even when the sink returns `Listening::Gone` after the first event; an unknown check returns `OpError::NotFound` before any event; a second `quick_fix_all` started while one runs gets the outcome a second HTTP client gets today (read the gate's rule for `WorkKind::QuickFixAll` first and assert that; FR-030); every fix during a restore returns `OpError::Busy`, including for an unknown finding (busy comes before the lookup here, as the handler documents)
- [X] T063 [US2] Convert `rust/crates/musaic-services/src/check_fixes.rs` and `fix_flow.rs` to `OpError`, replacing the quick-fix item's `sse_frame()` with a typed `QuickFixEvent`; convert the conversion's progress in `multi_disc_convert.rs` / `multi_disc_gather.rs` to a typed event if T011 confirmed it streams
- [X] T064 [US2] Implement `rust/crates/musaic-app/src/ops/fixes.rs`: `fix(check_id, finding_id, hint)` (calls `ai_jobs_changed()` itself when the fix enqueued a job), `quick_fix_all(check_id, sink)` (guard `WorkKind::QuickFixAll`, runs under `in_bulk_run`), `apply_filename_rename`, `delete_duplicate_file`, `apply_cluster_sort`, `convert_multi_disc` (guard `WorkKind::Apply`; with a sink if it streams), all through `App::with_fix_env` / `with_convert_env`
- [X] T065 [US4] Create `rust/crates/musaic-api/src/render/fixes.rs`; rewrite `rust/crates/musaic-api/src/routes/fixes.rs` as parse → one operation → render, removing `with_fix_env`, `run_fix_env`, `json_ok` and any leftover copy of `svc`
- [X] T066 [US5] Add `"fixes.rs"` to `CONVERTED` in `rust/crates/musaic-api/tests/thin_handlers.rs`; fill the fixes rows in `docs/operations.md`
- [X] T067 [US4] Move `rust/crates/musaic-services/tests/golden_check_fixes.rs` (and `recording_canonical_golden.rs`, `fix_name_search_anchor.rs` if their pinned bodies moved) to new files under `rust/crates/musaic-api/tests/`; run the gate (`check_fixes_http.rs`, `quick_fix_no_pause.rs`, `multi_disc_convert_http.rs`, `multi_disc_gather_http.rs`, `recording_canonical_http.rs` pin this group)

---

## Phase 11: jobs (slice 9)

**Goal**: the AI job queue (list, enqueue, cancel, delete, resume, providers) and the jobs
feed are operations.

**Independent Test**: `cargo test -p musaic-app --test ops_jobs` enqueues a job against the
in-memory provider seam and sees it on the jobs feed.

- [X] T068 [P] [US3] Write `rust/crates/musaic-app/tests/ops_jobs.rs` first: list jobs; enqueue and see the jobs feed yield a new list; enqueueing for an album that already has an active job returns the conflict outcome the HTTP route gives today; cancel, delete and bulk-delete; resume the queue; an unknown job id returns `OpError::NotFound`
- [X] T069 [US1] Replace the body builders in `rust/crates/musaic-app/src/jobs.rs` (`job_body`, `jobs_body`, `conflict_body`, `deleted_body`, `bulk_deleted_body`, `jobs_stream_payload`) with typed results and move their rendering to `rust/crates/musaic-api/src/render/jobs.rs`; convert `rust/crates/musaic-services/src/ai_jobs.rs` to `OpError`
- [X] T070 [US2] Implement `rust/crates/musaic-app/src/ops/jobs.rs`: one operation per jobs route in the inventory plus `feed() -> Feed<JobsList>`; every operation that changes the queue calls `ai_jobs_changed()` itself
- [X] T071 [US4] Rewrite `rust/crates/musaic-api/src/routes/jobs.rs` as parse → one operation → render (channel capacity 2 and pings stay)
- [X] T072 [US5] Add `"jobs.rs"` to `CONVERTED` in `rust/crates/musaic-api/tests/thin_handlers.rs`; fill the jobs rows in `docs/operations.md`
- [X] T073 [US4] Run the gate (`jobs_http.rs`, `jobs_worker_http.rs`, `ai_decisions.rs`, `ai_wording.rs` pin this group)

**Checkpoint**: every route in the inventory names an implemented operation.

---

## Phase 12: Closing and cross-cutting

**Purpose**: make the rules hold without exception and describe the layer.

- [X] T074 [US5] Delete `HttpError` from `rust/crates/musaic-services/src/apply.rs` and its export in `rust/crates/musaic-services/src/lib.rs`, and `impl From<musaic_services::HttpError> for ApiError` from `rust/crates/musaic-api/src/http.rs`; fix every remaining use until `grep -rn 'HttpError' rust/crates --include='*.rs'` prints nothing (FR-029)
- [X] T075 [US5] In `rust/crates/musaic-api/tests/thin_handlers.rs`, replace `CONVERTED` with "every `.rs` file under `rust/crates/musaic-api/src/routes/` except `mod.rs`", so a new routes file is covered without anyone listing it (FR-017, FR-021)
- [X] T076 [US5] Delete `AppState::begin_work` and `begin_work_as` from `rust/crates/musaic-api/src/state.rs`; make the guard-taking helpers in `rust/crates/musaic-app/src/scan.rs`, `envs.rs` and `jobs.rs` `pub(crate)` wherever no existing test and no other crate calls them. Do not edit an existing test to make this possible; list what had to stay public, and why, in `docs/operations.md`
- [X] T077 [P] [US5] Add an end-to-end case to `rust/crates/musaic-app/tests/ops_boundary.rs` or a new `rust/crates/musaic-app/tests/ops_all_groups.rs` asserting there is at least one passing operation test per group accessor on `Ops` (FR-022, SC-002), so a group added later without a test fails
- [X] T078 [P] [US5] Update `CLAUDE.md`: in the crate map, describe `musaic-app` as holding the operations surface (`musaic_app::ops`) and `musaic-api` as an adapter (parse, one operation, render; `lax`, `render/`, SSE framing); add a short "Operations surface" rule saying every face goes through `ops`, handlers take no guard and publish nothing, and where the three boundary tests live; fix the references that name handlers as owners of behaviour (rule 1's settings note, "Adding a health check" step 3)
- [X] T079 [P] [US5] Update `docs/desktop.md` ("Design: an in-process server and one webview"): the shell's loopback HTTP server is one face over `musaic_app::ops`, and other faces call the same operations in-process; link `docs/operations.md`
- [X] T080 [US5] Finish `docs/operations.md`: every inventory row has its operation, result, errors and streamed columns filled; remove the "planned" marks; record the final route count and the number of streamed operations; then tighten `rust/crates/musaic-api/tests/operations_inventory.rs` to fail on any operation-bearing row whose operation cell is empty or names more than one operation (SC-001)
- [X] T081 [US3] Create `rust/crates/musaic-api/tests/stream_equivalence.rs` (SC-007): for the scan, subscribe through `ops().scan()` and through `GET /api/scan/stream` to the same scan (both replay from the first event) and assert the HTTP frames equal, in order, the rendering of the typed events, ignoring pings; for each run (analyze, apply, quick-fix-all, and the conversion if it streams) do the same on two identical copies of a synthetic album, one driven through the operation's sink and one through the HTTP route; for each feed assert the first HTTP frame equals the rendering of the feed's first value
- [X] T082 [US4] Run the whole of `specs/001-operations-without-http/quickstart.md`: the gate, the three boundary tests with each of the four scratch changes that must make one fail (discard each afterwards), the frontend checks (`npm run lint`, `typecheck`, `typecheck:test`, `test` in `frontend/`), the desktop boot test, and a manual pass in the desktop app on a **copy** of a library (scan, browse, apply, run a check, run a fix, analyze, snapshot and restore). Record the outcome in the pull request **Done except the manual desktop pass**, which an HTTP smoke test on a temp copy of the synthetic library stood in for; that pass is still to do before merging.
- [X] T083 Remove build output, scratch config dirs, and each slice's branch and worktree as soon as that slice has merged

---

## Dependencies & Execution Order

### Phase dependencies

- **Phase 1 (Setup)**: none.
- **Phase 2 (Foundation)**: after Phase 1. Blocks every later phase.
- **Phases 3 to 11 (group slices)**: each after Phase 2, merged one at a time in the order
  given (research D8). Hard dependencies between them:
  - Phase 4 (library) introduces `AlbumRecord` / `TrackRecord`, used by tags, apply,
    checks and fixes.
  - Phase 7 (analyze) settles the run-with-sink shape used by apply and fixes.
  - Phase 6 (tags) removes `tags::svc`, which `routes/fixes.rs` imports until Phase 10.
  - Phase 10 (fixes) depends on the apply and checks operations it calls into.
  - Phase 11 (jobs) comes after settings and fixes, the two callers of
    `ai_jobs_changed()`.
- **Phase 12 (Closing)**: after all group slices; T074 cannot compile sooner.
- **Branches**: one branch and one pull request per phase from Phase 2 on (T001); a phase
  starts from `main` after the previous phase has merged.

### Within a slice

Test file first (it fails), then services, then `ops/<group>.rs`, then `render/` and the
handlers, then the `CONVERTED` list and inventory rows, then any moved service test, then
the gate. The gate task closes the slice; nothing merges without it.

### Parallel opportunities

- Phase 2: T007, T008 and T009 are independent files and can run together after T005 and
  T006.
- In each slice, the `ops_<group>.rs` test file ([P]) can be written while the service
  conversion is under way.
- Phase 12: T077, T078 and T079 are independent files.
- Two group slices can be developed at once only if they do not share a service file, and
  they will still conflict in three shared files: `rust/crates/musaic-api/tests/thin_handlers.rs`,
  `docs/operations.md` and `rust/crates/musaic-app/src/ops/record.rs`. Settings (Phase 5)
  and tags (Phase 6) are the safest pair. Merging stays one slice at a time.

### Parallel example: Phase 2

```text
After T005 and T006 are merged locally, in parallel:
  T007  musaic-app/tests/common/mod.rs + dev-dependencies
  T008  musaic-app/tests/ops_boundary.rs
  T009  musaic-api/tests/thin_handlers.rs
Then T010 → T011 → T012 → T013 in order.
```

---

## Story coverage

| Story | Delivered by | Independent test |
|---|---|---|
| US1 in-process operations (P1) | T005, T016, T022, T025, T056, T058, T069, and the `ops/<group>.rs` task of every slice | `cargo test -p musaic-app --test 'ops_*'`: at least one operation per group returns a typed result with no HTTP type in scope |
| US2 preconditions owned (P1) | T021, T023, T025, T031 to T034, T038 to T040, T052, T057, T062 to T064, T070 | a mutating operation returns `OpError::Busy` under a restore guard with nothing changed, `InvalidInput` when also invalid, and publishes its event unasked when free |
| US3 progress events (P2) | T014 to T016, T020, T024, T026, T044 to T046, T050, T051, T068, T081 | a scan through `ops().scan()` yields `start`, album / progress events, then exactly one terminal event |
| US4 existing app unchanged (P1) | the gate task of every slice (T013, T019, T030, T037, T043, T049, T055, T061, T067, T073), the handler rewrites, T081, T082 | every existing HTTP test and golden passes unmodified; no frontend file changed |
| US5 boundary stays in place (P3) | T008 to T012, the `CONVERTED` task of every slice, T074 to T080 | each of the four scratch changes in quickstart section 3 makes a boundary test fail |

---

## Implementation strategy

**MVP**: Phases 1 to 3 (setup, foundation, system). That is the smallest merge that puts
real operations behind the surface, with the boundary tests and the complete inventory in
place. It is not yet enough for an MCP server.

**First useful stop**: after Phase 4 (library and scan). The spec's two named acceptance
cases, a streamed operation and a refused-while-busy case, pass through the surface alone,
and an MCP server could already browse a library and run a scan.

**Then**: one slice per merge, in order, through Phase 11. Each is shippable: unconverted
groups keep working through their old handlers (clarification 5).

**Last**: Phase 12 makes the rules exceptionless and updates the documents.

**Stop conditions, any task**: a golden or an existing `musaic-api` test would need
editing; a frontend file would need editing; a precondition's order cannot be kept. Stop
that item, leave the rest of the slice green, and flag it.
