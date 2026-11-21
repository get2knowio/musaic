# Feature Specification: Agent Bridge — Shared Tools, stdio CLI and Secure Desktop Bridge

**Feature Branch**: `003-agent-bridge`

**Created**: 2026-11-20

**Status**: Draft

**Input**: User description: Notion "Prompt 1 · Shared Tools, stdio CLI & secure desktop bridge" (Musaic › 7 · MCP implementation specs). Implement the foundation for driving Musaic from an external agent over MCP, extending the design issue #174: one registered operation contract behind the desktop, a bundled CLI and an MCP server; pairing and scoped access for local agents through the running app; an approval contract for anything that changes the collection; setup and diagnostics for agent hosts.

## Background

Musaic owns the collection: its deterministic processing, its work, validation and
approval. The user's own agent (any MCP-capable assistant they already run) owns
reasoning, model access and model credentials. Musaic carries no model SDK and no model
credentials. Issue #174 sets this direction; this feature is its first deliverable. Two
later features build on it: durable delegated work with asynchronous MCP Tasks (it adds
its work tools to the registry delivered here), and MCP Skills, Events and agent-host
adapters.

### Current state, as checked on 2026-11-20 (main `00113c9b`)

- Every user-visible action is already one operation on the operations surface
  (`musaic_app::ops`: system, library, scan, settings, database, tags, analyze, apply,
  checks, fixes, jobs), and the HTTP routes are thin adapters over it (CLAUDE.md rule 7).
  That surface is the service boundary every new face here calls.
- The app runs its HTTP API in-process on an ephemeral `127.0.0.1` port. Existing routes
  are protected by a Host/Origin check and, in the desktop, a per-launch webview cookie.
  Neither is suitable as an agent credential and neither may be relaxed.
- The work gate and restore gate are in-process. Only the running app may open the
  database; a second process must talk to the running app, never to the database.
- Album analysis (BPM, ReplayGain) writes its results straight into the FLAC tags, so
  starting analysis is also a tag write.
- Musaic has one configured library folder. "Configured root" in this spec means that
  folder, named by an id, never by its path.

## Clarifications

### Session 2026-11-20 (decisions taken while writing the spec)

The run that wrote this spec was unattended; each point below is the most defensible
default, recorded so the plan can rely on it.

- Q: Where does the bridge's pairing secret live? → A: In the OS keyring, under an entry
  scoped to the config directory and the pairing, written by the desktop app when the user
  creates the pairing and read by the CLI (same OS user). The desktop never returns the
  secret over HTTP to the webview. The app's database keeps only a verifier and the
  pairing's metadata. This keeps the existing rule that secrets live only in the OS
  keyring; no credential file is written. Tests inject an in-memory secret store into the
  CLI's library entry point.
- Q: Where does an "Ask" approval appear? → A: As a card in the desktop's Review inbox (the
  existing single list of things that need the user), and in Settings → Agents under the
  pairing that asked. Approving or denying is possible only in the desktop.
- Q: What may a "rename" proposal rename to? → A: Only to what the configured folder and
  file name templates derive from the album's tags (as they would be after the proposal's
  tag changes). An agent cannot name an arbitrary target path.
- Q: What cover sources may a proposal name? → A: A cover candidate that Musaic's own
  resolution lists for the album, or a public HTTPS image URL, fetched under the existing
  public-HTTPS rule. Never a local file path.
- Q: Which permission kinds does starting analysis need? → A: Both `compute.analyze` and
  `tags.write`, because analysis writes tags. The stricter of the two decides.
- Q: How long do things last? → A: Access tokens 15 minutes; proposals and approval cards
  30 minutes; idempotency keys for the app's run (lost on restart, like access tokens);
  operation handles for the app's run (durable work is the next feature's job).
- Q: Which agent hosts does `setup` know in this feature? → A: Claude Desktop and Claude
  Code, whose MCP configuration files are documented JSON. For any other host, `setup`
  prints the exact entry to add by hand. Host adapters beyond these belong to the later
  host-adapter feature. No skill files are installed in this feature; the dry run lists
  "none" for skills.
- Q: What does a "real host connection" acceptance item require? → A: One recorded
  connection from a real installed host, with secrets stripped. It needs a live host and
  is reported as not verified if the delivering run cannot do it; it does not block the
  automated acceptance.

### Session 2026-11-20 (clarify, unattended: recommended options accepted)

- Q: How hard should failed token exchanges be throttled? → A: After 5 failures within
  60 seconds (across all clients, since an unauthenticated caller has no identity), refuse
  every exchange for 60 seconds, doubling on each further lockout up to 15 minutes; a
  success resets it. Recorded in FR-028.
- Q: What counts as a change of an album's "revision"? → A: Any change to the album's
  stored tags, cover, the set of its files, or any file's name, size or modification time.
  The digest is computed from those, so a proposal made before any of them changed is
  stale. Recorded in FR-051.
- Q: Should agent actions leave a trail the user can see? → A: Yes. Every agent call that
  starts work, applies a change, or is refused for authorization is recorded with the
  pairing name, tool, outcome and time (never a secret or argument value that could hold
  one) in the process log, and the last 100 per pairing are shown in Settings → Agents.
  Recorded in FR-052.
- Q: What request size does an agent route accept? → A: 256 KiB per request body; tool
  inputs that list ids accept at most 500 ids. Recorded in FR-026 and FR-006.
- Q: How many pairings may exist at once? → A: At most 20 unrevoked pairings per config
  dir; creating a 21st is refused with a message to revoke one. Recorded in FR-021.

### Session 2026-11-20 (plan-time corrections, after checking main `00113c9b`)

Five clauses above assumed things the code does not have. Each is amended in place and
marked "(amended at plan time)".

| Found | Where | Ruling |
|---|---|---|
| The apply pipeline takes no file backups. Its safety comes from temp-file renames and preserving rewrites. | `musaic-services/src/apply.rs`, `musaic-fsops/src/fsutil.rs:59` | FR-038 now says "existing apply safety" rather than "backups". Adding backups is not this feature's job. |
| Checks runs and analysis have no cancel. A tag write that has started cannot be stopped. | `musaic-app/src/ops/checks.rs:145`, `ops/analyze.rs:95-117` | FR-043: cancellation is cooperative at item boundaries (between albums and between checks). An apply can be cancelled only while it waits for approval. |
| Reading a resolution can fingerprint and call MusicBrainz, and holds the work guard while it does. | `musaic-app/src/ops/library.rs:232-235` | FR-053: `musaic_resolution_get` reads only what is stored. It does no network work and takes no work guard, and reports "not resolved yet" when nothing is stored. |
| There is no Windows ACL code anywhere, and `unsafe` is forbidden. | workspace | FR-018: on Windows the descriptor relies on the per-user profile directory's inherited ACL, with a follow-up issue filed (since resolved by issue #346: an explicit owner-only DACL, research R13). The descriptor holds no credential, and mutual proof (FR-020) protects against a spoofed endpoint. |
| Approval cards cannot outlive the operation handles and tokens that would act on them. | — | FR-034: cards are persisted for history and audit, and a restart expires every pending card ("app restarted"). Durable delegation that survives a restart belongs to the next feature. |

### Session 2026-11-20 (user decision: before-image of agent changes)

- Q: Approved agent edits cannot be undone, because the apply step keeps no copy of the
  old tags, names or cover. What should this feature do about it? → A: Record a
  before-image at apply time (FR-054): the old value of every tag field the change
  touches, per file; the old file and folder names; and the replaced cover, copied under
  the config dir. Each record is linked to the pairing, the proposal and the approval
  card. An Undo action that uses these records, and covers the user's own edits too, is a
  filed follow-up issue and not part of this feature.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - An agent reads the library through the running app (Priority: P1)

A user who already runs an MCP-capable assistant pairs it with Musaic once, in Settings →
Agents, choosing what it may do. They add Musaic to the assistant using the bundled CLI.
The assistant can then list albums, open an album with its tracks, read the deterministic
resolution evidence and list health findings, all through the running desktop app, with
no file paths and no access to the database file.

**Why this priority**: Read access is the smallest useful slice and proves the whole chain:
registry, CLI, MCP protocol, discovery, pairing, token exchange and the agent route family.

**Independent Test**: Start the app on a scratch config dir with the synthetic fixture
library, create a read-only pairing, run the CLI's MCP server against it, and confirm that
the tool list and the album list match what the desktop shows.

**Acceptance Scenarios**:

1. **Given** a running app and a paired CLI, **When** the agent calls `musaic_library_list`,
   **Then** it gets the same albums the desktop lists, in pages of at most the requested
   limit (1–100) with a cursor for the next page.
2. **Given** a paired CLI, **When** the agent calls `musaic_album_get` for an album id,
   **Then** it gets album and track metadata and a revision digest, and no local path.
3. **Given** the app is not running (or its descriptor is stale), **When** any CLI command
   runs, **Then** it exits nonzero with `APP_NOT_RUNNING` and tells the user to start
   Musaic; it never starts a second database owner.
4. **Given** an unpaired, revoked, expired, other-config-dir or wrong-protocol-profile
   client, **When** it calls any tool, **Then** it cannot read data or start work.

---

### User Story 2 - An agent proposes a fix and the user approves it (Priority: P1)

The agent proposes tag, cover or rename changes for an album. Musaic validates the proposal
against the album's current revision and returns an immutable proposal with a digest, a
diff and its effects, without touching the collection. When the agent asks to apply it,
Musaic follows the user's policy for that kind of change: Allow applies it, Ask puts an
approval card in the desktop, Refuse refuses. Immediately before applying, Musaic checks
the album, its files and the proposal again, and applies through the existing apply
pipeline, so backups, events and failure reporting behave as they do for a user's own edit.

**Why this priority**: Proposing and applying fixes is the reason to connect an agent; the
approval contract is what makes it safe.

**Independent Test**: With a pairing whose `tags.write` policy is Ask, preview a tag patch,
apply it, approve the card in the desktop and confirm the FLAC tags changed and the same
domain events were published as for a user edit; repeat with Deny and confirm no file
changed.

**Acceptance Scenarios**:

1. **Given** a valid patch on the current revision, **When** the agent calls
   `musaic_fix_preview`, **Then** it gets a proposal id, digest, diff, effects and expiry,
   and no file or tag in the library has changed.
2. **Given** a policy of Ask, **When** the agent calls `musaic_fix_apply`, **Then** a
   persisted approval card appears in the desktop, the agent gets an operation handle in
   "awaiting approval", and read-only tools keep answering meanwhile.
3. **Given** a pending card, **When** the user denies it or it expires, **Then** the
   operation ends denied/expired and no FLAC file is touched.
4. **Given** an approved card, **When** the album changed after the preview (new revision,
   files moved or renamed, Missing), **Then** the apply is refused as stale and nothing is
   written.
5. **Given** a proposal digest that does not match, **When** the agent applies, **Then** it
   is refused.
6. **Given** a restore in progress, **When** an approved apply would run, **Then** it is
   refused the same way a user's apply would be.

---

### User Story 3 - An agent starts scans, checks and analysis (Priority: P2)

The agent starts a library scan of the configured root, a checks run (optionally limited to
albums and checks) or analysis (BPM/ReplayGain) on albums, and follows each through an
operation handle it can poll or cancel. Each start follows the policy for its kind and
carries an idempotency key, so a retried call does not start the work twice.

**Why this priority**: Useful, but each piece already exists for the desktop; the new parts
are the handle, idempotency and policy.

**Independent Test**: With compute policies set to Allow, start a checks run, poll it to
completion, and confirm the findings match a desktop-started run; retry with the same key
and get the same handle.

**Acceptance Scenarios**:

1. **Given** the same idempotency key and the same input, **When** a start is called twice,
   **Then** both calls return the same operation handle.
2. **Given** the same key with different input, **When** a start is called, **Then** it is
   refused with an idempotency conflict.
3. **Given** a running operation, **When** the agent calls `musaic_operation_cancel`,
   **Then** it gets "cancellation requested" and the operation ends cancelled where the
   underlying work supports cancelling.
4. **Given** a scan already running (started by anyone), **When** the agent starts a scan,
   **Then** it attaches to that scan or is refused exactly as a second desktop caller
   would be.

---

### User Story 4 - The user manages agent pairings and policies (Priority: P2)

In Settings → Agents the user creates a pairing (a name and an access scope), sees each
pairing's last use, sets each permission kind to Allow, Ask or Refuse, and revokes a
pairing. A revoked pairing stops working at once, including any access token it holds.

**Why this priority**: Required for any agent to connect, but its UI can follow the backend
slices.

**Independent Test**: Create, restrict and revoke a pairing through the desktop and confirm
each change takes effect on the next agent call.

**Acceptance Scenarios**:

1. **Given** a new pairing, **When** it is created, **Then** the secret goes to the OS
   keyring only, the desktop shows how to connect a host, and the secret is never displayed
   in a log, response or URL.
2. **Given** a pairing whose scope lacks `tags.write`, **When** its policy for `tags.write`
   is set to Allow, **Then** tag writes are still refused (scope and policy intersect).
3. **Given** a revoked pairing, **When** its current access token is used, **Then** it is
   refused.
4. **Given** an app restart, **When** the agent reconnects, **Then** the pairing still works
   and the old access token does not.

---

### User Story 5 - The user connects a host with setup and checks it with doctor (Priority: P3)

The bundled CLI's `setup --dry-run` shows the hosts it detected and the exact changes it
would make. `setup` (explicit) applies them safely; `uninstall` removes only what Musaic
added; `doctor` reports what is wrong without printing secrets.

**Why this priority**: Convenience on top of a working bridge; users can also edit a host's
configuration by hand from the documentation.

**Independent Test**: In a scratch home directory with a host config containing unrelated
entries, run dry run, setup, uninstall and doctor, and compare the files byte for byte
against expectations.

**Acceptance Scenarios**:

1. **Given** a host config with unrelated entries, **When** setup applies, **Then** a backup
   is written, only the Musaic entry is added, and unrelated entries are unchanged.
2. **Given** a malformed host config, or an existing `musaic` entry Musaic did not write,
   **When** setup runs, **Then** it refuses and changes nothing.
3. **Given** a failure part way through, **When** setup stops, **Then** every file it
   touched is restored.
4. **Given** a working pairing, **When** doctor runs, **Then** it reports executable path
   and version, descriptor validity, pairing status, effective scopes, transport and
   negotiated profile, with no secret in its output.

---

### Edge Cases

- A descriptor that is a symlink, is owned by another user, is readable by others, names a
  non-loopback address, or names a port nobody (or the wrong instance) answers on is
  rejected; the CLI reports `APP_NOT_RUNNING` (or a descriptor error) rather than
  connecting.
- A process squatting on the descriptor's port after the app exited cannot learn the
  pairing secret: the bridge sends nothing secret until the instance has proven it is the
  one paired.
- Repeated failed token exchanges lock exchanges out (FR-028), and each failure is logged
  without the presented secret.
- A browser page (any request with an `Origin` header, or with a webview launch cookie and
  no agent token) is refused on every agent route.
- An agent sends unknown input fields, out-of-range limits, a path where an id is expected,
  or an oversized request: refused with a typed error, nothing started.
- A tool result that would be large (whole library) is paged; no tool returns the whole
  library in one result.
- The album is Missing: reads say so; proposals and applies are refused (rule 6).
- An approval is decided in the desktop after the agent has disconnected: the outcome is
  kept on the operation handle until the app restarts. A restart expires pending cards.
- Anything written to stdout by the CLI's MCP server other than protocol messages (a stray
  log line) is a defect; diagnostics go to stderr.
- Two config dirs (two app instances, e.g. a dev run and the real app): each has its own
  descriptor, pairings and keyring entries; a credential for one never works on the other.
- The host speaks only the older initialize-era protocol: the legacy profile serves it;
  envelopes of the two profiles are never mixed within one session.

## Requirements *(mandatory)*

### Functional Requirements

**Registry and faces**

- **FR-001**: The system MUST define every agent operation once, in a registry entry that
  states its stable id, description, input schema, output schema, effects, permission
  kind(s) and the operation it invokes on the existing operations surface.
- **FR-002**: The CLI subcommands, the MCP tool descriptors and the reference documentation
  MUST be generated from the registry, and a check command run in CI MUST fail when the
  checked-in generated catalog differs from what the registry generates.
- **FR-003**: Registry handlers MUST call the existing operations surface; no domain
  behaviour is reimplemented for the agent face, and existing services are not rewritten
  to introduce the registry.
- **FR-004**: The initial catalog MUST contain exactly these tools, each with the input and
  output listed in the Input above: `musaic_capabilities`, `musaic_library_list`,
  `musaic_album_get`, `musaic_resolution_get`, `musaic_findings_list`,
  `musaic_fix_preview`, `musaic_fix_apply`, `musaic_scan_start`, `musaic_checks_start`,
  `musaic_analysis_start`, `musaic_operation_get`, `musaic_operation_cancel`. CLI names are
  the hyphenated forms without the prefix where the mapping is mechanical.
- **FR-005**: The catalog MUST state what is deliberately not offered: raw SQL, shell
  execution, arbitrary filesystem paths, key read/write, trash/delete, database restore and
  agent-side permission or policy setting.
- **FR-006**: Every tool input MUST reject unknown fields and enforce its bounds (id lists
  hold at most 500 ids); every
  successful result's structured content MUST validate against the tool's output schema.
- **FR-007**: Tools MUST identify albums and roots by id; local filesystem paths MUST NOT
  appear in tool inputs or in ordinary tool results.
- **FR-008**: List-shaped results MUST be paged with an opaque cursor; the page limit is
  1–100.
- **FR-009**: Tool annotations (read-only, destructive, idempotent) MUST match the tool's
  actual effects; authorization MUST be enforced by the app regardless of annotations.
- **FR-010**: Domain refusals MUST be returned as typed tool errors with a stable code;
  malformed protocol messages, unknown methods and invalid params MUST be protocol errors
  of the active profile.

**CLI and MCP server**

- **FR-011**: A CLI MUST be bundled with desktop releases on macOS, Windows and Linux at a
  stable, documented location per platform.
- **FR-012**: The CLI's `mcp` command MUST serve MCP over stdio, writing only protocol
  messages to stdout and all diagnostics to stderr.
- **FR-013**: Other CLI subcommands MUST print structured (JSON) output and exit nonzero on
  failure, with the error code in the output.
- **FR-014**: The CLI MUST only talk to a running app; when none is running for the chosen
  config dir it MUST fail with `APP_NOT_RUNNING` and a remedy, and MUST NOT start the app
  or open the database itself.
- **FR-015**: The MCP server MUST support a modern profile pinned to MCP revision
  2026-07-28 and a separately versioned legacy (initialize-era) stdio profile, chosen per
  session by negotiation; the two profiles' envelopes MUST never mix in one session.
- **FR-016**: Any MCP SDK the implementation depends on MUST be pinned to an exact version
  shown by tests to handle both profiles; behaviour the SDK lacks MUST sit behind adapter
  modules with their own tests.

**Discovery and instance identity**

- **FR-017**: The running app MUST write a discovery descriptor per config dir holding a
  format version, a fresh instance id, the app version, supported protocol profiles, the
  numeric loopback port and the PID, and no credential.
- **FR-018**: The descriptor MUST be written atomically with owner-only permissions and
  removed at shutdown. On Windows owner-only means an explicit DACL that does not inherit
  from the folder and grants access to the current user alone (amended at plan time, and
  again for issue #346, which replaced the reliance on the profile directory's inherited
  ACL).
- **FR-019**: The CLI MUST reject a descriptor that is a symlink, owned by another user,
  accessible to others (on Windows: a DACL that inherits, or that grants anyone but the
  current user or SYSTEM, or one the CLI cannot read), of an unknown format version, or naming a non-loopback endpoint.
- **FR-020**: The CLI MUST treat the PID as a hint only and MUST confirm the instance's
  identity before it sends anything derived from the pairing secret; a stale descriptor
  (no answer, or a different instance) yields `APP_NOT_RUNNING`.

**Pairing and access**

- **FR-021**: The user MUST be able to create, list, inspect and revoke pairings in
  Settings → Agents, choosing an access scope (a set of permission kinds) at creation. At
  most 20 unrevoked pairings exist per config dir; a 21st is refused with a message to
  revoke one.
- **FR-022**: Creating a pairing MUST generate a random long-lived secret, store it only in
  the OS keyring entry for that config dir and pairing, and keep only a verifier and the
  pairing's metadata (name, scope, created, last used, revoked) in the app's database.
- **FR-023**: The pairing secret MUST never appear in command-line arguments, URLs, logs,
  HTTP responses or the webview.
- **FR-024**: The CLI MUST exchange its pairing credential, over the agent route family, for
  a short-lived access token bound to the pairing, the app instance and the pairing's
  scope; the default lifetime is 15 minutes and the CLI renews it by exchanging again.
- **FR-025**: Access tokens MUST stop working at app restart, at expiry and at revocation;
  pairings MUST survive restart until revoked.
- **FR-026**: Agent routes MUST live under a dedicated `/__musaic/agent` route family with
  their own authentication. They MUST require a loopback peer, the exact expected Host, a
  JSON content type on requests with bodies, no `Origin` header, and request bodies of at
  most 256 KiB; they MUST NOT accept the webview launch cookie as authority.
- **FR-027**: The existing launch-cookie gate and Host/Origin check MUST keep applying
  unchanged to every existing route.
- **FR-028**: Failed token exchanges MUST be rate limited: after 5 failures within 60
  seconds every exchange is refused for 60 seconds, doubling per further lockout up to 15
  minutes, reset by a success. Each failure is logged without the presented credential;
  authorization headers MUST be redacted from every log.
- **FR-029**: Pairings, descriptors, keyring entries and tokens MUST be isolated per config
  dir; a credential for one config dir MUST NOT work against another.
- **FR-030**: The documentation MUST state that pairing authenticates whoever holds the
  local credential (the OS user's processes), not a particular agent executable or model.

**Approval contract**

- **FR-031**: The permission kinds MUST be `library.read`, `proposal.submit`,
  `compute.scan`, `compute.analyze`, `tags.write`, `covers.write` and `files.rename`, each
  with a per-pairing policy of Allow, Ask or Refuse.
- **FR-032**: Defaults MUST be: `library.read` and `proposal.submit` Allow (once the user has
  created the pairing); `compute.*` Ask; `tags.write`, `covers.write` and `files.rename`
  Ask.
- **FR-033**: The effective decision MUST be the intersection of the pairing's scope and the
  policy (a kind outside the scope is Refuse); when an action needs several kinds, the
  strictest decision wins; no agent input can widen either.
- **FR-034**: An Ask MUST create a persisted approval card bound to the pairing, the exact
  proposal digest (or start request), the album revision, the action kind(s) and an expiry;
  cards are kept for history, and an app restart expires every pending card (amended at
  plan time).
- **FR-035**: Only the desktop user can approve or deny; no agent tool can approve, deny or
  change policy.
- **FR-036**: Immediately before applying, the system MUST revalidate the album's identity
  and presence, its file identities (same-file), its revision, the pairing's authority, the
  restore/work gate and the proposal's and card's expiry, and refuse as stale on any
  mismatch.
- **FR-037**: A denied, expired, refused or cancelled action MUST leave every FLAC file
  untouched.
- **FR-038**: Approved changes MUST go through the existing apply pipeline (and rename
  services), so its existing safety (temp-file renames, preserving rewrites; amended at
  plan time), published events, Missing-album rule and per-file outcomes
  apply unchanged; the agent receives per-file outcomes, including partial failures.
- **FR-039**: A pending approval MUST NOT block read-only tools for any pairing.

**Proposals and operations**

- **FR-040**: `musaic_fix_preview` MUST validate the patch against the album's current
  revision, store an immutable proposal (id, digest, diff, effects, expiry) and never write
  to the collection.
- **FR-041**: A proposal MAY change album- and track-level tags, set a cover from a
  resolution candidate or a public HTTPS URL, and request a rename to the template-derived
  names; each part maps to its permission kind.
- **FR-042**: Start tools and `musaic_fix_apply` MUST take an idempotency key; the same key
  and input from the same pairing returns the same operation handle, the same key with
  different input is refused.
- **FR-043**: `musaic_operation_get` MUST report state (awaiting approval, running,
  succeeded, failed, cancelled, denied, expired), counts and the result or error;
  `musaic_operation_cancel` MUST request cooperative cancellation, honoured at item
  boundaries: between checks and, inside a check run, before the check's next album;
  between albums and, inside an album's analysis, before its next file (never while a
  file is written); for a scan through the scan's own cancel; and for an apply only while
  it awaits approval (amended at plan time; finer-grained within a check run and an
  analysis since #349). A check run stopped part-way ends failed, keeps the findings it
  wrote, resolves none and records no inputs, so the background sweep runs it again; an
  analysis stopped part-way writes no album gain and still publishes `AlbumTagsChanged`
  for the files it wrote. A pairing can see and cancel only its own operations.
- **FR-044**: Every mutation an agent causes MUST publish the same domain events and follow
  the same work and restore gates as the equivalent desktop action (rules 1 and 7).
- **FR-045**: Every new agent operation MUST be an operation on the operations surface with
  an ops test, and every new HTTP route MUST have a row in `docs/operations.md`.

**Setup and doctor**

- **FR-046**: `setup --dry-run` MUST list detected supported hosts, the exact file changes
  and the skill files it would install (none in this feature), and change nothing.
- **FR-047**: `setup` MUST be an explicit action that backs up each file, merges only the
  Musaic-owned entry atomically, preserves unrelated content, refuses malformed files or a
  foreign entry of the same name, and restores every touched file if any step fails.
- **FR-048**: `uninstall` MUST remove only entries Musaic wrote.
- **FR-049**: `doctor` MUST report the executable path and version, descriptor validity,
  pairing status, effective scopes, transport and negotiated profile/extensions, MUST NOT
  print secrets and MUST NOT start an agent run.
- **FR-050**: The documentation MUST cover host setup, token lifetimes, rotation and
  revocation, the credential-holder limitation and PATH differences for GUI-launched hosts.

**Revision and audit**

- **FR-051**: An album's revision digest MUST change whenever its stored tags, its cover,
  its set of files, or any file's name, size or modification time changes; a proposal or
  approval bound to an older digest is stale.
- **FR-052**: Every agent call that starts work, applies a change, or is refused for
  authorization MUST be recorded with pairing name, tool, outcome and time, without
  secrets or argument values, in the process log; Settings → Agents MUST show the last 100
  such records per pairing.
- **FR-053**: Read tools MUST NOT start network work, take the work guard or write to the
  database; `musaic_resolution_get` returns the stored resolution or "not resolved yet"
  (added at plan time).
- **FR-054**: Before an agent-caused apply writes anything, the system MUST record a
  before-image linked to the pairing, proposal and approval (if any): for each file it
  will change, the prior value (or absence) of every tag field the change touches; the
  prior file and folder names when it renames; and, when it replaces a cover, a copy of
  the prior cover under the config dir. The record is written before the first file
  changes, survives restart, and is kept for at least 90 days. If the before-image cannot
  be recorded, the apply is refused and nothing is written.

### Key Entities

- **Operation definition**: a registry entry (stable id, description, schemas, effects,
  permission kinds, target operation). The generated catalog is its checked-in rendering.
- **Instance descriptor**: per config dir, written by the running app; how the CLI finds it.
- **Pairing**: a named, scoped, revocable grant to a local agent; its verifier and metadata
  live in the database, its secret in the OS keyring.
- **Access token**: short-lived, bound to pairing + instance + scope; memory only.
- **Policy**: per pairing and permission kind, Allow / Ask / Refuse.
- **Proposal**: immutable, digest-addressed set of changes to one album at one revision,
  with an expiry.
- **Approval card**: a pending Ask, bound to pairing, digest, revision, kinds and expiry;
  persisted for history; expired by a restart; decided only in the desktop.
- **Operation handle**: an agent-visible id for a started or awaiting action, with state,
  counts and result; kept for the app's run.
- **Change record (before-image)**: per agent-caused apply: pairing, proposal, approval,
  album, time; prior tag values per file for the touched fields, prior names, saved prior
  cover; the basis for a later Undo.
- **Agent activity record**: pairing, tool, outcome, time; no secrets or argument values;
  the last 100 per pairing are shown to the user.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: A freshly paired CLI lists exactly the same albums as the desktop for the
  synthetic fixture library (100% match by id).
- **SC-002**: 0 of the unauthorised client cases (unpaired, revoked, expired, other config
  dir, wrong profile, browser origin, hostile Host) can read any album or start any work.
- **SC-003**: 0 FLAC files change across every denied, expired, refused, stale or cancelled
  apply in the test suite (verified by file digests).
- **SC-004**: An agent-caused tag edit publishes the same set of domain events as the same
  edit made in the desktop.
- **SC-005**: The generated-catalog check fails on any registry change not reflected in the
  checked-in catalog, and passes otherwise.
- **SC-006**: 0 bytes that are not protocol messages appear on the MCP server's stdout across
  the test suite, and no test output, log or CLI output contains a pairing secret or access
  token.
- **SC-007**: Setup on a host config with unrelated entries leaves those entries
  byte-identical, and an interrupted setup leaves every touched file as it was.
- **SC-008**: While an approval is pending, read-only tools keep answering, each within
  1 second on the synthetic fixture library.
- **SC-009**: For every agent-caused apply in the test suite, the recorded before-image
  restores the exact prior tag values, names and cover bytes when replayed by a test
  helper (100% of touched fields).

## Assumptions

- Out of scope here and left to the later features: durable work that survives restart, MCP
  Tasks, Skills, Events, host adapters beyond Claude Desktop and Claude Code, a headless app
  owner (which needs its own single-instance design), MCP Apps.
- The in-app AI job queue is left as it is; the agent face does not use or replace it in
  this feature.
- Tool names and schemas are Musaic application contracts, not standardized MCP methods.
- Published MCP documentation is authoritative where it conflicts with repository text of
  the MCP project. The plan confirms the exact shape of revision 2026-07-28 and picks the
  legacy revision from what a real installed host negotiates.
- On macOS the OS keyring may ask the user once to let the CLI read the entry the app wrote;
  this is documented, not worked around.
- Live checks (a real host connection, platform installers) are recorded when the run can do
  them and otherwise reported as not verified.
