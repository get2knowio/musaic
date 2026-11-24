# Contract: desktop routes (added, changed, removed)

Every route is a thin face over one `musaic_app::ops` operation (rule 7) and has exactly one row in `docs/operations.md`. Launch cookie and Host/Origin check unchanged. Inventory: 112 rows → 116 after slice 2 (three batch routes and the group cancel, FR-036) → **112** after slice 4 (71 Python routes → 67; 39 `EXTRA_ROUTES` → 43; 2 aliases). Streamed rows stay 11 (the AI jobs feed is renamed "research requests" in the prose; same route).

Wire shapes below are settled (slice 2) and are what the backend renders. Times are ISO-8601 UTC seconds (`2026-11-23T10:00:00Z`), as every other research time on these routes.

## Added

| Method | Path | Operation | Returns | Errors | `docs/operations.md` row (behaviour column) |
| --- | --- | --- | --- | --- | --- |
| `GET` | `/api/research/batches` | `jobs.research_batches` | list of `BatchSummary` | Internal | thin adapter; reads rows only, any epoch, newest first; a batch with no request left is not listed |
| `GET` | `/api/research/batches/{batch_id}` | `jobs.research_batch` | `BatchDetail` (`BatchSummary` + `items: DelegatedItem[]`) | NotFound, Internal | thin adapter; reads rows only, Missing albums listed |
| `POST` | `/api/research/batches/{batch_id}/cancel` | `jobs.cancel_research_batch` | `BatchSummary` after the cancel, plus `cancelled: n` | NotFound, Busy, Internal | carries behaviour; work guard; cancels every queued, claimed or running item in one transaction (audit, outbox, event history per item); publishes after commit (`AgentStateChanged(work)`) |
| `POST` | `/api/ai/jobs/{job_id}/cancel` | `jobs.cancel_research_group` | `{"id": "<the card's id as asked>", "cancelled": n}` | NotFound (404, `{"detail": "Job not found"}`), Conflict (409, nothing unresolved to cancel: every item awaiting review or finished, or the id is an old job's history record), Busy (409), Internal | carries behaviour (FR-036); work guard; cancels every queued, claimed or running item of one job card (a group id, an old group id, or the old job id its requests name) in one transaction through the same path as "Cancel research" (audit, outbox, event history per item); awaiting-review and finished items untouched; publishes after commit (`AgentStateChanged(work)`). Body ignored. |

`GET /api/research/batches` answers a plain JSON array of `BatchSummary`, newest `startedAt` first (any epoch). `GET /api/research/batches/{batch_id}` answers one `BatchDetail`; an unknown batch, or one with no request left, is 404 `{"detail": "Batch not found"}`. The cancel answers `BatchSummary` with one more key, `"cancelled": n` (0 is a valid answer: nothing was waiting or held).

`BatchSummary`:

```json
{ "batchId": "…", "checkId": "unidentified-album", "startedAt": "2026-11-23T10:00:00Z",
  "finishedAt": null, "unfinished": true, "size": 120,
  "counts": { "queued": 100, "claimed": 5, "running": 3, "awaiting_review": 10, "applied": 1, "discarded": 1, "failed": 0, "cancelled": 0 } }
```

`unfinished` is true when `finishedAt` is null and no quick-fix-all run holds the batch. `counts` always carries all eight states (0 when none); `size` is their sum. `startedAt` and `finishedAt` are ISO-8601 UTC seconds; `finishedAt` is `null` until the run that made the batch reached its end.

`BatchDetail` is `BatchSummary` plus `"items": [DelegatedItem, ...]`: every item of the batch, any epoch and any state (applied and discarded included), held first then queued, awaiting review, failed, cancelled, applied, discarded, newest first within a state. `DelegatedItem` is the item shape of `GET /api/research/work` (below).

`DelegatedItem` (the items of `GET /api/research/work` and of `BatchDetail`), keys in this order:

```json
{ "workId": "…", "groupId": "…", "batchId": "…" , "albumId": "0123456789abcdef", "albumLabel": "Artist - Title",
  "kind": "metadata.identify", "state": "awaiting_review", "revision": 3, "holder": null,
  "lastHeartbeatAt": null, "leaseExpiresAt": null, "attempts": 1, "maxAttempts": 3,
  "lastError": null, "waitingForAgent": false }
```

`groupId` (new) is the id of the job card the item belongs to (the id `GET /api/ai/jobs/{job_id}`, `DELETE /api/ai/jobs/{job_id}`, `POST /api/ai/jobs/{job_id}/cancel` and `GET /api/research/groups/{group}` answer to). `batchId` (new) is the batch reference, `null` for an item no bulk run made. `GET /api/research/work` keeps listing the six states `queued`, `claimed`, `running`, `awaiting_review`, `failed`, `cancelled` of the current epoch, batched items included, so the Library can give every album with a request (batched or not) its pill and find a batched proposal awaiting review (`state == "awaiting_review"`).

## Changed

| Method | Path | Operation | Change |
| --- | --- | --- | --- |
| `POST` | `/api/checks/{check_id}/quick-fix-all` | `fixes.quick_fix_all` | optional query `research=1` (`1` or `true` turn it on; absent or any other value is off). With it, unsettled albums of the four research-capable checks (tags-out-of-sync, unidentified-album, no-cover-art, track-gaps) become requests in one batch; their `progress` frame has `outcome` `"sent-for-research"` (message "Sent for research.") or `"already-requested"` (message "Research is already requested for this album."), and a refused request is `"error"` with its reason. The `done` frame gains, after `still_open`, `"sent_for_research": n, "already_requested": n, "batch_id": "<uuid>" or null` (null when the run made no request), only when `research=1`, so resolver-only frames are byte-identical. `total` = `fixed + skipped + errors + not_applicable + still_open + sent_for_research + already_requested`. Missing albums skipped (not-applicable, before the resolver). Still one `BulkBatchCompleted`. Row text updated |
| `POST` | `/api/checks/{check_id}/findings/{finding_id}/fix` | `fixes.fix` | no legacy branch; always delegates what the resolver cannot settle (it already did with no provider). Message unchanged ("Research requested. You can follow it in Review."). Row text loses `ai_jobs_changed()` |
| `GET` | `/api/ai/jobs` | `jobs.list` | a card whose requests all belong to a batch is left out (the batch is one entry of `GET /api/research/batches`; plan.md, Settled items); history-only records are listed with their old type and status. The body stays `{"jobs": [...]}` |
| `DELETE` | `/api/ai/jobs` | `jobs.delete_many` | validation unchanged; over the single store: `acted_on` removes applied and discarded requests and the history records they leave unlinked, plus acted-on history-only records; `failed` removes failed and cancelled ones likewise; `queued` and `completed` answer `Conflict` like `running` (research D13) |
| `GET`/`DELETE` | `/api/ai/jobs/{job_id}` | `jobs.get` / `jobs.delete` | read and delete from the single store (a group id or a history record id) |
| `GET` | `/api/ai/jobs/{job_id}/cover-preview` | `apply.job_cover_preview` | reads `research_history.result_json` |
| `GET` | `/api/ai/jobs/active/{album_id}` (and alias) | `jobs.active_for_album` | single store only; batched items included |
| `GET` | `/api/ai/jobs/stream` (and alias) | `jobs.feed` | same frames; wakes on delegation changes only; `queue_state` gains `clearable` (the number "Clear applied or discarded" removes, research D13) and `clearable_failed` (the number "Clear failed" removes: failed and cancelled requests, batched ones included, and failed history-only records) |
| `GET` | `/api/research/work` | `jobs.delegated_work` | each item gains `groupId` and `batchId` (see `DelegatedItem` above); batched items are listed like any other |
| `GET` | `/api/settings` | `settings.get` | no `ai_provider_id`, `ai_model_id`, `ai_max_parallel`, `ai_queue_paused`, `ai_api_key_set`, `firecrawl_api_key_set` |
| `PATCH` | `/api/settings` | `settings.patch` | those names are ignored like any unknown key; no provider validation; no `ai_jobs_changed()` |
| `GET` | `/api/db/info` | `database.info` | the `ai_jobs` count key keeps its name and counts requests plus history-only records |

## Removed (rows deleted from `docs/operations.md`)

| Method | Path | Operation removed | Why |
| --- | --- | --- | --- |
| `GET` | `/api/ai/providers` | `jobs.providers` | provider list |
| `POST` | `/api/ai/queue/resume` | `jobs.resume_queue` | queue pause |
| `POST` | `/api/ai/jobs/{job_id}/retry` | `jobs.retry` | provider retry of an old job; research retry stays on `POST /api/research/{id}/decision` |
| `POST` | `/api/ai/jobs/{job_id}/acted-on` | `jobs.mark_acted_on` | acted only on unlinked old jobs, which v24 makes history-only (not reviewable) |

`EXTRA_ROUTES` entries, `golden/api/routes.json` (71 → 67 entries; `musaic-api/tests/inventory.rs:82,312` count), `golden/api/route_order.json` (routes and probes for the four paths), `captures/api-offline/api/081-get-api-ai-providers.json` and its `index.json` entry, and the `openapi.json` captures change with this table (see plan, Goldens).

## Agent bridge

No route added or removed: the two new tools go through `POST /__musaic/agent/v1/tools/{tool_id}`. `ROUTES` in `agent_guard.rs` is unchanged.
