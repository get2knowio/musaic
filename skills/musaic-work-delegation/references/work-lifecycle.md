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

## Leases

A claim lasts five minutes from the claim or the last renewal. When it expires without a renewal the work returns to the queue and your token stops working: calls with it answer `LEASE_EXPIRED` or `STALE_GENERATION`. Do not try to revive it; list the work again.

## Keys

The idempotency key of a claim and of a completion is yours. Use a new key for a new intent and the same key to repeat the same request after a lost answer. Reusing a key with different input answers `IDEMPOTENCY_CONFLICT`.

## Limits

An item has a small number of attempts. A retryable failure puts it back in the queue after a short wait; once the attempts are spent it becomes `failed` and waits for the person.
