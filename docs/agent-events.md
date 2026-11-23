# Musaic events

Generated from the event catalog; do not edit by hand.

Extension `io.modelcontextprotocol/events` on protocol revision `2026-07-28`. An event says that work changed; read the work with `musaic_work_get`. Delivery is at least once: deduplicate by `eventId` and rely on the atomic claim.

## `musaic.work.requested.v1`

Work became available to claim: it was created, requeued, released or retried.

Delivery: poll, push.

`data` holds exactly:

- `workId` (string)
- `kind` (one of `metadata.identify`, `metadata.correct`, `cover.research`)
- `state` (one of `queued`, `claimed`, `running`, `awaiting_review`, `applied`, `discarded`, `failed`, `cancelled`)
- `revision` (integer)
- `libraryEpoch` (string)

Arguments (all optional): `kinds`, `states`; any other key is refused.

## `musaic.work.updated.v1`

Delegated work changed state in any other way (claimed, running, awaiting review, applied, discarded, failed, cancelled).

Delivery: poll, push.

`data` holds exactly:

- `workId` (string)
- `kind` (one of `metadata.identify`, `metadata.correct`, `cover.research`)
- `state` (one of `queued`, `claimed`, `running`, `awaiting_review`, `applied`, `discarded`, `failed`, `cancelled`)
- `revision` (integer)
- `libraryEpoch` (string)

Arguments (all optional): `kinds`, `states`; any other key is refused.

## Polling

`events/poll` takes `name`, optional `arguments`, `cursor`, `maxAgeMs` and `maxEvents` (1 to 100, default 50). A missing cursor returns no events and the current cursor. A cursor that is unusable (undecodable, from another library, older than the 7 days kept, or older than `maxAgeMs`) returns `truncated: true`, no events and a fresh cursor: read the work list once, then continue from that cursor. `nextPollMs` suggests the next poll.

## Streaming

`events/stream` sends `notifications/events/active`, then `event`, `heartbeat` (granted `heartbeatMs`, 5000 to 60000, default 30000) and, on a revoked pairing, `terminated`. A cancelled request stops delivery. A host that cannot hold a stream polls instead.

## Errors

| Name | Code | When |
| --- | --- | --- |
| InvalidParams | -32602 | arguments do not match the schema, or a bad `maxEvents` |
| NotFound | -32023 | an unknown event name |
| Forbidden | -32024 | the pairing is revoked or may not read work |
| ResourceExhausted | -32025 | more than 4 waiting polls for the pairing, or 8 streams in the process |
| Unsupported | -32026 | `events/subscribe` and `events/unsubscribe` (webhook delivery is not offered) |
