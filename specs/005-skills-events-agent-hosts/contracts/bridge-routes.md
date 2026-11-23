# Contract: new agent bridge routes

All under `/__musaic/agent/v1/`, behind the existing agent guard (loopback peer, exact Host, no Origin, JSON, 256 KiB, bearer token). Each is one operation on `musaic_app::ops::agent` and one row in `docs/operations.md`. Request/response JSON only.

| Route | Operation | Request | Response |
| --- | --- | --- | --- |
| `POST events/list` | `agent.events_list` | `{}` | `{events: [EventDescriptor]}` |
| `POST events/poll` | `agent.events_poll` | `{name, arguments?, cursor?, maxAgeMs?, maxEvents?, waitMs?}` | `PollResult` |
| `POST session/observe` | `agent.observe_host` | `HostObservation` | `{}` |
| `GET session` | `agent.session_status` | none | `{observation?, skillVersion, subscriptionActive}` |

- `waitMs` 0 to 25 000. With no events and `waitMs > 0` the operation parks until woken (delegation publish, revocation, shutdown) or the wait ends, then queries once more.
- Errors use the existing agent error envelope with new codes `EVENT_NOT_FOUND`, `EVENT_FORBIDDEN`, `EVENT_LIMIT`, `EVENT_INVALID`, mapped by the MCP adapter to the JSON-RPC codes in `events.md`.
- `ops/agent.rs` stays free of `serde_json`; typed records live in `record.rs` / `musaic-commands`.
- `observe_host` bounds and cleans strings; it never fails a session for a bad value (it stores what is valid).

## Tests

- `musaic-api`: guard applies (no token → 401; Origin → refused); inventory row per route; thin handlers.
- `musaic-app` ops tests for each operation (`ops_agent*.rs`).
