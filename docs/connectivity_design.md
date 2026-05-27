# Design: SSE-driven cache freshness (issue #2)

Tracking issue: [#2 — Push more state changes through SSE; retire frontend invalidate-checklist anti-pattern](https://github.com/get2knowio/musaic/issues/2).

**Status: shipped.** The bus from issue #1 / move E (`musaic/library_events.py`) now drives frontend cache invalidation through a single SSE channel. This document is the as-built record — read it for the *why*, refer to the code for the *what*.

---

## The problem (recap from issue #2)

[Issue #1 / move E](library_events_design.md) retired the *backend* "every mutation site must remember the fan-out checklist" pattern by replacing it with an in-process event bus. A connectivity audit then surfaced the same anti-pattern living on the *frontend*: **57 ad-hoc `qc.invalidateQueries(...)` calls** scattered across mutation handlers, each one a checklist the author had to remember.

Concretely:

- `DetailSheet/index.tsx`'s apply-success handler invalidated 5 caches.
- `DetailSheet/CoverUploadDialog.tsx` invalidated the same 5 (different order, slightly different keys).
- `DetailSheet/PropTable.tsx` invalidated 4 (forgot `["tracks", id]`).
- `useChecks.ts` mutation hooks invalidated 3 each (forgot `["album-resolution", id]`).
- The fingerprint-pool was the only poller left in the app, refetching `/api/fingerprint-pool/status` every 2 s while running.

Now that the backend bus knows *every* state change, the frontend can subscribe once and let the same event taxonomy drive cache invalidation. Plus one stray poller can move to SSE for free.

## The shipped design

Three SSE channels, mounted once each at the App level:

| Channel | Hook | What it does |
|---|---|---|
| `/api/library-events/stream` | `useLibraryEventsStream` | Maps every `library_events` bus event → the TanStack Query keys it invalidates. Owns cache freshness for albums/tracks/resolution/findings/checks. |
| `/api/fingerprint-pool/stream` | `useFingerprintPoolStream` | Mirrors pool state (`idle`/`running`/`done`/…) into the cache so the titlebar pill renders live progress. Replaced 2 s polling. |
| `/api/checks/stream` | `useChecksStream` | Pre-existing — pushes the check-summary list on every run / ignore / finding change. Still drives `["checks"]` + `["findings"]` directly. |

Plus the pre-existing per-operation streams (`/api/scan/stream`, `/api/albums/{id}/apply/stream`, `/api/albums/{id}/analyze/stream`, `/api/ai/jobs/stream`) that aren't part of this sweep and continue to own their operations' progress UI.

### Backend: relay subscriber + bounded queue

`musaic/services/library_events_relay.py` registers a single subscriber on the bus at priority 400 (runs last). For each `Event` published, it:

1. Serializes the dataclass into `(event_name, json-safe payload)` via `_event_to_payload`. Tuples become lists; other primitives pass through.
2. Snapshots the per-connection queue list.
3. For each registered connection, schedules `_safe_put` on that connection's event loop via `loop.call_soon_threadsafe` — load-bearing because the background subscribers (`_post_scan_rerun`, the bulk-batch one) publish from daemon threads, and `Queue.put_nowait` isn't thread-safe in that context.

The SSE handler (`/api/library-events/stream` in `musaic/api/library.py`) creates one `asyncio.Queue(maxsize=256)` per connection, registers it via `register_connection`, yields a `ready` handshake event, then loops on `queue.get()` with a 30 s timeout (yields `: ping` on timeout, keeps connections alive through proxies). On `CancelledError` (client disconnect) it unregisters the queue so the relay stops publishing into a dead consumer.

**Backpressure**: if the queue is full when an event arrives, `_safe_put` drops the oldest entry, logs a warning, and pushes the new one. This is correct because the bus is a "something changed, refresh" signal — clients don't need every event, they need an eventually-correct cache. A client falling behind reconnects and refetches; it doesn't lose data.

**Priority 400**: the relay runs *after* the invalidation tier (100, in-process resolver memo + thumb drop) *and* the mutation tier (200, post-scan rerun). So when an SSE consumer sees an event, the backend state is already settled. This avoids the race where the frontend invalidates, refetches against not-yet-rerun state, gets stale data, and only sees the real state on the next event.

### Frontend: one subscriber, one invalidation map

`frontend/src/hooks/useLibraryEventsStream.ts` opens an `EventSource`, registers a listener per event type, and on each event invalidates the right cache keys:

```
AlbumTagsChanged(album_id)    → ["albums-snapshot"], ["tracks", album_id],
                                ["album-resolution", album_id], ["findings"],
                                ["checks"], ["check"]
AlbumCoverChanged(album_id)   → ["albums-snapshot"], ["findings"], ["checks"]
                                (cover URL is mtime-busted client-side via
                                 coverUrl() in lib/api.ts; no separate
                                 cover cache to drop)
AlbumMbAnchorSet(album_id)    → ["albums-snapshot"], ["album-resolution", id],
                                ["findings"], ["checks"], ["check"]
TrackFingerprinted(album_id)  → ["tracks", album_id], ["album-resolution", id],
                                ["findings"], ["checks"]
AlbumRenamed(old, new)        → ["albums-snapshot"], ["findings"], ["checks"],
                                ["check"]; removeQueries for both ids' tracks
                                + resolution (id changed)
AlbumDeleted(album_id)        → ["albums-snapshot"], ["findings"], ["checks"],
                                ["check"]; removeQueries for the id's tracks
                                + resolution
LibraryScanCompleted          → broad refresh (snapshot, findings, checks)
BulkBatchCompleted            → broad refresh (snapshot, findings, checks)
```

The map intentionally invalidates `["findings"]` / `["checks"]` on most events even though `useChecksStream` would push them too — the library-events stream lands one RTT earlier, and TanStack dedupes in-flight refetches so the rare overlap is invisible.

### Frontend: mutation handlers stop bookkeeping

Before this sweep, every mutation handler had a 3–5 line invalidation checklist. After:

| Handler | Before | After | Bus event(s) that cover it |
|---|---|---|---|
| `DetailSheet/index.tsx` apply-success | 5 invalidations | 0 | `AlbumTagsChanged` / `AlbumCoverChanged` from `services/apply` |
| `DetailSheet/index.tsx` clear-anchor | 1 invalidation | 0 | New: `AlbumTagsChanged` published by `clear_mb_anchor` endpoint |
| `DetailSheet/PropTable.tsx` clear-anchor | 4 invalidations | 0 | Same as above |
| `DetailSheet/CoverUploadDialog.tsx` | 5 invalidations | 0 | `AlbumCoverChanged` / `AlbumTagsChanged` from `services/apply` |
| `FilenameMismatchResolveDialog.tsx` | 3 invalidations | 0 | `AlbumRenamed` from `filename_mismatch.execute_renames` |
| `ArtistSortResolveDialog.tsx` | 3 invalidations | 0 | `AlbumTagsChanged` per album via `apply_direct` |
| `useChecks.useRunCheck` | 3 invalidations | 0 | `useChecksStream` (runner already calls `notify_changed()`) |
| `useChecks.useIgnoreFinding` | 3 invalidations | 0 | Same |
| `useChecks.useUnignoreFinding` | 3 invalidations | 0 | Same |
| `useChecks.useFixFinding` | 4 invalidations | 1 | Bus covers findings/checks; `["jobs"]` still explicit (jobs aren't on the bus) |
| `useChecks.useDeleteDuplicateFile` | 3 invalidations | 0 | `AlbumTagsChanged` / `AlbumDeleted` from the endpoint |
| `useApplyStream.updated_album` | 1 invalidation | 0 | `AlbumTagsChanged` from `services/apply` |
| `useAnalyzeStream.updated_album` / `done` | 2 invalidations | 0 | `AlbumTagsChanged` from `audio_analysis` |
| `HealthDetail` quick-fix `onDone` | 2 invalidations | 0 | `BulkBatchCompleted` |

Net: 42 manual invalidations deleted. The remaining 47 (down from the original 57) are all either:

- Inside the library-events / checks SSE subscribers themselves (the invalidation map; not mutation-handler bookkeeping).
- For caches the bus doesn't see: `["jobs"]` (own SSE channel), `["vocabulary"]` / `["album-tags"]` / `["all-tags"]` (not on the bus today), `["db-info"]` / `["db-snapshots"]` (settings page), `["filename-mismatch-plan-preview"]` / `["duplicate-cluster-preview"]` (preview-endpoint caches the dialog manages).

### `staleTime: Infinity` everywhere bus-driven

With the bus invalidating on every relevant mutation, `staleTime: 30s` / `60s` is just a race waiting to happen: refetch-on-focus can fire mid-mutation and overwrite the cache with a stale read. After this sweep:

| Hook | Before | After | Why |
|---|---|---|---|
| `useAlbumsSnapshot` | 30 s | `Infinity` | Bus invalidates on every album mutation |
| `useTracks` | 60 s | `Infinity` | `AlbumTagsChanged` + `TrackFingerprinted` cover it |
| `useAlbumResolution` | 5 min | `Infinity` | `AlbumMbAnchorSet` + `AlbumTagsChanged` cover it |
| `useVocabulary` / `useAlbumTags` | 30 s | `Infinity` | Mutation handlers write via `setQueryData` |
| `useFingerprintPoolStatus` | 2 s/30 s poll | `Infinity` | Replaced poller with `useFingerprintPoolStream` |

The remaining non-Infinity caches (`useCheck` 5 s, `useSettings` 30 s, `useDuplicateFindingCanonical` 1 day) each carry an explicit comment explaining why polling/refetch is correct in their case.

## What we explicitly didn't do

- **Consolidate the SSE channels into one.** Five concurrent connections (jobs, checks, library-events, fingerprint-pool, plus per-operation scan/apply/analyze) feels like a lot, but each has a clear scope and the per-operation ones die naturally at the end of the operation. Folding them into one would force every consumer to filter by operation id and reinvent operation-scoped state.
- **Optimistic updates anywhere new.** Existing optimistic patterns (`useVocabulary` `setQueryData`) stay; we didn't add more.
- **Cross-tab synchronization beyond what each tab's own SSE subscription provides.** Each tab opens its own EventSource, so each tab's cache is independently fresh. Good enough.
- **Replace TanStack Query.** Out of scope.

## Failure modes + their answers

- **What if a client falls behind?** Queue drops oldest, warns once per overflow burst, client gets the *next* event and refetches. Eventually-correct cache; no lost data, just one window of staleness.
- **What if the SSE channel disconnects?** `EventSource` auto-reconnects. On reconnect, the relay registers a fresh queue; events published during the gap are missed (acceptable — the user just gets one cache miss on the next mutation).
- **What if a mutation publishes the wrong event?** Frontend invalidates the wrong keys, UI shows stale data. Caught at integration time when the user clicks a button and the panel doesn't update. Tests in `test_library_events_relay.py` pin every event type's serialization shape so it can't silently drift.
- **What if two consumers process the same event concurrently?** Tan­Stack dedupes in-flight refetches by query key; the worst case is one redundant fetch.
- **What if the relay subscriber runs *before* the mutation completes?** Can't happen — the relay registers at priority 400 (last). Invalidation tier (100) and mutation tier (200) finish first.

## Test coverage

| Test file | What it pins |
|---|---|
| `tests/test_library_events_relay.py` (9 cases) | Serialization of each event type, fan-out across multiple connections, unregister stops delivery, backpressure drops oldest, cross-thread publish reaches queue, relay runs at priority 400 |
| `tests/test_fingerprint_pool.py` (4 cases, +1 new) | Existing control-plane tests + new: `changed_event()` is set after every state mutation (load-bearing for the SSE wakeup) |
| `tests/test_api_smoke.py` (4 new cases) | `/api/fingerprint-pool/status` endpoint shape, `/api/library-events/stream` + `/api/fingerprint-pool/stream` route registration |

TestClient + persistent SSE streams hang in pytest (no terminal event the server can send to cleanly close). End-to-end stream tests would be flaky; we keep them at unit-level (relay) + route-registration smoke (api_smoke) and rely on local/dev integration testing for the full path.

## Related docs

- [`library_events_design.md`](library_events_design.md) — the backend bus this builds on. Covers the recursion-guard reasoning, subscriber priorities, what's intentionally out of scope on the bus side.
