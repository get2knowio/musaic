# Contract: Events

Target: extension `io.modelcontextprotocol/events` (Beta) on protocol revision `2026-07-28`, checked against `modelcontextprotocol/ext-events` at `c77b8439` (#368). Offered on the modern profile whether or not the request's client capabilities list the extension (a client MAY declare it); a legacy session gets `-32601`.

## Event types (`docs/agent-events.json`, generated)

| name | meaning |
| --- | --- |
| `musaic.work.requested.v1` | Work became available: created, requeued, released or retried (resulting state `queued`) |
| `musaic.work.updated.v1` | Any other state change |

Descriptor: `{name, description, delivery: ["poll","push"], inputSchema, payloadSchema}`.

`inputSchema`:
```json
{ "type": "object", "additionalProperties": false, "properties": {
    "kinds":  { "type": "array", "items": { "enum": ["metadata.identify","metadata.correct","cover.research"] } },
    "states": { "type": "array", "items": { "enum": ["queued","claimed","running","awaiting_review","applied","discarded","failed","cancelled"] } } } }
```

`payloadSchema` (`data`): required `workId` (string), `kind`, `state`, `revision` (integer), `libraryEpoch` (string); `additionalProperties: false`.

## `events/list`

Result `{ "resultType": "complete", "events": [EventDescriptor, EventDescriptor] }`.

## `events/poll`

Params `{name, arguments?, cursor?, maxAgeMs?, maxEvents?}`. `maxEvents` 1 to 100, default 50.

Result:
```json
{ "resultType": "complete",
  "events": [ { "eventId": "evt_1a2b3c4d_42", "name": "musaic.work.requested.v1",
                "timestamp": "2026-11-23T10:00:00Z",
                "data": { "workId": "…", "kind": "metadata.identify", "state": "queued", "revision": 1, "libraryEpoch": "…" } } ],
  "cursor": "<opaque>", "truncated": false, "hasMore": false, "nextPollMs": 30000 }
```

- Events in a poll carry no `cursor`; the response's `cursor` is the position (a per-event `cursor` is push only).
- `cursor` null or absent: no events, head cursor.
- Unusable cursor (undecodable, other epoch, older than retention, below the `maxAgeMs` floor): `truncated: true`, no events, head cursor. Never an error.
- Ordered by sequence. Events whose work the principal may not currently see are skipped; the cursor still advances.

## `events/stream`

Params as poll, plus `heartbeatMs` (granted value clamped 5 000 to 60 000, default 30 000). Messages, each with `_meta["io.modelcontextprotocol/subscriptionId"]` = the request id:

1. `notifications/events/active` `{cursor, truncated, heartbeatMs}` first, and again with `truncated: true` on a gap.
2. `notifications/events/event` `{eventId, name, timestamp, data, cursor}`.
3. `notifications/events/heartbeat` `{cursor}` whenever `heartbeatMs` passes with nothing else sent (the loop may poll more often than that; it does not beat more often).
4. `notifications/events/terminated` `{error: {code, message, data?}}` on revocation or a removed type, then the final result `{ "resultType": "complete", "_meta": { "io.modelcontextprotocol/subscriptionId": <request id> } }`.

Cancelled by `notifications/cancelled` for the request id; the server stops delivering and releases the loop. The final result may be omitted after a cancel.

## Errors

| Name | Code | When |
| --- | --- | --- |
| InvalidParams | -32602 | Arguments do not match `inputSchema`; bad `maxEvents` |
| NotFound | -32023 | Unknown event name (`data.kind = "event"`) |
| Forbidden | -32024 | Pairing missing, revoked or lacks the work-read permission |
| ResourceExhausted | -32025 | More than 4 waiting polls for the pairing (`data: {limit: "waitingPolls", max: 4}`), or more than 8 streams in the process (`data: {limit: "streams", max: 8}`) |
| Unsupported | -32026 | `events/subscribe`, `events/unsubscribe` (webhook delivery is not offered; `data: {feature: "deliveryMode", value: "webhook"}`) |

## Guarantees

At-least-once; ordered within a subscription; `eventId` stable across redelivery, restarts and reconnects. Clients deduplicate by `eventId` and rely on the atomic claim.

## Tests

1. `events/list` equals the checked-in catalog.
2. Null cursor → empty + cursor; create work → one `requested`; claim → one `updated` (holder only sees it).
3. Reopen the database and restart the bridge: same cursor → same following `eventId`s.
4. Advance the clock 8 days → `truncated`. Rotate the epoch → `truncated`.
5. Filters by kind and state.
6. Two principals receive the same `requested`; both claim; one succeeds.
7. Revoke during a waiting poll → `Forbidden`; stream → `terminated`.
8. Fifth waiting poll → `ResourceExhausted`. A parked poll does not delay a concurrent tool call.
9. Stream: `active` first, heartbeat on idle, cancel stops delivery.
10. Legacy session → `-32601`; a modern request that does not declare the extension is served.
11. `data` has exactly the five keys for every transition in a full lifecycle.
