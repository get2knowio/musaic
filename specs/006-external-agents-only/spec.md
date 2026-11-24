# External agents only: remove the built-in model provider

**Feature Branch**: `006-external-agents-only`
**Created**: 2026-11-23
**Status**: Draft
**Input**: Remove the built-in model provider; external agents only

## Overview

Musaic carries two ways of getting help with albums its own resolver cannot settle. One is a built-in model provider with its own job queue, worker, settings and keys. It cannot run in a shipped build (the app is only ever built without a configured provider), yet its switches still decide what the desktop offers. The other is the delegated path of specs 003 to 005: Musaic records a research request, an assistant the listener runs connects over MCP, picks the request up, and returns a proposal the listener reviews.

This feature removes the first and makes the second the only one. Musaic never launches an agent program and never calls a model itself. It exposes tools, a skill and events; an assistant the listener runs connects to it.

What a listener with a connected assistant gains:

- **Fix** on a research-capable finding offers the optional hint again, and says truthfully that what the resolver cannot settle goes to the assistant.
- **Fix all** (bulk quick-fix) turns every album the resolver cannot settle into a research request, gathered into one batch that the desktop shows as one thing and that an assistant can pick up several items at a time.
- **Request research** is offered on every album, not only when a model "is not ready".

What disappears: the model-provider settings and keys, the queue pause and resume, the "AI suggestions are not available in this build." notice, and every label that says "AI job".

What stays: the local resolver (AcoustID vote, MusicBrainz name search, tracklist alignment, Cover Art Archive), which settles the easy cases before anything is delegated; the review flow, where the listener alone decides what is applied; and the job list and its stream, which become the desktop's view of research requests.

The old job table is migrated into the delegated-work store and dropped, so one store holds every research request, past and present.

## Clarifications

### Session 2026-11-23

- Q: What is a batch to an agent: a field on each research request with a list filter and a multi-item claim, or a batch object with a lifecycle of its own? → A: A field. Each request carries an optional batch reference (opaque, absent for requests not made by a bulk run). Listing work accepts a batch filter. A new multi-item claim takes up to a stated number of eligible items (of a batch, or of the queue) in one call and returns the ones it won, each with its own lease; an item another principal took first is simply not returned, never a whole-call failure. Several held leases can be renewed in one call. There is no batch state, no batch claim and no batch result: a batch's status is what its items say. Each item's events stay one per item, as today, and carry the batch reference so an agent can coalesce them; no new event type is added. FR-017, FR-034, FR-035 and the Batch entity say so.
- Q: Is there an upper bound on a batch or on how many requests one bulk run may create, and on a multi-item claim? → A: No cap on a batch: a bulk run makes one request per unsettled album (one per kind), because a cap would silently leave albums unresearched. A multi-item claim takes at most 25 items per call; a work list page stays at most 100. FR-017 and SC-003 updated.
- Q: Does a bulk run send unsettled albums to the assistant without asking, even when no assistant has been added? → A: The Fix all dialog shows a choice, "Send what the resolver can't settle to my assistant", on by default when at least one assistant has been added and off otherwise. With it off the run is resolver-only, as today, and makes no batch. The choice is shown before the run starts, with the number of findings in scope. FR-012 and User Story 2 updated.
- Q: What does "Clear applied or discarded" clear once the old table is gone? → A: Every finished research request whose outcome is applied or discarded, native and migrated alike (including history-only rows of an unknown old type that ended applied or discarded), and the count on the button is exactly that number. Failed and cancelled requests are not cleared, because they keep their Retry. A batch whose items have all been cleared no longer appears. Clearing removes requests from history; it never touches the albums, their files or the backups made when a proposal was applied. FR-026 updated.
- Q: What happens to old job rows whose type this version does not know (and so cannot become a research request)? → A: They are kept, as history only: shown in the history view with their original title, hint, times, status and type name, never listed to an agent, never retryable and never reviewable, and cleared by "Clear applied or discarded" when their outcome was applied or discarded. They count in the database information and block nothing. FR-020, FR-021 and the Historical request entity updated.
- Q: (Owner addition 2026-11-23) What does Cancel on a research request's card do? → A: It cancels the request behind the card (FR-036), through a group-level cancel; it no longer calls the old delete route, which refuses research.
- Q: Does the Library page show the status of batched requests? → A: Yes. An album with a request in a batch gets the same tile/row status pill as any other album with a request, in every active state, and the Library "to review" filter finds a batched proposal awaiting review. The Activity panel and Review still show one entry per batch. The Library derives this from the job cards plus the per-item delegated work list, not from batched cards in the jobs list.

### Session 2026-11-23 (reconciliation against `main` 8d2d3b5e)

What the code says, where the brief's survey differed or was incomplete, and the ruling. Rulings are folded into the requirements below.

| Finding | Where | Ruling |
| --- | --- | --- |
| Bulk Fix all never delegates because its per-finding quick paths are resolver-only and return a plain result ("No deterministic fixes available…", "MusicBrainz / Cover Art Archive doesn't have a cover…"); the "no other source" switch does not reach them. | `rust/crates/musaic-services/src/check_fixes.rs:568-577` (`try_quick_fix_finding`), `:726`, `:860`, `:924`, `:1042`; `:1306` (`quick_fix_loop`) | Bulk delegation is new behaviour in the quick path, not the removal of a switch. FR-012. |
| The research-capable fixes are four: unidentified album, tags out of sync, track gaps, no cover art. | `check_fixes.rs:681,708,850,914,1032` | FR-007 lists all four. |
| A research request does not follow its album when the album's id changes: the id move covers the old job table but not the delegated-work store, and nothing else repoints it. | `rust/crates/musaic-db/src/renames.rs:84-98` (`move_album_rows`), `:104-112` (`ALBUM_ID_HOLDERS`) | Existing gap, fixed here. FR-023. |
| The merge and conversion blockers look only at the old job table; delegated work blocks nothing today. A second conversion reader exists beyond the one the survey named. | `rust/crates/musaic-db/src/merge.rs:483,535-550,638-661`; `rust/crates/musaic-fsops/src/multi_disc.rs:1571-1582` (`active_jobs`) and `:1885-1895` | Existing gap, fixed here. FR-022. |
| Three more readers of the old table that the survey did not list: the no-cover-art check's suppression rule, the background sweep's inputs digest, and the cover preview of an old job's stored result. Also the research-jobs summary. | `rust/crates/musaic-checks/src/checks/no_cover_art.rs:37-51`; `rust/crates/musaic-checks/src/inputs.rs:168-174`; `rust/crates/musaic-app/src/ops/apply.rs:392-405`; `rust/crates/musaic-db/src/research_jobs.rs:49` | All move to the single store. FR-021, FR-024. |
| Old waiting and running jobs were already turned into waiting research requests by the earlier delegation migration (v21 to v22); old completed results became awaiting review with an unconverted marker and are hydrated at run time from the old row's stored result; rows of an unknown type were skipped. | `rust/crates/musaic-db/src/delegated_work.rs:130-184` (`migrate_legacy`) | A request already waiting at upgrade stays waiting, unchanged. The stored result must move with the row, or the unconverted proposals stop being reviewable. Unknown types become history-only rows (Q5). |
| The album panel's Request research is shown only when no model is ready, which is always true in a shipped build; so it is not wrong today, only gated on the wrong thing. | `frontend/src/components/DetailSheet/index.tsx:498` | FR-009 removes the gate. |
| "Clear N applied or discarded" counts only old `acted_on` rows in the Activity panel. | `frontend/src/components/ActivityPanel.tsx:419,464-476` | FR-026. |
| Desktop cancel already covers waiting and held work. | `delegated_work.rs:1129-1157` | Batch cancel (FR-015) is that, for every item of a batch. |

Still verified good on 2026-11-23 at `8d2d3b5e`: schema v23 (`rust/crates/musaic-db/src/lib.rs:46`); the single Fix already delegates when no provider runs (`rust/crates/musaic-app/src/ops/fixes.rs:81-106`); the hint is hidden because hint entry depends on a ready model (`frontend/src/lib/aiGating.ts:121-123`); the "not available in this build" notice (`frontend/src/pages/Settings.tsx:221-234`, `aiGating.ts:50`); the Settings → Agents labels (`frontend/src/lib/agentCopy.ts:119-124`, `frontend/src/pages/SettingsAgents.tsx:150,185`); the Activity card "AI jobs" (`ActivityPanel.tsx:431`); event payload fields (`rust/crates/musaic-commands/src/events.rs:275-281`).

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Fix sends what the resolver cannot settle to the assistant (Priority: P1)

A listener has added an assistant. They open a "no cover art" finding and choose Fix. Musaic tries MusicBrainz and the Cover Art Archive first. When those have nothing, it records a research request for the assistant instead of answering that the build has no other source. Before the request is made the listener may add a hint ("the 2009 remaster, black sleeve"). The finding shows that research is pending, and the request appears in Review and in the Activity panel.

**Why this priority**: This is the everyday path. Today the server already delegates on a single Fix, but the desktop hides the hint and describes the action as if nothing would happen.

**Independent Test**: With an assistant added and the network offline, choose Fix on each of the four research-capable checks for an album the resolver cannot settle; observe the hint entry, the button's description, one queued request per album and kind, and the request in Review. Repeat with no assistant added and observe the same request, marked as waiting for an assistant.

**Acceptance Scenarios**:

1. **Given** a finding of a research-capable check (unidentified album, tags out of sync, track gaps, no cover art), **When** the listener chooses Fix, **Then** they can enter an optional hint before the request is made, and the button's description says that the resolver is tried first and anything left is sent to the assistant for research.
2. **Given** the resolver settles the album, **When** Fix runs, **Then** the change is applied as today and no request is made.
3. **Given** the resolver wrote part of what the finding asks for, **When** the rest is unresolved, **Then** what was written is kept and named, and a request is made for the rest.
4. **Given** no assistant has been added, **When** a request is made, **Then** it is kept and shown as waiting for an assistant, never as failed.
5. **Given** an album that already has an unresolved request of the same kind and input (the same hint), **When** Fix runs again, **Then** no second request is made and the existing one is shown (FR-011; a Fix with a different hint asks something else and is a request of its own).

---

### User Story 2 - Fix all creates one batch of requests, not hundreds of cards (Priority: P1)

A listener with 300 unidentified albums runs Fix all, leaving "Send what the resolver can't settle to my assistant" on. The resolver settles 180. The other 120 become research requests that belong to one batch. Review and the Activity panel show one entry for that batch ("120 research requests from Fix all · Unidentified album"), with counts by state, which the listener can open to see the albums, and cancel as a whole. The assistant, told that work is waiting, lists the batch and claims up to 25 of its items in one call, rather than one call per album.

**Why this priority**: Bulk runs are where the resolver leaves the most behind, and today they delegate nothing ("this build has no other source to try"). Without batching, the same run would bury the listener and the assistant in per-album handling.

**Independent Test**: On a synthetic library with a mix of resolvable and unresolvable albums, run Fix all for one research-capable check with the network offline. Observe one batch whose size equals the number of unsettled albums, one entry for it in Review and Activity, and a test agent that lists the batch and claims several of its items in one call.

**Acceptance Scenarios**:

1. **Given** a bulk run with sending to the assistant on, in which the resolver cannot settle some albums, **When** the run ends, **Then** each of those albums has one research request, all of the run's requests belong to one batch, and the run's summary names how many were sent for research.
2. **Given** a bulk run in which the resolver settles every album, or one run with sending to the assistant off, **When** it ends, **Then** no batch is created and no request is made.
3. **Given** the Fix all dialog, **When** it opens, **Then** the choice to send unsettled albums to the assistant is on if at least one assistant has been added and off otherwise, and the listener can change it before the run starts.
4. **Given** a batch, **When** the listener opens Review or the Activity panel, **Then** the batch appears once, with its origin (the check and when it ran), its size and its counts by state, and its items are reachable from it.
5. **Given** a batch, **When** the listener cancels it, **Then** every item of it that is not yet awaiting review or finished is cancelled in one action, and items already awaiting review stay for review.
6. **Given** a batch, **When** an assistant lists work, **Then** it can ask for that batch's items only, and claim up to 25 of them in one call, receiving the ones it won; each claimed item still has its own lease, result and review.
7. **Given** two assistants, **When** both claim from the same batch, **Then** no item is held by both.
8. **Given** a bulk run on Missing albums, **When** it runs, **Then** Missing albums are skipped and no request is made for them.

---

### User Story 3 - A cleaner desktop with no provider to configure (Priority: P2)

A listener opens Settings. There is no model-provider section, no "not available in this build" notice, no FireCrawl key and no queue pause. Settings → Agents uses plain words: "Add an assistant", "Added · 2 minutes ago", "Connected", "Listening for work", "Last request picked up". The Activity panel's card is called research requests, not "AI jobs". The album panel always offers Request research.

**Why this priority**: The removed settings promise something the app cannot do, and the old labels describe the plumbing rather than what the listener cares about.

**Independent Test**: Walk Settings, Settings → Agents, the Activity panel, the album panel, Review and the finding buttons; no text says "AI job", "AI backend", "provider", "Resume queue" or "not available in this build", and every label in the wording table reads as specified.

**Acceptance Scenarios**:

1. **Given** any build, **When** Settings is opened, **Then** no model-provider, model, parallelism, queue-pause, model-key or FireCrawl setting or notice appears.
2. **Given** Settings → Agents, **When** an entry is shown, **Then** its labels follow the wording table (FR-030).
3. **Given** an album that is present, not being applied, and has no unresolved request, **When** its panel is opened, **Then** Request research is offered.
4. **Given** the Activity panel, **When** requests exist, **Then** they are under a card named for research requests, with no pause or resume control.

---

### User Story 4 - Upgrading keeps every piece of history (Priority: P1)

A listener upgrades from a version that still had the old job table, or restores a backup made by the Python app. On first start the database is snapshotted and migrated. Everything the history view showed is still there: titles, hints, times, applied and discarded outcomes, failures and results waiting for review, including old rows whose job type this version does not know. Merging multi-disc rows, converting a set, the database info counts and the cover check all behave as before, now reading the one store.

**Why this priority**: Losing history or blocking a merge wrongly after an upgrade breaks trust in a tool that manages the listener's own collection.

**Independent Test**: Migrate fixture databases at each supported older version (the Python app's v16, and v21, v22 and v23 ones holding jobs in every status and of an unknown type) on a copy; compare the history view before and after; run the migration twice; run a merge and a conversion blocked by an unresolved request.

**Acceptance Scenarios**:

1. **Given** a database at any supported older version, **When** the app starts, **Then** it is snapshotted, migrated to the new version, and the old job table no longer exists.
2. **Given** old jobs in each status (queued, running, completed, failed, applied, discarded) and of each type (metadata, cover, metadata and cover, unknown), **When** the history view is opened after migration, **Then** each shows the same title, hint, times and outcome as before.
3. **Given** an old completed job whose result was never reviewed, **When** the listener opens it after migration, **Then** it is still reviewable from its stored result, including any cover it suggested.
4. **Given** an album with an unresolved research request, **When** the listener tries to merge its rows or convert its set, **Then** the merge or conversion is refused and names the request, as it was for an active old job.
5. **Given** an album folder is renamed, adopted by a scan under a new spelling, or merged into another row, **When** its id changes, **Then** its research requests and their history follow it to the new id.
6. **Given** a migrated database, **When** the migration runs again (a second start), **Then** nothing changes.

---

### User Story 5 - Nothing of the provider remains to leak or confuse (Priority: P3)

A listener who once stored a model key or a FireCrawl key, or whose database from the Python app still holds them in plaintext, upgrades. Those entries are removed once from the keyring and the database. Logs, settings responses and backups never mention them again.

**Why this priority**: Cleanup of a retired capability; lower risk than the user flows, but it closes the door on a stale secret.

**Independent Test**: Start the app on a scratch config with in-memory secrets holding both keys and an old database holding both plaintext rows; after start neither exists in the secret store, the settings table, the settings response or the log, and a second start does nothing.

**Acceptance Scenarios**:

1. **Given** model or FireCrawl keys in the keyring, **When** the upgraded app starts, **Then** they are deleted once and never read again.
2. **Given** an old database with those keys in plaintext, **When** it is opened or restored, **Then** the rows are removed as other secret rows are.

---

### Edge Cases

- A bulk run is stopped or the app quits part-way: requests already made stay, still in one batch, and the batch says it is from an unfinished run. Only a run that reaches its end counts as finished.
- A bulk run finds an album that already has an unresolved request of the same kind: the existing request is kept and counted, not duplicated, and it does not join the new batch.
- A batch item's album goes Missing, is renamed or is merged before the assistant reaches it: the item follows the album id (rename, adopt, merge) or, for Missing, stays as it is and the assistant's result is refused as stale under spec 004's rules.
- A restore or library switch rotates the library epoch: a batch's unresolved items are invalidated like any other work, and the batch shows them that way.
- The listener applies one proposal from a batch and discards another: each is decided on its own; the batch's counts follow.
- An old job row refers to an album that no longer exists: it is kept as history and shown as an unknown album, as today.
- An old job's stored result cannot be read: it is kept as history and shown as an unsupported old result, never dropped and never applied.
- No assistant has ever been added and the listener runs Fix all: sending to the assistant starts off, so the run is resolver-only unless the listener turns it on; if they do, requests are made and the run's summary and the batch say they wait for an assistant.
- A multi-item claim asks for 25 items when only 3 are eligible, or another assistant takes some first: it returns the ones it won (possibly none) and is not an error.
- An assistant claims several batch items and then stops working: each lease expires on its own and each item is requeued under spec 004's rules; nothing in the batch is lost.

## Requirements *(mandatory)*

### Functional Requirements

#### Removing the built-in model provider

- **FR-001**: Musaic MUST NOT contain a built-in model provider, a model worker, a model-provider setting, or any code path that calls a model. It MUST NOT start an agent program.
- **FR-002**: The local resolver (AcoustID vote, MusicBrainz name search, tracklist alignment, Cover Art Archive lookups, and the building of suggestions from their answers) MUST remain and MUST run before any research request is made, on every path that can make one.
- **FR-003**: The provider settings (provider, model, parallelism, queue paused), the model key and the FireCrawl key MUST disappear from Settings, from the settings the app reads and writes, and from the server's responses. Writing one of their names MUST be refused or ignored like any unknown setting.
- **FR-004**: The queue pause and resume, the provider list, and the retry of an old job by the provider MUST disappear from the desktop and the server. Retry of a failed or cancelled research request stays (spec 005, FR-040).
- **FR-005**: The "AI suggestions are not available in this build." notice, the "this build has no other source to try" answer and every "Asking Claude…" message MUST disappear; each place that showed one says what now happens (a research request is made, or the resolver's answer).
- **FR-006**: The desktop MUST NOT branch on whether a model is ready. Wherever two texts existed (one naming a model, one neutral), only the neutral text remains.

#### Single fix and research requests

- **FR-007**: Fix on a finding of a research-capable check (unidentified album, tags out of sync, track gaps, no cover art) MUST offer an optional hint before the request is made, and its description MUST say that the resolver is tried first and anything left goes to the assistant.
- **FR-008**: When the resolver cannot settle such a finding, Fix MUST make a research request, keep and name anything the resolver already wrote, and say where the request can be followed.
- **FR-009**: Request research MUST be offered on the album panel for any present album that is not being applied and has no unresolved request, whatever the assistant situation.
- **FR-010**: A request made while no assistant has been added, or while none is connected, MUST be kept and shown as waiting for an assistant.
- **FR-011**: At most one unresolved request per album, kind and input MUST exist (spec 004). A second Fix, Request research or bulk run reuses the existing one.

#### Bulk fix and batches

- **FR-012**: Fix all for a research-capable check MUST offer, before the run starts, the choice to send what the resolver cannot settle to the assistant (on by default when at least one assistant has been added, off otherwise). With it on, the run MUST make a research request for every album the resolver could not settle, skipping Missing albums, with no cap on how many. With it off, the run is resolver-only and makes no request.
- **FR-013**: All requests made by one bulk run MUST belong to one batch. A run that makes no request makes no batch. A request that already existed for that album and kind (FR-011) keeps its own batch membership, or none, and is counted in the run's summary as already requested.
- **FR-014**: A batch MUST record its origin (the check, when the run started, whether the run finished) and MUST be shown in Review and in the Activity panel as one entry with its size and its counts by state; its items MUST be reachable from that entry.
- **FR-015**: The listener MUST be able to cancel a batch in one action, cancelling each of its items that is waiting or held by an assistant; items awaiting review or finished are unaffected.
- **FR-016**: Each batch item MUST keep its own lease, result, review and history. Approval and discard stay per item (spec 004: a desktop decision bound to one exact proposal).
- **FR-017**: Each research request MUST carry an optional batch reference visible to an assistant in its summary and detail. An assistant MUST be able to list work filtered by batch, claim up to 25 eligible items (of a batch, or of the queue) in one call, and renew several of its held leases in one call. A multi-item claim has the same atomic guarantee as a single claim (no item is ever held by two principals), returns the items it won each with its own lease, and is not an error when it wins fewer than asked or none. A batch has no state, claim or result of its own.
- **FR-018**: The bulk run's summary MUST say how many albums were settled, how many were sent for research (and in which batch), and how many were skipped or failed.

- **FR-034**: Work events stay one per item and keep their reference-only payload; a batch item's events MUST also carry its batch reference (an opaque id, no text), so an agent can coalesce a burst. No new event type is added.
- **FR-035**: The single-item work tools keep their behaviour; the work-delegation skill MUST teach an agent to prefer the batch filter and the multi-item claim when a burst of requests arrives.
- **FR-036**: Cancel on a research request's card (Activity row, Review, album panel) MUST cancel the request behind it, every unresolved (waiting, held or running) item of the card, through the research cancel path (same guard, audit and events as Cancel research); items awaiting review or finished are untouched. It MUST NOT fail with a message pointing at review actions. Removing a card is offered only for finished history, and the label and confirm text say which happens.

#### Old job history and the single store

- **FR-019**: A new schema version MUST move everything the history view and review need from the old job table (title, hint, timestamps, status, outcome, error, stored result, and the link between a combined metadata-and-cover job and its two requests) into the delegated-work store, and MUST then drop the old table.
- **FR-020**: Every supported older database (from the Python app's v16 onward) and every restored older backup MUST migrate to the new version, in order, after the startup snapshot, including rows whose album no longer exists. An old row with an unknown job type MUST be kept as a history-only record: shown in history with its original title, hint, times, status and type name, never listed to an agent, never retryable or reviewable, and blocking nothing. A request that was waiting when the upgrade runs stays waiting, unchanged.
- **FR-021**: After migration the history view MUST show every old job as it did before: same title, hint, times and outcome. An old completed result that was never reviewed MUST stay reviewable from its stored result, including a suggested cover's preview.
- **FR-022**: Merging multi-disc rows and converting or gathering a multi-disc set MUST be refused while any member has an unresolved research request (waiting, held, or awaiting review), and finished requests MUST be repointed to the surviving row, as was done for old jobs.
- **FR-023**: When an album's id changes (folder rename, scan adoption, merge), its research requests and their history MUST move to the new id in the same transaction as the album row.
- **FR-024**: The database information MUST count research requests (history-only rows included) from the single store. The "no cover art" check MUST keep its current rule, read from the single store: within the same window as today, an album whose cover research failed, or finished (applied or discarded) with no cover candidate in its result (a no-match outcome included), is not reported again. The background sweep's inputs MUST still change when an album's research requests change.
- **FR-025**: The migration MUST be repeatable: a second run changes nothing.
- **FR-026**: "Clear applied or discarded" MUST remove from history every finished request whose outcome is applied or discarded (native, migrated and history-only alike), and its count MUST be exactly that number. Failed and cancelled requests stay, with their Retry. A batch whose items are all cleared no longer appears. Clearing never touches albums, files or the backups made by an apply.

#### Keys

- **FR-027**: On first start after upgrade, the model key and FireCrawl key entries in the keyring MUST be deleted once. Their names MUST stay on the list of secret settings scrubbed from any opened or restored database.
- **FR-028**: No key, token or secret for a model service MUST be read, stored, logged or returned.

#### Routes, contract and wording

- **FR-029**: The job list, job detail, active-job-by-album, job creation and job stream that the delegated path uses MUST keep their addresses and response shapes; they are the desktop's view of research requests. Addresses that existed only for the provider are removed.
- **FR-030**: The desktop and `docs/agents.md` MUST use this wording (the words "pairing" and "pair" stay in code, database and the tool contract):

  | Now | Becomes | True when |
  | --- | --- | --- |
  | Pair an assistant | Add an assistant | the listener creates the entry |
  | Paired · 2 minutes ago | Added · 2 minutes ago | the credential exists |
  | Bridge reachable · not yet | Connected · not yet | the assistant's host has reached Musaic |
  | Subscription active · no | Listening for work · no | it is currently polling for requests |
  | Last work claimed | Last request picked up | unchanged meaning |

  "Revoke" stays. The Activity panel's "AI jobs" card is renamed for research requests for an assistant, and no label anywhere says "AI job".
- **FR-031**: Any change to the agent tools, events or skill for batches MUST be reflected in the generated tool and event catalogs and the skill, with a new skill version, and the repository's drift checks MUST pass.
- **FR-032**: The documentation (README, operations inventory, agent and MCP guides, project instructions) MUST describe external agents as the only way research is done, and MUST NOT describe a built-in provider.

#### Scope

- **FR-033**: This feature adds no model provider, no agent launcher, no cloud connectivity and no listener beyond loopback.

### Key Entities

- **Research request**: one delegated-work item (spec 004) for one album and kind, with its state, lease, result and review. Made by Fix, Request research or a bulk run.
- **Batch**: the set of research requests made by one bulk run, identified by an opaque reference carried on each item and on its events, with its origin (check, start time, finished or not) and derived counts by state. It has no state, claim or result of its own; no cap on its size.
- **Historical request**: a research request migrated from the old job table, keeping its title, hint, times, outcome and raw stored result. **History-only record**: an old row of an unknown type, kept for the history view only (FR-020).
- **Assistant entry**: what Settings → Agents shows for one pairing: added, connected, listening for work, last request picked up.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: For each of the four research-capable checks, a Fix that the resolver cannot settle produces exactly one research request and offers a hint, in 100% of trials, with no assistant needed at the time.
- **SC-002**: A bulk run that leaves N albums unsettled produces exactly N requests in one batch and one entry in each of Review and the Activity panel, for N from 1 to 1,000.
- **SC-003**: An assistant can claim every item of a 1,000-item batch in at most 40 claim calls (25 per call), and no item is ever held by two assistants in a race.
- **SC-004**: After migration, 100% of old jobs in every status and of every type (unknown included) appear in history with unchanged title, hint, times and outcome, and a second start changes nothing.
- **SC-005**: Zero occurrences of "AI job", "AI backend", "Resume queue", "not available in this build" or a model-provider setting in the shipped desktop text.
- **SC-006**: Zero model keys or FireCrawl keys remain in the keyring, the settings table, settings responses or the log after one start of the upgraded app.
- **SC-007**: A merge or conversion is refused in 100% of trials where any member has an unresolved research request, and research requests follow an album through rename, adoption and merge in 100% of trials.
- **SC-008**: The repository's required checks pass, including the tool, event and skill drift checks.

## Assumptions

- Specs 003, 004 and 005 are merged and are the base: pairing, the nine work tools, leases, results, review, the event history, the skill and Settings → Agents. Their state machine and validation rules are unchanged except where FR-017 and FR-034 add batch access (spec 005's FR-016 payload gains the optional batch reference).
- A shipped build never had a usable built-in provider, so no listener loses a working capability; only tests exercised it.
- The four research-capable checks are the ones whose fix can make a request today (unidentified album, tags out of sync, track gaps, no cover art).
- Old jobs that were waiting or running were already turned into waiting research requests by the earlier delegation migration; a restored older backup goes through that step again on its way to the new version.
- "Batch" is a grouping for display and retrieval; it does not change how an individual request is researched, reviewed or applied.
- Approving several proposals of a batch at once is out of scope: each proposal is reviewed on its own. It is tracked as GitHub issue #375.
