# Implementation Plan: Operations Usable Without HTTP

**Branch**: `001-operations-without-http` (the feature's name; each slice is developed on its own branch from `main` and merged separately) | **Date**: 2026-11-19 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `/specs/001-operations-without-http/spec.md`

## Summary

Make every Musaic operation callable in-process, with typed inputs, results, errors and
progress events, so the MCP server (next on the roadmap) and any later face call the same
code the HTTP API does.

The surface is a new module tree, `musaic_app::ops`, reached from an `App` handle. Each
operation owns its preconditions (meaning validation, the work guard, the library and
target checks, the Missing rule), publishes its event and sends its follow-up
notification. Services return a typed `OpError` instead of a status code. The
Python-compatible rendering of results and events moves up into `musaic-api`, whose
handlers become: parse, call one operation, render. Four tests keep the boundary in
place. The work lands as a foundation slice, nine route-group slices and a closing slice,
each leaving the workspace green with no golden, no existing HTTP test and no frontend
file modified.

Decisions and their reasons are in [research.md](research.md); the types in
[data-model.md](data-model.md); the rules in [contracts/](contracts/); how to verify in
[quickstart.md](quickstart.md).

## Technical Context

**Language/Version**: Rust 1.99.1 (edition 2024, pinned by `rust/rust-toolchain.toml`); `unsafe_code = "forbid"`

**Primary Dependencies**: existing workspace crates only. `musaic-app` keeps `tokio` with the `sync` feature (watch channels, no runtime). No new runtime dependency. Dev-dependencies added to `musaic-app`: `musaic-testdata`, `tempfile`, and `serde_json` for reading `cargo metadata` in the boundary test.

**Storage**: SQLite (`library.db`, schema v20) and the FLAC library on disk. No schema change, no new stored data.

**Testing**: `cargo test --workspace`; new suites `musaic-app/tests/ops_*.rs`, `musaic-app/tests/ops_boundary.rs`, `musaic-api/tests/thin_handlers.rs`, `musaic-api/tests/operations_inventory.rs`. The existing `musaic-api` tests and `rust/testdata/golden` are the definition of "no observable change" and are not edited.

**Target Platform**: macOS, Linux, Windows desktop (Tauri 2 shell); the standalone `musaic-api` server for UI development

**Project Type**: desktop app: Rust workspace backend plus a React UI. This feature touches the backend only.

**Performance Goals**: no user-noticeable change. No new benchmark gate; an operation does the work its handler did, minus nothing, plus at most a struct-to-body rendering step.

**Constraints**: byte-identical HTTP behaviour; frontend untouched; the six architectural rules and the security posture hold; clippy `-D warnings` and `cargo fmt` clean; one route group per slice, each green.

**Scale/Scope**: about 72 routes in ten route files (about 3,100 lines of handlers); about 30 guard acquisitions, two event publishes and two job-queue notifications to move out of handlers; `HttpError` used in 11 service modules (about 105 mentions); eight streamed operations, possibly nine (see research).

No item is left as NEEDS CLARIFICATION. Three things are decided by reading each handler
in slice 0 rather than here: the exact route count, whether the second stream in
`fixes.rs` is the multi-disc conversion, and the confirmed names of the 400 / 410 / 413 /
502 / 503 error variants.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

`.specify/memory/constitution.md` is the unfilled template, so there are no ratified
principles to check against. The gates used instead are the project's standing rules in
`CLAUDE.md`, which the spec makes binding (FR-025 to FR-028).

| Gate | Before design | After design |
|---|---|---|
| Rule 1: publish events, never invoke the fan-out | Pass | Pass: publishing moves from two handlers into operations; no operation calls a rerun or subscriber. |
| Rule 2: findings store references, dialogs re-derive | Pass | Pass: the three preview operations wrap the existing `derive_*` functions. |
| Rule 3: path identity by same-file | Pass | Pass: no path comparison is added or moved out of `musaic-fsops` / `musaic-tags` / `musaic-scan`. |
| Rule 4: one accessor per album-field cascade | Pass | Pass: `AlbumRecord::display_artist()` calls `musaic-core`'s accessor; it defines no fallback order. |
| Rule 5: SSE-driven invalidation in the UI | Pass | Pass: the relay and both UI streams are unchanged on the wire; the frontend is not edited. |
| Rule 6: presence is data | Pass | Pass: the Missing check becomes part of each file-touching operation; the two row-dropping paths stay the only two. |
| Security posture | Pass | Pass: loopback bind, Host/Origin check, launch cookie, CSP and `check_public_url` stay in their crates; operations return no secret value. An in-process caller is trusted exactly as the in-process HTTP server is. |
| `unsafe` forbidden, clippy `-D warnings`, `cargo fmt` | Pass | Pass |
| No golden or existing HTTP test modified | Pass | Pass, with one consequence recorded in research D4: a service-level test that pins a rendered body moves to a new `musaic-api` test file when its rendering moves. |
| Crate dependency direction | Pass | Pass: nothing new depends upward; `musaic-app` gains no dependency. |

No violation to justify; Complexity Tracking is empty.

## Project Structure

### Documentation (this feature)

```text
specs/001-operations-without-http/
├── spec.md
├── plan.md              # this file
├── research.md          # Phase 0: current state, decisions D1-D10, risks
├── data-model.md        # Phase 1: Ops, Operation, OpError, records, events, Feed
├── quickstart.md        # Phase 1: how to verify each slice and the end state
├── contracts/
│   ├── operations-surface.md   # what a face may rely on; what a handler may be
│   ├── errors.md               # OpError variants and the single HTTP mapping
│   └── events.md               # sequences for runs, scan and feeds
├── checklists/
│   └── requirements.md
└── tasks.md             # Phase 2, written by /speckit-tasks
```

### Source Code (repository root)

```text
rust/crates/
├── musaic-services/src/
│   ├── error.rs                 # NEW: OpError (re-exported by musaic_app::ops)
│   ├── apply.rs                 # HttpError lives here today; deleted in the closing slice
│   └── *.rs                     # per slice: return OpError and typed results;
│                                #   sse_frame() / response-body builders move out
├── musaic-app/
│   ├── src/
│   │   ├── lib.rs               # pub mod ops
│   │   ├── ops/                 # NEW: the operations surface
│   │   │   ├── mod.rs           # Ops handle, App::ops(), re-exports
│   │   │   ├── error.rs         # re-export of OpError, Subject
│   │   │   ├── record.rs        # the allow-listed record newtypes
│   │   │   ├── events.rs        # Listening, Feed<T>, event enums
│   │   │   ├── system.rs
│   │   │   ├── library.rs
│   │   │   ├── scan.rs
│   │   │   ├── settings.rs
│   │   │   ├── database.rs
│   │   │   ├── tags.rs
│   │   │   ├── analyze.rs
│   │   │   ├── apply.rs
│   │   │   ├── checks.rs
│   │   │   ├── fixes.rs
│   │   │   └── jobs.rs
│   │   ├── scan.rs, jobs.rs, envs.rs   # existing guard-taking helpers become
│   │   │                               #   pub(crate), used by ops
│   │   └── app.rs
│   └── tests/                   # NEW directory
│       ├── common/              # scratch config dir, synthetic library, gate helpers
│       ├── ops_boundary.rs      # dependency and source checks (FR-020)
│       └── ops_<group>.rs       # one per group (FR-022)
└── musaic-api/
    ├── src/
    │   ├── render/              # NEW: typed result / event → Python-compatible body
    │   │   ├── mod.rs           # From<OpError> for ApiError (the one status mapping)
    │   │   └── <group>.rs
    │   ├── routes/<group>.rs    # per slice: parse → one operation → render
    │   ├── lax.rs, sse.rs, http.rs, starlette_routing.rs   # stay
    │   └── state.rs             # begin_work / begin_work_as deleted in the closing slice
    └── tests/
        ├── thin_handlers.rs          # NEW (FR-021)
        ├── operations_inventory.rs   # NEW (FR-002)
        ├── stream_equivalence.rs     # NEW, closing slice (SC-007)
        └── (existing files: unmodified)

docs/
├── operations.md        # NEW: the layer, the placement decision, the route inventory
└── desktop.md           # updated in the closing slice
CLAUDE.md                # crate map and rules updated in the closing slice
frontend/                # untouched
```

**Structure Decision**: the surface is `musaic_app::ops` (research D1). No new crate. The
HTTP face gains a `render/` module so that rendering has one home beside `lax` (parsing).
The inventory lives in `docs/operations.md` so it ships with the project, not with this
feature's spec folder.

## Delivery Slices

Each slice is one reviewable change that leaves `cargo test --workspace`, clippy and fmt
green, with the checks of quickstart section 1 passing.

| # | Slice | Contents | Proves |
|---|---|---|---|
| 0 | Foundation and inventory | `OpError`; empty `ops` tree and `App::ops()`; `render/` with the error mapping; `ops_boundary`, `thin_handlers` (empty group list) and `operations_inventory` tests; `docs/operations.md` with every route classified; the three slice-0 confirmations. No handler changes behaviour. | FR-001, FR-002, FR-019, FR-020 |
| 1 | system | health, pool status, pool feed, library-events feed | the `Feed<T>` shape |
| 2 | library | config, albums, album, tracks, cover, delete, anchor clear, resolution; scan status / start / subscribe / rescan / cancel | guard ownership; the scan sequence; first streamed `ops_` test and the busy test |
| 3 | settings | settings, prompts, UI prefs; database info, backup, restore, snapshots | publish and notification inside operations; restore through the gate |
| 4 | tags | tag reads and writes, vocabulary | Missing rule inside operations |
| 5 | analyze | the analysis run | run-with-sink on the smallest streamed run |
| 6 | apply | apply run, clear, cover upload / download / preview (`preview.rs`) | the apply pipeline's events, typed |
| 7 | checks | lists, run, ignore / unignore, sweep, previews, check-runs feed | rule 2 previews as operations |
| 8 | fixes | fix one, quick-fix-all, the per-check actions, conversion | follow-up job notification owned by the operation |
| 9 | jobs | list, enqueue, cancel, delete, resume, providers, jobs feed | the last feed |
| 10 | Closing | delete `HttpError`, `AppState::begin_work*`, the converted-groups list; tighten visibility where existing tests allow; `CLAUDE.md` crate map, `docs/desktop.md` | FR-007, FR-017, FR-024, FR-029 without exception |

**Per-slice recipe** (the same for slices 1 to 9):

1. From the inventory, list the group's routes with what moves for each.
2. In the group's services: return `OpError` and typed results; move `sse_frame()` and
   response-body builders out.
3. Write `ops/<group>.rs`: one operation per route, preconditions in the recorded order.
4. Write `render/<group>.rs` by moving the body-building code; rewrite each handler as
   parse → operation → render.
5. Add the group to `thin_handlers`' list; fill its operation names into the inventory.
6. Add `musaic-app/tests/ops_<group>.rs`.
7. Move any service test whose pinned body moved, into a new `musaic-api` test file.
8. Run quickstart sections 1 to 3.

**Stop conditions** (any slice): a golden or an existing `musaic-api` test would need
editing; a frontend file would need editing; a precondition's order cannot be preserved.
Stop that item and flag it.

## Complexity Tracking

No constitution violations to justify.
