# CLAUDE.md

Guidance for Claude Code (claude.ai/code) and other coding agents working in this
repository. `AGENTS.md` points here.

## What this is

**Musaic** is a local-first desktop app for browsing and maintaining a personal FLAC
collection. A Rust workspace (`rust/`) holds the backend and a Tauri 2 desktop shell; the
React/TypeScript UI lives in `frontend/`. The SQLite database (`~/.musaic/library.db`) is
the same file format the earlier Python app used, migrated in place. The Python
implementation is preserved at the git tag `python-final`.

The desktop shell runs the HTTP API in-process on an ephemeral `127.0.0.1` port and points
its single webview at that origin, so the UI talks to the backend with ordinary `fetch`,
`EventSource` and `<img>` requests — there is no Tauri IPC.

## Layout

```
rust/                    Cargo workspace (members = crates/*)
rust/crates/             the crates below
rust/testdata/           public test data (goldens, databases, synthetic-library generator)
rust/rust-toolchain.toml the pinned toolchain
frontend/                React + Vite + TypeScript + Tailwind v4 + TanStack Query
scripts/                 packaging and release helpers (package_macos.sh, desktop_notices.py)
docs/                    design notes (desktop.md: the shell, packaging, quitting, data locations)
```

### Crate map

| Crate | Role |
|---|---|
| `musaic-core` | Shared primitives: album ids (first 16 hex chars of the SHA-1 of the album's resolved path), config (`config.json`, config dir), the album-field accessors, Python-compatible string helpers the stored data depends on |
| `musaic-log` | Small logging facade for the lower crates; `musaic-app` installs the sink that writes `server.log` |
| `musaic-db` | Opens, creates and migrates the SQLite database (schema v22; a v16 file from the Python app is backed up, then migrated; a v17+ file is first copied to a snapshot at startup, `musaic-services/src/startup.rs`); all SQL for albums, findings, check inputs, fingerprints, tags, backups and snapshots, and for folding the rows of a multi-disc set into one (`merge.rs`) |
| `musaic-secrets` | API keys in the OS keyring (`keyring` crate), the one-time move of keys out of the settings table, the in-memory test store |
| `musaic-scan` | Library scanner: finds album folders, parses `Artist - Album [FLAC]` names, reads FLAC metadata into album rows |
| `musaic-tags` | FLAC mutation primitives: Vorbis comment writes, cover embed, `cover.jpg` sidecar, the per-file apply steps |
| `musaic-fsops` | File moves that are not tag writes: filename/directory templates, rename plans and their execution, trash, multi-disc set detection and the conversion plan (`multi_disc.rs`), the per-conversion record under `<config dir>/conversions/` (`conversion_log.rs`) |
| `musaic-dsp` | Chromaprint fingerprints, duplicate-track clustering, ReplayGain (BS.1770) |
| `musaic-bpm` | Tempo (BPM) detection |
| `musaic-net` | Outbound HTTP: MusicBrainz, AcoustID and Cover Art Archive clients, pacing and retry, the public-HTTPS rule for cover downloads, replay transport for tests |
| `musaic-resolve` | The metadata resolver rules as pure functions: AcoustID vote, name search acceptance, tracklist alignment, similarity |
| `musaic-events` | In-process event bus, the post-scan / bulk-run guard flags, the subscriber catalog, the SSE relay core |
| `musaic-checks` | Health-check framework: `Check` trait, registry of the eight checks, findings runner, post-scan reruns, the inputs digest each completed run records (`inputs.rs`) |
| `musaic-services` | Synchronous services: the album apply pipeline, library scan, fingerprinting and its pool, the background check sweep, analysis (BPM / ReplayGain), check fixes, library operations, the multi-disc conversion (`multi_disc_convert.rs`; a set kept in several folders: `multi_disc_gather.rs`) |
| `musaic-ai` | Deterministic resolver synthesis and historical job compatibility. The desktop persists unresolved research in `musaic-db::delegated_work` and delegates it to paired agents |
| `musaic-app` | The application object without HTTP: builder, startup and shutdown, bus wiring, job worker host, process log; and the **operations surface**, `musaic_app::ops` (one module per group: system, library, scan, settings, database, tags, analyze, apply, checks, fixes, jobs), where every user-visible action lives with its guard, validation order, events and follow-up notifications (`docs/operations.md`) |
| `musaic-api` | The HTTP face on axum, an adapter over `musaic_app::ops`: every `/api` route parses its request (`lax`), calls one operation and renders the result (`render/`) and the `OpError` status; also SSE framing, Host/Origin check, launch-cookie gate; and a standalone server binary (`musaic-api`) used for UI development |
| `musaic-commands` | The agent tool contract, a leaf crate (no db, app, axum or tokio): the 21 typed tools and their schemas, permission kinds and `decide`, error codes, the pairing proofs, the descriptor, and the generated catalog (`docs/agent-tools.{json,md}`) |
| `musaic-mcp` | The MCP adapter over the registry, on `rmcp` (pinned `=3.11.0`): stdio, both the 2026-07-28 and 2025-11-25 profiles |
| `musaic-cli` | The bundled `musaic-cli`: the stdio MCP server, `call`, `doctor`, `setup`/`uninstall`, `catalog --check`. It must not link `musaic-db`, `rusqlite`, `musaic-app` or `musaic-api` (`tests/no_db_owner.rs`) |
| `musaic-desktop` | The Tauri 2 shell: single-instance lock, loopback server, navigation lock, native dialogs, reveal in file manager, window state, quit handling |
| `musaic-testdata` | Dev-only: locates public test data and the optional private test data |

## Commands

All Rust commands run from `rust/`. `rustup` picks up `rust-toolchain.toml` (Rust
**1.99.1**, with `rustfmt` and `clippy`); `rust-version` in `Cargo.toml` says 1.99 — keep
the two in step. The workspace forbids `unsafe` code.

The desktop crate's build script copies `frontend/dist/` into the build, so **build the UI
once before any `cargo build` / `cargo test` of the whole workspace**:

```bash
(cd frontend && npm ci && npm run build)
```

### Rust

```bash
cd rust
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check            # cargo fmt --all to fix
```

Tests that need the synthetic audio library fail with the command that generates it (see
Tests below). Linux also needs the WebKitGTK development packages listed in the README.

### Frontend

```bash
cd frontend
npm ci
npm run lint
npm run typecheck          # app sources
npm run typecheck:test     # the *.test.ts(x) files (tsconfig.vitest.json)
npm test                   # vitest (jsdom)
npm run build              # → frontend/dist
```

### Running

```bash
# the desktop app on a scratch config dir, keys in memory, no network
cd rust
cargo run -p musaic-desktop -- --config-dir "$(mktemp -d)/config" --dev-memory-secrets --network offline

# UI development with hot reload: the API server on :8765 + Vite on :5173 (proxies /api)
cargo run -p musaic-api -- --config-dir "$(mktemp -d)/config" --verification-memory-secrets --network offline
(cd frontend && npm run dev)
```

`--dev-memory-secrets` (desktop) and `--verification-memory-secrets` (server) keep keys in
memory and are refused unless the config dir is under the system temp dir or a directory
named `work`, so neither can be pointed at a real `~/.musaic`. Drop `--network offline` to
make live MusicBrainz / AcoustID / Cover Art Archive requests. `musaic-api --help` lists
every server flag.

Never point a development run at `~/.musaic` or at a real music library; use a copy.

### Agent tool catalog

```bash
cd rust
cargo run -p musaic-cli -- catalog --check     # --write regenerates docs/agent-tools.{json,md}
```

CI runs the check; change a tool in `musaic-commands`, regenerate, commit both files.

### Packaging

```bash
cargo install tauri-cli --version 2.12.4 --locked
cargo install cargo-about --version 0.9.3 --locked --features cli
scripts/package_macos.sh       # unsigned Musaic.app + .dmg under rust/target/release/bundle/
```

The script builds the UI, writes `rust/target/desktop-notices/NOTICES.txt`
(`scripts/desktop_notices.py`, via cargo-about), then runs
`cargo tauri build --bundles app,dmg --config tauri.bundle.conf.json` in
`rust/crates/musaic-desktop/`. `.github/workflows/release.yml` does the same per platform
(AppImage + deb on Linux, NSIS on Windows), after running the Rust tests (`rust-tests.yml`) on each
platform it builds. It also builds an unsigned Windows installer (NSIS, since MSI rejects
pre-release versions) from the release after v0.2.0-rc.1; the Windows tests are required in
CI and gate that build like the others.
Signing, notarisation and an updater are not configured.

The desktop's hidden-window boot test is `#[ignore]`d; run it after building the UI:

```bash
cargo test -p musaic-desktop --test boot -- --ignored --nocapture
```

## Tests and test data

- Unit tests live in `src/` modules; integration tests in `rust/crates/<crate>/tests/`.
- Tests find data through the dev-only crate `musaic-testdata`:
  `musaic_testdata::public("golden/…")` for public data in `rust/testdata/`,
  `musaic_testdata::synthetic_library()` for the generated audio, and
  `musaic_testdata::require_private!(…)` for private data.
- **Public tier** (`rust/testdata/`): synthetic inputs and the results recorded for them.
  Runs from any clone; this is what CI runs.
- **Private tier**: recorded third-party API answers and data derived from a real music
  library are not in this repository. Tests that need them look in
  `$MUSAIC_PRIVATE_TESTDATA` (or `../private-testdata` next to `rust/` when the variable
  is unset) and otherwise print `skipped: private test data not available (<test>)` and
  pass. Setting the variable to an empty value turns the fallback off.
- **The synthetic audio library** is generated, never committed. From `rust/`, with
  Python 3.10+ and `numpy`, `soundfile`, `mutagen`, `Pillow`:

  ```bash
  pip install numpy==2.5.4 soundfile==0.14.0 mutagen==1.48.1 Pillow==13.0.0   # in a venv
  python3 testdata/synthetic-library/gen_fixture_library.py
  ```

  It writes `rust/testdata/synthetic-library/library/` and rewrites
  `library.manifest.json` (every file's SHA-256 plus the tool versions used). With the
  tool versions the manifest records the output is byte-identical, so a regenerated
  manifest differs only in its timestamp (restore it with `git checkout`); CI pins those
  versions and warns when the library digest changes. Do not commit a manifest
  regenerated with other versions.
- Goldens under `rust/testdata/golden/` were recorded from the Python app. A deliberate
  behaviour change edits the golden in the same change and says why.
- Fixture databases are copied to a temp dir before they are opened; never open one in
  place.

## Architectural rules

These retire whole classes of bugs. Each names where it lives.

### 1. Publish events; never invoke the fan-out

A mutation publishes a typed event at the moment it happens; subscribers do the
cross-cutting work (post-scan check reruns, resolver memo invalidation, thumbnail drop,
the SSE relay). Callers never call those directly.

- Event types: `rust/crates/musaic-events/src/event.rs` (`AlbumTagsChanged`,
  `AlbumCoverChanged`, `AlbumRenamed`, `AlbumMbAnchorSet`, `TrackFingerprinted`,
  `AlbumDeleted`, `AlbumMarkedMissing`, `LibraryScanCompleted`, `BulkBatchCompleted`,
  `LibraryScanStopped`, `SettingsChanged`, `ChecksRerunFinished`, `AgentStateChanged`). Only a scan that saved
  its snapshot publishes `LibraryScanCompleted` (post-scan reruns, fingerprint pool
  kick-off); an aborted, cancelled or failed scan publishes `LibraryScanStopped(reason)`
  and starts nothing.
- `ChecksRerunFinished(checks, failed, album_ids)` reports that a batch of check runs has
  finished and its findings are written; references only (check ids, the ones whose run
  failed, the albums the batch was limited to, empty for the whole library or a long
  list). Published once per batch by `musaic-checks/src/rerun.rs` (`RerunReport`): the
  post-scan and rename reruns (`post_scan.rs`, so also a rerun replayed after a restore),
  one per background sweep pass (`check_sweep.rs`, also when the pass is cut short), and a
  user-started run (`check_ops::run_check_logged`). A bulk loop still yields one rerun and
  one event. A failed run is listed in `failed`; a batch where no run started publishes
  nothing. It is a report, never a trigger: no rerun subscriber is registered for it, and
  the catalog skips the check sweep wake and the findings notify for it, so it cannot
  loop (`musaic-events/tests/rerun_finished.rs`).
- Bus (priorities, background subscribers, isolation): `musaic-events/src/bus.rs`.
- The subscriber list: `musaic-events/src/catalog.rs` (`CATALOG`, `register_all`); the
  real side effects: `musaic-checks/src/post_scan.rs` and
  `musaic-services/src/gated_reruns.rs`; wired in `musaic-app/src/app.rs`.
- The background check sweep (`musaic-services/src/check_sweep.rs`) is one more
  subscriber (`_check_sweep_wake`): every event but `ChecksRerunFinished` wakes it, and it reruns each check where
  the inputs digest recorded by its last completed run (`check_inputs`,
  `musaic-checks/src/inputs.rs`) no longer matches. It yields to scans, restores and
  any work holding the work gate, uses the background network path (and skips network
  checks offline), plans nothing while the library folder is absent, and writes only
  what a check run writes. A settings write publishes `SettingsChanged` (names only); the
  operation `settings.patch` (`musaic-app/src/ops/settings.rs`) does it, not the route.
- Recursion guard and bulk suppression: `musaic-events/src/context.rs`
  (`in_post_scan_rerun`, `in_bulk_run`). A bulk loop runs under `in_bulk_run` and
  publishes `BulkBatchCompleted` once at the end, for one library-wide rerun instead of N.
- Producers, for example: `musaic-services/src/apply.rs` (end of the apply pipeline),
  `scan.rs`, `fingerprint.rs`, `analyze.rs`, `library_ops.rs`, `check_fixes.rs`.

A merge of several album rows into one (rule 6) has no event of its own: it is
`AlbumRenamed(old, survivor)` per merged row, under `in_bulk_run`.

Delegated work, review decisions and durable agent operations commit an audit entry and a reference-only outbox event in the same transaction. The app publishes pending events after commit. The Review UI refreshes on the `work` event family.

`AgentStateChanged(what, ids)` reports that a pairing, an approval card, an agent's
activity or its change records (an undo) changed (references only), so the UI refreshes
Settings, Agents and the Review inbox. The desktop's Undo of an agent change
(`musaic-app/src/agent/undo.rs`) renames back through the rename service and restores
through the apply pipeline (`apply::apply_restore`), so it publishes what any rename and
apply publish. Like `ChecksRerunFinished` it is a report: no rerun subscriber, no sweep wake, no
findings notify; only the relay carries it.

New mutation → publish the right event. A direct call to the post-scan rerun outside the
subscriber wiring is a smell.

### 2. Findings store references; dialogs re-derive

A finding stores stable references (album id, sometimes file names). A Resolve dialog asks
a preview endpoint that re-derives the plan against current disk and database state, so it
never shows ghost rows.

- `GET /api/checks/filename-mismatch/findings/{id}/plan-preview` →
  `musaic-fsops/src/plan.rs` (`derive_plan`).
- `GET /api/checks/duplicate-tracks/findings/{id}/cluster-preview` →
  `musaic-checks/src/checks/duplicate_tracks.rs` (`derive_cluster`, `cluster_preview`).
- `GET /api/checks/multi-disc-layout/findings/{id}/conversion-preview` →
  `musaic-fsops/src/multi_disc.rs` (`derive_conversion`; the plan itself is the pure
  `plan_conversion`). Read-only: no conversion is built yet.
- Operations: `musaic-app/src/ops/checks.rs`; the HTTP routes over them: `musaic-api/src/routes/checks.rs`.

Use stored `detail` for labels only, never as the source of what to apply.

### 3. Path identity by same-file, never by string

"Are these the same file?" is answered by the filesystem (`same_file::is_same_file`, i.e.
same device + inode / file index), never by comparing path strings — case-insensitive
volumes and NFC/NFD spellings make equal files look different.
See `musaic-fsops/src/rename.rs` (`samefile`) and `musaic-tags/src/sidecar.rs`.

Album ids, by contrast, hash the resolved path string (symlinks followed, no case or
Unicode normalisation) (`musaic-core/src/album_id.rs`, `musaic-scan/src/listing.rs` `py_resolve`):
changing that orphans every user's database. On Windows the resolved path is the OS's
final path, as Python's `Path.resolve()` gives there (8.3 short names expanded, on-disk
case, no `\\?\` prefix: `musaic-core/src/config.rs` `windows_realpath`), so one folder
has one id however its path was typed.

An id, a stored path and a file row are derived from the spelling the filesystem
lists, which can differ from the one asked for (HFS+ stores names decomposed, NFD): code
that creates or moves a row reads the name back from the parent directory after the
rename and never records the requested one (`musaic-scan/src/listing.rs`
`listed_spelling`, used by `musaic-fsops/src/rename.rs` and `musaic-tags/src/apply.rs`).

### 4. One accessor per album-field cascade

The "album artist" and sort-key fallbacks are defined once:
`musaic-core/src/album_fields.rs` (`display_artist(…, include_dir_artist)`, `sort_key`)
and `frontend/src/lib/albumFields.ts` (`displayArtist`). Path-rendering callers pass
`include_dir_artist = true`, so a library with only `ARTIST` tags never loses the artist
token from a proposed folder name. The MusicBrainz search keeps its own order (folder
name first) on purpose.

### 5. SSE-driven cache invalidation in the UI

The backend relays every bus event to `GET /api/library-events/stream`
(`musaic-events/src/relay.rs`: priority 400, after every other subscriber; bounded
drop-oldest queue per connection; route in `musaic-api/src/routes/system.rs`). One
App-level hook, `frontend/src/hooks/useLibraryEventsStream.ts`, maps events to TanStack
Query invalidations for albums, tracks and resolution, and for findings and checks on
`ChecksRerunFinished`: the rerun a mutation triggers runs in the background, so the
mutation's own event reaches the UI before the findings are written, and findings are
refreshed only on the event published after them. `useChecksStream.ts`
(`/api/checks/stream`) stays for what the bus does not carry: a running check's state
(every run start and end), ignore / unignore, and the sweep's progress; its invalidation
of findings at the end of a run overlaps the bus path, so the bus handler reuses a
refetch already in flight (`cancelRefetch: false`). Mutation
handlers in the UI do not call `invalidateQueries`. Caches kept fresh this way use
`staleTime: Infinity`; any other stale time in `frontend/src/hooks/` needs a comment
saying why.

### 6. Presence is data: scans observe, users decide

A scan never deletes an album row. An album whose folder is gone gets `missing_since`
set (`AlbumMarkedMissing`) and keeps its fingerprints, anchors and tags; a returning album
clears it (`AlbumTagsChanged` with field `missing_since`). A scan that would mark a large
share of the library Missing at once is aborted as untrustworthy (an unmounted drive).
A running scan can be cancelled (`POST /api/scan/cancel`; the app's shutdown does the
same); the snapshot save is the commit point, so an aborted or cancelled scan writes
nothing and nobody is marked Missing from a partial walk.

- `musaic-db/src/albums.rs`: `save_snapshot`, `mark_album_missing`, `delete_album`.
- Scan states (`idle`, `running`, `cancelling`, `done`, `aborted`, `cancelled`, `error`),
  the sanity check, cancellation and the scan stream's events (`start`, `album`,
  `progress`, `marked_missing`, `cancelling`, then exactly one of `done` / `aborted` /
  `cancelled` / `error`): `musaic-services/src/scan.rs` (module docs). Routes
  `GET /api/scan/status`, `GET /api/scan/stream`, `POST /api/scan/rescan`,
  `POST /api/scan/cancel`: `musaic-api/src/routes/library.rs`.
- Checks: `musaic-checks/src/runner.rs` drops Missing albums from scope and keeps their
  open findings; SQL-only checks filter `missing_since IS NULL` themselves.
- Two paths drop a row, and nothing else may: the user's "Remove from library"
  (`DELETE /api/albums/{id}` → `musaic-services/src/library_ops.rs`), and a
  user-confirmed merge of the discs of one set into one album
  (`musaic-db/src/merge.rs` `merge_album_rows`), which deletes the merged rows only
  after moving everything on them to the surviving row in the same transaction, and
  refuses (changing nothing) while any per-file row, active AI job or conflicting
  anchor is left on them. Each file's rows move first, one file per transaction
  (`merge::move_track_rows`). The merged rows' findings stay under their old ids as
  resolved history. After the commit the caller publishes `AlbumRenamed(merged id →
  surviving id)` for each merged row, under `in_bulk_run` (its subscribers handle
  several old ids naming one new id: `musaic-events/tests/merged_renames.rs`).
  A merged row may be Missing only when the caller vouches that this merge emptied
  its folder (`MergeRequest::emptied`); the survivor never.
- A scan adopts instead of marking Missing when a vanished Present row's stored path
  still opens and is the same file (rule 3) as exactly one folder found under another
  spelling that has no row and no rows under its id: the row and everything on it move
  to the new id inside the snapshot save (`musaic-services/src/scan_adopt.rs` plans it,
  `musaic-db` `albums::save_snapshot_adopting` / `renames::move_album_rows` do it), and
  `AlbumRenamed` is published after the commit under `in_bulk_run`. Any ambiguity refuses.
  Adopted rows count neither as vanished nor as new for the sanity check.
- A scan leaves the albums and folders of an unfinished multi-disc conversion exactly
  as they are (no refresh, no Missing, no new row for its folders): its record under
  `<config dir>/conversions/` names them (`multi_disc_convert::held_albums`, handed to
  `ScanService::set_held`), and only the conversion's resume, which reads the record,
  puts a half-gathered set together. Finishing or abandoning the conversion
  (`multi_disc_gather::abandon`) ends the hold.

New operation that touches an album's files → skip Missing albums.

### 7. Operations surface: every face goes through `ops`

What the app does is an operation, a blocking function on `musaic_app::ops`
(`app.ops().<group>().<operation>(…)`); `docs/operations.md` is the inventory, one row per
route. The operation takes the work guard, validates in the documented order, publishes the
events and wakes the job queue; it returns typed results and `OpError`, and knows no status
code or wire format. A face (the HTTP routes today, an MCP server or a native UI later)
only parses, calls one operation and renders: a handler under `musaic-api/src/routes/`
takes no guard, publishes nothing, sends no notification and calls nothing but `ops()`.
Streams are an operation's sink or `Feed`; the face adds only framing and pings. A new
mutation is a new operation (rule 1 still applies inside it), with an inventory row. Three
tests hold the boundary: `musaic-app/tests/ops_boundary.rs` (no HTTP dependency or wire
word under `src/ops/`), `musaic-api/tests/thin_handlers.rs` (every file under `routes/`
but `mod.rs`) and `musaic-api/tests/operations_inventory.rs` (every route has a row naming
one operation); `musaic-app/tests/ops_all_groups.rs` requires an ops test per group and
`musaic-api/tests/stream_equivalence.rs` that the streams are the rendered events.

The agent bridge follows the same rule: `/__musaic/agent/v1/*` and `/api/agent/*` are
faces over `musaic_app::ops::agent` (the tool route parses with
`musaic_commands::parse_call` and renders the typed `ToolOutput`; `ops/agent.rs` stays free
of `serde_json`). The CLI is a client: it never links the database, so the app stays the
database's only owner.

## Security posture (keep it)

- The API binds `127.0.0.1` only. `musaic-api/src/host_check.rs` refuses requests whose
  `Host` or `Origin` is not loopback (DNS rebinding, cross-site POSTs).
- The desktop app adds a per-launch cookie (`musaic-api/src/launch.rs`) and a strict CSP
  header (`musaic-desktop/src/gate.rs`); the webview has no IPC capabilities and a
  navigation lock (`musaic-desktop/src/nav.rs`).
- Every remote image fetch goes through `musaic-net/src/preview.rs`
  (`check_public_url`): `https`, public host, re-checked on each redirect.
- Keys live only in the OS keyring (`musaic-secrets`); nothing writes a key to SQLite or
  to the log. Agent pairing secrets live there too (service `musaic-agent`); the database
  holds only a SHA-256 fingerprint of each.
- The agent route family `/__musaic/agent/v1/*` has its own guard
  (`musaic-api/src/agent_guard.rs`: loopback peer, exact Host, no Origin, JSON, 256 KiB,
  bearer token; mutual HMAC proof at the handshake) and never accepts the launch cookie.
  The launch gate and the Host / Origin check are unchanged for every other route. The
  tools return no filesystem path. See `docs/agents.md`.

See `SECURITY.md`.

## Adding a health check

1. Add `rust/crates/musaic-checks/src/checks/<name>.rs` implementing `Check`
   (`musaic-checks/src/base.rs`: `meta()` and `run(ctx, scope, emit)`); export it from
   `checks/mod.rs`. A check that looks at albums one at a time calls `ctx.checkpoint()?`
   before each, so a cancelled run (an agent's) stops between albums.
2. Add it to `Registry::default` in `musaic-checks/src/registry.rs` (order is display
   order).
3. A fix, if any, goes in `musaic-services/src/check_fixes.rs`, and mutates through the
   apply pipeline or publishes the matching event (rule 1). The face reaches it through
   `musaic_app::ops::fixes` (rule 7), never from a route.
4. Exclude Missing albums (rule 6); store references in findings (rule 2).
5. Tell the background sweep what the check depends on (`Check` in `base.rs`):
   `granularity()` (per album, or the whole library), `input_extras()` for settings or
   resolver configuration it reads (never a secret value), `refresh_after()` if its
   result also changes with time or with MusicBrainz, `uses_network()` if it resolves.
   A setting it reads must be listed in `checks_for_setting` in `check_sweep.rs`.
6. An album it cannot look at (folder unreadable, MusicBrainz request failed) is reported
   with `ctx.could_not_evaluate(album_id)`, never as "nothing wrong": the runner then keeps
   that album's findings and does not record its inputs.

## Conventions

- Rust edition 2024; `unsafe_code = "forbid"` workspace-wide.
- Clippy with `-D warnings` and `cargo fmt` must be clean; CI checks both.
- Keep comments about *why*; the code says what.
- Cover images are normalised to JPEG before they are written.
- MusicBrainz is paced (one request per second) by `musaic-net`; do not bypass the client.
