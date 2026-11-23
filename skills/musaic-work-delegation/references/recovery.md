# Recovery

Assume every call can fail, every connection can drop and every event can arrive twice. The rule is always the same: **list, read, then act**.

| Situation | What to do |
| --- | --- |
| You reconnect after a gap | List queued work again. Do not trust an event you missed. |
| An event cursor says `truncated` | The history you missed is gone. List the work, then continue from the fresh cursor. |
| The same event arrives twice | Ignore the second one (same `eventId`). If you did act, the claim is atomic: a second claim answers `ALREADY_CLAIMED`. |
| `ALREADY_CLAIMED` | Another agent holds it. Move to the next item. |
| `STALE_CONTEXT` | The album, the library or the request changed. Stop; list the work and read it again if it is still open. |
| `LEASE_EXPIRED` or `STALE_GENERATION` | You no longer hold the work. Do not submit. List the work to see whether it is open again. |
| `RATE_LIMITED` | The item waits for its next retry. Leave it. |
| `IDEMPOTENCY_CONFLICT` | You reused a key for different input. Use a new key for the new intent. |
| The answer to `musaic_work_complete` was lost | Repeat it with the same key and input; you get the first answer. |
| The app is not running | Stop. Musaic must be open; do not try to start it. |
| You cannot finish | `musaic_work_release`, or `musaic_work_fail` with a plain reason and `retryable` set honestly. |

## Never double-apply

Applying is the person's decision in Musaic. A result you submitted twice with the same key is stored once. A result for work you no longer hold is refused. You cannot apply anything yourself.
