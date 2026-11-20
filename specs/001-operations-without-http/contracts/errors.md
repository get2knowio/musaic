# Contract: Operation Errors

The one error type of the operations surface, and the one place it becomes HTTP.

## Variants

| Variant | Means | Carries (besides `message`) | HTTP status (in `musaic-api` only) |
|---|---|---|---|
| `InvalidInput` | Reserved for a value whose meaning is refused where today's route answers 422. No operation raises it yet. | `field` | 422 |
| `Rejected` | The request is understood but refused: today's 400s, whether the content (an upload that is not an image, an empty file) or the meaning (unknown tag name, unknown preference, a scope that is neither 'all' nor 'albums'). | `details` (optional) | 400 |
| `NotFound` | The album, track, finding, check, job, snapshot or cover asked for does not exist. | `what`, `id` | 404 |
| `Busy` | A restore is running, or work is running and a restore was asked for. | the gate's reason | 409 |
| `NoLibrary` | No library folder is configured, or the configured one is not there. | which of the two | 409 |
| `Conflict` | The current state forbids it (album is Missing, album already has an active job where that is a refusal, a merge that would leave rows behind). | `details` (optional) | 409 |
| `Gone` | The thing existed and no longer does. | | 410 |
| `TooLarge` | An input exceeds a size the operation enforces. | the limit | 413 |
| `Upstream` | MusicBrainz, AcoustID or the Cover Art Archive failed or refused. | which service | 502 |
| `Unavailable` | A needed part is not ready or not configured (offline, no key). | | 503 |
| `Internal` | Anything unexpected. | the cause, for the log | 500 |
| `Failed` | A failure whose cause today's route puts in the body (the album lookup of the library routes, a conversion record that could not be written). Added during the build. | `cause`, `details` (optional) | 500 |

The 400 / 410 / 413 / 502 / 503 rows were confirmed in slice 0 (T004) by reading every
raise site in `musaic-services/src` and `musaic-api/src`:

- **400**: apply (unsupported file type, empty file), settings patch (invalid value,
  unknown preference), checks (scope, unknown run argument), job refusals (unknown job type,
  bad action, bulk without status). All are `Rejected`; today no meaning validation answers
  422, so `InvalidInput` (422) has no raise site and is kept only for a route that gains one.
- **410**: the only raise site is the launch-link gate (`launch.rs`), which is the HTTP face
  and not an operation. `Gone` stays as the variant for an operation that needs it.
- **413**: cover upload size (`apply.rs`, and the body cap in the HTTP face). `TooLarge`; the
  request-body cap stays an HTTP-face concern.
- **502**: cover preview fetch failure. `Upstream`.
- **503**: the keyring being unavailable on a settings patch. `Unavailable`.

No variant needed two statuses; none was renamed.

## Details

Some errors carry more than a sentence: a filename rename, multi-disc conversion or abandon
refusal carries a reason code, a user message, blockers and a fresh plan digest; a job
enqueue or retry conflict carries the existing job. They travel as the optional typed
`details: Option<Details>` on `Rejected` and `Conflict` (never a JSON value):

- `Details::Refusal(Refusal { reason_code, user_message, blockers: Vec<Blocker { code, message }>, digest: Option<String> })`
- `Details::ExistingJob { job_id, status }`

The HTTP face renders them as the extra top-level body keys the route has today; that
rendering is added by the slice that converts such a route.

## Rules

1. **The status is a function of the variant.** The mapping is one `match` in
   `musaic-api`. If a variant would need two statuses, split the variant.
2. **`message` is what today's `detail` string is.** The HTTP face renders
   `{"detail": message}` (and the plain-text `Internal Server Error` for `Internal`, with
   the cause written to the log as now).
3. **Wire-level 422s are not `OpError`s.** A request that cannot be parsed into the
   operation's typed input is refused by `lax` in `musaic-api`, with today's structured
   detail list, before any operation is called.
4. **Order**: an operation reports `InvalidInput` before `Busy` (spec, story 2,
   scenario 2) unless the inventory records otherwise for that route. The built
   operations follow today's routes (several take the guard first); each deviation is
   listed in `docs/operations.md`.
5. **No secret in a message.** As today, a key value never appears in an error or the log.
6. **`From` conversions**: `gate::Busy`, `musaic_db::Error` and `rusqlite::Error` convert
   into `OpError` (`Busy`, `Internal`, `Internal`), so `?` works in services and
   operations. `From<OpError> for ApiError` exists in `musaic-api` and nowhere else.

## After the migration

`HttpError` is deleted (T074): `grep -rn HttpError rust/crates --include='*.rs'` finds nothing; the word list of
`ops_boundary.rs` spells it in two parts, so the ban stays effective.
