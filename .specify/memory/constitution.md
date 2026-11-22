<!--
Sync Impact Report
- Version change: (unfilled template) → 1.0.0
- Principles defined (all new):
  I. The Library Is the User's
  II. Local-First, Secure by Default
  III. One Operations Surface, Events for Fan-Out
  IV. Derive From Current State
  V. Test-Backed, Deterministic Changes
  VI. Plain, Accessible Interface
- Added sections: Technical Constraints; Development Workflow; Governance
- Removed sections: none
- Templates: plan-template.md's "Constitution Check" reads this file at run time (no edit
  needed); spec-template.md and tasks-template.md impose nothing that conflicts.
- Deferred TODOs: none
-->

# Musaic Constitution

## Core Principles

### I. The Library Is the User's

Musaic maintains a personal music collection. Losing or silently changing a user's files is the
worst failure it can have.

- A scan observes and never deletes. An album whose folder is gone is marked Missing and
  keeps everything recorded about it. Only the user's "Remove from library" and a
  user-confirmed merge of a multi-disc set's rows may drop an album row.
- Files and tags change only on an explicit user action (or an approved fix) and only through
  the apply pipeline and the file-operation services. Reading, browsing, checking and listening
  never write to the library.
- Any operation that touches an album's files skips Missing albums.
- "Is this the same file?" is answered by the filesystem (device and inode, or file index),
  never by comparing path strings.
- Album ids hash the resolved path. Changing that derivation is forbidden, because it would
  orphan every user's database.
- A database migration first takes a snapshot of the file it changes.
- Development and test runs never point at a real `~/.musaic` or a real music library.
- No release ships for a platform whose tests have known failures.

### II. Local-First, Secure by Default

Musaic runs on the user's machine and owes nothing to a server.

- The API binds to `127.0.0.1` only. It refuses non-loopback Host and Origin headers.
- The desktop app adds a per-launch cookie, a strict CSP and a navigation lock. The webview has
  no IPC capabilities.
- Secrets live only in the OS keyring. Nothing writes a key to the database, a log or a
  response.
- Remote images are fetched only over public HTTPS, and every redirect is re-checked.
- Outbound requests go through the paced network clients (MusicBrainz at one request per
  second).
- Agents and other faces get catalog references and structured errors, never raw file paths,
  audio bytes or credentials, unless a spec explicitly decides otherwise.
- `SECURITY.md` describes this posture. A change that weakens it requires a constitution
  amendment.

### III. One Operations Surface, Events for Fan-Out

Every user-visible action is one operation on `musaic_app::ops`. That operation owns its guard,
validation order, events and follow-up notifications, and it returns typed results and typed
errors.

- Faces (the HTTP routes, the desktop shell, a CLI or MCP server) only parse input, call one
  operation and render the result. They take no guard, publish nothing and know no business
  rule. `docs/operations.md` has one row per route.
- A mutation publishes a typed event when it happens. Subscribers do the cross-cutting work:
  check reruns, cache drops and the SSE relay. Callers never invoke that fan-out directly. A bulk
  loop publishes once at its end.
- The UI's caches are invalidated by the event stream, not by mutation handlers.
- The boundary tests (`ops_boundary`, `thin_handlers`, `operations_inventory`, `ops_all_groups`,
  `stream_equivalence`) MUST pass. A new operation adds an inventory row and an ops test.

### IV. Derive From Current State

Stored data holds references; what is shown or applied is re-derived.

- A finding stores stable references (album id, file names). A resolve dialog asks a preview to
  re-derive the plan against the current disk and database, and applies only what was
  re-derived. Stored detail is used for labels only.
- Each album-field fallback cascade (display artist, sort key) is defined once, in Rust and in
  the UI, and every caller uses that accessor.
- A check that cannot look at an album reports "could not evaluate", never "nothing wrong".
- Unavailable data is shown as unavailable, never as empty.

### V. Test-Backed, Deterministic Changes

- Every behaviour change ships with tests that would fail without it.
- The public test tier runs offline from any clone. Private or third-party data stays out of the
  repository, and tests that need it skip cleanly.
- Goldens change only as a deliberate behaviour change that says why, in the same change.
- Tests are deterministic. Timing comes from fake clocks or from observed progress, never from
  fixed deadlines. Fixture databases are copied before they are opened.
- `cargo fmt`, `clippy -D warnings`, ESLint, both TypeScript checks, the test suites and the
  production build are clean before merge. `unsafe` is forbidden workspace-wide.

### VI. Plain, Accessible Interface

- The UI speaks in plain language about the user's music, not about internal mechanics.
- Every control is operable by keyboard alone, shows visible focus and announces its name, state
  and value to assistive technology.
- Layouts work from 720 × 480 upward. Overlays never cover a primary action.
- Each command is defined once in the command registry. Native menus, shortcuts and on-screen
  controls share its availability rules, and every native menu command has an on-screen
  equivalent on all platforms.
- Browsing never starts work: selection, navigation and opening a finding have no side effects.

## Technical Constraints

- The backend is a Rust workspace (edition 2024) with a pinned toolchain; `rust-version` and
  `rust-toolchain.toml` move together. The desktop shell is Tauri 2, serving the API in-process
  on an ephemeral loopback port.
- The UI is React with TypeScript, Vite, Tailwind and TanStack Query. It talks to the backend
  only through ordinary `fetch`, `EventSource` and `<img>` requests.
- Storage is the SQLite database, migrated in place with a snapshot first. Cover images are
  normalised to JPEG before they are written.
- Crate dependencies point one way: core and db, then domain crates, then services, then app
  (ops), then faces. The crate map in `CLAUDE.md` is the reference.
- Supported desktop platforms are macOS, Windows and Linux. A platform's tests gate its
  installer.

## Development Workflow

- Work is delivered as reviewable slices, each leaving the workspace green. A feature
  specification under `specs/` names its slices. The plan's "Constitution Check" gates each
  plan against this document.
- Changes land through pull requests, squash-merged after the required CI check
  (`ci passed (linux, macos, frontend)`) is green. The repository owner decides when to merge.
- Docs change with the code they describe: `CLAUDE.md` (crate map, rules),
  `docs/operations.md` and `docs/desktop.md`.
- Deferred work becomes a GitHub issue, never an inline TODO. Comments explain why; the code
  says what.
- Public text (code, docs, commits, issues, PRs) carries no internal identifiers, private
  planning links or private test data.
- Workspaces stay clean: worktrees, branches, temp libraries and build output are removed once
  their work is merged.

## Governance

- This constitution sets the non-negotiable rules. `CLAUDE.md` is the detailed runtime
  guidance that implements them (its architectural rules 1–7 and security posture). Where the
  two disagree, the conflict is resolved by amending one of them in the same change, never by
  ignoring either.
- Amendments are made by pull request that edits this file. The pull request states the
  version bump and why, and updates `CLAUDE.md` and the affected docs in the same change.
- Versioning is semantic:
  - MAJOR removes or redefines a principle.
  - MINOR adds a principle or section, or materially expands one.
  - PATCH clarifies wording.
- Compliance: every plan's Constitution Check lists each principle as pass, or as justified
  deviation with a reason. Reviews (human and automated) check changes against these
  principles. An unjustified violation blocks the merge.

**Version**: 1.0.0 | **Ratified**: 2026-11-20 | **Last Amended**: 2026-11-20
