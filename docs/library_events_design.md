# Design: `library_events` (Architectural Move E)

Tracking issue: [#1 — Tech debt: systemic patterns behind recurring whack-a-mole bugs](https://github.com/get2knowio/musaic/issues/1).

**Status: shipped through Phase 3 (all phases complete).** This document was originally written as a design-only proposal; everything below is now implemented. See [What actually shipped](#what-actually-shipped) for the as-built summary and commit list. The historical design narrative is preserved for context — read it for the *why*, refer to the code (`musaic/library_events.py`, `musaic/services/event_subscribers.py`) for the *what*.

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

#### Post-Phase-1 hardening: guard also wraps `run_post_scan_checks`

Initial Phase 1 implementation put the guard only in the
`_post_scan_rerun` subscriber. Real-world test revealed a gap: the
**direct** callers of `run_post_scan_checks` (scan tail, apply's
background thread, sync fix-finding endpoint, etc.) don't go through
the subscriber — so a check publishing events from inside one of
their reruns would spawn follow-on `_post_scan_rerun` background
threads. Idempotent (the spawned reruns see no new findings to act
on), but wasteful — and ahead of Phase 2 we should not be leaning on
the dual-fire safety net.

Resolution: `run_post_scan_checks` and `run_post_scan_checks_async`
both set the same contextvar on entry and reset on exit. The
subscriber's early-exit stays as an optimization (avoids the
thread-spawn cost when the answer is already known). Both
mechanisms target the same `library_events.in_post_scan_rerun`
contextvar; either one being set covers any nested publish.

```python
# services/post_scan.py
def run_post_scan_checks(library_dir, *, album_ids=None):
    if library_events.in_post_scan_rerun.get():
        return   # already inside one — defensive against direct callers
    token = library_events.in_post_scan_rerun.set(True)
    try:
        for cls in REGISTRY:
            if cls.meta.runs_during_scan:
                asyncio.run(checks_runner.run_check(cls.meta.id, ...))
    finally:
        library_events.in_post_scan_rerun.reset(token)
```

Covered by `tests/test_post_scan_guard.py` — 6 tests including the
async variant + the publish-from-inside-check scenario that
validates the bus's `copy_context()` wrap propagates the flag into
background subscribers spawned during the rerun.

This brings the invariant fully in line with the design-review
statement: *at most one post-scan rerun is in-flight per dispatch
chain*, regardless of which entry point started it.

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

## Migration plan (as executed)

Three-phase rollout, all complete:

1. **Phase 1 — ship the bus + dual-fire.** Landed as `5fb57b8`. Bus + 5 subscribers + 26 new tests. Producers got `publish(...)` calls *alongside* their existing direct `run_post_scan_checks` calls — idempotency made the double-fire safe. Hardened with `ac7c911` (move contextvar into `run_post_scan_checks` itself so direct callers also gate against re-entry).
2. **Phase 2 — delete direct calls.** Producer-by-producer, one commit each: `b4cd01c` scan → `c9dc241` apply (+ bulk-suppression moved to subscriber) → `17ea3d9` audio_analysis + worker + resolver → `2add1c3` sync-fix + delete-duplicate → `c560e89` fingerprint_pool → `f086108` apply_filename_rename (added `_filename_mismatch_rerun` subscriber for the one non-runs_during_scan check the bus drives) → `3e26257` quick_fix_all (added `BulkBatchCompleted` event for the coalesced end-of-batch rerun). Zero direct callers of `run_post_scan_checks` remain outside `event_subscribers.py`.
3. **Phase 3 — rewrite the docstring + remove dead language.** Landed as `7889fe4`. `services/post_scan.py`'s caller-responsibility narrative replaced with a description of the event-driven flow; "Phase 1" / "dual-fire" framing stripped from `library_events.py`, `event_subscribers.py`, and `main.py`.

No surprises during the rollout — the three load-bearing recursion-guard tests gating Phase 1 (`test_in_flight_rerun_short_circuits_nested_publish`, `test_background_subscriber_inherits_in_flight_flag`, `test_concurrent_unrelated_rerun_is_not_suppressed`) all passed first try, and the subsequent producer-by-producer deletions caused no regressions across the full test suite. The optional fallback "stop at Phase 1 if Phase 2 turns up surprises" was not needed.

## Open questions (resolved)

The points the design left open got resolved during implementation:

- **Coarse vs fine events** → Coarse. `AlbumTagsChanged(fields=tuple[str, ...])` carries the changed field names as a tuple; subscribers ignore it today but the field is there for future filtering. No per-field event types added.
- **Decorator vs runtime call** → Runtime call. `publish(EventType(...))` ended up clean enough at every callsite, and avoided the generator-function tax decorators would have imposed on `apply_album_changes_iter`.
- **Events with no subscriber** → No-op silently. The bus skips when `_subscribers[event_type]` is empty (the `extend` over the type's MRO just yields nothing). Tested in `test_publish_into_void_does_not_warn_or_raise`.
- **`AlbumDeleted`** → Shipped. `cache.delete_album` publishes it; `_thumb_drop` listens. This closed a gap where deleted-album findings used to linger until the next library scan.

Two new events were added during Phase 2 that the design didn't anticipate:

- **`LibraryScanCompleted(album_count: int)`** — replaced the original taxonomy's `AlbumScanned(album_id)`. Per-album events during scan would have spawned N background subscribers (one per album); a single library-scoped event at the end is more efficient and matches what `services/scan.py` actually wants.
- **`BulkBatchCompleted()`** — fired by `api/health.py:quick_fix_all` after its bulk loop exits + `in_bulk_run` is reset. Routes to the same library-wide subscriber as `LibraryScanCompleted`. Without this, the bulk endpoint would have been the one persistent direct caller.

## Out of scope (still — and still right)

- A frontend event bus. The frontend uses TanStack Query + SSE; that stays.
- Cross-process / cross-host events. Single-process for a local-first app.
- Event sourcing. Events are notifications, not the source of truth. The DB is.

---

## What actually shipped

Final state, post-Phase-3:

- **`musaic/library_events.py`** — 8 frozen event types (`Event` → `AlbumScopedEvent` → `AlbumTagsChanged` / `AlbumCoverChanged` / `AlbumMbAnchorSet` / `TrackFingerprinted` / `AlbumDeleted`; `AlbumRenamed` (carries old+new ids); `LibraryScanCompleted`; `BulkBatchCompleted`), `register()` + `publish()` API, priority-ordered dispatch, background subscribers via `Thread(target=copy_context().run, ...)`, `in_post_scan_rerun: ContextVar[bool]` recursion guard.
- **`musaic/services/event_subscribers.py`** — 5 subscriber concerns (`_resolver_invalidate`, `_thumb_drop`, `_post_scan_rerun` + rename + library-wide variants, `_filename_mismatch_rerun`, `_findings_runner_notify`) wired via `register_all()` from the FastAPI lifespan.
- **`musaic/services/post_scan.py`** — reframed as the low-level worker `_post_scan_rerun` drives; both sync + async variants set/reset `in_post_scan_rerun` on entry/exit.
- **9 producer sites** publish events: `services/scan.py`, `services/apply.py`, `services/audio_analysis.py`, `services/worker.py`, `services/resolver.py`, `mb_lookup.py` (×2: `find_release` + `find_release_cached` backfill), `fingerprint.py`, `cache.delete_album`, `checks/filename_mismatch.execute_renames` (dir + file branches), and api endpoints (`delete_duplicate_file`, `quick_fix_all`).
- **Tests:** 36 new across `test_library_events.py` (13 — dispatch + ordering + isolation + the 3 load-bearing recursion-guard scenarios), `test_event_subscribers.py` (17 — per-subscriber invocation contracts including bulk-run suppression), `test_post_scan_guard.py` (6 — sync + async guard semantics). Total 141 passing (was 77 before the issue-1 work began).

The stale-findings bug class is now mechanically unreachable for new code paths — adding a new producer means thinking about which event to publish, not which 5 fan-out calls to remember.
