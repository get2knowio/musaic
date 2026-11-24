# Contract: agent tools (changes only)

Registry `musaic-commands/src/lib.rs` (`REGISTRY`), schemas in `tools/work.rs`. Contract version stays `"1"`: every change is additive. Tool count 21 → 23. Error codes unchanged (35). The single-item tools keep their behaviour (FR-035).

## Changed: `musaic_work_list`

Input gains an optional filter:

```json
{ "state": "queued", "kind": "metadata.identify", "batchId": "6f0c…", "cursor": null, "limit": 50 }
```

- `batchId`: string, `^[0-9a-f-]{36}$`; when present only that batch's items are listed. Unknown batch → an empty page, not an error (a batch is not a resource).
- `limit` stays 1..100, default 50. Visibility rule unchanged (queued and hydrated, or held by the caller; current epoch).

## Changed: `WorkSummary` (output of list, get, claim and every status reply)

Gains `batchId: string | null` (omitted when null). No other field changes.

## New: `musaic_work_claim_many`

CLI name `work-claim-many`. Kinds `[LibraryRead, ProposalSubmit]` (the research scope, like `musaic_work_claim`). Annotations: readOnly false, destructive false, idempotent true, openWorld false.

Input (`deny_unknown_fields`):

```json
{ "batchId": "6f0c…", "kind": "cover.research", "max": 25, "idempotencyKey": "burst-2026-11-23T10:00:00Z" }
```

| Field | Rule |
| --- | --- |
| `batchId` | optional; restricts to that batch |
| `kind` | optional |
| `max` | optional integer 1..=25, default 25 |
| `idempotencyKey` | required, `^[A-Za-z0-9._:-]{8,128}$` (as `musaic_work_claim`) |

Output:

```json
{ "claims": [ { "workId": "…", "claimGeneration": 1, "leaseToken": "…", "leaseExpiresAt": "…", "inputDigest": "…" } ] }
```

Rules:
- Eligible: current epoch, `queued`, hydrated (`base_revision` non-empty), `available_at` ≤ now, filters matched; taken in `created_at, id` order.
- One transaction; each item is claimed with a conditional update, so no item is ever held by two principals. Items another principal took first, or whose context is stale, are skipped.
- Winning fewer than `max`, or none, is a normal result (`claims: []`), never an error.
- Each claim has its own lease (5 min), attempt, result and review; one `musaic.work.updated.v1` event per claimed item, with its `batchId`.
- Replay: the same key and the same input from the same principal return the same claims (tokens re-derived, never stored). The same key with different input → `IDEMPOTENCY_CONFLICT` (existing code).
- Errors: `POLICY_DENIED` (scope), `INVALID_INPUT`, `IDEMPOTENCY_CONFLICT`.

## New: `musaic_work_renew_many`

CLI name `work-renew-many`. Kinds `[LibraryRead, ProposalSubmit]`. Annotations: readOnly false, destructive false, idempotent false, openWorld false.

Input:

```json
{ "leases": [ { "workId": "…", "claimGeneration": 1, "leaseToken": "…" } ] }
```

`leases`: 1..=25 entries, distinct `workId`.

Output:

```json
{ "renewed": [ { "workId": "…", "leaseExpiresAt": "…" } ],
  "refused": [ { "workId": "…", "code": "LEASE_EXPIRED" } ] }
```

Each lease is checked and renewed as `musaic_work_renew` does (codes `LEASE_EXPIRED`, `STALE_GENERATION`, `NOT_FOUND`); one refusal does not stop the others. Renewal writes no event (state unchanged), as today.

## Skill

`skills/musaic-work-delegation/SKILL.md` `metadata.version` 1.0.1 → **1.1.0**. `SKILL.md` and `references/work-lifecycle.md` gain: when several `requested` events with the same `batchId` arrive, list by `batchId` and claim with `musaic_work_claim_many`; renew held leases with `musaic_work_renew_many`; each item is still completed, failed or released on its own. `references/tools.md` and `references/events.md` are regenerated. Then `cargo run -p musaic-cli -- catalog --write`; CI's `catalog --check` must pass.

## Generated files that change

`docs/agent-tools.json`, `docs/agent-tools.md` (23 tools; `docs/agents.md` says "21 tools" by hand and is edited in slice 6), `docs/agent-events.json`, `docs/agent-events.md`, `skills/musaic-work-delegation/references/{tools,events}.md`, `skills/musaic-work-delegation/skill.lock.json`.
