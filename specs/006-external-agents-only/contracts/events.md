# Contract: work events (change to spec 005's payload)

Event names unchanged: `musaic.work.requested.v1`, `musaic.work.updated.v1`. One event per item per state change, as today (FR-034).

## `payloadSchema` (`data`)

```json
{ "type": "object", "additionalProperties": false,
  "required": ["workId", "kind", "state", "revision", "libraryEpoch"],
  "properties": {
    "workId": { "type": "string" },
    "kind": { "enum": ["metadata.identify", "metadata.correct", "cover.research"] },
    "state": { "enum": ["queued","claimed","running","awaiting_review","applied","discarded","failed","cancelled"] },
    "revision": { "type": "integer" },
    "libraryEpoch": { "type": "string" },
    "batchId": { "type": "string", "description": "Opaque reference to the bulk run that made this request. Absent for requests not made by a bulk run." } } }
```

`batchId` is the only change: optional, a reference only (no text), absent when the item has none. It is stored on the event-history row when the event is recorded, and that row keeps it after the item is cleared. A poll, like every work event, returns only events of items that still exist and that the agent may see (spec 005's visibility rule), so it returns no event, and no `batchId`, for a cleared item.

## Compatibility note (goes into `docs/mcp.md` and `docs/agent-events.md`)

> Since skill 1.1.0, work event payloads may carry `batchId`. The field is optional and additive, so the event types stay `.v1`. Read the payload schema from `events/list` rather than a cached copy: a client that validates against the earlier schema with `additionalProperties: false` will reject events that carry it.

## Why not `.v2`

See research D10: no existing field changes meaning, the descriptor is served live, and a second type would split every stream and force resubscription for no semantic change.

## Unchanged

`inputSchema` (no batch filter on poll or stream), cursors, retention, truncation, delivery, heartbeat and error rules of spec 005.
