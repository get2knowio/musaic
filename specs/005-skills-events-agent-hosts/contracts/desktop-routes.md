# Contract: new desktop routes

Behind the launch cookie and the Host/Origin check, like every `/api` route. One operation and one `docs/operations.md` row each.

| Route | Operation | Notes |
| --- | --- | --- |
| `GET /api/agent/diagnostics` | `agent.diagnostics` | Per pairing: `{pairingId, name, scopes, revoked, paired: {at}, reachable: {at?}, subscription: {active, at?}, lastClaim: {at?, workId?}, host: {clientName?, clientVersion?, profile?, tasks?, skills?, events?, observedAt?}}` |
| `GET /api/research/work` | `jobs.delegated_work` | `[{workId, albumId, kind, state, revision, holder?, lastHeartbeatAt?, leaseExpiresAt?, attempts, maxAttempts, lastError?, waitingForAgent}]` for the six listed states |

- Retry of work uses the existing `POST /api/research/{id}/decision` with `retry`.

## Events

- `AgentStateChanged(Hosts, [pairingId])` → UI invalidates `["agent","diagnostics"]`.
- `AgentStateChanged(Work, …)` already invalidates `["research"]`, which covers `["research","work"]`; diagnostics also refresh on `Work` (last claim).

All queries use `staleTime: Infinity`. No mutation handler calls `invalidateQueries`.

## UI

- **Settings → Agents**: per pairing, four labelled facts with relative times ("Paired", "Bridge reachable", "Subscription active", "Last work claimed"), host name/version "reported by the host", profile and the three extension flags. No "Ready" label anywhere. A "Process pending Musaic work" disclosure with copyable instructions when a pairing has no active subscription.
- **Review → Delegated work**: grouped by state, each row linking to the album, with holder / heartbeat / lease for held work, "Waiting for an agent" for unattended queued work, and Retry on failed and cancelled rows.

## Tests

Vitest for each component and hook; `useLibraryEventsStream` mapping for the new family; `musaic-api` inventory and thin-handler tests; ops tests.
