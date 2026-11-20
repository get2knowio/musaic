# Contract: The Operations Surface

What a face may rely on, and what a route handler is allowed to be. The per-route list is
the inventory in `docs/operations.md` (created in slice 0); this file is the rules.

## For a face

1. Build or receive an `App`. Call `app.ops()`.
2. Call one operation per thing you want done. Do not take the gate, publish on the bus,
   or call the job-queue notification yourself; you cannot need to.
3. Run operations on a thread you can block. Wait on feeds either blocking or async.
4. Handle `OpError` by variant ([errors.md](errors.md)).
5. For progress, pass a sink or hold a feed ([events.md](events.md)).

## Groups and what each covers

| Group | Covers (routes file today) |
|---|---|
| `system()` | health, fingerprint pool status and feed, library change events feed (`system.rs`) |
| `library()` | library folder get/set, album list (with refresh), album, tracks, cover bytes, remove from library, clear MusicBrainz anchor, resolution (`library.rs`) |
| `scan()` | status, start, subscribe, rescan, cancel (`library.rs`) |
| `settings()` | settings read and patch, AcoustID prompt and dismissal, UI preferences (`settings.rs`) |
| `database()` | info, backup, restore from a file, snapshots: list, create, delete, restore (`settings.rs`) |
| `tags()` | tag reads and writes, vocabulary (`tags.rs`) |
| `analyze()` | BPM / ReplayGain analysis run (`analyze.rs`) |
| `apply()` | apply corrections run, clear, cover upload/download/preview (`apply.rs`, `preview.rs`) |
| `checks()` | list checks and findings, run a check, ignore / unignore, sweep status, previews (plan, cluster, conversion), check-runs feed (`checks.rs`) |
| `fixes()` | fix one finding, quick-fix-all run, filename rename apply, duplicate file delete, artist-sort apply, multi-disc conversion (`fixes.rs`) |
| `jobs()` | list, enqueue, cancel, delete, resume, providers, jobs feed (`jobs.rs`) |

The exact operation list per group is whatever the inventory maps the group's routes to;
the count of operations equals the count of operation-bearing routes.

## What every operation guarantees

| Guarantee | Rule it serves |
|---|---|
| Takes the work guard itself when it writes, and holds it until its work ends. | restore gate |
| Refuses, changing nothing, when its input's meaning is invalid, the app is busy, no library is configured, or the target is absent. | FR-008, FR-010 |
| Skips or refuses a Missing album when it would touch the album's files. | rule 6 |
| Publishes the event for what it changed, at the moment it changed; never calls a rerun or other subscriber directly. | rule 1 |
| Re-derives a plan or preview from current disk and database state; uses a finding's stored detail for labels only. | rule 2 |
| Sends its follow-up notification (the job queue wake after an enqueue, the settings change). | FR-008 |
| Returns no secret value. | security posture |

## What a route handler is

```text
parse (lax: path, query, body → typed input, or 422)
  → blocking(|| ops.<group>().<operation>(input))
  → render (typed result → ordered body; OpError → status + detail)
```

A handler may: parse; pick HTTP-only settings off `AppState` (upload caps, ping interval);
receive an upload to a temp file or frame a download; open an SSE channel and forward
rendered events.

A handler may not: take a work guard; publish an event; call `ai_jobs_changed()` or any
notification; call a service for something an operation covers; decide a precondition's
order.

## What stays outside the surface (HTTP face only)

Static UI files, the launch-cookie gate, the Host / Origin check, Starlette-style routing
and its aliases, `lax` parsing, body size limits as HTTP 413s on the request body, response
headers (ETag, cache control), SSE framing and pings.

## Stability

Internal to this workspace. No versioning, no compatibility promise (clarification 4).
