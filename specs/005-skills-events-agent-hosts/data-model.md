# Data model: MCP Skills, Events and external-agent hosts

Schema v22 → v23, one migration (`v22_to_v23`, same pattern as `v21_to_v22`). All times are milliseconds since the epoch from the injected clock.

## New tables

### `delegation_events`

| Column | Type | Notes |
| --- | --- | --- |
| `sequence` | INTEGER PRIMARY KEY | The audit sequence of the committed change (`delegation_audit.sequence`) |
| `library_epoch` | TEXT NOT NULL | Epoch at commit |
| `work_id` | TEXT NOT NULL | |
| `event_type` | TEXT NOT NULL | `requested` or `updated` |
| `kind` | TEXT NOT NULL | Work kind at commit |
| `state` | TEXT NOT NULL | Resulting state |
| `revision` | INTEGER NOT NULL | Work revision after the change |
| `committed_at` | INTEGER NOT NULL | |

Index on `(library_epoch, sequence)` and on `committed_at`.

Rules: inserted by `record_change` in the caller's transaction, only when the work item is new or its state changed. Never updated. Deleted when `committed_at` is more than 7 days old, or when its epoch is no longer current.

### `delegation_event_state` (single row) *(added by the analysis: truncation needs a prune watermark)*

| Column | Type | Notes |
| --- | --- | --- |
| `singleton` | INTEGER PRIMARY KEY CHECK (singleton = 1) | |
| `pruned_through` | INTEGER NOT NULL DEFAULT 0 | Highest `sequence` ever pruned or removed by an epoch rotation; set in the pruning transaction |

### `agent_host_observations`

| Column | Type | Notes |
| --- | --- | --- |
| `pairing_id` | TEXT PRIMARY KEY | References `agent_pairings(id)`; row removed with the pairing |
| `client_name`, `client_version` | TEXT | As reported, bounded to 80 characters, control characters stripped |
| `profile` | TEXT | `2026-07-28` or `2025-11-25` |
| `tasks`, `skills`, `events` | INTEGER (0/1) | What the host declared or, for Skills, used |
| `observed_at` | INTEGER NOT NULL | |

## Changed tables

- `work_attempts`: add `last_heartbeat_at INTEGER`. Set on claim, renew and progress.

## In-memory state (`AgentHub`)

- `last_seen[pairing]`: time of the latest authenticated agent request.
- `waiting_polls[pairing]`: count of parked polls and the time of the latest one.
- The condition variable that parked polls wait on.

None of it is authoritative; it resets on restart and the UI shows times, not claims.

## Derived values

- **eventId**: `evt_<first 8 hex of epoch>_<sequence>`.
- **Cursor**: `base64url("v1.<epoch>.<sequence>")`; head cursor uses the largest audit sequence of the current epoch (0 when none). A cursor is stale (truncated) when its sequence is below `pruned_through`, its epoch differs, it does not decode, or it is older than `maxAgeMs`.
- **Event name**: `musaic.work.<event_type>.v1`.
- **Subscription active**: `waiting_polls[pairing]` non-zero, or latest within 90 s.
- **Waiting for an agent**: work is `queued`, and no pairing has an active subscription.

## Lifecycle notes

- Library epoch rotation (restore, root switch): delete `delegation_events` rows of other epochs in the rotation transaction, as the outbox does.
- Revoking a pairing: notify parked polls; delete its observation row.
- Skill bundle: no database state. The install record lives in the host's skills directory.

## Wire records (in `musaic-commands`, no database types)

- `EventDescriptor {name, description, delivery, inputSchema, payloadSchema}`
- `EventOccurrence {eventId, name, timestamp, data: WorkEventData, cursor?}`
- `WorkEventData {workId, kind, state, revision, libraryEpoch}`
- `PollParams {name, arguments: {kinds?, states?}, cursor?, maxAgeMs?, maxEvents?, waitMs?}` (`waitMs` is bridge-only)
- `PollResult {events, cursor, truncated, hasMore, nextPollMs}`
- `HostObservation {clientName, clientVersion, profile, tasks, skills, events}`
