# Research: MCP Skills, Events and external-agent hosts

Baseline: `main` at `d8f6019e`; published MCP documentation as read on 2026-11-23.

## Pinned targets

- **Protocol revision**: `2026-07-28` for both extensions; `2025-11-25` remains supported for tools, resources and prompts.
- **Skills**: extension `io.modelcontextprotocol/skills`, Beta, no `revision` setting published (implicit initial revision). Settings: `directoryRead` (boolean, default false).
- **Events**: extension `io.modelcontextprotocol/events`, Beta, no `revision` setting published. Settings: `listChanged` (boolean, default false).
- **SDK**: `rmcp =3.11.0`.

Sources read: the Skills overview and its specification site, the Events overview, the Events design record (SEP-3415) and the extensions overview. Afterwards (issue #368) the full specifications: `modelcontextprotocol/ext-events` at `c77b84395eff0ae32f53b0516c2214fe276ed050` (`specification/beta/events/events.mdx`), and `modelcontextprotocol/ext-skills` at `1631b3f193c9bd90c5693743d7e0fc39faf266d3` (`specification/beta/skills.mdx`, `schema/beta/schema.ts`). What that check confirmed and changed is in the compatibility note in `docs/mcp.md`.

## Decisions

### D1. Negotiation

- **Decision**: Skills and Events handlers answer only when the request is on the modern profile. Neither requires a client declaration *(amended, #368: Events required the request's `_meta` client capabilities to list `io.modelcontextprotocol/events`; the Events specification says a client MAY declare it, so a request without the declaration is served)*. Legacy sessions get `-32601` for `skills/*` and `events/*`.
- **Rationale**: both extensions say they are not defined before `2026-07-28`. rmcp gates `skills/*` only on the server's declaration, so Musaic adds the profile gate.
- **Alternative rejected**: declaring the extensions in `initialize` for `2025-11-25` (the extensions overview permits the field, but neither extension is defined there).

### D2. Skill bundle and identity

- **Decision**: files under `skills/musaic-work-delegation/`: `SKILL.md`, `references/work-lifecycle.md`, `metadata-evidence.md`, `cover-research.md`, `result-schemas.md`, `async-operations.md`, `recovery.md`, `tools.md` (generated), `events.md` (generated). Embedded at build time. URI `skill://musaic-work-delegation/<relative path>`; digest `sha256:<64 lowercase hex>` over the embedded bytes; size is their length. The manifest lists every file once, including `SKILL.md`. `resultType: "complete"`, `ttlMs` 3 600 000, `cacheScope: "public"`.
- **Rationale**: static bytes compiled into the binary mean the manifest and the bytes served cannot disagree, and nothing reads the filesystem at run time. rmcp's `SkillCatalog` is filesystem-only and needs a feature Musaic does not enable.
- **Frontmatter**: `name`, `description`, `metadata.version` (semantic version). Keys under `metadata` prefixed `io.modelcontextprotocol/` are reserved and unused.
- **Lookup**: the requested URI must equal a listed URI byte for byte. Before comparing, refuse anything that contains `..`, `%`, `\`, a control character, a query or fragment, a trailing slash, or a scheme other than `skill`. Unknown → `-32602` with a fixed message that does not echo a path.
- **`directoryRead`**: not declared. Declaring it would oblige support for every served directory; the manifest already answers the question.
- **Lock**: `skills/musaic-work-delegation/skill.lock.json` `{version, files:[{path, digest, size}]}`. The lock itself is not served.

### D3. Fallbacks

- **Decision**: `resources/list` returns the skill files (same URIs, `mimeType: text/markdown`) on both profiles; `resources/read` serves them. `prompts/list` has `musaic_research_album` with an optional `album_id` argument; `prompts/get` returns the `SKILL.md` body plus a short user-turn instruction ("process pending Musaic work" or "research album `<id>`"). The album id is validated as 16 hex characters and is the only interpolated value.
- **Rationale**: "the same versioned guide as an ordinary resource" is satisfied by the same bytes at the same URIs without a second copy to drift.

### D4. Installed skill

- **Decision**: `musaic-cli setup --skill [--host claude-code]` writes the bundle to `<home>/.claude/skills/musaic-work-delegation/` and an install record `.musaic-skill.json` there (`version`, per-file digest). Rules: target absent → write. Target present with a record and every file matching the record → replace (owned, unmodified). Target present and any file differs from the record, or there is no record → move the directory to `<name>.musaic-backup-<stamp>` first, then write. `uninstall --skill` removes only recorded files whose digest still matches, then the record, then the directory if empty. `--dry-run` prints the plan. Output always includes: "Installed. This does not show that the host has loaded the skill."
- **Deviation (T015)**: the backup goes to `<home>/.claude/musaic-skill-backups/musaic-work-delegation.musaic-backup-<stamp>`, not to a sibling of the skill directory: a second directory under the skills folder whose `SKILL.md` has the same `name` would be loaded as a duplicate. A symlink or plain file at the target is refused, not followed.
- **Rationale**: mirrors the owned-file, backup-first behaviour setup already has for config entries, with an explicit record because a directory of Markdown cannot be recognised by shape.
- Only Claude Code's skills directory is known and verified locally; other hosts are listed as unsupported for `--skill`.

### D5. Event history

- **Decision**: new table `delegation_events` (see data-model). `record_change` gains the work item's resulting state and inserts an event row when the item is new or its state changed: `requested` when the resulting state is `queued`, `updated` otherwise. Renewals and progress that do not change state write no event. *(Amended: the earlier "Operation-only transitions write none" contradicted FR-015, "any other state change". Task T017 enumerates every transition of spec 004's state machine and records here which path writes it; any transition that changes the stored work state writes one event. Only changes that leave the state as it was write none.)*
- **eventId**: `evt_<epoch first 8 hex>_<sequence>`; stable because the sequence is the audit sequence of the committed change.
- **Cursor**: `base64url("v1." + epoch + "." + sequence)`. Opaque to clients. A cursor that does not decode, names another epoch, or names a sequence below the prune watermark (`delegation_event_state.pruned_through`, the highest sequence ever pruned) *(amended: was "below the oldest retained row minus one", which wrongly truncated a cursor whose neighbouring audit sequences wrote no event row)*, yields `truncated: true` and a fresh cursor at the current head. `maxAgeMs` raises the floor the same way.
- **Null cursor**: returns no events and the head cursor.
- **Retention**: rows older than 7 days are deleted by the app tick (injected clock). Rows of another epoch are deleted on rotation. `delegated_work` and the audit are untouched.
- **Alternative rejected**: stop deleting outbox rows and add columns. The outbox's delete-on-publish is what the desktop relay relies on; a second consumer with its own retention is cleaner as its own table.

**Transition inventory (T017).** `record_change` is the one place every stored work-state change passes through, so the rule is mechanical: after the update, it writes one event when the work item has no earlier event or the stored state differs from the state of its latest event (`requested` when the state is `queued`, else `updated`). Writers, by `transition` name in the audit:

| Transition (audit name) | Stored state change | Event |
| --- | --- | --- |
| `queued` (enqueue) | new row, `queued` | `requested` |
| `grouped`, `legacy_hydrated`, `legacy_result_converted`, `renewed`, `progress` after the first, `review_reserved`, verification updates | none | none |
| `claimed` | `queued` to `claimed` | `updated` |
| `progress` (first) | `claimed` to `running` | `updated` |
| `attempt_failed`, `released`, `expired` | to `queued` (attempts left) | `requested` |
| `attempt_failed`, `released`, `expired` | to `failed` (budget spent, or not retryable) | `updated` |
| `cancelled` (agent with `work.cancel`, desktop) | to `cancelled` | `updated` |
| `stale_context` (album or epoch change) | to `cancelled` | `updated` |
| `local_result`, completion (`complete`) | to `awaiting_review` | `updated` |
| `applied`, `review_interrupted` | to `applied` or `failed` | `updated` |
| `discarded` | to `discarded` | `updated` |
| `retried` | `failed` or `cancelled` to `queued` | `requested` |
| `migrated` (v21 to v22) | rows created by the migration | none: the history tables do not exist at that step; work that is queued at upgrade has no event until it changes, so an agent lists work once on connect (the skill says so) |

The tests `musaic-db/tests/delegation_events.rs` walk a full lifecycle on separate connections and check each row of this table that a database call can reach.

### D6. Event types, filters, scope

- Types: `musaic.work.requested.v1`, `musaic.work.updated.v1`; `delivery: ["poll","push"]` (or `["poll"]` if the stream risk materialises).
- `inputSchema`: `{kinds?: WorkKind[], states?: WorkState[]}`, additional properties refused (`InvalidParams`).
- `payloadSchema` / `data`: `{workId, kind, state, revision, libraryEpoch}`, nothing else.
- **Scope**: `events/*` needs the same permission kind the work-list tool needs (`decide` on that kind; no new kind). Visibility per row repeats `delegated_work::visible` against the work item's *current* row at delivery: queued work with a base revision is visible to every such principal, held work only to its holder. An event whose work is no longer visible is skipped, and the cursor still advances past it.
- **Revocation**: each poll re-reads the pairing; revoked → `Forbidden`. The stream turns that into `notifications/events/terminated`.

### D7. Bridge and waiting

- **Decision**: `POST /__musaic/agent/v1/events/list` and `…/events/poll`. Poll body adds `waitMs` (0 to 25 000). With no events and `waitMs > 0`, the operation parks on a condition variable in `AgentHub` that `publish_pending`, revocation and shutdown notify, then re-queries once. At most 4 parked polls per pairing; the fifth gets `ResourceExhausted`.
- **Rationale**: keeps the guard's buffered-JSON rule and the thin-handler rule intact; the bus/outbox publish is the wake hint and the table is the truth. A parked call holds one blocking-pool thread and no buffer.
- **Stream** (stdio process): `events/stream` → send `notifications/events/active` (cursor, truncated, granted `heartbeatMs`, default 30 000, clamp 5 000 to 60 000) → loop { poll with `waitMs` = min(heartbeat, 25 000); for each event send `notifications/events/event`; if none, send `notifications/events/heartbeat` with the cursor, only when `heartbeatMs` has passed since the last message sent *(amended: was after every empty poll, which beat faster than a granted 60 000 ms)*; on `truncated`, send a fresh `active` }. All notifications carry `_meta["io.modelcontextprotocol/subscriptionId"]` = the request id. Cancel (`notifications/cancelled`), stdin EOF or a failed write ends the loop. `Forbidden` / `NotFound` from the bridge → `notifications/events/terminated` then the final result. Up to 8 concurrent streams per process.
- **Error codes**, defined once in `musaic-commands::events` and confirmed against the Events specification's Error Handling table (#368): `InvalidParams -32602`, `NotFound -32023` (`data: {kind: "event"}`), `Forbidden -32024`, `ResourceExhausted -32025` (`data: {limit, max}`), `Unsupported -32026` (`data: {feature: "deliveryMode", value: "webhook"}`).

**Spike outcome (T024, T025).** rmcp 3.11.0 can do all three things the stream needs, so `events/stream` is implemented and `push` is declared. (1) `ServerHandler::on_custom_request` receives any method it does not know (`events/list`, `events/poll`, `events/stream`, `events/subscribe`, `events/unsubscribe`) with its raw `params`. (2) rmcp runs every request in its own task, so a custom request may stay open: the handler sends `notifications/events/*` with `context.peer.send_notification(ServerNotification::CustomNotification(..))` while the loop runs, and the request's response is sent when the handler returns. (3) A `notifications/cancelled` for the request id cancels `context.ct` (the serve loop removes the id from its token pool and cancels it), which the loop observes with `select!`. Limits found: a blocking poll already parked in the app cannot be aborted, so a cancelled stream's last poll finishes on its own wait (at most 25 s) and the CLI shuts its runtime down with a one-second timeout so it does not delay exit; rmcp still sends the final result after a cancel (the contract allows omitting it). `get_info` cannot differ between `initialize` and `server/discover`, so the Skills and Events extensions are declared once and the handlers answer only on the 2026-07-28 profile (amended, #368: Events no longer also require the client's declaration), as the plan's fallback says. rmcp refuses a `skills/get` for a URI that is not a `SKILL.md` with a message that repeats the URI, so a thin `Gate` service in front of the handler answers it with the fixed `unknown resource` first.

### D8. Host observations

- **Decision**: the stdio process posts `session/observe` `{clientName, clientVersion, profile, tasks, skills, events}` when it first learns them and when they change (modern requests carry them per request; legacy at `initialize`). Strings are bounded (80 chars, control characters stripped) and shown as reported, labelled "reported by the host". Stored per pairing in `agent_host_observations`. `AgentHub` keeps in memory `last_seen` (any authed request) and `last_waiting_poll`. "Subscription active" = a waiting poll within the last 90 seconds. "Last work claimed" = latest `work_attempts` row for the principal. `GET …/session` returns the pairing's own observation for `doctor`.
- Publishes `AgentStateChanged(Hosts, [pairing])` when a stored observation changes or subscription state flips; throttled to one per pairing per 5 seconds.
- `musaic_capabilities.extensions` lists tasks, skills and events as *offered by the server*; negotiated state is in diagnostics.

### D9. Delegated work in the desktop

- **Decision**: `GET /api/research/work` → `jobs.delegated_work`: items in `queued`, `claimed`, `running`, `awaiting_review`, `failed`, `cancelled` with album id, kind, state, revision, holder name, `lastHeartbeatAt`, `leaseExpiresAt`, attempts, last error (bounded), and `waitingForAgent` (queued and no pairing with an active subscription). New column `work_attempts.last_heartbeat_at`, set on claim, renew and progress. UI section in Review; `running` is labelled "Investigating". Retry calls the existing `jobs.research_decide` with `retry`.
- Query key `["research","work"]`: already invalidated by the `work` family.

### D10. Launcher (removed from scope)

Musaic never launches an agent program. The user connects their own agent host to the bundled stdio server; Musaic starts no process. The launcher design, its tables (`agent_launcher`, `agent_runs`), its crate and its UI were cut; nothing of them is implemented. "Waiting for an agent" means a queued item while no pairing has an active subscription.

### D11. `operations` refresh

The UI's catch-all for `AgentStateChanged` invalidates every `["agent"]` query. New variants `hosts` and `launcher` get their own cases (`["agent","diagnostics"]`, `["agent","launcher"]`); the existing behaviour for `operations` is left alone.

## Compatibility note (seed for `docs/mcp.md`)

- Spec 004's text calls an Events occurrence's content a "payload"; the published field is `data`, and the type field is `name`.
- Earlier drafts and prototypes used a top-level `events` capability and an `events/subscribe` call for local push; the published contract uses `capabilities.extensions` and `events/stream`. `events/subscribe` is webhook-only and not offered.
- Skills are fetched by `SKILL.md` URI, not by name.
- rmcp 3.11.0 implements Skills; Events are implemented here to the documented wire contract.
- Events error codes `-32023` to `-32026` were provisional in the design record; the Events specification assigns them (#368).
