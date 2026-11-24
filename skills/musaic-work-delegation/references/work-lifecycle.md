# Work lifecycle

A unit of work is one question about one album: `metadata.identify`, `metadata.correct` or `cover.research`. It has a state and a revision that grows with every change.

## States

| State | Meaning | Who moves it on |
| --- | --- | --- |
| `queued` | Waiting for an agent | an agent claims it |
| `claimed` | Held by one agent under a lease | the holder reports progress, completes, fails or releases |
| `running` | Held, and progress was reported | as above |
| `awaiting_review` | A result is waiting for the person | the person approves or discards in Musaic |
| `applied` | The person approved and Musaic applied it | final |
| `discarded` | The person discarded it | final |
| `failed` | Out of attempts, or failed for good | the person may retry in Musaic |
| `cancelled` | Stopped by the person or because the album changed | the person may retry in Musaic |

You can move work only from `queued` to `claimed` (a claim) and from `claimed` or `running` to the next state. Everything after `awaiting_review` is the person's.

## The order of calls

1. `musaic_work_list` with `state` `queued` shows what is open. Page with the cursor.
2. `musaic_work_get` reads the immutable album context (artist, title, track names and ids) and the input digest. The context describes the album at the time of the request. If the album changed since, later calls answer `STALE_CONTEXT`.
3. `musaic_work_claim` with a fresh idempotency key. The answer holds `claimGeneration`, a `leaseToken` and the lease expiry. Keep all three; every later call needs them.
4. While you work, `musaic_work_renew` about once a minute and `musaic_work_progress` when the stage changes. Progress takes a sequence number that must go up; an older number is ignored.
5. End with exactly one of `musaic_work_complete`, `musaic_work_fail` or `musaic_work_release`.

## Many items from one bulk run

A person can ask Musaic to research many albums at once. Every request of that run carries the same `batchId`, both on the work summary and in the `data` of its events. A request made on its own has no `batchId`; the key is absent, not empty.

1. When several `requested` events with the same `batchId` arrive, call `musaic_work_list` with `state` `queued` and that `batchId`. An unknown batch lists nothing; it is not an error.
2. Claim with `musaic_work_claim_many`, optionally with `batchId` and `kind`, and with `max` from 1 to 25 (25 when you leave it out). Each claim in the answer is independent, with its own `claimGeneration`, lease and expiry. Winning fewer items than `max`, or none, is a normal answer: another agent may have taken them, or their context may be stale. Repeating the call with the same idempotency key and the same input returns the same claims.
3. Renew with `musaic_work_renew_many`, listing up to 25 of the leases you hold, each `workId` once. The answer lists each lease under `renewed` or under `refused` with its code (`LEASE_EXPIRED`, `STALE_GENERATION`, `STALE_CONTEXT` or `NOT_FOUND`; `STALE_CONTEXT` means the album changed, its folder is gone or the library was switched). One refusal does not stop the others. A refused lease is no longer yours: stop working on that item and do not retry the renewal blindly; list the work again if you want to know what happened.
4. Finish each item on its own with `musaic_work_complete`, `musaic_work_fail` or `musaic_work_release`. There is no batch result and no call that finishes several items.

The single-item tools keep working for items of a batch. The event payload schema may gain optional fields such as `batchId`: read it from `events/list` each session rather than validating against a cached copy.

## Leases

A claim lasts five minutes from the claim or the last renewal. When it expires without a renewal the work returns to the queue and your token stops working: calls with it answer `LEASE_EXPIRED` or `STALE_GENERATION`. Do not try to revive it; list the work again.

## Keys

The idempotency key of a claim and of a completion is yours. Use a new key for a new intent and the same key to repeat the same request after a lost answer. Reusing a key with different input answers `IDEMPOTENCY_CONFLICT`.

## Limits

An item has a small number of attempts. A retryable failure puts it back in the queue after a short wait; once the attempts are spent it becomes `failed` and waits for the person.
