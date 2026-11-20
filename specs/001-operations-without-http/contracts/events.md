# Contract: Progress Events and Feeds

What a caller of a long-running operation may rely on. The sequences are today's; the
source for each is named so the slice that converts it reads the real thing.

## Runs (the caller's own operation)

```text
ops.<group>().<run>(input, &mut |event| -> Listening) -> Result<Outcome, OpError>
```

- A refusal (`InvalidInput`, `Busy`, `NotFound`, `NoLibrary`) is returned **before** the
  sink is called once. Over HTTP that is the status-code response instead of a stream.
- After the first event, the result is `Ok` and the outcome is also the terminal event.
- Events arrive in order, on the calling thread.
- `Listening::Gone` stops the calls. The run continues, finishes the item in hand and the
  rest, and still publishes its bus events (`BulkBatchCompleted` for a bulk run).
- The work guard is held from before the first event until after the terminal one.

| Run | Sequence | Source today |
|---|---|---|
| Apply | per-step progress items, then `updated_album` or an error item | `musaic-services/src/apply.rs`, `routes/apply.rs` |
| Analyze | per-track items, then a terminal item | `musaic-services/src/analyze.rs`, `routes/analyze.rs` |
| Quick-fix-all | `start`, one `progress` per open finding, `done` | `check_fixes::quick_fix_all`, `routes/fixes.rs` |
| Multi-disc conversion (confirmed, `docs/operations.md`: the ninth streamed operation; one route, streamed only when the request asks for `text/event-stream`, else one JSON answer) | `progress {step, done, total}` (droppable when the listener is slow), then exactly one of `result` or `refused` (a refusal carries its status and body) | `routes/fixes.rs` `convert_multi_disc` |

## Scan (one operation, any number of observers)

```text
ops.scan().start(force)      -> Result<ScanState, OpError>
ops.scan().subscribe(force)  -> Result<ScanSubscription, OpError>   // starts one if none runs
ops.scan().cancel()          -> CancelOutcome                        // never refused
ops.scan().status()          -> ScanState
```

- Sequence: `start`; then `album`, `progress` and `marked_missing` in walk order;
  optionally `cancelling`; then **exactly one** of `done`, `aborted`, `cancelled`,
  `error`; nothing after it.
- Every subscriber replays the current scan from its first event.
- `subscribe` takes the guard before the library check, as the stream route does today; a
  guard that turns out not to be needed (a scan already runs) is released at once.
- `cancel` takes no guard and works while a restore holds the gate. Outcomes:
  `requested`, `already_requested`, `too_late`, `not_running`.
- Source: `musaic-services/src/scan.rs` (module docs), `scan_run::forward_events`,
  `musaic-app/src/scan.rs`.

## Feeds (shared state, observed)

```text
ops.<group>().<feed>() -> Feed<T>
```

| Feed | `T` | Delivery | Source today |
|---|---|---|---|
| Check runs | running-check state (every run start and end), ignore / unignore of a finding, and sweep progress | current state, then one per change (coalesced) | `routes/checks.rs` `checks_stream` |
| AI jobs | the jobs list | current state, then one per change (coalesced) | `routes/jobs.rs`, `musaic-app/src/jobs.rs` |
| Fingerprint pool | pool state | current state, then one per change (coalesced) | `routes/system.rs`, `fingerprint_pool.rs` |
| Library change events | a bus event | every event, bounded queue per subscriber, oldest dropped when full | `musaic-events/src/relay.rs` |

- A feed has no terminal event. It ends when the application begins shutting down.
- A slow subscriber never delays the producer or another subscriber.
- `ChecksRerunFinished` reaches the library-events feed as now: a report, never a trigger.

## What the HTTP face adds, and only it

- SSE framing (`event:` / `data:` lines) and the rendering of each typed event to its
  Python-compatible payload (`musaic-api/src/render/`).
- The `: ping` comment after the ping interval.
- Channel capacity per route, and ending the response body on shutdown.
- Turning a closed connection into `Listening::Gone` or a dropped feed.

## Equivalence check

For each streamed operation, the frames an HTTP stream sends are, in order, the rendering
of the typed events the operation produced, with nothing added but pings (SC-007). The
existing stream tests (`sse_socket.rs`, `scan_cancel_http.rs`, `pool_http.rs`,
`jobs_http.rs`, `analyze_http.rs`, `quick_fix_no_pause.rs`) pin the HTTP side and pass
unmodified.
