# Design: `library_events` (Architectural Move E)

Tracking issue: [#1 — Tech debt: systemic patterns behind recurring whack-a-mole bugs](https://github.com/get2knowio/musaic/issues/1).

Status: **design only** — no implementation in this commit. This document scopes the change, captures the trade-offs, and is the gate before any code lands.

---

## The problem (recap from issue #1)

Today every state-mutating helper expects its caller to fan-out:

- fire `run_post_scan_checks` (so satisfied findings auto-resolve),
- invalidate the right React Query cache keys (so the UI refreshes),
- drop the resolver memo for the album (so the next read recomputes),
- emit the `jobs_changed` SSE event (so other open tabs refresh),
- sometimes re-scan the album row (so denormalized fields update).

Different callers remember different subsets of that list. We've fixed at least five "satisfied finding lingers" bugs this way (synchronous fix-finding endpoint, `fingerprint_pool.warm_library`, `audio_analysis.analyze_album_iter`, resolver side-effect path, filename-mismatch apply endpoint) and every new endpoint is a fresh opportunity to forget.

`services/post_scan.py`'s module docstring tries to enumerate the rule; we keep finding callers that violated it anyway. Caller-responsibility for cross-cutting concerns is the wrong default — it scales linearly with the number of endpoints, not with the number of concerns.

## The proposal

A small `musaic/library_events.py` module that publishes typed events at the moment of mutation. Subscribers register at app startup and handle one concern each. Callers publish once; the right things happen.

```python
# Producer
library_events.publish(AlbumTagsChanged(album_id=aid, fields=["title", "year"]))

# Subscriber (registered once at app startup)
@library_events.on(AlbumTagsChanged)
def _rerun_tag_checks(ev: AlbumTagsChanged) -> None:
    run_post_scan_checks(library_path, scope_album_ids=[ev.album_id])
```

The dispatch is in-process and synchronous by default with an option for fire-and-forget background threading; we are not introducing a message bus, a queue, or cross-process delivery. The "events" are domain-language hooks, not infrastructure.

## Event taxonomy (first cut)

The smallest set that covers every existing post-scan caller:

| Event | Producer(s) today | Replaces |
|---|---|---|
| `AlbumScanned(album_id, library_path)` | `services.scan._run_scan` per album, `services.apply.apply_album_changes_iter` after rescan | one post-scan tail at end of scan loop |
| `AlbumTagsChanged(album_id, fields)` | `services.apply.apply_album_changes_iter`, `audio_analysis.analyze_album_iter` | post-scan trigger in those paths |
| `AlbumCoverChanged(album_id)` | `services.apply.apply_album_changes_iter` (when `cover_url` was applied) | (currently bundled into apply's post-scan) |
| `AlbumRenamed(old_id, new_id)` | `filename_mismatch.execute_renames` dir-rename branch | scoped post-scan on the new id |
| `AlbumMbAnchorSet(album_id, source)` | `mb_lookup.find_release`/`find_release_cached` (side effect), `services.worker._persist_ai_mbid`, `services.resolver` Tier 0b/0c writes | resolver's "did the anchor change?" hack + worker's explicit call |
| `TrackFingerprinted(album_id, filenames)` | `fingerprint.fingerprint_album`, batched at the album level | `fingerprint_pool.warm_library`'s post-scan + the per-call ones inside `unidentified-album` / `track-gaps` quick-fixes |
| `AlbumDeleted(album_id)` | `cache.delete_album` (today inside scan diff) | (no current post-scan; would let us mark findings on deleted albums as resolved deterministically) |

Two principles that fall out of the taxonomy:

1. **Events name the domain change, not the desired side effect.** "Album tags changed" is forever; "rerun post-scan checks" is one of N possible reactions. Naming after the change keeps the bus stable when new subscribers show up.
2. **Events carry the minimum identity, not the full payload.** `AlbumTagsChanged(album_id, fields)` not `AlbumTagsChanged(album_id, new_tags_dict)`. Payloads stamp time; references re-read. This is the same principle that drove Move D — the bus inherits it.

## Subscriber catalog (first cut)

What every existing endpoint manually invokes today, mapped to subscribers it would delegate to:

| Subscriber | Concern | Reacts to |
|---|---|---|
| `_post_scan_rerun` | Scope-aware `run_post_scan_checks`; scope is the event's `album_id` when present, else library-wide | All album-scoped events; `AlbumScanned` is library-wide |
| `_resolver_invalidate` | `services.resolver.invalidate(album_id)` | `AlbumTagsChanged`, `AlbumMbAnchorSet`, `TrackFingerprinted` |
| `_thumb_drop` | Drop the album's pre-extracted thumbnail | `AlbumCoverChanged`, `AlbumDeleted`, `AlbumRenamed` (new id needs a fresh extract on next load) |
| `_sse_state_notify` | Emit `jobs_changed` / `state` SSE events so connected clients refetch | Every event |
| `_findings_runner_notify` | Wake the `checks_runner.changed_event()` so the `/api/checks/stream` socket re-renders | After `_post_scan_rerun` lands (chained, not direct) |

`apply_album_changes_iter` shrinks dramatically: its current trailing 30 lines collapse to two `publish` calls (`AlbumTagsChanged` and optionally `AlbumCoverChanged`). The thread it spawns to call `run_post_scan_checks` becomes the subscriber's problem, not the producer's.

## Cross-cutting design decisions

### Dispatch model

**Decision: synchronous in-process pub/sub, with subscribers marked `background=True` running in a daemon thread.**

Why synchronous default: subscribers that don't do I/O (cache invalidation, SSE notify) should run inline so the producer's transaction boundary doesn't have to wait for them. We already pay for those inline today.

Why background-thread option: `run_post_scan_checks` can take seconds (medium-cost checks); we already background-thread it from `apply.py`. Letting a subscriber declare itself `background=True` keeps the producer's HTTP request snappy without leaking thread-spawning into producer code. Same posture as `apply.py:225-228` today, just factored out.

Why not asyncio.create_task: most producers are synchronous (apply.py, worker.py, resolver.py); forcing an event loop just for dispatch would push complexity onto them. Async callers (`api/health.py` SSE handlers) can `await` an explicit `library_events.publish_async(...)` variant when they need ordering guarantees.

### Subscriber isolation

A failing subscriber must not block the others. The dispatcher logs and continues on any exception — same posture as the existing `try/except: log.exception("...failed (continuing)")` blocks scattered across producers today. Failed subscribers do not roll back the event; events are facts about what already happened, not commands.

### Recursion guard

A `tags-out-of-sync` check that calls `resolve_album` from `run()` could trigger an `AlbumMbAnchorSet` from inside Tier 0b's side-effect write. The `_post_scan_rerun` subscriber would then call back into the check runner — and we'd re-enter the same check, potentially indefinitely.

**Decision: a `_dispatching` contextvar set by the dispatcher.** While set, subscribers may inspect it to decide whether to short-circuit. The `_post_scan_rerun` subscriber is the only one that needs the guard today; it checks and bails when already inside a check run. Cheaper and clearer than trying to detect cycles at the bus level.

#### Second design-review pass on the guard

The first-pass decision is roughly right but the framing was imprecise. Tightening below before any code lands.

**Sharpen the invariant.** "Inside a check run" is the wrong scope — that would suppress legitimate post-scan reruns triggered from outside any check (e.g. an apply endpoint firing while a long-running checks-stream subscriber is rendering findings in a different request). The actual invariant is narrower:

> *At most one post-scan rerun is in-flight per library_path within a single dispatch chain.*

The guard prevents a post-scan rerun from triggering another post-scan rerun via the events emitted by the checks it runs. It must not prevent unrelated, concurrent, externally-triggered reruns.

So the contextvar should be specifically `_in_post_scan_rerun: bool`, owned by the `_post_scan_rerun` subscriber, not a generic `_dispatching` flag at the bus level. The bus stays dumb; the subscriber owns its own re-entry semantics. This also keeps the guard testable in isolation without simulating the whole dispatcher.

**Contextvar propagation across threads is a real risk.** `background=True` subscribers run in a daemon thread. Python contextvars do NOT auto-propagate to new threads (unlike asyncio tasks). If the bus does `Thread(target=subscriber, ...)` naively, the background `_post_scan_rerun` runs with `_in_post_scan_rerun=False` even when the publisher's frame had it set — guard silently fails open.

The fix is mechanical but easy to forget: the dispatcher must wrap every background subscriber in `contextvars.copy_context().run(subscriber, ev)`. That's a bus-level responsibility, not a subscriber-level one — get it wrong once and every subscriber's contextvar semantics break. Worth a focused unit test:

```python
def test_background_subscriber_inherits_contextvar_from_publisher():
    flag = ContextVar("flag", default=False)
    seen: list[bool] = []
    bus.register(MyEvent, lambda ev: seen.append(flag.get()), background=True)
    flag.set(True)
    bus.publish(MyEvent())
    wait_for_background_subscribers()
    assert seen == [True]
```

**Asymmetric failure cost.** If the guard misfires false-positive (suppresses a rerun that should run), the user's finding stays stale — exactly the bug class E is supposed to retire. If it misfires false-negative (lets a recursive rerun through), the worst case is wasted CPU on an idempotent op. **Default-allow is correct; default-suppress is dangerous.** Keep the guard scoped tightly.

**Idempotency is the real safety net, not the guard.** `run_post_scan_checks` is idempotent — a second run over the same state finds the same findings and resolves the same satisfied ones. Even if the guard fails open, the recursion terminates after one extra round because Tier 0a cache-hits replace Tier 0b/0c side-effect writes on the second pass; nothing new gets published. The guard is an optimization, not a correctness mechanism. Stating this explicitly affects how we test it (asserting the *count* of reruns is wrong; asserting *terminates* is right).

**Scope coalescing is a separate concern from recursion.** `services.scan._run_scan` could plausibly publish `AlbumScanned` per album — N=5000 background `_post_scan_rerun` threads would pile up even with the guard correctly preventing nesting. That's a thread-pool / batching concern, not a recursion concern.

Resolution: scan does *not* publish per-album events. It publishes one `LibraryScanCompleted` at the end and lets `_post_scan_rerun` do one library-wide pass. The per-album events are for one-off mutations (apply, AI worker, manual fix). Batch operations emit batch-level events. This boundary needs to be enforced by convention — there's no mechanical way to stop a future producer from publishing N events in a loop — but it's at least documentable in the event taxonomy.

For the within-window case (bulk quick-fix-all → 80 fixes → 80 `AlbumTagsChanged` events), a debounce inside `_post_scan_rerun` is the right answer: collect `album_id`s into a pending set, fire one coalesced rerun after a short idle window (250ms). Tag this as Phase 2 work — not Phase 1 — since it's an optimization, not a correctness requirement.

**Edge cases the guard must handle (acceptance criteria):**

1. *Sync publisher, sync subscriber, no recursion.* Guard set → subscriber runs → guard reset. Trivial path.
2. *Sync publisher inside `_post_scan_rerun` re-publishes.* Guard already set → subscriber sees it → returns immediately. The desired short-circuit.
3. *Background subscriber from `_post_scan_rerun`'s thread re-publishes.* Background thread inherits the `True` flag via `copy_context()` → subscriber sees it → returns. Same outcome, different propagation path.
4. *Concurrent unrelated rerun from a different request.* Different thread, never entered `_post_scan_rerun`'s frame → contextvar default `False` → rerun proceeds normally. Critical false-positive check.
5. *Subscriber raises while guard is set.* `finally: token.reset()` must run or the contextvar leaks into the next event on the same thread. Standard try/finally; easy to get wrong; covered by test.
6. *Async publisher via `publish_async`.* Same contextvar semantics in async land — asyncio tasks do inherit the parent's context. Should be free as long as we don't `loop.run_in_executor` without `copy_context()`.

**What the guard does NOT need to handle:**

- Cross-event causality tracing. We're not building a span graph.
- Permanent suppression. The guard is per-dispatch-chain, not per-library.
- Cross-process coordination. Single-process app; SQLite has its own locking.
- Detecting unrelated check runs. The `/api/checks/{id}/run` endpoint can run any check at any time; that's fine and orthogonal.

**Implementation sketch (Phase 1 only):**

```python
# library_events.py
_in_post_scan_rerun: ContextVar[bool] = ContextVar("_in_post_scan_rerun", default=False)

def _post_scan_rerun_subscriber(ev: AlbumScopedEvent) -> None:
    if _in_post_scan_rerun.get():
        log.debug("post_scan_rerun: skipped (already inside one)")
        return
    token = _in_post_scan_rerun.set(True)
    try:
        run_post_scan_checks(
            library_path,
            scope_album_ids=[ev.album_id] if ev.album_id else None,
        )
    finally:
        _in_post_scan_rerun.reset(token)

# In the dispatcher:
def _dispatch_background(subscriber, ev):
    ctx = contextvars.copy_context()
    Thread(target=ctx.run, args=(subscriber, ev), daemon=True).start()
```

Three tests are load-bearing for Phase 1:
- `test_in_flight_rerun_short_circuits_nested_publish` — scenario 2 above.
- `test_background_subscriber_inherits_in_flight_flag` — scenario 3.
- `test_concurrent_unrelated_rerun_is_not_suppressed` — scenario 4 — this is the false-positive guard.

If any of those three turn red during Phase 1, the guard's framing is wrong and we revisit before Phase 2.

### Scoping

Album-scoped events carry `album_id`; the `_post_scan_rerun` subscriber translates that into a `scope_kind="albums"` rerun (the runner already supports this — that machinery landed in commit `15d49a0`). Library-wide events (`AlbumScanned`) fan out to a whole-library rerun. The runner's existing `scope_album_ids` auto-resolve scoping prevents per-album reruns from over-resolving unrelated findings.

### Ordering

Subscribers register with an integer priority (lower = earlier). Defaults: invalidation subscribers (`_resolver_invalidate`, `_thumb_drop`) at 100, mutation subscribers (`_post_scan_rerun`) at 200, notification subscribers (`_sse_state_notify`, `_findings_runner_notify`) at 300. This matters because a UI refetch (`_sse_state_notify`) that lands before the resolver cache is invalidated would serve stale data.

We're not promising cross-event ordering — events fired in sequence are dispatched in sequence, but interleaving with other producers' events isn't guaranteed. Producers that need atomicity can publish a single coarser event (`AlbumChanged` with a flag set) rather than two finer ones.

### Failure semantics

- Producer publishes → returns immediately (sync subscribers run, background subscribers spawn).
- Sync subscriber raises → logged, swallowed, next subscriber runs.
- Background subscriber raises → logged inside the thread, never surfaces.
- No retry, no dead-letter queue. We are not building durability — this is a process-local hook.

## What this retires

When E is fully rolled out and the direct `run_post_scan_checks` callers have been deleted:

- `services/post_scan.py`'s 70-line "caller responsibility" module docstring (or rather, it becomes "this is how the bus's post-scan subscriber works"). No more enumeration of "which callers must remember what."
- `apply.py`'s background-thread `run_post_scan_checks` spawn lives in the bus subscriber instead.
- `worker.py`'s "I just wrote an anchor, also fire post-scan" two-call dance becomes one `publish(AlbumMbAnchorSet(...))`.
- `resolver.py`'s "snapshot pre-anchor and compare after" hack (lines 453-484) becomes one `publish` at the Tier 0b/0c write site.
- Every "I forgot the fan-out" bug class — they can't happen if the producer's only job is `publish`.

## What this does NOT do

- It doesn't replace SSE. The bus is server-internal; SSE remains the wire to the frontend. The `_sse_state_notify` subscriber is just where the existing emit lives now.
- It doesn't address persistence. Subscribers that need durable replay (none today) would have to layer their own log.
- It doesn't change the resolver's lazy-by-default posture. The bus reacts to anchor *writes*; resolve_album still runs on demand.
- It doesn't introduce a third-party plugin system. Subscribers are hard-coded modules registered at app startup, named in `main.py` lifespan.

## Migration plan

Three-phase rollout to avoid a big-bang switch:

1. **Phase 1 — ship the bus + dual-fire.** Add `library_events.py` and the subscribers. Producers publish events but keep their existing direct `run_post_scan_checks` calls. Verify subscribers fire correctly + idempotently (double rerun is wasted work, not a bug). Add `tests/test_library_events.py` covering dispatch, ordering, isolation, recursion guard.
2. **Phase 2 — delete direct calls.** Producer by producer, remove the direct `run_post_scan_checks` (and cache.invalidate, thumb drop, etc.) calls now that the bus covers them. One PR per producer keeps the blast radius small. Each PR runs `tests/test_*` to confirm no regression.
3. **Phase 3 — update the docstring + remove the dead helpers.** Rewrite `services/post_scan.py`'s module docstring to describe the bus instead of the caller-responsibility rule. Anything that's now dead code goes.

We can stop at Phase 1 if Phase 2 turns up surprises — Phase 1 is strictly additive and reversible.

## Open questions

The decisions above are the leading defaults; these are the points where committing without more thought would be premature.

- **Coarse vs fine events.** Is `AlbumTagsChanged(fields=[...])` the right shape, or should it be a separate event per field (`AlbumTitleChanged`, etc.)? Coarse is cheaper to publish; fine lets subscribers filter without inspecting a payload. Lean: coarse, because every subscriber today wants the whole change anyway.
- **Should publish be a typed decorator or a runtime call?** Decorator (`@library_events.published("album.tags-changed")` on the mutation function) is prettier but harder to call from inside a generator. Runtime `publish()` is uglier but works everywhere. Lean: runtime call.
- **Should events queue when there's no subscriber for a given type?** No-op or warn? Lean: no-op silently in prod, log at debug. Tests can assert via a fixture.
- **`AlbumDeleted` — do we need it today?** Nothing currently consumes it, but having the event would let `_post_scan_rerun` auto-resolve findings on deleted albums (we currently rely on the next library scan picking that up). Cheap to ship, useful enough.

## Out of scope (explicitly)

- A frontend event bus. The frontend already uses TanStack Query + SSE; that stays.
- Cross-process / cross-host events. Single-process is enough for a local-first app.
- Event sourcing. Events are notifications, not the source of truth. The DB is.

---

**Recommendation:** land Phase 1 as a single PR after another design review pass. Phase 2 + Phase 3 should NOT be bundled with Phase 1 — keep the cutover incremental so each deletion of a direct `run_post_scan_checks` call can be verified independently.

The biggest risk is the recursion guard — getting it wrong would silently skip post-scan reruns in legitimate cases. A focused test fixture (`tests/test_library_events.py::test_subscriber_can_publish_without_reentry`) needs to exercise it before Phase 2 starts.
