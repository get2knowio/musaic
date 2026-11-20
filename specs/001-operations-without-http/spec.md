# Feature Specification: Operations Usable Without HTTP

**Feature Branch**: `001-operations-without-http`

**Created**: 2026-11-19

**Status**: Draft

**Input**: User description: "Finish making Musaic's operations usable without HTTP. One operations surface, below HTTP, that is the single way every face performs a Musaic operation." (full text in the request that created this spec; its last sentence, the out-of-scope list, arrived cut off after "replacing the", see Assumptions)

## Background

Musaic's roadmap has an MCP server next, and a non-web UI is a possible later step. Both
are new **faces**: ways of driving Musaic that are not the existing web UI over the
loopback HTTP API. Both need to call Musaic's operations in-process.

Today the only complete and correct way to perform an operation is to send an HTTP request,
because part of each operation's behaviour still lives in the HTTP route handlers. A new
face would have to either talk HTTP to itself or copy that behaviour and risk getting it
wrong.

### Current state, as checked on 2026-11-19

- An earlier pass already moved shared pieces below HTTP (the work/restore gate, the quit
  rule, thumbnails, the AI queue host); the HTTP layer re-exports them. The layers below
  HTTP already have no dependency on the HTTP libraries.
- Services still speak HTTP's language: a refusal is a status code plus a detail string,
  and results are untyped JSON values (about twenty service modules).
- Route handlers still own behaviour. Handlers in the library, apply, checks, fixes,
  settings and tags groups take the work guard themselves (about thirty acquisitions),
  after doing their own input validation; settings handlers publish the settings-changed
  event themselves; fixes and settings handlers send the "AI jobs changed" notification
  themselves.
- Streamed operations: the request says "seven streaming endpoints" and names eight. Both
  are right: seven routes are streams by path (scan, apply, analyze, checks, AI jobs,
  fingerprint pool, library events) and quick-fix-all streams its progress as the response
  to an action route. This spec treats all **eight** as streamed operations, and any
  further one the inventory finds (planning found a possible ninth, the multi-disc
  conversion) is held to the same requirements.
- The route table is the router's own and already has an inventory test; the request
  counts 72 routes. The inventory this feature delivers is the authority on the number.

## Clarifications

### Session 2026-11-19

All five answers are the recommended option, accepted in one go ("accept all
recommendations").

- Q: The out-of-scope list was cut off after "replacing the". Is replacing the desktop
  shell's in-process loopback HTTP server (so the web UI would call operations directly)
  out of scope? → A: Yes, out of scope. The web UI keeps talking to the loopback HTTP API;
  this work only makes a second way in possible.
- Q: How far must the HTTP-shaped types be removed: only from the operations surface, or
  from everything beneath the HTTP face? → A: Status-code errors are removed from
  everything beneath the HTTP face. Untyped JSON may remain as an internal detail of the
  services where the shape is owned by stored data, but never crosses the operations
  surface.
- Q: When one face has started a long-running operation, what does a second caller get?
  → A: The same running operation. A second caller attaches to it or is refused exactly as
  a second HTTP client is today; there is never a second scan, apply or bulk fix running
  beside the first because it was started through a different face.
- Q: Is the operations surface a stable, published interface or an internal one? → A:
  Internal to this project, with no stability promise. It may change when the MCP server
  is built; it is not versioned or documented for outside use.
- Q: While the work is part-way through, may route groups not yet converted keep calling
  the services directly? → A: Yes. Each slice converts one group completely; the automated
  checks cover the converted groups and grow with each slice, and cover every group at the
  end.

## User Scenarios & Testing *(mandatory)*

The "users" of this feature are the people and agents who build a face on Musaic (the MCP
server next, possibly a native UI later) and the maintainer who reviews changes. The
person using the app must notice nothing.

### User Story 1 - Perform any Musaic operation in-process (Priority: P1)

Someone building a new face holds a handle to the running application and nothing else. They
can read library, album, track, findings, jobs and settings state; run and cancel a scan;
apply corrections; run checks and fixes, singly and in bulk; analyze; manage tags, covers,
backups and restore. Each call takes typed inputs and gives back either a typed result or a
typed error that says what went wrong. They never build a request, read a status code or
pick apart a JSON body.

**Why this priority**: This is the point of the feature. Without it the MCP server cannot
be built except by calling the HTTP API from inside the same process.

**Independent Test**: A test suite with no HTTP library in scope drives at least one
representative operation from each route group (library, apply, checks, fixes, jobs, tags,
settings, analyze, system) against a scratch library and checks the typed results.

**Acceptance Scenarios**:

1. **Given** a running application with a scanned library, **When** a caller asks for the
   album list, one album, its tracks and its open findings through the operations surface,
   **Then** it receives typed values holding the same information the HTTP API reports for
   the same library.
2. **Given** an album id that does not exist, **When** a caller asks for that album,
   **Then** it receives a "not found" error naming what was not found, with no status code
   in it.
3. **Given** an input with an invalid field, **When** a caller invokes a mutating
   operation, **Then** it receives an "invalid input" error that names the field, and
   nothing has been changed.
4. **Given** no library folder is configured, **When** a caller invokes an operation that
   needs one, **Then** it receives a "no library configured" error.
5. **Given** any operation the HTTP API offers, **When** the inventory is consulted,
   **Then** it names the one operation that performs it.

---

### User Story 2 - A face cannot get an operation wrong (Priority: P1)

Someone building a face calls one operation and the operation does everything that belongs
to it: validates its input, takes the work guard, skips Missing albums, publishes the right
change event, and sends any follow-up notification (for example waking the AI job queue
after a fix enqueues a job). There is no sequence of calls the face must remember, so there
is no way to call them in the wrong order or forget one.

**Why this priority**: The architectural rules (publish events, presence is data, the
restore gate) protect the user's files and database. If each face had to re-implement them,
the second face would be where they break.

**Independent Test**: With a restore (or other exclusive work) in progress, call a mutating
operation through the operations surface and observe a typed "busy" refusal with nothing
changed; with the gate free, call it and observe the expected change event and follow-up
notification without the caller asking for either.

**Acceptance Scenarios**:

1. **Given** a restore is in progress, **When** a caller invokes a mutating operation,
   **Then** it is refused with a "busy / restore in progress" error and nothing is changed.
2. **Given** an input that is invalid **and** a restore in progress, **When** a caller
   invokes a mutating operation, **Then** the error is "invalid input", as it is over HTTP
   today (validation is reported before busy).
3. **Given** a settings change made through the operations surface, **When** it succeeds,
   **Then** the settings-changed event is published once, without the caller publishing it.
4. **Given** a fix that enqueues an AI job, **When** it is run through the operations
   surface, **Then** the job queue and every jobs subscriber are notified, without the
   caller notifying them.
5. **Given** an album marked Missing, **When** an operation that touches album files is
   invoked for it, **Then** the operation skips or refuses it exactly as the HTTP API does
   today.

---

### User Story 3 - Follow a long-running operation's progress (Priority: P2)

Someone building a face starts a long-running operation (scan, apply, analyze,
quick-fix-all) or subscribes to a feed (check runs, AI jobs, fingerprint pool, library
change events) and receives typed progress events in the documented order, ending in
exactly one terminal event where the operation has an end. They can cancel a scan. No event
arrives as pre-formatted text.

**Why this priority**: An MCP server or native UI is of little use for scans and applies if
it cannot report progress or learn that an operation finished, but read and one-shot
operations (stories 1 and 2) are valuable before this.

**Independent Test**: Through the operations surface only, start a scan of the synthetic
library, collect the events, and check the sequence: a start, then album / progress /
marked-missing events, optionally a cancelling event, then exactly one of done, aborted,
cancelled or error, and nothing after it.

**Acceptance Scenarios**:

1. **Given** a configured library, **When** a caller runs a scan and collects its events,
   **Then** the events arrive in the order the scan stream documents today and end with
   exactly one terminal event.
2. **Given** a running scan, **When** a caller cancels it, **Then** the caller sees a
   cancelling event followed by the single terminal "cancelled" event, and nothing is
   written to the library snapshot.
3. **Given** a subscriber to library change events, **When** an album is changed by any
   operation, **Then** the subscriber receives the typed change event.
4. **Given** a subscriber that stops listening part-way, **When** the operation continues,
   **Then** the operation still runs to its own end and its outcome is the same as if the
   subscriber had stayed.
5. **Given** each streamed operation, **When** its HTTP stream is compared
   with the typed events, **Then** the HTTP stream is a one-for-one rendering of those
   events in the same order.

---

### User Story 4 - The existing app behaves exactly as before (Priority: P1)

A person using the Musaic desktop app, and the React UI itself, see no difference. Every
request gets the same response, with the same status, the same body down to key order, the
same error wording and the same stream contents as before.

**Why this priority**: This is the hard constraint that makes the work safe to merge slice
by slice. A regression here ships to users; nothing else in this feature does.

**Independent Test**: Every existing HTTP-level test and every recorded golden passes
without modification after each slice, and the frontend's own checks pass with no frontend
file changed.

**Acceptance Scenarios**:

1. **Given** the existing HTTP test suites and goldens, **When** any slice of this work is
   merged, **Then** all of them pass unmodified.
2. **Given** a change that would require editing a golden or an existing HTTP test's
   expectation, **When** it is discovered, **Then** work on that item stops and it is
   flagged to the maintainer rather than the golden being changed.
3. **Given** the desktop app built from the finished work, **When** a person scans,
   browses, applies a correction and runs a fix, **Then** the UI behaves as it did before.

---

### User Story 5 - The boundary stays in place after the work is done (Priority: P3)

The maintainer can see, from an inventory, what every route was and what it became; can
rely on an automated check to fail if the operations surface ever starts depending on the
HTTP layer; can rely on another to fail if a route handler takes the work guard, publishes
an event or sends a notification of its own; and finds the new layer described in the
project's contributor documentation.

**Why this priority**: It keeps the result from eroding, and makes each slice reviewable,
but it delivers nothing to a face builder on its own.

**Independent Test**: Introduce, in a scratch change, a dependency from the operations
surface on the HTTP layer, and separately a work-guard acquisition in a route handler; each
makes an automated check fail.

**Acceptance Scenarios**:

1. **Given** the inventory, **When** a reviewer looks up any route, **Then** they find its
   classification (already a thin adapter, or carried behaviour that had to move), what
   moved, and the operation it now calls.
2. **Given** a change that makes the operations surface depend on the HTTP libraries or
   the HTTP face, **When** the checks run, **Then** they fail.
3. **Given** a change that adds a work-guard acquisition, an event publish or a
   post-action notification to a route handler, **When** the checks run, **Then** they
   fail.
4. **Given** the contributor guide and the desktop design note, **When** a new contributor
   reads them, **Then** they learn where the operations surface lives, why it lives there,
   and that every face goes through it.

---

### Edge Cases

- **Validation and busy together**: an invalid request made while a restore runs must
  report invalid input, not busy, as today. Operations that today check in another order
  (if the inventory finds any) keep their order; the inventory records it.
- **Operations that take the guard only sometimes** (for example a read that refreshes
  only when asked to): the operation, not the face, decides whether the guard is needed.
- **A guard that must outlive the call**: a streamed operation holds the work guard until
  its terminal event, not until the call that started it returns, including when the
  subscriber has gone away.
- **Two faces, one operation**: a scan, apply or bulk fix started through one face is the
  same running operation for every other face; a second start attaches or is refused as a
  second HTTP client is today (FR-030).
- **A slow or absent subscriber**: feeds that today drop the oldest events for a slow
  connection keep that behaviour per subscriber; a slow subscriber never stalls the
  operation or other subscribers.
- **Shutdown during an operation**: the quit rule and scan cancellation on shutdown behave
  as today; a streamed operation interrupted by shutdown still ends its event sequence the
  way it does today.
- **Python-compatible leniency**: inputs the HTTP API accepts leniently today (for example
  loosely typed fields) are still accepted over HTTP. The lenient parsing belongs to the
  HTTP face; the operation receives the already-typed value. Where a rule is about the
  *meaning* of a value rather than its wire form, the operation enforces it, so a face
  that does no parsing is still protected.
- **Error wording**: where today's HTTP error body carries specific wording that a typed
  error category alone cannot reproduce, the typed error carries enough information for
  the HTTP face to render the identical body.
- **Routes with no operation behind them** (static files, the launch gate, the host
  check): these are properties of the HTTP face and stay there; the inventory lists them
  as such.
- **Uploads and downloads** (database restore from an uploaded file, database backup
  download, cover images): the operation works in terms of files or bytes; receiving the
  upload and framing the download stay in the HTTP face.
- **A golden would have to change**: out of scope; stop and flag.

## Requirements *(mandatory)*

### Functional Requirements

**Inventory**

- **FR-001**: The work MUST produce an inventory of every route the HTTP API exposes, each
  classified as either already a thin adapter or carrying behaviour that must move, stating
  for the latter what behaviour moves (validation, guard acquisition, ordering, event
  publish, follow-up notification, stream assembly) and naming the operation the route
  calls once done.
- **FR-002**: The inventory MUST be kept with the project and be complete: every route in
  the application's route table appears in it exactly once.

**The operations surface**

- **FR-003**: There MUST be one operations surface, below the HTTP layer, through which a
  caller holding an application handle can do everything the HTTP API's routes do: read
  library, album, track, findings, jobs and settings state; run and cancel scans; apply
  corrections; run checks and fixes, single and bulk; analyze; manage tags, covers, backups
  and restore; and subscribe to progress and change events.
- **FR-004**: Every operation MUST take typed inputs and return a typed result or a typed
  error. No status codes, untyped JSON values, or request/response header or body types
  may appear in the operations surface.
- **FR-029**: By the end of the work, no error beneath the HTTP face may be expressed as a
  status code. Untyped JSON MAY remain inside the services as an internal detail where the
  shape is owned by stored data, but MUST NOT cross the operations surface.
- **FR-005**: Errors MUST say what went wrong, distinguishing at least: not found; busy
  (including restore in progress); invalid input, naming the field; no library configured;
  conflict; and internal failure.
- **FR-006**: A typed error MUST carry enough information for the HTTP face to reproduce
  today's response for it exactly, without the error itself naming a status code.
- **FR-007**: The operations surface MUST be the single way every face performs an
  operation; the HTTP face MUST NOT reach past it to the services beneath for anything an
  operation covers.

**Operations own their preconditions**

- **FR-008**: Each operation MUST perform its own validation of the meaning of its inputs,
  acquisition of the work guard where the operation needs it, exclusion of Missing albums
  where the operation touches album files, publication of the right change event, and any
  follow-up notification. A caller MUST NOT need to perform, or be able to omit, any of
  these.
- **FR-009**: The order in which an operation's preconditions are checked MUST be the
  order the HTTP API exhibits today for that route (in general: validate, then take the
  work guard, then act).
- **FR-010**: A refused or failed operation MUST leave the library, the database and the
  event feed as it found them, to the same degree as today.

**Long-running operations**

- **FR-011**: Every streamed operation (the eight named: scan, apply, analyze,
  quick-fix-all, check runs, AI jobs, fingerprint pool, library change events; plus any
  other the inventory identifies) MUST expose its progress
  as typed events, delivered to the caller as a stream or through a callback.
- **FR-012**: Each typed event sequence MUST carry the same guarantees its HTTP stream
  documents today, including order and, for operations that end, exactly one terminal
  event with nothing after it (for a scan: start; album, progress and marked-missing
  events; optionally cancelling; then exactly one of done, aborted, cancelled or error).
- **FR-013**: A long-running operation MUST run to its own end independently of whether
  its caller keeps listening, and MUST hold whatever guard it needs until that end.
- **FR-014**: Cancelling a scan MUST be available as an operation, with the same outcome
  as today.
- **FR-030**: A running long-running operation MUST be one operation for all faces: a
  second caller, through any face, attaches to it or is refused exactly as a second HTTP
  client is today, and events from an operation started by one face reach subscribers on
  every face.

**The HTTP face as an adapter**

- **FR-015**: Each route handler MUST do only this: parse the request, call one operation,
  render the response. Wire framing of event streams MUST stay in the HTTP face.
- **FR-016**: Everything that exists for compatibility with the earlier Python app's HTTP
  behaviour (lenient field parsing, response key order, routing quirks, error body shapes)
  MUST stay in the HTTP face.
- **FR-017**: No route handler may contain a work-guard acquisition, an event publish, or
  a post-action notification of its own.
- **FR-018**: The HTTP API's observable behaviour MUST NOT change: same routes, statuses,
  bodies, headers, stream contents and ordering for the same inputs and state.

**Placement and guard rails**

- **FR-019**: The work MUST decide where the operations surface lives (within the
  application layer, within the services layer, or as a new unit of its own), record the
  decision with its reason, and respect the existing direction of dependencies between the
  project's units.
- **FR-020**: An automated check MUST fail if the operations surface comes to depend on
  the HTTP libraries or on the HTTP face.
- **FR-021**: An automated check MUST fail if a route handler violates FR-017.
- **FR-022**: A new test suite MUST drive representative operations from every route group
  through the operations surface only, with no HTTP types in scope, including at least one
  streamed operation and at least one refused-while-busy case.

**Delivery**

- **FR-023**: The work MUST be delivered in reviewable slices, one route group at a time
  (library, apply, checks, fixes, jobs, tags, settings, analyze, system), each leaving
  every test, lint and format check passing. A slice converts its group completely; groups
  not yet converted MAY keep calling the services directly until their slice. The checks
  of FR-020 and FR-021 apply to the converted groups from the slice that converts them,
  and to every group at the end, when FR-007 and FR-017 hold without exception.
- **FR-024**: The contributor guide's map of the project's units and the desktop design
  note MUST describe the new layer when the work is complete.

**Standing constraints**

- **FR-025**: The six architectural rules (publish events; findings store references and
  dialogs re-derive; path identity by same-file; one accessor per album-field cascade;
  event-driven cache invalidation in the UI; presence is data) and the security posture
  MUST continue to hold.
- **FR-026**: The frontend MUST NOT be changed.
- **FR-027**: The project's existing code-quality gates (no unsafe code, lint clean with
  warnings as errors, formatter clean) MUST continue to pass.
- **FR-028**: No existing HTTP-level test and no recorded golden may be modified. If one
  would have to change, the affected item is out of scope: stop and flag it.

### Key Entities

- **Operation**: one thing Musaic can do or report, callable in-process, with typed
  inputs, a typed result, and its preconditions built in. Every route maps to one.
- **Operation error**: a typed description of why an operation did not succeed (not found,
  busy, invalid input with its field, no library configured, conflict, internal), with
  enough detail for a face to explain it in its own terms.
- **Progress event**: a typed event from a long-running operation or feed, belonging to a
  documented sequence; some sequences end with a single terminal event.
- **Face**: anything that lets a person or agent drive Musaic. Today: the HTTP API serving
  the web UI. Next: an MCP server. Each face translates between its own wire form and
  operations, and holds no operation behaviour.
- **Work guard**: the existing mechanism that refuses or serialises work while a restore
  or other exclusive work runs. Operations take it; faces never do.
- **Route inventory**: the record of every route, its classification, what moved, and the
  operation behind it.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: 100% of the routes in the application's route table appear in the inventory
  with a classification, and every route that represents an operation names the single
  operation it calls.
- **SC-002**: A caller with no HTTP library available can complete at least one
  representative operation from each of the nine route groups, including one streamed
  operation observed through to its terminal event and one operation refused while the
  application is busy.
- **SC-003**: Zero existing HTTP-level tests and zero goldens are modified across the
  whole of the work, and all of them pass after every slice.
- **SC-004**: Zero frontend files are changed, and the frontend's own lint, type and test
  checks pass at the end.
- **SC-005**: Zero route handlers contain a work-guard acquisition, an event publish, or a
  post-action notification, and an automated check enforces that count staying at zero.
- **SC-006**: Zero references to status codes, untyped JSON values or request/response
  types remain in the operations surface, and an automated check fails when a dependency
  on the HTTP libraries or the HTTP face is introduced.
- **SC-007**: For each streamed operation, the typed event sequence and the
  HTTP stream for the same run match one-for-one in content and order.
- **SC-008**: The work lands as at least nine separately reviewable slices (one per route
  group), and the full set of project checks passes after each one.
- **SC-009**: A person using the desktop app before and after the work can find no
  difference in behaviour while scanning, browsing, applying corrections, running checks
  and fixes, analyzing, and backing up and restoring.

## Assumptions

- **Out of scope**: building the MCP server; any native or other UI work; changing the
  HTTP API's shape; replacing the desktop shell's in-process loopback HTTP server (the web
  UI keeps talking to it). The last item completes a sentence the request left cut off
  after "replacing the"; see Clarifications. Also out of scope: new operations the HTTP
  API does not offer today, and fixing behaviour that looks wrong but is recorded in a
  golden.
- **"Typed" does not mean every field is modelled.** Where an operation's result is, by
  its nature, an open-ended bag of tag values or a document whose shape is owned by stored
  data, the typed result may hold that part as structured data. What may not appear is
  JSON as the general-purpose return type or anything specific to HTTP (FR-029).
- **The operations surface is internal.** It is for faces built in this project, carries
  no stability promise, and is not versioned or documented for outside use; the MCP
  server's needs may reshape it later.
- **At least eight streamed operations**, not seven; see Background. The inventory fixes
  the number.
- **The count of routes** is whatever the application's route table holds when the
  inventory is made (72 by the request's count), not a fixed number in this spec.
- **Where the surface lives is decided during planning** (FR-019), not here. The earlier
  pass that moved the gate, the quit rule, thumbnails and the AI queue host below HTTP is
  kept and built on.
- **Routes that are purely properties of the HTTP face** (static UI files, the launch
  cookie gate, host and origin checks) have no operation and stay where they are.
- **The existing tests are the definition of "no observable change".** Where they do not
  pin a behaviour, the current behaviour is still kept; new tests may be added, existing
  ones are not edited.
- **Private test data** may be unavailable; tests that need it skip as they do today, and
  the new operations-surface suite runs on the public and synthetic data alone.
- **No project constitution is ratified** (the constitution file is still the unfilled
  template), so the contributor guide's architectural rules and conventions are treated as
  the governing principles.
