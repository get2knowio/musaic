# Data Model: Operations Usable Without HTTP

**Spec**: [spec.md](spec.md) | **Research**: [research.md](research.md)

This feature adds no stored data and changes no schema. The "model" is the set of types
the operations surface is made of. Names are proposals; the slices may adjust them, the
rules may not.

## Ops (the handle)

- Obtained from an `App`: `app.ops()`. Cheap to clone, holds the `App`.
- One accessor per group: `system()`, `library()`, `scan()`, `settings()`, `database()`,
  `tags()`, `analyze()`, `apply()`, `checks()`, `fixes()`, `jobs()`.
- Holds no state of its own. Everything shared (running scan, gate, feeds) lives in `App`,
  so two faces holding two `Ops` see one application (FR-030).

## Operation

| Attribute | Rule |
|---|---|
| Inputs | Typed values. Ids are newtypes where a mix-up is possible (`AlbumId`, `FindingId`, `CheckId`, `JobId`, `SnapshotName`). Optional inputs are `Option`, with defaults applied inside the operation. |
| Result | A named struct or enum, or `()`. Open-ended documents only through the record newtypes below. |
| Error | `OpError`. |
| Preconditions | Performed inside, in the order recorded for its route in the inventory: meaning validation, guard, library configured, target exists, target not Missing. |
| Effects | Publishes its event(s) through the bus; sends its follow-up notification(s). |
| Threading | Blocking. Callable from any thread; carries no runtime. |

**Lifecycle of a mutating operation**

```text
called → validate meaning ──fail──▶ Err(InvalidInput)            nothing changed
       → take guard       ──busy──▶ Err(Busy)                    nothing changed
       → check library / target ──▶ Err(NoLibrary | NotFound | Conflict)
       → act (services, under the guard)
       → publish event(s)
       → follow-up notification(s)
       → release guard → Ok(result)
```

The first two steps are in the order today's handler has them; a few routes differ
(the scan stream takes the guard before the library check; a list with `refresh` takes it
only then). The inventory is the authority per route.

## OpError

See [contracts/errors.md](contracts/errors.md) for the variants and the HTTP mapping.

- Every variant carries `message: String`, a plain sentence.
- `InvalidInput` also carries `field: String`.
- `NotFound` also carries `what: Subject` (`Album`, `Track`, `Finding`, `Check`, `Job`,
  `Snapshot`, `Cover`, …) and the id asked for.
- `Busy` also carries the `Busy` reason from the gate (restore running / work running).
- `Internal` carries the cause for the log; a face shows a generic message.
- No variant carries a status code. No two variants need the same message to be told
  apart.

## Record newtypes (the allow-list)

Documents whose shape is owned by stored data, held in key order as stored.

| Type | Holds | Typed accessors (examples) |
|---|---|---|
| `AlbumRecord` | one album as the library reports it | `id()`, `path()`, `missing_since()`, `display_artist()` (through `musaic-core`'s accessor, rule 4) |
| `TrackRecord` | one track's row and tags | `file_name()`, `track_number()`, `tags()` |
| `TagSet` | a tag-name → values map | `get(name)`, `iter()` |
| `SettingsRecord` | the settings document | `get(name)`, `iter()`; never a secret value |

Rules:
- Defined in `ops::record`, nowhere else. Adding one is a reviewed change to the
  allow-list in `ops_boundary.rs`.
- Each exposes ordered iteration for a renderer and typed accessors for everyone else. A
  face other than HTTP should never need the iteration.

## Progress events

One enum per streamed operation; see [contracts/events.md](contracts/events.md).

| Operation | Shape | Terminal |
|---|---|---|
| Scan | start + subscribe (replay from first event) | exactly one of `Done`, `Aborted`, `Cancelled`, `Error` |
| Apply | run with sink | exactly one terminal event |
| Analyze | run with sink | exactly one terminal event |
| Quick-fix-all | run with sink | `Done` |
| Multi-disc conversion (if confirmed) | run with sink | exactly one terminal event |
| Check runs | feed | none (a feed) |
| AI jobs | feed | none |
| Fingerprint pool | feed | none |
| Library change events | feed (bounded, drop-oldest) | none |

State transitions for a scan are the existing ones (`idle`, `running`, `cancelling`, then
`done` / `aborted` / `cancelled` / `error`); this feature does not change them.

## Listening

`Listening::Yes | Listening::Gone`, returned by a sink. `Gone` means "stop calling me";
the run continues to its end and still publishes its bus events.

## Feed\<T\>

- `next()` (blocking, with timeout) and `changed()` (async) yield `T`.
- First value: the current state. Then one per change; several changes between two reads
  may arrive as one value, except on the library-events feed, which queues events and
  drops the oldest when full.
- Ends when the application begins shutting down.

## Route inventory row

Kept in `docs/operations.md`, checked against `route_table()`.

| Column | Values |
|---|---|
| Method, path | as registered |
| Group | system, library, settings, tags, analyze, apply, checks, fixes, jobs, or "HTTP face only" |
| Classification | thin adapter / carries behaviour |
| What moves | any of: validation, guard, ordering, publish, notification, stream assembly, body construction; or "nothing" |
| Operation | `ops().<group>().<name>` |
| Result | struct name, or record newtype |
| Errors | the `OpError` variants it can return |
| Streamed | no / run / scan / feed |
| Precondition order | only where it differs from the default |
