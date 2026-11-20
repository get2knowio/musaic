# Research: Operations Usable Without HTTP

**Date**: 2026-11-19 | **Spec**: [spec.md](spec.md) | **Plan**: [plan.md](plan.md)

Every finding below was read from the code on `main` at `adaa4b18`. Where something was
inferred rather than read, it says so.

## What the code looks like today

| Fact | Where |
|---|---|
| `musaic-app` holds the application object (`App`, `AppInner`, `AppBuilder`), the gate handle, bus, scan service, fingerprint pool, check sweeper, AI queue host, clock and secrets. Its manifest says "no HTTP; musaic-api and later faces wrap it". It depends on `tokio` with the `sync` feature only (watch channels, no runtime). | `musaic-app/Cargo.toml`, `musaic-app/src/app.rs` |
| `musaic-api`'s `AppState` wraps `App` with HTTP-only settings (SPA dir, ping interval, allowed hosts, upload caps) and dereferences to it. `begin_work` / `begin_work_as` live on `AppState` and turn `Busy` into a 409. | `musaic-api/src/state.rs` |
| `App` already has a few operation-like methods, but they take a guard the caller acquired: `start_scan(lib, force, guard)`, `scan_and_subscribe(lib, force, guard)`, `cancel_scan()`, `enqueue_job`, `with_fix_env`, `with_apply_env`. | `musaic-app/src/scan.rs`, `jobs.rs`, `envs.rs` |
| Services are synchronous. Handlers run them on `spawn_blocking` through `http::blocking`, which also carries the `in_bulk_run` / `in_post_scan_rerun` flags onto the worker thread. | `musaic-api/src/http.rs` |
| `HttpError { status: u16, detail: String }` is defined in `musaic-services/src/apply.rs` and used in 11 service modules (about 105 mentions). `library_ops` functions are generic over `E: From<HttpError> + From<rusqlite::Error> + …` and are called as `list_albums::<ApiError>`. | `musaic-services/src/*.rs` |
| `musaic_services::Json` is an ordered value (object keys in insertion order, Python float repr) that every service payload is built from, so the HTTP layer can write bytes identical to the Python app's. | `musaic-services/src/json.rs` |
| Status codes raised today: 400, 404, 409, 410, 413, 422, 500, 502, 503 for operations; 403 and 405 belong to the HTTP face (host check, routing). | grep of `HttpError::new(` / `ApiError::new(` |
| Handlers take the guard about thirty times, in `library.rs`, `apply.rs`, `checks.rs`, `fixes.rs`, `settings.rs`, `tags.rs`. `settings.rs` publishes `SettingsChanged` twice. `fixes.rs` and `settings.rs` call `ai_jobs_changed()`. | `musaic-api/src/routes/` |
| Typed events exist for some streams but render themselves: `sse_frame()` is defined on the scan event, the apply item, the quick-fix item and the pool state, all in `musaic-services`. | `scan.rs:273`, `apply.rs:117`, `check_fixes.rs:361`, `fingerprint_pool.rs:140` |
| Watch-channel feeds (checks, jobs, pool) are assembled in handlers: subscribe to a `watch::Receiver`, send a first payload, then one per change, with a ping timer. | `routes/checks.rs:343`, `jobs.rs:173`, `system.rs:70` |
| A stream the request did not list: `routes/fixes.rs` opens a second SSE channel near line 371, under `WorkKind::Apply`. By position it is the multi-disc conversion (inferred, not read through). The inventory must confirm it; if so there are nine streamed operations, not eight. | `musaic-api/src/routes/fixes.rs:365-402` |
| The route table is recorded by the same calls that register handlers (`route_table()`), and `tests/inventory.rs` compares it with the Python app's route list. | `musaic-api/src/lib.rs`, `route_table.rs` |
| The dependency closure of `musaic-app` contains no `axum`, `tower`, `hyper` or `musaic-api`. It does contain the `http` crate, pulled in by `ureq` under `musaic-net`. | `cargo tree -p musaic-app -e normal` |
| A tenth routes file, `preview.rs` (cover preview), is not one of the nine named groups. | `musaic-api/src/routes/preview.rs` |

## Decisions

### D1. Where the surface lives

**Decision**: In `musaic-app`, as a module tree `musaic_app::ops` with one submodule per
route group, reached from an `App` handle (`app.ops()`).

**Rationale**:
- Every precondition an operation must own is held by `App`: the gate, the bus, the scan
  service, the AI queue host (`musaic-app::ai`), the clock, the secrets store. An operation
  that "takes the guard, acts, publishes, then notifies the job queue" can only be written
  where all four are in reach.
- `musaic-services` cannot host it: the AI queue host and `ai_jobs_changed()` are above it,
  and services run under a guard somebody else took.
- The dependency direction already fits. `musaic-api` and `musaic-desktop` depend on
  `musaic-app`; nothing below does. The MCP server will be one more crate beside
  `musaic-api`.
- The crate already exists for this purpose and already has no HTTP dependency.

**Alternatives considered**:
- *A new crate `musaic-ops` above `musaic-app`*: gives the dependency check a manifest of
  its own, but the crate would need `App`'s internals public to work, which weakens the
  thing it is meant to protect (a face could then reach those internals too).
- *`musaic-services`*: rejected above.
- *Methods directly on `App`*: `App` already has about thirty public methods for hosts
  (builder, shutdown, log). A separate `ops` handle keeps "what a face may call" a
  reviewable list and gives the source checks one directory to scan.

### D2. Synchronous operations; the face picks the thread

**Decision**: Operations are ordinary blocking functions. The HTTP face keeps calling them
through `http::blocking`. Nothing in `ops` is `async` except waiting on a feed.

**Rationale**: the services are synchronous, `musaic-app` has no async runtime, and the
bulk/rerun context flags are thread-local state the face already carries across. An MCP
server on stdio or a native UI can call a blocking function without a runtime at all.

**Alternatives considered**: `async fn` operations would force every face to bring a
runtime and would move `spawn_blocking` (a runtime detail) below the faces.

**Consequence to keep**: some preconditions are checked today *before* the hop to the
blocking thread (validation, guard, library check) so the refusal is immediate. An
operation does those first, inside the call, so the observable order is unchanged.

### D3. One typed error, no status code

**Decision**: `OpError`, an enum defined in `musaic-services` (services must return it) and
re-exported from `musaic_app::ops`. Variants name what went wrong; each carries a plain
message and, where it applies, the subject. `HttpError` is deleted when the last slice
lands. See [contracts/errors.md](contracts/errors.md).

**Rationale**:
- The messages the services produce today are already plain sentences ("No library
  directory configured"). Carrying that sentence in the typed error lets the HTTP face
  render today's body exactly (FR-006) and gives an MCP server something to show.
- The status is a function of the variant alone, written once in `musaic-api`. If the
  inventory finds one variant that would need two statuses, the variant is split. A status
  field is never added.

**Alternatives considered**:
- *Structured facts only, wording produced by the HTTP face*: cleaner in principle, but
  about a hundred messages would have to be re-derived in `musaic-api`, each a chance to
  change a byte a golden pins. Deferred, not ruled out for later.
- *Per-operation error enums*: more precise, but nine groups times several enums, each
  with its own HTTP mapping, for a surface with no stability promise (clarification 4).

**Open point for the inventory**: 422 bodies are today a structured list built by `lax`
during parsing. Meaning-level refusals raised by services are strings. The inventory
records, per route, which refusals are wire-level (stay in `lax`) and which are
meaning-level (`OpError::InvalidInput { field, .. }`).

### D4. Typed results; rendering moves up; a small allow-list for open-ended documents

**Decision**:
- Each operation returns a named struct or enum.
- The code that builds the Python-compatible ordered body from it moves to
  `musaic-api/src/render/<group>.rs`. For bodies assembled in handlers today (most of
  `library.rs`, `settings.rs`) that is a move within the crate.
- Where a result is a document whose shape is owned by stored data (the album row, a
  track's tag map), the result holds it in a named newtype (`AlbumRecord`, `TagSet`) with
  typed accessors for the fields code reads, plus ordered iteration for rendering. These
  newtypes are an explicit allow-list in `ops::record`; nothing else in an operation
  signature may be an untyped value.

**Rationale**: this is what spec assumption "typed does not mean every field is modelled"
and clarification 2 allow, and it is what keeps byte-identical output achievable: the
album body's key order comes from the database row, not from code.

**Alternatives considered**:
- *Model every field*: the album and tag documents are open-ended by nature; a struct
  would need a catch-all map anyway and would re-order keys.
- *Keep `Json` as the return type and call it typed*: fails FR-004.

**Consequence**: service tests that assert on a rendered body (the
`musaic-services/tests/golden_*.rs` family) follow the rendering. Where a slice moves a
body's construction out of a service, the test that pins it moves to a **new** file under
`musaic-api/tests/`. Golden files are not touched; existing `musaic-api` tests are not
touched. Where the body stays a stored-data document inside the service, its test stays.

### D5. Long-running operations: a sink for runs, a feed for subscriptions

**Decision**: two shapes, both typed. See [contracts/events.md](contracts/events.md).
- **Runs** (apply, analyze, quick-fix-all, and the conversion stream if the inventory
  confirms it): the operation takes a sink, `&mut dyn FnMut(Event) -> Listening`, calls it
  for each event in order, and returns after the terminal event. `Listening::Gone` stops
  the calls, never the work. The guard lives inside the operation for its whole run.
- **Scan**: started by one operation, observed by another. `scan().start(..)` and
  `scan().subscribe(..)` wrap today's `ScanService` buffer-and-replay, so every subscriber
  on every face replays the current scan from its first event (FR-030).
- **Feeds** (check runs, AI jobs, fingerprint pool, library change events): the operation
  returns a `Feed<T>` with a blocking `next()` and an async `changed()`. The feed yields
  the current state first, then one typed value per change, coalescing as the watch
  channel does today. Library events keep the relay's bounded drop-oldest queue per
  subscriber.

**Rationale**: these are the shapes the code already has (`emit` callbacks, the scan
subscription, `watch` receivers). The work is to give each a typed event and to move
`sse_frame()` out of the services into `musaic-api/src/render/`.

**Alternatives considered**: a single `Stream` trait object for all eight would need an
async runtime in `musaic-app` and would hide the real difference between "my run" and "a
shared feed".

**What stays in `musaic-api`**: SSE framing, the ping timer, channel capacities, ending
the body on shutdown, and noticing a closed connection.

### D6. Keeping handlers thin, and proving it

**Decision**: four checks, all ordinary tests so they run in CI with `cargo test`.

1. `musaic-app/tests/ops_boundary.rs`
   - reads `cargo metadata` and fails if `axum`, `tower`, `hyper` or `musaic-api` is in
     the dependency closure of `musaic-app`, or if `http` / `http-body` is a *direct*
     dependency of `musaic-app` or `musaic-services`;
   - scans `musaic-app/src/ops/**` and fails on `axum`, `http::`, `StatusCode`,
     `HttpError`, `serde_json`, `status_code`, or `Json` outside `ops/record.rs`. (The
     bare word `status` is not banned: a job's status and the scan's status are domain
     terms.)
2. `musaic-api/tests/thin_handlers.rs` (new file) scans `src/routes/<group>.rs` for
   converted groups and fails on `begin_work`, `.gate`, `.publish(`, `ai_jobs_changed`,
   `notify_`, on `musaic_services::` paths other than an allow-list of pure helpers, and
   on reaching into the application's parts (`st.0.` or an `App` method other than
   `ops()` and `shutdown_signal()`); HTTP-only settings are read through accessors on
   `AppState`.
   The list of converted groups grows by one per slice; the last slice replaces the list
   with "every file in `routes/`".
3. `musaic-api/tests/operations_inventory.rs` (new file) fails unless every row of
   `route_table()` appears exactly once in `docs/operations.md`'s inventory table.
4. `musaic-api/tests/stream_equivalence.rs` (new file, closing slice) compares, for each
   streamed operation, the frames the HTTP stream sends with the rendering of the typed
   events the operation produced (SC-007).

**Rationale**: the `http` crate cannot be banned from the closure (the MusicBrainz client
brings it), so the dependency rule is "not direct, and no HTTP server stack at all", with
the source scan covering use.

**Also**: `AppState::begin_work` and `begin_work_as` are deleted in the last slice, which
makes a guard acquisition in a handler a compile error as well as a test failure, to the
extent existing tests do not call them (to be checked in that slice; existing tests are
not edited to make it so).

### D7. The inventory is a document checked against the router

**Decision**: `docs/operations.md` holds the layer's description and the inventory table:
method, path, group, classification (thin / carries behaviour), what moves, operation,
result modelling (struct / record), error variants, streamed or not. Slice 0 fills every
row's classification by reading each handler; each later slice fills in its group's
operation names as it lands.

**Rationale**: FR-001 and FR-002 make the inventory a deliverable kept with the project;
tying it to `route_table()` keeps it complete without anyone remembering to update it.

### D8. Slice order

**Decision**: foundation first, then smallest and most pattern-setting groups first, the
heaviest last:

0. Foundation and inventory
1. system (health, pool status and feed, library-events feed): proves the feed shape
2. library (config, albums, scan start/subscribe/cancel, resolution): proves guard
   ownership and the scan sequence
3. settings (settings patch, prompts, UI prefs, database backup/restore/snapshots)
4. tags
5. analyze: proves the run-with-sink shape on the smallest streamed run
6. apply (with `preview.rs`)
7. checks
8. fixes (single, bulk, the per-check actions, conversion)
9. jobs
10. Closing: delete `HttpError`, remove the converted-groups list, tighten visibility,
    update CLAUDE.md and `docs/desktop.md`

**Rationale**: the request names the nine groups but not an order. Fixes depends on the
jobs notification and on the apply and checks patterns, so it comes late; jobs is last of
the groups because fixes and settings are its only callers of `ai_jobs_changed()` and
both will already route through `ops`.

### D9. Tests for the surface

**Decision**: `musaic-app/tests/ops_<group>.rs`, one file per group, using
`musaic-testdata` and `tempfile` only. `musaic-app`'s dev-dependencies gain nothing from
the HTTP stack; `ops_boundary.rs` checks dev-dependencies too. The streamed case is a scan
of the synthetic library collected to its terminal event; the refused-while-busy case
holds a restore guard and calls a mutating operation.

**Rationale**: FR-022. Putting the suite in the crate that owns the surface makes "no HTTP
types in scope" true by construction.

### D10. `preview.rs`

**Decision**: treated as part of the apply group (cover preview and download feed the
apply dialog). Its remote fetch keeps going through `musaic-net`'s `check_public_url`.

## Risks

| Risk | Mitigation |
|---|---|
| Moving body construction changes a byte (key order, float repr, a missing-when-false key such as `kept_stored_spelling`). | Renderers are moved, not rewritten; the existing HTTP tests and goldens run unmodified after every slice; a failing golden stops the item (FR-028). |
| A precondition's order changes when it moves from handler to operation (for example `scan_stream` takes the guard *before* the library check, `list_albums` takes it only when `refresh` is set). | The inventory records each route's order; `request_order.rs` and `validation_bytes.rs` already pin many of these. |
| Wire validation and meaning validation are entangled in a handler. | Rule: `lax` produces typed inputs or a 422; anything that needs the database, disk or settings to decide is the operation's. Recorded per route in the inventory. |
| Existing `musaic-api` tests reach into `st.0.bus`, the gate or services directly, so visibility cannot be tightened. | Tightening is best-effort in the closing slice; the source-scan checks do not depend on it. |
| The size: about 3,100 lines of handlers and 105 uses of `HttpError`. | One group per slice, each green; `HttpError` and `OpError` coexist until the closing slice (clarification 5). |
| A ninth streamed operation. | Confirmed or dismissed in slice 0; the shape in D5 covers it either way. |
